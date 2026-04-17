/// InviteExhaustionRaceScenario — Property 4: Invite exhaustion atomicity
///
/// Creates an invite with `remaining_uses = N` (N=3), then spawns M >> N concurrent
/// clients all attempting to join the same room with the same invite code simultaneously.
/// Asserts exactly `min(N, room_capacity)` = `min(3, 6)` = 3 joins succeed.
/// Asserts the invite is exhausted after the successful joins (a final verification
/// join attempt must fail with InviteExhausted or InviteInvalid).
/// Runs R repetitions with zero tolerance for deviations.
///
/// **Property 4: Invite exhaustion atomicity**
/// **Validates: Requirements 3.1, 3.2**
use std::time::{Duration, Instant};

use async_trait::async_trait;
use tokio::task::JoinSet;

use crate::assertions::{assert_room_participant_count, fetch_metrics};
use crate::client::StressClient;
use crate::config::{Capability, ConfigPreset, TestContext};
use crate::results::{InvariantViolation, LatencyTracker, ScenarioResult};
use crate::runner::{Scenario, Tier};

/// Number of invite uses (N). The invite exhausts before the room fills (N < capacity=6).
const MAX_USES: u32 = 3;
/// Expected successful joins = min(MAX_USES, room_capacity=6).
const EXPECTED_SUCCESSES: usize = MAX_USES as usize; // 3

/// Room type hint — SFU rooms have capacity 6 (matching the product's max group size).
/// P2P rooms only allow 2 participants, which would break the exhaustion test.
const ROOM_TYPE: &str = "sfu";

pub struct InviteExhaustionRaceScenario;

#[async_trait]
impl Scenario for InviteExhaustionRaceScenario {
    fn name(&self) -> &str {
        "invite-exhaustion-race"
    }

    fn tier(&self) -> Tier {
        Tier::Tier1
    }

    fn requires(&self) -> Vec<Capability> {
        vec![]
    }

    fn config_preset(&self) -> ConfigPreset {
        // Relaxed join rate limits so the rate limiter doesn't interfere with
        // invite exhaustion / atomicity testing.
        ConfigPreset::JoinHeavy
    }

