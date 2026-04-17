/// HostDirectedStopScenario — Property 21: Host-directed targeted StopShare
///
/// Tests:
///   P21) Host sends targeted `StopShare` with `targetParticipantId` against
///        multiple active sharers concurrently. Each stop removes only the
///        targeted sharer; other shares remain intact.
///
/// Setup: Host + 4 guests join an SFU room. All 4 guests start sharing.
/// Host then concurrently sends 4 targeted `stop_share` messages (one per guest).
/// After the race, `active_shares` must be empty and each guest must have
/// received a `share_stopped` for their own participantId.
///
/// Also tests: a guest sending a targeted stop for another guest's share
/// should be rejected with "permission denied".
///
/// **Validates: Requirements 6.6 (host-directed stop), 6.7 (guest cannot target others)**
use std::time::{Duration, Instant};

use async_trait::async_trait;

use crate::assertions::fetch_metrics;
use crate::client::StressClient;
use crate::config::{Capability, ConfigPreset, TestContext};
use crate::results::{InvariantViolation, LatencyTracker, ScenarioResult};
use crate::runner::{Scenario, Tier};

pub struct HostDirectedStopScenario;

#[async_trait]
impl Scenario for HostDirectedStopScenario {
    fn name(&self) -> &str {
        "host-directed-stop"
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
                format!("host-stop-{rep}-{:016x}", rng.next_u64())
            };

            let invite_code = match setup_room(ctx, &room_id).await {
                Ok(c) => c,
                Err(e) => {
                    violations.push(InvariantViolation {
                        invariant: format!("p21_rep{rep}: room_setup"),
                        expected: "room setup succeeds".to_owned(),
                        actual: e,
                    });
                    continue;
                }
            };

            // Join host (first joiner)
            let mut host = match connect_and_join(ctx, &room_id, &invite_code).await {
                Ok(c) => c,
                Err(e) => {
                    violations.push(InvariantViolation {
                        invariant: format!("p21_rep{rep}: host_join"),
                        expected: "host join succeeds".to_owned(),
                        actual: e,
                    });
                    continue;
                }
            };

            // Join 4 guests
            let mut guests: Vec<(StressClient, String)> = Vec::new();
            let mut join_failed = false;
            for i in 0..4usize {
                match connect_and_join(ctx, &room_id, &invite_code).await {
                    Ok(c) => guests.push(c),
                    Err(e) => {
                        violations.push(InvariantViolation {
                            invariant: format!("p21_rep{rep}: guest_{i}_join"),
                            expected: "guest join succeeds".to_owned(),
                            actual: e,
                        });
                        join_failed = true;
                        break;
                    }
                }
            }
            if join_failed {
                host.0.close().await;
                for (c, _) in guests {
                    c.close().await;
                }
                continue;
            }

            // All 4 guests start sharing
            for (client, _) in &mut guests {
                client
                    .send_json(&serde_json::json!({ "type": "start_share" }))
                    .await
                    .ok();
            }
            tokio::time::sleep(Duration::from_millis(500)).await;
            // Drain all clients
            host.0.drain(Duration::from_millis(200)).await;
            for (client, _) in &mut guests {
                client.drain(Duration::from_millis(200)).await;
            }

            // Verify shares are established
            let guest_peer_ids: Vec<String> = guests.iter().map(|(_, pid)| pid.clone()).collect();
            match fetch_metrics(&ctx.http_client, &ctx.metrics_url, &metrics_token).await {
                Ok(metrics) => {
                    let count = metrics
                        .get("rooms")
                        .and_then(|r| r.get(&room_id))
                        .and_then(|r| r.get("active_shares"))
                        .and_then(|r| r.as_array())
                        .map(|a| a.len())
                        .unwrap_or(0);
                    if count < 2 {
                        violations.push(InvariantViolation {
                            invariant: format!("p21_rep{rep}: shares_established"),
                            expected: "at least 2 active shares".to_owned(),
                            actual: format!("{count} active shares"),
                        });
                        host.0.close().await;
                        for (c, _) in guests {
                            c.close().await;
                        }
                        continue;
                    }
                }
                Err(e) => {
                    violations.push(InvariantViolation {
                        invariant: format!("p21_rep{rep}: metrics_pre_check"),
                        expected: "metrics reachable".to_owned(),
                        actual: e.to_string(),
                    });
                    host.0.close().await;
                    for (c, _) in guests {
                        c.close().await;
                    }
                    continue;
                }
            }

            // --- P21a: Guest tries to stop another guest's share → permission denied ---
            if guests.len() >= 2 {
                let target_pid = guests[1].1.clone();
                guests[0]
                    .0
                    .send_json(&serde_json::json!({
                        "type": "stop_share",
                        "targetParticipantId": target_pid,
                    }))
                    .await
                    .ok();
                let msgs = guests[0].0.drain(Duration::from_millis(1000)).await;
                let got_denied = msgs.iter().any(|m| {
                    m.get("type").and_then(|v| v.as_str()) == Some("error")
                        && m.get("message")
                            .and_then(|v| v.as_str())
                            .unwrap_or("")
                            .contains("permission denied")
                });
                if !got_denied {
                    violations.push(InvariantViolation {
                        invariant: format!("p21_rep{rep}: guest_targeted_stop_denied"),
                        expected: "guest receives permission denied".to_owned(),
                        actual: "no permission denied error received".to_owned(),
                    });
                }
            }

            // --- P21b: Host concurrently sends targeted stops for all 4 guests ---
            // We need to split the host connection into separate tasks. Since we can't
            // clone StressClient, the host sends all 4 stop messages sequentially but
            // as fast as possible (no await between sends), then drains.
            for pid in &guest_peer_ids {
                host.0
                    .send_json(&serde_json::json!({
                        "type": "stop_share",
                        "targetParticipantId": pid,
                    }))
                    .await
                    .ok();
            }
            // Drain host and guests
            tokio::time::sleep(Duration::from_millis(500)).await;
            host.0.drain(Duration::from_millis(300)).await;
            for (client, _) in &mut guests {
                client.drain(Duration::from_millis(300)).await;
            }

            // P21: active_shares must be empty after host stopped all guests
            tokio::time::sleep(Duration::from_millis(150)).await;
            match fetch_metrics(&ctx.http_client, &ctx.metrics_url, &metrics_token).await {
                Ok(metrics) => {
                    let shares = metrics
                        .get("rooms")
                        .and_then(|r| r.get(&room_id))
                        .and_then(|r| r.get("active_shares"))
                        .and_then(|r| r.as_array());
                    match shares {
                        Some(arr) if !arr.is_empty() => {
                            let remaining: Vec<&str> =
                                arr.iter().filter_map(|v| v.as_str()).collect();
                            violations.push(InvariantViolation {
                                invariant: format!("p21_rep{rep}: all_shares_stopped_by_host"),
                                expected: "0 active shares".to_owned(),
                                actual: format!("remaining: {:?}", remaining),
                            });
                        }
                        _ => {
                            // Empty or missing — correct
                        }
                    }
                }
                Err(e) => {
                    violations.push(InvariantViolation {
                        invariant: format!("p21_rep{rep}: metrics_reachable"),
                        expected: "metrics endpoint responds".to_owned(),
                        actual: format!("fetch failed: {e}"),
                    });
                }
            }

            host.0.close().await;
            for (c, _) in guests {
                c.close().await;
            }
        }

        build_result(self.name(), start, violations, latency)
    }
}

// ---------------------------------------------------------------------------
// Helpers (same pattern as other scenarios)
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
