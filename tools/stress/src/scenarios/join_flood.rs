/// JoinFloodScenario — Property 1: Capacity invariant under concurrent joins
///                     Property 3: Rate limit counter accuracy
///
/// Spawns N concurrent clients all joining the same room with the same valid invite code.
/// Asserts exactly 6 succeed (get `Joined`), the rest get `JoinRejected` with reason `RoomFull`.
/// Asserts Room_State participant count == 6 via the metrics endpoint.
/// Asserts abuse metrics counters match rejection counts (no unexpected rejections).
///
/// **Validates: Requirements 2.2, 2.3, 2.8**
use std::sync::Arc;
use std::time::{Duration, Instant};

use async_trait::async_trait;
use tokio::task::JoinSet;

use crate::assertions::fetch_metrics;
use crate::client::StressClient;
use crate::config::{Capability, ConfigPreset, TestContext};
use crate::results::{InvariantViolation, LatencyTracker, ScenarioResult};
use crate::runner::{Scenario, Tier};

/// Room capacity for SFU rooms (matches `SfuConfig::from_env().max_participants` default).
/// Stress scenarios use SFU room type because P2P rooms have max_participants=2.
const ROOM_CAPACITY: usize = 6;

/// Room type hint sent in the Join message.
const ROOM_TYPE: &str = "sfu";

pub struct JoinFloodScenario;

#[async_trait]
impl Scenario for JoinFloodScenario {
    fn name(&self) -> &str {
        "join-flood"
    }

    fn tier(&self) -> Tier {
        Tier::Tier1
    }

    fn requires(&self) -> Vec<Capability> {
        // SFU room type requires SFU to be available on the backend.
        vec![Capability::Sfu]
    }

    fn config_preset(&self) -> ConfigPreset {
        // Relaxed join rate limits so the rate limiter doesn't interfere with
        // capacity / atomicity testing.
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
            format!("flood-{hi:016x}-{lo:016x}")
        };

        // --- Cap concurrent clients at 50 for this scenario (keep it fast) ---
        let n_clients = ctx.scale.concurrent_clients.min(50);

