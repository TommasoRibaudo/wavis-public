/// ShareStateConsistencyScenario — Property 22: ShareState snapshot consistency
///
/// Tests:
///   P22) Late joiners receive a `share_state` snapshot (via `share_state_snapshot`
///        in `handle_sfu_join`) while shares are being started/stopped concurrently.
///        The snapshot must always contain only valid participant IDs that are
///        currently in the room — never stale or impossible peer IDs.
///
/// Setup: Host + 3 guests join an SFU room. 2 guests start sharing. Then a new
/// participant joins while the other 2 guests toggle shares on/off. The late
/// joiner's `share_state` message must contain only peer IDs that are actual
/// room participants.
///
/// **Validates: Requirements 6.8 (share_state snapshot consistency)**
use std::time::{Duration, Instant};

use async_trait::async_trait;

use crate::assertions::fetch_metrics;
use crate::client::StressClient;
use crate::config::{Capability, ConfigPreset, TestContext};
use crate::results::{InvariantViolation, LatencyTracker, ScenarioResult};
use crate::runner::{Scenario, Tier};

pub struct ShareStateConsistencyScenario;

#[async_trait]
impl Scenario for ShareStateConsistencyScenario {
    fn name(&self) -> &str {
        "share-state-consistency"
    }

    fn tier(&self) -> Tier {
        Tier::Tier1
    }

    fn requires(&self) -> Vec<Capability> {
        vec![Capability::Sfu, Capability::ScreenShare]
    }

    fn config_preset(&self) -> ConfigPreset {
        ConfigPreset::JoinHeavy
    }