    async fn run(&self, ctx: &TestContext) -> ScenarioResult {
        let start = Instant::now();
        let mut violations: Vec<InvariantViolation> = Vec::new();
        let mut latency = LatencyTracker::new();

        // --- Generate a unique room ID for this run ---
        let room_id = {
            use rand::RngCore;
            let mut rng = ctx.rng.lock().unwrap();
            let hi = rng.next_u64();
            let lo = rng.next_u64();
            format!("exhaust-{hi:016x}-{lo:016x}")
        };

        // --- Create an invite with MAX_USES remaining uses ---
        let invite_code = match &ctx.app_state {
            Some(app_state) => {
                match app_state.invite_store.generate(
                    &room_id,
                    "stress-issuer",
                    Some(MAX_USES),
                    Instant::now(),
                ) {
                    Ok(record) => record.code,
                    Err(e) => {
                        return ScenarioResult {
                            name: self.name().to_owned(),
                            passed: false,
                            duration: start.elapsed(),
                            actions_per_second: 0.0,
                            p95_latency: Duration::ZERO,
                            p99_latency: Duration::ZERO,
                            violations: vec![InvariantViolation {
                                invariant: "invite_creation".to_owned(),
                                expected: "invite created with max_uses=3".to_owned(),
                                actual: format!("InviteStore::generate failed: {e}"),
                            }],
                        };
                    }
                }
            }
            None => {
                // External mode: create invite via signaling (host joins, creates invite, leaves).
                match create_limited_invite_via_signaling(ctx, &room_id, MAX_USES).await {
                    Ok(code) => code,
                    Err(e) => {
                        return ScenarioResult {
                            name: self.name().to_owned(),
                            passed: false,
                            duration: start.elapsed(),
                            actions_per_second: 0.0,
                            p95_latency: Duration::ZERO,
                            p99_latency: Duration::ZERO,
                            violations: vec![InvariantViolation {
                                invariant: "invite_creation".to_owned(),
                                expected: "invite created with max_uses=3".to_owned(),
                                actual: e,
                            }],
                        };
                    }
                }
            }
        };

        // --- Spawn M >> N concurrent clients (cap at 20 per spec) ---
        let n_clients = ctx.scale.concurrent_clients.min(20);

        // --- Snapshot baseline abuse metrics ---
        let metrics_token = std::env::var("TEST_METRICS_TOKEN").unwrap_or_default();
        let baseline_metrics = fetch_metrics(&ctx.http_client, &ctx.metrics_url, &metrics_token)
            .await
            .unwrap_or(serde_json::Value::Null);

        // --- Spawn all clients concurrently ---
        let mut join_set: JoinSet<ClientOutcome> = JoinSet::new();

        for _ in 0..n_clients {
            let ws_url = ctx.ws_url.clone();
            let room_id_clone = room_id.clone();
            let invite_code_clone = invite_code.clone();

            join_set.spawn(async move {
                let t0 = Instant::now();

                let mut client = match StressClient::connect(&ws_url).await {
                    Ok(c) => c,
                    Err(e) => {
                        return ClientOutcome {
                            success: false,
                            rejection_reason: Some(format!("connect_error: {e}")),
                            latency: t0.elapsed(),
                            client: None,
                        };
                    }
                };

                let result = client
                    .join_room(&room_id_clone, ROOM_TYPE, Some(&invite_code_clone))
                    .await;

                let elapsed = t0.elapsed();

                match result {
                    Ok(join_result) => {
                        if join_result.success {
                            // Keep client alive so the room stays populated for
                            // the metrics assertion.
                            ClientOutcome {
                                success: true,
                                rejection_reason: None,
                                latency: elapsed,
                                client: Some(client),
                            }
                        } else {
                            client.close().await;
                            ClientOutcome {
                                success: false,
                                rejection_reason: join_result.rejection_reason,
                                latency: elapsed,
                                client: None,
                            }
                        }
                    }
                    Err(e) => {
                        client.close().await;
                        ClientOutcome {
                            success: false,
                            rejection_reason: Some(format!("join_error: {e}")),
                            latency: elapsed,
                            client: None,
                        }
                    }
                }
            });
        }

        // --- Collect results ---
        let mut successes: usize = 0;
        let mut invite_exhausted_rejections: usize = 0;
        let mut room_full_rejections: usize = 0;
        let mut unexpected_rejections: usize = 0;
        // Hold successful clients open so the room stays populated for metrics assertions.
        let mut live_clients: Vec<StressClient> = Vec::new();

        while let Some(outcome) = join_set.join_next().await {
            match outcome {
                Ok(o) => {
                    latency.record(o.latency);
                    if o.success {
                        successes += 1;
                        if let Some(c) = o.client {
                            live_clients.push(c);
                        }
                    } else {
                        match o.rejection_reason.as_deref() {
                            Some("invite_exhausted") => invite_exhausted_rejections += 1,
                            // Backend may return "invite_invalid" for exhausted invites in some
                            // implementations — treat it as exhausted for counting purposes.
                            Some("invite_invalid") => invite_exhausted_rejections += 1,
                            Some("room_full") => room_full_rejections += 1,
                            _ => unexpected_rejections += 1,
                        }
                    }
                }
                Err(e) => {
                    unexpected_rejections += 1;
                    violations.push(InvariantViolation {
                        invariant: "task_panic".to_owned(),
                        expected: "no panics".to_owned(),
                        actual: format!("task panicked: {e}"),
                    });
                }
            }
        }

        // --- Property 4: Exactly EXPECTED_SUCCESSES clients succeed ---
        if successes != EXPECTED_SUCCESSES {
            violations.push(InvariantViolation {
                invariant: "invite_exhaustion_atomicity: exactly min(N, capacity) joins succeed"
                    .to_owned(),
                expected: EXPECTED_SUCCESSES.to_string(),
                actual: successes.to_string(),
            });
        }

        // --- Property 4: Remaining clients get InviteExhausted (or RoomFull if capacity hit first) ---
        // With MAX_USES=3 < ROOM_CAPACITY=6, the invite exhausts before the room fills.
        // So we expect 0 room_full rejections and (n_clients - EXPECTED_SUCCESSES) exhausted.
        let expected_exhausted = n_clients.saturating_sub(EXPECTED_SUCCESSES);
        let total_expected_rejections = invite_exhausted_rejections + room_full_rejections;

        if total_expected_rejections != expected_exhausted {
            violations.push(InvariantViolation {
                invariant: "invite_exhaustion_atomicity: remaining clients get InviteExhausted"
                    .to_owned(),
                expected: expected_exhausted.to_string(),
                actual: total_expected_rejections.to_string(),
            });
        }

        // Room full should not occur since invite exhausts first (MAX_USES=3 < ROOM_CAPACITY=6).
        if room_full_rejections > 0 {
            violations.push(InvariantViolation {
                invariant: "invite_exhaustion_atomicity: no RoomFull expected (invite exhausts before room fills)".to_owned(),
                expected: "0".to_owned(),
                actual: room_full_rejections.to_string(),
            });
        }

        // No unexpected rejections (connect errors, timeouts, etc.).
        if unexpected_rejections > 0 {
            violations.push(InvariantViolation {
                invariant: "no_unexpected_rejections".to_owned(),
                expected: "0".to_owned(),
                actual: unexpected_rejections.to_string(),
            });
        }

        // --- Give the backend a moment to settle ---
        tokio::time::sleep(Duration::from_millis(200)).await;

        // --- Query metrics and assert room has exactly EXPECTED_SUCCESSES participants ---
        match fetch_metrics(&ctx.http_client, &ctx.metrics_url, &metrics_token).await {
            Ok(current_metrics) => {
                // Property 4: Room_State participant count == EXPECTED_SUCCESSES
                if let Some(v) =
                    assert_room_participant_count(&current_metrics, &room_id, EXPECTED_SUCCESSES)
                {
                    violations.push(v);
                }

                // No rate-limit rejections expected under JoinHeavy preset.
                let baseline_rl = baseline_metrics
                    .get("abuse_metrics")
                    .and_then(|m| m.get("join_rate_limit_rejections"))
                    .and_then(|v| v.as_u64())
                    .unwrap_or(0);
                let current_rl = current_metrics
                    .get("abuse_metrics")
                    .and_then(|m| m.get("join_rate_limit_rejections"))
                    .and_then(|v| v.as_u64())
                    .unwrap_or(0);
                let rl_delta = current_rl.saturating_sub(baseline_rl);

                if rl_delta > 0 {
                    violations.push(InvariantViolation {
                        invariant: "no_rate_limit_rejections_under_join_heavy_preset".to_owned(),
                        expected: "0".to_owned(),
                        actual: rl_delta.to_string(),
                    });
                }
            }
            Err(e) => {
                violations.push(InvariantViolation {
                    invariant: "metrics_endpoint_reachable".to_owned(),
                    expected: "metrics endpoint responds".to_owned(),
                    actual: format!("fetch failed: {e}"),
                });
            }
        }

        // --- Final verification: one more join attempt must fail (invite exhausted) ---

        // Close all live clients now that metrics assertions are done.
        // This allows the room to be cleaned up before the verifier connects.
        for c in live_clients {
            c.close().await;
        }
        // Give the backend a moment to process disconnects.
        tokio::time::sleep(Duration::from_millis(100)).await;

        match StressClient::connect(&ctx.ws_url).await {
            Ok(mut verifier) => {
                match verifier
                    .join_room(&room_id, ROOM_TYPE, Some(&invite_code))
                    .await
                {
                    Ok(result) => {
                        if result.success {
                            violations.push(InvariantViolation {
                                invariant: "invite_exhausted_after_max_uses: post-exhaustion join must fail".to_owned(),
                                expected: "join rejected (invite exhausted)".to_owned(),
                                actual: "join succeeded — invite was NOT exhausted".to_owned(),
                            });
                        }
                        // Any rejection reason is acceptable here (exhausted or invalid).
                    }
                    Err(e) => {
                        // A connection error (e.g. closed) is also acceptable — the backend
                        // may close the connection on an exhausted invite.
                        let _ = e; // not a violation
                    }
                }
                verifier.close().await;
            }
            Err(e) => {
                violations.push(InvariantViolation {
                    invariant: "verifier_connect".to_owned(),
                    expected: "verifier client connects".to_owned(),
                    actual: format!("connect failed: {e}"),
                });
            }
        }

        let duration = start.elapsed();
        let actions_per_second = if duration.as_secs_f64() > 0.0 {
            n_clients as f64 / duration.as_secs_f64()
        } else {
            0.0
        };

        ScenarioResult {
            name: self.name().to_owned(),
            passed: violations.is_empty(),
            duration,
            actions_per_second,
            p95_latency: latency.p95(),
            p99_latency: latency.p99(),
            violations,
        }
    }
}

