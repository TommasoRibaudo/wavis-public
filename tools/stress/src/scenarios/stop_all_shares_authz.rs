/// StopAllSharesAuthzScenario — Property 20: StopAllShares authorization
///
/// Tests:
///   P20) Multiple guests concurrently send `StopAllShares` while the host also
///        sends it. Only the host succeeds (guests receive "permission denied").
///        After the race, `active_shares` is empty.
///
/// Setup: Host + 3 guests join an SFU room. All 4 start sharing (multi-share).
/// Then all 4 concurrently send `stop_all_shares`. Only the host's request should
/// succeed; guests should get error responses. Final `active_shares` must be empty.
///
/// **Validates: Requirements 6.5 (host-only stop_all_shares authorization)**
use std::time::{Duration, Instant};

use async_trait::async_trait;
use tokio::task::JoinSet;

use crate::assertions::fetch_metrics;
use crate::client::StressClient;
use crate::config::{Capability, ConfigPreset, TestContext};
use crate::results::{InvariantViolation, LatencyTracker, ScenarioResult};
use crate::runner::{Scenario, Tier};

pub struct StopAllSharesAuthzScenario;

#[async_trait]
impl Scenario for StopAllSharesAuthzScenario {
    fn name(&self) -> &str {
        "stop-all-shares-authz"
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
                format!("stopall-authz-{rep}-{:016x}", rng.next_u64())
            };

            // --- Setup: create room, get invite code ---
            let invite_code = match setup_room(ctx, &room_id).await {
                Ok(c) => c,
                Err(e) => {
                    violations.push(InvariantViolation {
                        invariant: format!("p20_rep{rep}: room_setup"),
                        expected: "room setup succeeds".to_owned(),
                        actual: e,
                    });
                    continue;
                }
            };

            // --- Join host (first joiner) + 3 guests ---
            let mut host = match connect_and_join(ctx, &room_id, &invite_code).await {
                Ok(c) => c,
                Err(e) => {
                    violations.push(InvariantViolation {
                        invariant: format!("p20_rep{rep}: host_join"),
                        expected: "host join succeeds".to_owned(),
                        actual: e,
                    });
                    continue;
                }
            };

            let mut guests: Vec<(StressClient, String)> = Vec::new();
            let mut join_failed = false;
            for i in 0..3usize {
                match connect_and_join(ctx, &room_id, &invite_code).await {
                    Ok(c) => guests.push(c),
                    Err(e) => {
                        violations.push(InvariantViolation {
                            invariant: format!("p20_rep{rep}: guest_{i}_join"),
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

            // --- All 4 participants start sharing ---
            for (client, _pid) in std::iter::once(&mut host).chain(guests.iter_mut()) {
                client
                    .send_json(&serde_json::json!({ "type": "start_share" }))
                    .await
                    .ok();
            }
            // Drain to let share_started propagate
            tokio::time::sleep(Duration::from_millis(500)).await;
            for (client, _) in std::iter::once(&mut host).chain(guests.iter_mut()) {
                client.drain(Duration::from_millis(200)).await;
            }

            // Verify all 4 are sharing
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
                            invariant: format!("p20_rep{rep}: shares_established"),
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
                        invariant: format!("p20_rep{rep}: metrics_pre_check"),
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

            // --- Race: all 4 send stop_all_shares concurrently ---
            let barrier = std::sync::Arc::new(tokio::sync::Barrier::new(4));
            let mut set: JoinSet<(bool, bool, StressClient)> = JoinSet::new();

            // Host task
            {
                let b = barrier.clone();
                let (mut client, _pid) = host;
                set.spawn(async move {
                    b.wait().await;
                    client
                        .send_json(&serde_json::json!({ "type": "stop_all_shares" }))
                        .await
                        .ok();
                    let msgs = client.drain(Duration::from_millis(1500)).await;
                    let got_error = msgs
                        .iter()
                        .any(|m| m.get("type").and_then(|v| v.as_str()) == Some("error"));
                    // is_host = true
                    (true, got_error, client)
                });
            }

            // Guest tasks
            for (client, _pid) in guests {
                let b = barrier.clone();
                set.spawn(async move {
                    let mut client = client;
                    b.wait().await;
                    client
                        .send_json(&serde_json::json!({ "type": "stop_all_shares" }))
                        .await
                        .ok();
                    let msgs = client.drain(Duration::from_millis(1500)).await;
                    let got_error = msgs.iter().any(|m| {
                        m.get("type").and_then(|v| v.as_str()) == Some("error")
                            && m.get("message")
                                .and_then(|v| v.as_str())
                                .unwrap_or("")
                                .contains("permission denied")
                    });
                    // is_host = false
                    (false, got_error, client)
                });
            }

            let mut host_got_error = false;
            let mut guests_denied = 0usize;
            let mut alive: Vec<StressClient> = Vec::new();

            while let Some(res) = set.join_next().await {
                match res {
                    Ok((true, got_err, c)) => {
                        host_got_error = got_err;
                        alive.push(c);
                    }
                    Ok((false, got_perm_denied, c)) => {
                        if got_perm_denied {
                            guests_denied += 1;
                        }
                        alive.push(c);
                    }
                    Err(_) => {}
                }
            }

            // P20: host should NOT get a permission error
            if host_got_error {
                violations.push(InvariantViolation {
                    invariant: format!("p20_rep{rep}: host_no_error"),
                    expected: "host stop_all_shares succeeds (no error)".to_owned(),
                    actual: "host received an error".to_owned(),
                });
            }

            // P20: all 3 guests should get permission denied
            if guests_denied != 3 {
                violations.push(InvariantViolation {
                    invariant: format!("p20_rep{rep}: guests_permission_denied"),
                    expected: "3 guests receive permission denied".to_owned(),
                    actual: format!("{guests_denied} guests received permission denied"),
                });
            }

            // P20: active_shares must be empty after host's stop_all_shares
            tokio::time::sleep(Duration::from_millis(150)).await;
            match fetch_metrics(&ctx.http_client, &ctx.metrics_url, &metrics_token).await {
                Ok(metrics) => {
                    let shares = metrics
                        .get("rooms")
                        .and_then(|r| r.get(&room_id))
                        .and_then(|r| r.get("active_shares"))
                        .and_then(|r| r.as_array())
                        .map(|a| a.len())
                        .unwrap_or(0);
                    if shares != 0 {
                        violations.push(InvariantViolation {
                            invariant: format!("p20_rep{rep}: active_shares_empty_after_stop_all"),
                            expected: "0 active shares".to_owned(),
                            actual: format!("{shares} active shares"),
                        });
                    }
                }
                Err(e) => {
                    violations.push(InvariantViolation {
                        invariant: format!("p20_rep{rep}: metrics_reachable"),
                        expected: "metrics endpoint responds".to_owned(),
                        actual: format!("fetch failed: {e}"),
                    });
                }
            }

            for c in alive {
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