    async fn run(&self, ctx: &TestContext) -> ScenarioResult {
        let start = Instant::now();
        let mut violations: Vec<InvariantViolation> = Vec::new();
        let latency = LatencyTracker::new();
        let metrics_token = std::env::var("TEST_METRICS_TOKEN").unwrap_or_default();

        for rep in 0..ctx.scale.repetitions {
            let room_id = {
                use rand::RngCore;
                let mut rng = ctx.rng.lock().unwrap();
                format!("share-snap-{rep}-{:016x}", rng.next_u64())
            };

            let invite_code = match setup_room(ctx, &room_id).await {
                Ok(c) => c,
                Err(e) => {
                    violations.push(InvariantViolation {
                        invariant: format!("p22_rep{rep}: room_setup"),
                        expected: "room setup succeeds".to_owned(),
                        actual: e,
                    });
                    continue;
                }
            };

            // Join host + 2 sharers
            let mut host = match connect_and_join(ctx, &room_id, &invite_code).await {
                Ok(c) => c,
                Err(e) => {
                    violations.push(InvariantViolation {
                        invariant: format!("p22_rep{rep}: host_join"),
                        expected: "host join succeeds".to_owned(),
                        actual: e,
                    });
                    continue;
                }
            };

            let mut sharers: Vec<(StressClient, String)> = Vec::new();
            let mut join_failed = false;
            for i in 0..2usize {
                match connect_and_join(ctx, &room_id, &invite_code).await {
                    Ok(c) => sharers.push(c),
                    Err(e) => {
                        violations.push(InvariantViolation {
                            invariant: format!("p22_rep{rep}: sharer_{i}_join"),
                            expected: "sharer join succeeds".to_owned(),
                            actual: e,
                        });
                        join_failed = true;
                        break;
                    }
                }
            }
            if join_failed {
                host.0.close().await;
                for (c, _) in sharers {
                    c.close().await;
                }
                continue;
            }

            // Collect all known peer IDs in the room
            let mut all_peer_ids: Vec<String> = vec![host.1.clone()];
            for (_, pid) in &sharers {
                all_peer_ids.push(pid.clone());
            }

            // Both sharers start sharing
            for (client, _) in &mut sharers {
                client
                    .send_json(&serde_json::json!({ "type": "start_share" }))
                    .await
                    .ok();
            }
            tokio::time::sleep(Duration::from_millis(500)).await;
            host.0.drain(Duration::from_millis(200)).await;
            for (client, _) in &mut sharers {
                client.drain(Duration::from_millis(200)).await;
            }

            // Now: one sharer toggles (stop then start) while a late joiner connects.
            // This creates a window where the share_state snapshot might be inconsistent.
            sharers[0]
                .0
                .send_json(&serde_json::json!({ "type": "stop_share" }))
                .await
                .ok();

            // Late joiner connects concurrently with the share toggle
            let mut late_joiner = match connect_and_join(ctx, &room_id, &invite_code).await {
                Ok(c) => c,
                Err(e) => {
                    violations.push(InvariantViolation {
                        invariant: format!("p22_rep{rep}: late_joiner_join"),
                        expected: "late joiner join succeeds".to_owned(),
                        actual: e,
                    });
                    host.0.close().await;
                    for (c, _) in sharers {
                        c.close().await;
                    }
                    continue;
                }
            };
            all_peer_ids.push(late_joiner.1.clone());

            // The sharer re-starts sharing
            sharers[0]
                .0
                .send_json(&serde_json::json!({ "type": "start_share" }))
                .await
                .ok();

            // Drain the late joiner — look for share_state message
            tokio::time::sleep(Duration::from_millis(300)).await;
            let msgs = late_joiner.0.drain(Duration::from_millis(1500)).await;

            let share_state_msgs: Vec<&serde_json::Value> = msgs
                .iter()
                .filter(|m| m.get("type").and_then(|v| v.as_str()) == Some("share_state"))
                .collect();

            // P22: every participantId in share_state must be a known room participant
            for ss_msg in &share_state_msgs {
                if let Some(pids) = ss_msg.get("participantIds").and_then(|v| v.as_array()) {
                    for pid_val in pids {
                        let pid = pid_val.as_str().unwrap_or("");
                        if pid.is_empty() {
                            violations.push(InvariantViolation {
                                invariant: format!("p22_rep{rep}: share_state_no_empty_ids"),
                                expected: "non-empty participant ID".to_owned(),
                                actual: "empty string in share_state.participantIds".to_owned(),
                            });
                        } else if !all_peer_ids.contains(&pid.to_string()) {
                            violations.push(InvariantViolation {
                                invariant: format!("p22_rep{rep}: share_state_valid_peer_ids"),
                                expected: format!("participantId in {:?}", all_peer_ids),
                                actual: format!("stale/unknown participantId: '{pid}'"),
                            });
                        }
                    }
                }
            }

            // Also verify via metrics that active_shares ⊆ all_peer_ids
            match fetch_metrics(&ctx.http_client, &ctx.metrics_url, &metrics_token).await {
                Ok(metrics) => {
                    if let Some(arr) = metrics
                        .get("rooms")
                        .and_then(|r| r.get(&room_id))
                        .and_then(|r| r.get("active_shares"))
                        .and_then(|r| r.as_array())
                    {
                        for v in arr {
                            let sid = v.as_str().unwrap_or("");
                            if !sid.is_empty() && !all_peer_ids.contains(&sid.to_string()) {
                                violations.push(InvariantViolation {
                                    invariant: format!(
                                        "p22_rep{rep}: metrics_shares_subset_of_participants"
                                    ),
                                    expected: format!("active_shares ⊆ {:?}", all_peer_ids),
                                    actual: format!("stale share: '{sid}'"),
                                });
                            }
                        }
                    }
                }
                Err(e) => {
                    violations.push(InvariantViolation {
                        invariant: format!("p22_rep{rep}: metrics_reachable"),
                        expected: "metrics endpoint responds".to_owned(),
                        actual: format!("fetch failed: {e}"),
                    });
                }
            }

            late_joiner.0.close().await;
            host.0.close().await;
            for (c, _) in sharers {
                c.close().await;
            }
        }

        build_result(self.name(), start, violations, latency)
    }
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

async fn connect_and_join(
    ctx: &TestContext,
    room_id: &str,
    invite_code: &str,
) -> Result<(StressClient, String), String> {
    let mut c = StressClient::connect(&ctx.ws_url)
        .await
        .map_err(|e| format!("connect: {e}"))?;
    let r = c
        .join_room(room_id, "sfu", Some(invite_code))
        .await
        .map_err(|e| format!("join: {e}"))?;
    if !r.success {
        c.close().await;
        return Err(format!("rejected: {:?}", r.rejection_reason));
    }
    Ok((c, r.peer_id))
}

async fn setup_room(ctx: &TestContext, room_id: &str) -> Result<String, String> {
    match &ctx.app_state {
        Some(app_state) => app_state
            .invite_store
            .generate(
                room_id,
                "stress-issuer",
                Some(20),
                std::time::Instant::now(),
            )
            .map(|r| r.code)
            .map_err(|e| format!("invite generation failed: {e:?}")),
        None => create_invite_via_signaling(ctx, room_id).await,
    }
}

async fn create_invite_via_signaling(ctx: &TestContext, room_id: &str) -> Result<String, String> {
    let mut host = StressClient::connect(&ctx.ws_url)
        .await
        .map_err(|e| format!("connect failed: {e}"))?;
    let join = host
        .join_room(room_id, "sfu", None)
        .await
        .map_err(|e| format!("join failed: {e}"))?;
    if !join.success {
        host.close().await;
        return Err(format!("join rejected: {:?}", join.rejection_reason));
    }
    host.send_json(&serde_json::json!({ "type": "invite_create", "maxUses": 20 }))
        .await
        .map_err(|e| format!("InviteCreate send failed: {e}"))?;
    let msg = host
        .recv_type("invite_created", Duration::from_secs(5))
        .await
        .map_err(|e| format!("InviteCreated recv failed: {e}"))?;
    let code = msg
        .get("inviteCode")
        .and_then(|v| v.as_str())
        .ok_or_else(|| "InviteCreated missing inviteCode".to_owned())?
        .to_owned();
    host.send_json(&serde_json::json!({ "type": "leave" }))
        .await
        .ok();
    host.close().await;
    Ok(code)
}

fn build_result(
    name: &str,
    start: Instant,
    violations: Vec<InvariantViolation>,
    latency: LatencyTracker,
) -> ScenarioResult {
    let duration = start.elapsed();
    ScenarioResult {
        name: name.to_owned(),
        passed: violations.is_empty(),
        duration,
        actions_per_second: if duration.as_secs_f64() > 0.0 {
            1.0 / duration.as_secs_f64()
        } else {
            0.0
        },
        p95_latency: latency.p95(),
        p99_latency: latency.p99(),
        violations,
    }
}