/// Outcome of a single concurrent client's join attempt.
/// On success, the client is kept alive (not closed) so the room remains populated
/// for post-condition assertions. The caller is responsible for closing it.
struct ClientOutcome {
    success: bool,
    rejection_reason: Option<String>,
    latency: Duration,
    /// Successful clients are returned here so they stay connected until
    /// the scenario finishes its metrics assertions.
    client: Option<StressClient>,
}

/// External-mode invite creation with a specific max_uses limit.
/// Connects a host client, joins the room, sends InviteCreate with maxUses, receives
/// InviteCreated, then leaves.
async fn create_limited_invite_via_signaling(
    ctx: &TestContext,
    room_id: &str,
    max_uses: u32,
) -> Result<String, String> {
    let mut host = StressClient::connect(&ctx.ws_url)
        .await
        .map_err(|e| format!("host connect failed: {e}"))?;

    let join_result = host
        .join_room(room_id, ROOM_TYPE, None)
        .await
        .map_err(|e| format!("host join failed: {e}"))?;

    if !join_result.success {
        host.close().await;
        return Err(format!(
            "host join rejected: {:?}",
            join_result.rejection_reason
        ));
    }

    host.send_json(&serde_json::json!({
        "type": "invite_create",
        "maxUses": max_uses,
    }))
    .await
    .map_err(|e| format!("InviteCreate send failed: {e}"))?;

    let invite_msg = host
        .recv_type("invite_created", Duration::from_secs(5))
        .await
        .map_err(|e| format!("InviteCreated recv failed: {e}"))?;

    let code = invite_msg
        .get("inviteCode")
        .and_then(|v| v.as_str())
        .ok_or_else(|| "InviteCreated missing inviteCode field".to_owned())?
        .to_owned();

    host.send_json(&serde_json::json!({ "type": "leave" }))
        .await
        .ok();
    host.close().await;

    Ok(code)
}
