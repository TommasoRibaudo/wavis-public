/// MultiShareFloodScenario — Property 23: Multi-share exhaustion flood
///
/// Tests:
///   P23) All 6 participants rapidly toggle `StartShare`/`StopShare` in a tight
///        loop. The `active_shares ⊆ participants` invariant must always hold
///        and no ghost shares remain after all participants disconnect.
///
/// Setup: Host + 5 guests join an SFU room. Each participant sends a burst of
/// alternating start_share/stop_share messages. After the burst, verify
/// active_shares only contains valid peer IDs. Then disconnect all participants
/// and verify active_shares is empty (cleanup_share_on_disconnect works).
///
/// **Validates: Requirements 6.9 (multi-share invariant under contention)**
use std::time::{Duration, Instant};

use async_trait::async_trait;
use tokio::task::JoinSet;

use crate::assertions::fetch_metrics;
use crate::client::StressClient;
use crate::config::{Capability, ConfigPreset, TestContext};
use crate::results::{InvariantViolation, LatencyTracker, ScenarioResult};
use crate::runner::{Scenario, Tier};

pub struct MultiShareFloodScenario;

#[async_trait]
impl Scenario for MultiShareFloodScenario {
    fn name(&self) -> &str {
        "multi-share-flood"
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
                format!("share-flood-{rep}-{:016x}", rng.next_u64())
            };

            let invite_code = match setup_room(ctx, &room_id).await {
                Ok(c) => c,
                Err(e) => {
                    violations.push(InvariantViolation {
                        invariant: format!("p23_rep{rep}: room_setup"),
                        expected: "room setup succeeds".to_owned(),
                        actual: e,
                    });
                    continue;
                }
            };

            // Join 6 participants (first is host, rest are guests)
            let mut clients: Vec<(StressClient, String)> = Vec::new();
            let mut join_failed = false;
            for i in 0..6usize {
                match connect_and_join(ctx, &room_id, &invite_code).await {
                    Ok(c) => clients.push(c),
                    Err(e) => {
                        violations.push(InvariantViolation {
                            invariant: format!("p23_rep{rep}: participant_{i}_join"),
                            expected: "join succeeds".to_owned(),
                            actual: e,
                        });
                        join_failed = true;
                        break;
                    }
                }
            }
            if join_failed {
                for (c, _) in clients {
                    c.close().await;
                }
                continue;
            }

            let all_peer_ids: Vec<String> = clients.iter().map(|(_, pid)| pid.clone()).collect();

            // --- Flood: each participant sends 10 alternating start/stop bursts ---
            let barrier = std::sync::Arc::new(tokio::sync::Barrier::new(clients.len()));
            let mut set: JoinSet<StressClient> = JoinSet::new();

            for (client, _pid) in clients {
                let b = barrier.clone();
                set.spawn(async move {
                    let mut c = client;
                    b.wait().await;
                    for cycle in 0..10u32 {
                        if cycle % 2 == 0 {
                            c.send_json(&serde_json::json!({ "type": "start_share" }))
                                .await
                                .ok();
                        } else {
                            c.send_json(&serde_json::json!({ "type": "stop_share" }))
                                .await
                                .ok();
                        }
                        // Tiny delay to avoid overwhelming the action rate limiter
                        tokio::time::sleep(Duration::from_millis(20)).await;
                    }
                    // Drain responses
                    c.drain(Duration::from_millis(500)).await;
                    c
                });
            }

            let mut alive: Vec<StressClient> = Vec::new();
            while let Some(res) = set.join_next().await {
                if let Ok(c) = res {
                    alive.push(c);
                }
            }

            // P23a: active_shares ⊆ all_peer_ids (no ghost shares from unknown peers)
            tokio::time::sleep(Duration::from_millis(200)).await;
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
                                        "p23_rep{rep}: active_shares_subset_of_participants"
                                    ),
                                    expected: format!("active_shares ⊆ {:?}", all_peer_ids),
                                    actual: format!("ghost share: '{sid}'"),
                                });
                            }
                        }
                        // Also check no duplicates
                        let mut seen = std::collections::HashSet::new();
                        for v in arr {
                            let sid = v.as_str().unwrap_or("");
                            if !sid.is_empty() && !seen.insert(sid) {
                                violations.push(InvariantViolation {
                                    invariant: format!("p23_rep{rep}: no_duplicate_shares"),
                                    expected: "no duplicate entries in active_shares".to_owned(),
                                    actual: format!("duplicate: '{sid}'"),
                                });
                            }
                        }
                    }
                }
                Err(e) => {
                    violations.push(InvariantViolation {
                        invariant: format!("p23_rep{rep}: metrics_reachable_mid"),
                        expected: "metrics endpoint responds".to_owned(),
                        actual: format!("fetch failed: {e}"),
                    });
                }
            }

            // P23b: disconnect all participants, then verify active_shares is empty
            for c in alive {
                c.close().await;
            }

            // Give backend time to run cleanup_share_on_disconnect for all 6
            tokio::time::sleep(Duration::from_millis(500)).await;

            match fetch_metrics(&ctx.http_client, &ctx.metrics_url, &metrics_token).await {
                Ok(metrics) => {
                    // Room may have been cleaned up entirely (empty rooms are removed).
                    // If the room still exists, active_shares must be empty.
                    if let Some(room) = metrics.get("rooms").and_then(|r| r.get(&room_id)) {
                        let shares = room
                            .get("active_shares")
                            .and_then(|r| r.as_array())
                            .map(|a| a.len())
                            .unwrap_or(0);
                        if shares != 0 {
                            violations.push(InvariantViolation {
                                invariant: format!(
                                    "p23_rep{rep}: no_ghost_shares_after_disconnect"
                                ),
                                expected: "0 active shares after all disconnect".to_owned(),
                                actual: format!("{shares} ghost shares remain"),
                            });
                        }
                    }
                    // Room gone entirely = also correct (auto-cleanup)
                }
                Err(e) => {
                    violations.push(InvariantViolation {
                        invariant: format!("p23_rep{rep}: metrics_reachable_post"),
                        expected: "metrics endpoint responds".to_owned(),
                        actual: format!("fetch failed: {e}"),
                    });
                }
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