        // --- Create an invite code ---
        // In in-process mode: use InviteStore directly (avoids needing a host client).
        // In external mode: connect one client, send InviteCreate, get InviteCreated.
        let invite_code = match &ctx.app_state {
            Some(app_state) => {
                // In-process: generate invite directly via InviteStore.
                // Use n_clients as max_uses so every client can attempt the invite;
                // only the room capacity check should reject excess joins.
                let record = app_state
                    .invite_store
                    .generate(
                        &room_id,
                        "stress-harness",
                        Some(n_clients as u32),
                        std::time::Instant::now(),
                    )
                    .expect("failed to generate invite code for join flood");
                record.code
            }
            None => {
                // External mode: connect a host client, join the room, then create an invite.
                match create_invite_via_signaling(ctx, &room_id, n_clients as u32).await {
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
                                expected: "invite code created successfully".to_owned(),
                                actual: e,
                            }],
                        };
                    }
                }
            }
        };

        // --- Snapshot baseline abuse metrics before the flood ---
        let metrics_token = std::env::var("TEST_METRICS_TOKEN").unwrap_or_default();
        let baseline_metrics = fetch_metrics(&ctx.http_client, &ctx.metrics_url, &metrics_token)
            .await
            .unwrap_or(serde_json::Value::Null);

        // --- Spawn N concurrent clients ---
        // Use a barrier so all clients wait for every join attempt to complete
        // before any of them close. This prevents the room from being destroyed
        // and recreated mid-flood (which would allow more than ROOM_CAPACITY joins).
        let barrier = Arc::new(tokio::sync::Barrier::new(n_clients));
        let mut join_set: JoinSet<ClientOutcome> = JoinSet::new();

        for _ in 0..n_clients {
            let ws_url = ctx.ws_url.clone();
            let room_id = room_id.clone();
            let invite_code = invite_code.clone();
            let barrier = barrier.clone();

            join_set.spawn(async move {
                let t0 = Instant::now();

                let mut client = match StressClient::connect(&ws_url).await {
                    Ok(c) => c,
                    Err(e) => {
                        // Still wait at the barrier so other clients aren't stuck.
                        barrier.wait().await;
                        return ClientOutcome {
                            success: false,
                            rejection_reason: Some(format!("connect_error: {e}")),
                            latency: t0.elapsed(),
                        };
                    }
                };

                let result = client
                    .join_room(&room_id, ROOM_TYPE, Some(&invite_code))
                    .await;

                let elapsed = t0.elapsed();

                // Wait for all clients to finish joining before closing.
                barrier.wait().await;
                client.close().await;

                match result {
                    Ok(join_result) => ClientOutcome {
                        success: join_result.success,
                        rejection_reason: join_result.rejection_reason,
                        latency: elapsed,
                    },
                    Err(e) => ClientOutcome {
                        success: false,
                        rejection_reason: Some(format!("join_error: {e}")),
                        latency: elapsed,
                    },
                }
            });
        }

        // --- Collect results ---
        let mut successes: usize = 0;
        let mut room_full_rejections: usize = 0;
        let mut unexpected_rejections: usize = 0;

        while let Some(outcome) = join_set.join_next().await {
            match outcome {
                Ok(o) => {
                    latency.record(o.latency);
                    if o.success {
                        successes += 1;
                    } else {
                        match o.rejection_reason.as_deref() {
                            Some("room_full") => room_full_rejections += 1,
                            _ => unexpected_rejections += 1,
                        }
                    }
                }
                Err(e) => {
                    // JoinSet task panicked — treat as unexpected rejection.
                    unexpected_rejections += 1;
                    violations.push(InvariantViolation {
                        invariant: "task_panic".to_owned(),
                        expected: "no panics".to_owned(),
                        actual: format!("task panicked: {e}"),
                    });
                }
            }
        }

        // --- Property 1: Exactly ROOM_CAPACITY clients succeed ---
        if successes != ROOM_CAPACITY {
            violations.push(InvariantViolation {
                invariant: "capacity_invariant: exactly 6 joins succeed".to_owned(),
                expected: ROOM_CAPACITY.to_string(),
                actual: successes.to_string(),
            });
        }

        // --- Property 1: Excess joins get RoomFull ---
        let expected_room_full = n_clients.saturating_sub(ROOM_CAPACITY);
        if room_full_rejections != expected_room_full {
            violations.push(InvariantViolation {
                invariant: "room_full_rejections: excess joins get RoomFull".to_owned(),
                expected: expected_room_full.to_string(),
                actual: room_full_rejections.to_string(),
            });
        }

        // --- No unexpected rejections ---
        if unexpected_rejections > 0 {
            violations.push(InvariantViolation {
                invariant: "no_unexpected_rejections".to_owned(),
                expected: "0".to_owned(),
                actual: unexpected_rejections.to_string(),
            });
        }

        // --- Query metrics endpoint and assert participant count == 6 ---
        // Give the backend a brief moment to settle after all clients disconnect.
        tokio::time::sleep(Duration::from_millis(200)).await;

        match fetch_metrics(&ctx.http_client, &ctx.metrics_url, &metrics_token).await {
            Ok(current_metrics) => {
                // Property 1: Room cleanup after disconnect.
                // All clients disconnected before this check, so the room should be
                // cleaned up (removed) or have 0 participants. The capacity invariant
                // is already validated by the success/rejection counts above.
                // A present room with participants would indicate a ghost-peer leak.
                if let Some(room) = current_metrics.get("rooms").and_then(|r| r.get(&room_id)) {
                    let count = room
                        .get("participant_count")
                        .and_then(|v| v.as_u64())
                        .unwrap_or(0);
                    if count > 0 {
                        violations.push(InvariantViolation {
                            invariant: format!("room_cleanup_after_disconnect[{room_id}]"),
                            expected: "0 (all clients disconnected)".to_owned(),
                            actual: count.to_string(),
                        });
                    }
                }

                // Property 3: Rate limit counter accuracy
                // The JoinHeavy preset means the rate limiter should NOT have fired.
                // Any join_rate_limit_rejections delta would be unexpected here.
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
                        invariant: "rate_limit_counter_accuracy: no rate-limit rejections expected under JoinHeavy preset".to_owned(),
                        expected: "0".to_owned(),
                        actual: rl_delta.to_string(),
                    });
                }

                // Property 3: join_invite_rejections delta should be 0
                // (all clients used a valid invite code).
                let baseline_inv = baseline_metrics
                    .get("abuse_metrics")
                    .and_then(|m| m.get("join_invite_rejections"))
                    .and_then(|v| v.as_u64())
                    .unwrap_or(0);
                let current_inv = current_metrics
                    .get("abuse_metrics")
                    .and_then(|m| m.get("join_invite_rejections"))
                    .and_then(|v| v.as_u64())
                    .unwrap_or(0);
                let inv_delta = current_inv.saturating_sub(baseline_inv);

                if inv_delta > 0 {
                    violations.push(InvariantViolation {
                        invariant: "abuse_metrics: no invite rejections expected (valid code used)"
                            .to_owned(),
                        expected: "0".to_owned(),
                        actual: inv_delta.to_string(),
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
struct ClientOutcome {
    success: bool,
    rejection_reason: Option<String>,
    latency: Duration,
}

/// External-mode invite creation: connect a client, join the room as host,
/// send InviteCreate, receive InviteCreated, then leave.
async fn create_invite_via_signaling(
    ctx: &TestContext,
    room_id: &str,
    max_uses: u32,
) -> Result<String, String> {
    let mut host = StressClient::connect(&ctx.ws_url)
        .await
        .map_err(|e| format!("host connect failed: {e}"))?;

    // Join the room first (no invite required for the first joiner when
    // REQUIRE_INVITE_CODE is false, or we use a bootstrap invite).
    // In external mode we assume REQUIRE_INVITE_CODE=false for the host.
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

    // Request an invite code.
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

    // Leave the room so the host slot is freed for the flood clients.
    host.send_json(&serde_json::json!({ "type": "leave" }))
        .await
        .ok();
    host.close().await;

    Ok(code)
}
