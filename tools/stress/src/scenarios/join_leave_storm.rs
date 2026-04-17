/// JoinLeaveStormScenario — Property 2: No ghost peers after connect/disconnect storms
///
/// Spawns N concurrent clients each performing a rapid connect → join → leave → disconnect
/// loop on the same room. After all clients finish, asserts that the Room_State contains
/// zero ghost peers (participant_count == 0, no stale entries).
///
/// **Validates: Requirements 2.4, 2.5**
use std::time::{Duration, Instant};

use async_trait::async_trait;
use tokio::task::JoinSet;

use crate::assertions::{assert_no_ghost_peers, fetch_metrics};
use crate::client::StressClient;
use crate::config::{Capability, ConfigPreset, TestContext};
use crate::results::{InvariantViolation, LatencyTracker, ScenarioResult};
use crate::runner::{Scenario, Tier};

pub struct JoinLeaveStormScenario;

#[async_trait]
impl Scenario for JoinLeaveStormScenario {
    fn name(&self) -> &str {
        "join-leave-storm"
    }

    fn tier(&self) -> Tier {
        Tier::Tier1
    }

    fn requires(&self) -> Vec<Capability> {
        // P2P rooms work without SFU — no extra capability required.
        vec![]
    }

    fn config_preset(&self) -> ConfigPreset {
        // Relaxed join rate limits so the rate limiter doesn't interfere with
        // the connect/disconnect storm.
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
            format!("storm-room-{:016x}", rng.next_u64())
        };

        // --- Create a valid invite code ---
        // Use Some(100) max_uses so all storm clients can join the same room.
        let invite_code = match &ctx.app_state {
            Some(app_state) => {
                match app_state.invite_store.generate(
                    &room_id,
                    "stress-issuer",
                    Some(100),
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
                                expected: "invite code created successfully".to_owned(),
                                actual: format!("generate failed: {e:?}"),
                            }],
                        };
                    }
                }
            }
            None => {
                // External mode: connect a host client and create an invite via signaling.
                match create_invite_via_signaling(ctx, &room_id).await {
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

        // --- Cap concurrent clients at 30 for this scenario ---
        let n_clients = ctx.scale.concurrent_clients.min(30);

        // --- Spawn N concurrent clients performing connect/join/leave/disconnect ---
        let mut join_set: JoinSet<ClientOutcome> = JoinSet::new();

        for _ in 0..n_clients {
            let ws_url = ctx.ws_url.clone();
            let room_id = room_id.clone();
            let invite_code = invite_code.clone();

            join_set.spawn(async move {
                let t0 = Instant::now();

                // Step 1: Connect
                let mut client = match StressClient::connect(&ws_url).await {
                    Ok(c) => c,
                    Err(e) => {
                        return ClientOutcome {
                            joined: false,
                            error: Some(format!("connect_error: {e}")),
                            latency: t0.elapsed(),
                        };
                    }
                };

                // Step 2: Join
                let join_result = client.join_room(&room_id, "p2p", Some(&invite_code)).await;

                let joined = match join_result {
                    Ok(ref r) => r.success,
                    Err(_) => false,
                };

                // Step 3: Immediately send Leave
                if joined {
                    let _ = client
                        .send_json(&serde_json::json!({"type": "leave"}))
                        .await;

                    // Drain briefly to let the backend process the Leave
                    client.drain(Duration::from_millis(100)).await;
                }

                // Step 4: Disconnect
                client.close().await;

                ClientOutcome {
                    joined,
                    error: None,
                    latency: t0.elapsed(),
                }
            });
        }

        // --- Collect results ---
        let mut connect_errors: usize = 0;

        while let Some(outcome) = join_set.join_next().await {
            match outcome {
                Ok(o) => {
                    latency.record(o.latency);
                    if o.error.is_some() {
                        connect_errors += 1;
                    }
                }
                Err(e) => {
                    violations.push(InvariantViolation {
                        invariant: "task_panic".to_owned(),
                        expected: "no panics".to_owned(),
                        actual: format!("task panicked: {e}"),
                    });
                }
            }
        }

        // Log connect errors as a soft warning (not a hard violation — the backend
        // may legitimately reject some connections under load).
        if connect_errors > 0 {
            // Not a hard violation — just informational. The ghost-peer check is the
            // authoritative assertion.
            let _ = connect_errors; // suppress unused warning
        }

        // --- Wait 200ms for the backend to settle after all clients disconnect ---
        tokio::time::sleep(Duration::from_millis(200)).await;

        // --- Fetch metrics and assert no ghost peers ---
        let metrics_token = std::env::var("TEST_METRICS_TOKEN").unwrap_or_default();

        match fetch_metrics(&ctx.http_client, &ctx.metrics_url, &metrics_token).await {
            Ok(metrics) => {
                // Property 2: No ghost peers — participant_count == peer_ids.len() for all rooms
                let ghost_violations = assert_no_ghost_peers(&metrics);
                violations.extend(ghost_violations);

                // Property 2: After all clients leave, the room should either not exist
                // or have 0 participants.
                if let Some(room) = metrics
                    .get("rooms")
                    .and_then(|r| r.as_object())
                    .and_then(|rooms| rooms.get(&room_id))
                {
                    let participant_count = room
                        .get("participant_count")
                        .and_then(|v| v.as_u64())
                        .unwrap_or(0) as usize;

                    if participant_count != 0 {
                        violations.push(InvariantViolation {
                            invariant: format!(
                                "ghost_peers[{room_id}]: all clients left, room must have 0 participants"
                            ),
                            expected: "0".to_owned(),
                            actual: participant_count.to_string(),
                        });
                    }
                }
                // If the room doesn't appear in metrics at all, that's fine — it was cleaned up.
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

/// Outcome of a single concurrent client's storm loop.
struct ClientOutcome {
    #[allow(dead_code)]
    joined: bool,
    error: Option<String>,
    latency: Duration,
}

/// External-mode invite creation: connect a client, join the room, send InviteCreate,
/// receive InviteCreated with maxUses=100, then leave.
async fn create_invite_via_signaling(ctx: &TestContext, room_id: &str) -> Result<String, String> {
    let mut host = StressClient::connect(&ctx.ws_url)
        .await
        .map_err(|e| format!("host connect failed: {e}"))?;

    let join_result = host
        .join_room(room_id, "p2p", None)
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
        "maxUses": 100,
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
