/// InviteRevocationRaceScenario — Property 5: Post-revocation join rejection
///
/// Two-phase test:
///   Phase 1: Fill the room to capacity with valid invite joins (up to 6 clients).
///   Revoke the invite while phase-1 clients hold their slots.
///   Phase 2: Disconnect phase-1 clients to free slots, then send a second wave
///   of clients. These must all be rejected with InviteRevoked (not RoomFull).
///
/// Asserts that:
///   - Phase 1 successes do not exceed room capacity (6)
///   - At least 1 phase-2 client observes `InviteRevoked`
///   - No phase-2 client succeeds (the key race property)
///   - A final verification join fails (invite is permanently revoked)
///
/// **Property 5: Post-revocation join rejection**
/// **Validates: Requirements 3.3**
use std::time::{Duration, Instant};

use async_trait::async_trait;
use tokio::task::JoinSet;

use crate::assertions::fetch_metrics;
use crate::client::StressClient;
use crate::config::{Capability, ConfigPreset, TestContext};
use crate::results::{InvariantViolation, LatencyTracker, ScenarioResult};
use crate::runner::{Scenario, Tier};

/// Number of concurrent clients in each wave.
const PHASE1_CLIENTS: usize = 10;
const PHASE2_CLIENTS: usize = 10;
/// Max uses — high enough that the invite won't exhaust during phase 1.
const MAX_USES: u32 = 100;

/// Room type hint — SFU rooms have capacity 6 (matching the product's max group size).
const ROOM_TYPE: &str = "sfu";
const ROOM_CAPACITY: usize = 6;

pub struct InviteRevocationRaceScenario;

#[async_trait]
impl Scenario for InviteRevocationRaceScenario {
    fn name(&self) -> &str {
        "invite-revocation-race"
    }

    fn tier(&self) -> Tier {
        Tier::Tier1
    }

    fn requires(&self) -> Vec<Capability> {
        vec![]
    }

    fn config_preset(&self) -> ConfigPreset {
        ConfigPreset::JoinHeavy
    }

    async fn run(&self, ctx: &TestContext) -> ScenarioResult {
        let start = Instant::now();
        let mut violations: Vec<InvariantViolation> = Vec::new();
        let mut latency = LatencyTracker::new();

        // --- Generate a unique room ID ---
        let room_id = {
            use rand::RngCore;
            let mut rng = ctx.rng.lock().unwrap();
            let hi = rng.next_u64();
            let lo = rng.next_u64();
            format!("revoke-{hi:016x}-{lo:016x}")
        };

        // --- Create invite with high max_uses so it won't exhaust before revocation ---
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
                                expected: format!("invite created with max_uses={MAX_USES}"),
                                actual: format!("InviteStore::generate failed: {e}"),
                            }],
                        };
                    }
                }
            }
            None => match create_invite_via_signaling(ctx, &room_id, MAX_USES).await {
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
                            expected: format!("invite created with max_uses={MAX_USES}"),
                            actual: e,
                        }],
                    };
                }
            },
        };

        // =====================================================================
        // PHASE 1: Fill the room to capacity with valid invite joins.
        // Successful clients stay connected to hold their slots.
        // =====================================================================
        let mut join_set: JoinSet<ClientOutcome> = JoinSet::new();

        for _ in 0..PHASE1_CLIENTS {
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
                    Ok(jr) if jr.success => ClientOutcome {
                        success: true,
                        rejection_reason: None,
                        latency: elapsed,
                        client: Some(client),
                    },
                    Ok(jr) => {
                        client.close().await;
                        ClientOutcome {
                            success: false,
                            rejection_reason: jr.rejection_reason,
                            latency: elapsed,
                            client: None,
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

        // Collect phase-1 results.
        let mut phase1_successes: usize = 0;
        let mut live_clients: Vec<StressClient> = Vec::new();

        while let Some(result) = join_set.join_next().await {
            match result {
                Ok(mut o) => {
                    latency.record(o.latency);
                    if o.success {
                        phase1_successes += 1;
                        if let Some(c) = o.client.take() {
                            live_clients.push(c);
                        }
                    }
                }
                Err(e) => {
                    violations.push(InvariantViolation {
                        invariant: "phase1_task_panic".to_owned(),
                        expected: "no panics".to_owned(),
                        actual: format!("task panicked: {e}"),
                    });
                }
            }
        }

        // Phase-1 capacity sanity.
        if phase1_successes > ROOM_CAPACITY {
            violations.push(InvariantViolation {
                invariant: "phase1_capacity: successes must not exceed room capacity".to_owned(),
                expected: format!("<= {ROOM_CAPACITY}"),
                actual: phase1_successes.to_string(),
            });
        }

        // =====================================================================
        // REVOKE the invite while phase-1 clients hold their slots.
        // =====================================================================
        match &ctx.app_state {
            Some(app_state) => {
                let _ = app_state.invite_store.revoke(&invite_code);
            }
            None => {
                // External mode: send InviteRevoke via a connected host client.
                if let Ok(mut host) = StressClient::connect(&ctx.ws_url).await {
                    if let Ok(r) = host.join_room(&room_id, ROOM_TYPE, None).await
                        && r.success
                    {
                        let _ = host
                            .send_json(&serde_json::json!({
                                "type": "invite_revoke",
                                "inviteCode": invite_code,
                            }))
                            .await;
                        // Wait for revocation to propagate.
                        tokio::time::sleep(Duration::from_millis(100)).await;
                    }
                    host.close().await;
                }
            }
        }

        // Small settle time to ensure revocation is visible.
        tokio::time::sleep(Duration::from_millis(50)).await;

        // =====================================================================
        // Disconnect phase-1 clients to free room slots.
        // =====================================================================
        for c in live_clients {
            c.close().await;
        }
        // Give the backend time to process disconnects and free slots.
        tokio::time::sleep(Duration::from_millis(200)).await;

        // =====================================================================
        // PHASE 2: Send a second wave of clients. The invite is revoked and
        // slots are free, so they should all get InviteRevoked (not RoomFull).
        // =====================================================================
        let mut join_set2: JoinSet<ClientOutcome> = JoinSet::new();

        for _ in 0..PHASE2_CLIENTS {
            let ws_url = ctx.ws_url.clone();
            let room_id_clone = room_id.clone();
            let invite_code_clone = invite_code.clone();

            join_set2.spawn(async move {
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
                client.close().await;

                match result {
                    Ok(jr) => ClientOutcome {
                        success: jr.success,
                        rejection_reason: jr.rejection_reason,
                        latency: elapsed,
                        client: None,
                    },
                    Err(e) => ClientOutcome {
                        success: false,
                        rejection_reason: Some(format!("join_error: {e}")),
                        latency: elapsed,
                        client: None,
                    },
                }
            });
        }

        // Collect phase-2 results.
        let mut phase2_successes: usize = 0;
        let mut invite_revoked_count: usize = 0;
        let mut room_full_count: usize = 0;
        let mut other_rejections: usize = 0;

        while let Some(result) = join_set2.join_next().await {
            match result {
                Ok(o) => {
                    latency.record(o.latency);
                    if o.success {
                        phase2_successes += 1;
                    } else {
                        match o.rejection_reason.as_deref() {
                            Some("invite_revoked") | Some("invite_invalid") => {
                                invite_revoked_count += 1;
                            }
                            Some("room_full") => room_full_count += 1,
                            _ => other_rejections += 1,
                        }
                    }
                }
                Err(e) => {
                    violations.push(InvariantViolation {
                        invariant: "phase2_task_panic".to_owned(),
                        expected: "no panics".to_owned(),
                        actual: format!("task panicked: {e}"),
                    });
                }
            }
        }

        // --- Property 5: No phase-2 successes (invite is revoked) ---
        if phase2_successes > 0 {
            violations.push(InvariantViolation {
                invariant: "post_revocation_join_rejection: no joins succeed after revocation"
                    .to_owned(),
                expected: "0 post-revocation successes".to_owned(),
                actual: format!("{phase2_successes} client(s) succeeded after revocation"),
            });
        }

        // --- Property 5: At least 1 client observed InviteRevoked ---
        if invite_revoked_count == 0 {
            violations.push(InvariantViolation {
                invariant: "revocation_observed: at least 1 client must observe InviteRevoked"
                    .to_owned(),
                expected: ">= 1 InviteRevoked rejection".to_owned(),
                actual: format!(
                    "0 InviteRevoked rejections (successes={phase2_successes}, room_full={room_full_count}, other={other_rejections})"
                ),
            });
        }

        // --- Give backend a moment to settle, then query metrics ---
        let metrics_token = std::env::var("TEST_METRICS_TOKEN").unwrap_or_default();
        match fetch_metrics(&ctx.http_client, &ctx.metrics_url, &metrics_token).await {
            Ok(_metrics) => {}
            Err(e) => {
                violations.push(InvariantViolation {
                    invariant: "metrics_endpoint_reachable".to_owned(),
                    expected: "metrics endpoint responds".to_owned(),
                    actual: format!("fetch failed: {e}"),
                });
            }
        }

        // --- Final verification: a new join attempt must fail (invite is revoked) ---
        match StressClient::connect(&ctx.ws_url).await {
            Ok(mut verifier) => {
                match verifier
                    .join_room(&room_id, ROOM_TYPE, Some(&invite_code))
                    .await
                {
                    Ok(result) => {
                        if result.success {
                            violations.push(InvariantViolation {
                                invariant: "post_revocation_join_rejection: final verification join must fail".to_owned(),
                                expected: "join rejected (invite revoked or invalid)".to_owned(),
                                actual: "join succeeded — invite was NOT revoked".to_owned(),
                            });
                        }
                    }
                    Err(_e) => {
                        // Connection error is acceptable — backend may close on revoked invite.
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

        let total_clients = PHASE1_CLIENTS + PHASE2_CLIENTS;
        let duration = start.elapsed();
        let actions_per_second = if duration.as_secs_f64() > 0.0 {
            total_clients as f64 / duration.as_secs_f64()
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
    /// Successful clients are returned here so they stay connected.
    client: Option<StressClient>,
}

/// External-mode: connect a host client, join the room, create an invite with max_uses,
/// then leave. Returns the invite code.
async fn create_invite_via_signaling(
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
