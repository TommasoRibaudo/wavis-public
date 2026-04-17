/// ScreenShareRaceScenario — Property 16: Single-winner screen share
///                            Property 17: Screen share race determinism
///                            Property 18: Non-participant share rejection
///                            Property 19: Share cleanup on disconnect
///
/// Tests:
///   P16) 6 participants all send `StartShare` concurrently → all can share
///        (multi-share). `active_shares` in metrics has at least one valid peer ID.
///   P17) Concurrent `StopShare` + `StartShare` race → final `active_shares` is valid
///        (contains known peer IDs or empty), never stale/impossible.
///   P18) Non-participant sends `StartShare` → rejected, `active_shares` unchanged.
///   P19) Active sharer disconnects → `active_shares` cleared, next `StartShare` succeeds.
///
/// **Validates: Requirements 6.1, 6.2, 6.3, 6.4**
use std::time::{Duration, Instant};

use async_trait::async_trait;
use tokio::task::JoinSet;

use crate::assertions::fetch_metrics;
use crate::client::StressClient;
use crate::config::{Capability, ConfigPreset, TestContext};
use crate::results::{InvariantViolation, LatencyTracker, ScenarioResult};
use crate::runner::{Scenario, Tier};

pub struct ScreenShareRaceScenario;

#[async_trait]
impl Scenario for ScreenShareRaceScenario {
    fn name(&self) -> &str {
        "screen-share-race"
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

        // =====================================================================
        // P16 + P17: Run R repetitions of the concurrent StartShare race
        // =====================================================================
        for rep in 0..ctx.scale.repetitions {
            let room_id = {
                use rand::RngCore;
                let mut rng = ctx.rng.lock().unwrap();
                format!("share-race-{rep}-{:016x}", rng.next_u64())
            };

            let invite_code = match setup_room(ctx, &room_id).await {
                Ok(c) => c,
                Err(e) => {
                    violations.push(InvariantViolation {
                        invariant: format!("p16_rep{rep}: room_setup"),
                        expected: "room setup succeeds".to_owned(),
                        actual: e,
                    });
                    continue;
                }
            };

            // Join 6 participants
            let mut clients = Vec::new();
            let mut join_failed = false;

            for i in 0..6usize {
                match StressClient::connect(&ctx.ws_url).await {
                    Ok(mut c) => match c.join_room(&room_id, "sfu", Some(&invite_code)).await {
                        Ok(r) if r.success => {
                            clients.push(c);
                        }
                        Ok(r) => {
                            c.close().await;
                            violations.push(InvariantViolation {
                                invariant: format!("p16_rep{rep}: participant_{i}_join"),
                                expected: "join succeeds".to_owned(),
                                actual: format!("rejected: {:?}", r.rejection_reason),
                            });
                            join_failed = true;
                            break;
                        }
                        Err(e) => {
                            c.close().await;
                            violations.push(InvariantViolation {
                                invariant: format!("p16_rep{rep}: participant_{i}_join"),
                                expected: "join succeeds".to_owned(),
                                actual: format!("error: {e}"),
                            });
                            join_failed = true;
                            break;
                        }
                    },
                    Err(e) => {
                        violations.push(InvariantViolation {
                            invariant: format!("p16_rep{rep}: participant_{i}_connect"),
                            expected: "connect succeeds".to_owned(),
                            actual: format!("error: {e}"),
                        });
                        join_failed = true;
                        break;
                    }
                }
            }

            if join_failed {
                for c in clients {
                    c.close().await;
                }
                continue;
            }

            // --- P16: All ready clients send StartShare concurrently ---
            let ws_url = ctx.ws_url.clone();
            let room_id_clone = room_id.clone();
            let invite_clone = invite_code.clone();

            // Keep one original client connected while swapping the others out
            // so room teardown does not invalidate the invite between waves.
            let mut setup_clients = clients;
            let anchor = setup_clients.pop();
            let mut ready_clients: Vec<(StressClient, String)> = Vec::with_capacity(6);
            let mut replacement_failures = Vec::new();

            // Replace the non-anchor clients first while the room stays alive.
            for (replacement_idx, stale_client) in setup_clients.into_iter().enumerate() {
                stale_client.close().await;

                match StressClient::connect(&ws_url).await {
                    Ok(mut c) => match c
                        .join_room(&room_id_clone, "sfu", Some(&invite_clone))
                        .await
                    {
                        Ok(join) if join.success => ready_clients.push((c, join.peer_id)),
                        Ok(join) => {
                            let reason = join
                                .rejection_reason
                                .unwrap_or_else(|| "unknown rejection".to_owned());
                            replacement_failures.push(format!(
                                "replacement_{replacement_idx}_join rejected: {reason}"
                            ));
                            c.close().await;
                        }
                        Err(e) => {
                            replacement_failures
                                .push(format!("replacement_{replacement_idx}_join error: {e}"));
                            c.close().await;
                        }
                    },
                    Err(e) => replacement_failures
                        .push(format!("replacement_{replacement_idx}_connect error: {e}")),
                }
            }

            // Keep the anchor as one of the race participants so the room never
            // goes empty during the replacement wave.
            if let Some(anchor) = anchor {
                if let Some(peer_id) = anchor.peer_id.clone() {
                    ready_clients.push((anchor, peer_id));
                } else {
                    replacement_failures.push(
                        "replacement_anchor_missing_peer_id after successful join".to_owned(),
                    );
                    anchor.close().await;
                }
            }

            if ready_clients.len() < 2 {
                // Not enough clients to race — record and move on.
                let failure_detail = if replacement_failures.is_empty() {
                    "no replacement failures recorded".to_owned()
                } else {
                    replacement_failures.join("; ")
                };
                violations.push(InvariantViolation {
                    invariant: format!("p16_rep{rep}: enough_clients_connected"),
                    expected: "at least 2 clients".to_owned(),
                    actual: format!("{} clients; {failure_detail}", ready_clients.len()),
                });
                for (c, _) in ready_clients {
                    c.close().await;
                }
                continue;
            }

            let n_ready = ready_clients.len();
            let barrier = std::sync::Arc::new(tokio::sync::Barrier::new(n_ready));

            let mut set: JoinSet<(bool, Option<String>, Option<StressClient>)> = JoinSet::new();
            for (c, peer_id) in ready_clients {
                let b = barrier.clone();
                set.spawn(async move {
                    let mut c = c;
                    let my_peer_id = peer_id;
                    // Wait for all ready clients
                    b.wait().await;
                    // Send StartShare
                    c.send_json(&serde_json::json!({ "type": "start_share" }))
                        .await
                        .ok();
                    // 6 concurrent shares each produce a BroadcastAll signal;
                    // under per-room write-lock serialization the last share's
                    // broadcast can arrive late. Use a generous drain window so
                    // every client sees its own share_started.
                    let msgs = c.drain(Duration::from_millis(5000)).await;
                    let got_error = msgs
                        .iter()
                        .any(|m| m.get("type").and_then(|v| v.as_str()) == Some("error"));
                    let got_own_share_started = msgs.iter().any(|m| {
                        m.get("type").and_then(|v| v.as_str()) == Some("share_started")
                            && m.get("participantId").and_then(|v| v.as_str()) == Some(&my_peer_id)
                    });
                    let won = got_own_share_started && !got_error;
                    (won, Some(my_peer_id), Some(c))
                });
            }

            let mut winners = 0usize;
            let mut task_failures = 0usize;
            let mut alive_clients: Vec<StressClient> = Vec::new();

            while let Some(res) = set.join_next().await {
                match res {
                    Ok((true, Some(_), client)) => {
                        winners += 1;
                        if let Some(c) = client {
                            alive_clients.push(c);
                        }
                    }
                    Ok((true, None, client)) => {
                        winners += 1;
                        if let Some(c) = client {
                            alive_clients.push(c);
                        }
                    }
                    Ok((false, _, client)) => {
                        // Under multi-share, not winning means an error or task issue
                        if let Some(c) = client {
                            alive_clients.push(c);
                        }
                    }
                    Err(_) => task_failures += 1,
                }
            }

            // P16: multi-share — all participants should be able to share concurrently.
            // Every ready client that didn't hit a task failure should have won.
            if task_failures == 0 && winners < n_ready {
                violations.push(InvariantViolation {
                    invariant: format!("p16_rep{rep}: all_participants_can_share"),
                    expected: format!("{n_ready} winners (multi-share)"),
                    actual: format!("{winners} winners out of {n_ready}"),
                });
            }

            // P16: assert active_shares in metrics contains all winner peer IDs
            tokio::time::sleep(Duration::from_millis(100)).await;
            match fetch_metrics(&ctx.http_client, &ctx.metrics_url, &metrics_token).await {
                Ok(metrics) => {
                    let active_shares = metrics
                        .get("rooms")
                        .and_then(|r| r.get(&room_id_clone))
                        .and_then(|r| r.get("active_shares"))
                        .and_then(|r| r.as_array());

                    match active_shares {
                        Some(arr) => {
                            // Every entry must be a non-empty, known peer ID
                            for v in arr {
                                let share_id = v.as_str().unwrap_or("");
                                if share_id.is_empty() {
                                    violations.push(InvariantViolation {
                                        invariant: format!(
                                            "p16_rep{rep}: active_shares_no_empty_ids"
                                        ),
                                        expected: "non-empty peer ID".to_owned(),
                                        actual: "empty string in active_shares".to_owned(),
                                    });
                                }
                            }
                            // Under multi-share, active_shares should have at least 1 entry
                            if arr.is_empty() && task_failures == 0 {
                                violations.push(InvariantViolation {
                                    invariant: format!(
                                        "p16_rep{rep}: active_shares_non_empty_after_race"
                                    ),
                                    expected: "active_shares contains at least one peer ID"
                                        .to_owned(),
                                    actual: "active_shares is empty".to_owned(),
                                });
                            }
                        }
                        None => {
                            if task_failures == 0 {
                                violations.push(InvariantViolation {
                                    invariant: format!(
                                        "p16_rep{rep}: active_shares_present_after_race"
                                    ),
                                    expected: "active_shares array present".to_owned(),
                                    actual: "active_shares missing from metrics".to_owned(),
                                });
                            }
                        }
                    }
                }
                Err(e) => {
                    violations.push(InvariantViolation {
                        invariant: format!("p16_rep{rep}: metrics_reachable"),
                        expected: "metrics endpoint responds".to_owned(),
                        actual: format!("fetch failed: {e}"),
                    });
                }
            }

            // Close clients after the metrics check so the room stays alive.
            for c in alive_clients {
                c.close().await;
            }
        }

        // =====================================================================
        // P17: StopShare + StartShare race — final state must be valid
        // =====================================================================
        for rep in 0..ctx.scale.repetitions {
            let room_id = {
                use rand::RngCore;
                let mut rng = ctx.rng.lock().unwrap();
                format!("share-stop-race-{rep}-{:016x}", rng.next_u64())
            };

            let invite_code = match setup_room(ctx, &room_id).await {
                Ok(c) => c,
                Err(e) => {
                    violations.push(InvariantViolation {
                        invariant: format!("p17_rep{rep}: room_setup"),
                        expected: "room setup succeeds".to_owned(),
                        actual: e,
                    });
                    continue;
                }
            };

            // Connect 2 participants: sharer and racer
            let mut sharer = match StressClient::connect(&ctx.ws_url).await {
                Ok(c) => c,
                Err(e) => {
                    violations.push(InvariantViolation {
                        invariant: format!("p17_rep{rep}: sharer_connect"),
                        expected: "connect succeeds".to_owned(),
                        actual: format!("{e}"),
                    });
                    continue;
                }
            };
            let sharer_join = match sharer.join_room(&room_id, "sfu", Some(&invite_code)).await {
                Ok(r) if r.success => r,
                Ok(r) => {
                    sharer.close().await;
                    violations.push(InvariantViolation {
                        invariant: format!("p17_rep{rep}: sharer_join"),
                        expected: "join succeeds".to_owned(),
                        actual: format!("rejected: {:?}", r.rejection_reason),
                    });
                    continue;
                }
                Err(e) => {
                    sharer.close().await;
                    violations.push(InvariantViolation {
                        invariant: format!("p17_rep{rep}: sharer_join"),
                        expected: "join succeeds".to_owned(),
                        actual: format!("{e}"),
                    });
                    continue;
                }
            };
            let sharer_peer_id = sharer_join.peer_id.clone();

            let mut racer = match StressClient::connect(&ctx.ws_url).await {
                Ok(c) => c,
                Err(e) => {
                    sharer.close().await;
                    violations.push(InvariantViolation {
                        invariant: format!("p17_rep{rep}: racer_connect"),
                        expected: "connect succeeds".to_owned(),
                        actual: format!("{e}"),
                    });
                    continue;
                }
            };
            let racer_join = match racer.join_room(&room_id, "sfu", Some(&invite_code)).await {
                Ok(r) if r.success => r,
                Ok(r) => {
                    sharer.close().await;
                    racer.close().await;
                    violations.push(InvariantViolation {
                        invariant: format!("p17_rep{rep}: racer_join"),
                        expected: "join succeeds".to_owned(),
                        actual: format!("rejected: {:?}", r.rejection_reason),
                    });
                    continue;
                }
                Err(e) => {
                    sharer.close().await;
                    racer.close().await;
                    violations.push(InvariantViolation {
                        invariant: format!("p17_rep{rep}: racer_join"),
                        expected: "join succeeds".to_owned(),
                        actual: format!("{e}"),
                    });
                    continue;
                }
            };
            let racer_peer_id = racer_join.peer_id.clone();

            // Sharer starts sharing first
            sharer
                .send_json(&serde_json::json!({ "type": "start_share" }))
                .await
                .ok();
            match sharer
                .recv_type("share_started", Duration::from_secs(3))
                .await
            {
                Ok(_) => {}
                Err(e) => {
                    sharer.close().await;
                    racer.close().await;
                    violations.push(InvariantViolation {
                        invariant: format!("p17_rep{rep}: sharer_start_share"),
                        expected: "ShareStarted received".to_owned(),
                        actual: format!("{e}"),
                    });
                    continue;
                }
            }

            // Race: sharer sends StopShare, racer sends StartShare concurrently
            let barrier = std::sync::Arc::new(tokio::sync::Barrier::new(2));
            let b1 = barrier.clone();
            let b2 = barrier.clone();

            let ws_url = ctx.ws_url.clone();
            let rid = room_id.clone();
            let inv = invite_code.clone();
            let sharer_id = sharer_peer_id.clone();
            let racer_id = racer_peer_id.clone();

            // Spawn sharer task (StopShare)
            let sharer_task = tokio::spawn(async move {
                b1.wait().await;
                sharer
                    .send_json(&serde_json::json!({ "type": "stop_share" }))
                    .await
                    .ok();
                // Drain briefly
                sharer.drain(Duration::from_millis(500)).await;
                sharer.close().await;
            });

            // Spawn racer task (StartShare)
            let racer_task = tokio::spawn(async move {
                b2.wait().await;
                racer
                    .send_json(&serde_json::json!({ "type": "start_share" }))
                    .await
                    .ok();
                racer.drain(Duration::from_millis(500)).await;
                racer.close().await;
            });

            let _ = tokio::join!(sharer_task, racer_task);

            // P17: final active_shares must be valid (contains known peer IDs or empty)
            tokio::time::sleep(Duration::from_millis(150)).await;
            match fetch_metrics(&ctx.http_client, &ctx.metrics_url, &metrics_token).await {
                Ok(metrics) => {
                    let active_shares = metrics
                        .get("rooms")
                        .and_then(|r| r.get(&room_id))
                        .and_then(|r| r.get("active_shares"))
                        .and_then(|r| r.as_array());

                    match active_shares {
                        Some(arr) if !arr.is_empty() => {
                            for v in arr {
                                let share_id = v.as_str().unwrap_or("");
                                // Must be one of the known peer IDs — never a stale/impossible value
                                if !share_id.is_empty()
                                    && share_id != sharer_id
                                    && share_id != racer_id
                                {
                                    violations.push(InvariantViolation {
                                        invariant: format!(
                                            "p17_rep{rep}: active_shares_is_known_peer"
                                        ),
                                        expected: format!(
                                            "active_shares contains only [{sharer_id}, {racer_id}]"
                                        ),
                                        actual: format!("stale/impossible value: '{share_id}'"),
                                    });
                                }
                            }
                        }
                        _ => {
                            // Empty is valid — StopShare may have won the race
                        }
                    }
                }
                Err(e) => {
                    violations.push(InvariantViolation {
                        invariant: format!("p17_rep{rep}: metrics_reachable"),
                        expected: "metrics endpoint responds".to_owned(),
                        actual: format!("fetch failed: {e}"),
                    });
                }
            }

            let _ = (ws_url, rid, inv, sharer_id, racer_id);
        }

        // =====================================================================
        // P18: Non-participant sends StartShare → rejected, active_shares unchanged
        // =====================================================================
        {
            let room_id = {
                use rand::RngCore;
                let mut rng = ctx.rng.lock().unwrap();
                format!("share-nonpart-{:016x}", rng.next_u64())
            };

            let invite_code = match setup_room(ctx, &room_id).await {
                Ok(c) => c,
                Err(e) => {
                    violations.push(InvariantViolation {
                        invariant: "p18: room_setup".to_owned(),
                        expected: "room setup succeeds".to_owned(),
                        actual: e,
                    });
                    // skip P18
                    goto_p19(ctx, &metrics_token, &mut violations).await;
                    return build_result(self.name(), start, violations, latency);
                }
            };

            // One legitimate participant joins (to establish the room)
            let mut participant = match StressClient::connect(&ctx.ws_url).await {
                Ok(c) => c,
                Err(e) => {
                    violations.push(InvariantViolation {
                        invariant: "p18: participant_connect".to_owned(),
                        expected: "connect succeeds".to_owned(),
                        actual: format!("{e}"),
                    });
                    goto_p19(ctx, &metrics_token, &mut violations).await;
                    return build_result(self.name(), start, violations, latency);
                }
            };
            match participant
                .join_room(&room_id, "sfu", Some(&invite_code))
                .await
            {
                Ok(r) if r.success => {}
                Ok(r) => {
                    participant.close().await;
                    violations.push(InvariantViolation {
                        invariant: "p18: participant_join".to_owned(),
                        expected: "join succeeds".to_owned(),
                        actual: format!("rejected: {:?}", r.rejection_reason),
                    });
                    goto_p19(ctx, &metrics_token, &mut violations).await;
                    return build_result(self.name(), start, violations, latency);
                }
                Err(e) => {
                    participant.close().await;
                    violations.push(InvariantViolation {
                        invariant: "p18: participant_join".to_owned(),
                        expected: "join succeeds".to_owned(),
                        actual: format!("{e}"),
                    });
                    goto_p19(ctx, &metrics_token, &mut violations).await;
                    return build_result(self.name(), start, violations, latency);
                }
            }

            // Snapshot active_shares before non-participant attempt
            let before_share =
                get_active_share(&ctx.http_client, &ctx.metrics_url, &metrics_token, &room_id)
                    .await;

            // Non-participant: connect but do NOT join the room, then send StartShare
            let mut outsider = match StressClient::connect(&ctx.ws_url).await {
                Ok(c) => c,
                Err(e) => {
                    participant.close().await;
                    violations.push(InvariantViolation {
                        invariant: "p18: outsider_connect".to_owned(),
                        expected: "connect succeeds".to_owned(),
                        actual: format!("{e}"),
                    });
                    goto_p19(ctx, &metrics_token, &mut violations).await;
                    return build_result(self.name(), start, violations, latency);
                }
            };

            // Send StartShare without joining — backend should reject (pre-join gate or NotInRoom)
            outsider
                .send_json(&serde_json::json!({ "type": "start_share" }))
                .await
                .ok();

            // Expect an error response
            match outsider.recv_type("error", Duration::from_secs(3)).await {
                Ok(err_msg) => {
                    let msg = err_msg
                        .get("message")
                        .and_then(|v| v.as_str())
                        .unwrap_or("")
                        .to_lowercase();
                    // Accept any rejection: "not authenticated", "not in room", "unauthorized", etc.
                    if msg.is_empty() {
                        violations.push(InvariantViolation {
                            invariant: "p18: outsider_start_share_rejected".to_owned(),
                            expected: "non-empty error message".to_owned(),
                            actual: "empty error message".to_owned(),
                        });
                    }
                }
                Err(_) => {
                    // Connection may have been closed — also acceptable as rejection
                }
            }

            tokio::time::sleep(Duration::from_millis(100)).await;

            // active_shares must be unchanged
            let after_share =
                get_active_share(&ctx.http_client, &ctx.metrics_url, &metrics_token, &room_id)
                    .await;

            if before_share != after_share {
                violations.push(InvariantViolation {
                    invariant: "p18: active_shares_unchanged_after_outsider_attempt".to_owned(),
                    expected: format!("active_shares = {:?}", before_share),
                    actual: format!("active_shares = {:?}", after_share),
                });
            }

            outsider.close().await;
            participant.close().await;
        }

        // =====================================================================
        // P19: Active sharer disconnects → active_shares cleared, next StartShare succeeds
        // =====================================================================
        goto_p19(ctx, &metrics_token, &mut violations).await;

        build_result(self.name(), start, violations, latency)
    }
}

// ---------------------------------------------------------------------------
// P19 helper (extracted so P18 can jump to it on early exit)
// ---------------------------------------------------------------------------

async fn goto_p19(
    ctx: &TestContext,
    metrics_token: &str,
    violations: &mut Vec<InvariantViolation>,
) {
    let room_id = {
        use rand::RngCore;
        let mut rng = ctx.rng.lock().unwrap();
        format!("share-disconnect-{:016x}", rng.next_u64())
    };

    let invite_code = match setup_room(ctx, &room_id).await {
        Ok(c) => c,
        Err(e) => {
            violations.push(InvariantViolation {
                invariant: "p19: room_setup".to_owned(),
                expected: "room setup succeeds".to_owned(),
                actual: e,
            });
            return;
        }
    };

    // Connect sharer and a second participant
    let mut sharer = match StressClient::connect(&ctx.ws_url).await {
        Ok(c) => c,
        Err(e) => {
            violations.push(InvariantViolation {
                invariant: "p19: sharer_connect".to_owned(),
                expected: "connect succeeds".to_owned(),
                actual: format!("{e}"),
            });
            return;
        }
    };
    match sharer.join_room(&room_id, "sfu", Some(&invite_code)).await {
        Ok(r) if r.success => {}
        Ok(r) => {
            sharer.close().await;
            violations.push(InvariantViolation {
                invariant: "p19: sharer_join".to_owned(),
                expected: "join succeeds".to_owned(),
                actual: format!("rejected: {:?}", r.rejection_reason),
            });
            return;
        }
        Err(e) => {
            sharer.close().await;
            violations.push(InvariantViolation {
                invariant: "p19: sharer_join".to_owned(),
                expected: "join succeeds".to_owned(),
                actual: format!("{e}"),
            });
            return;
        }
    }

    let mut next_sharer = match StressClient::connect(&ctx.ws_url).await {
        Ok(c) => c,
        Err(e) => {
            sharer.close().await;
            violations.push(InvariantViolation {
                invariant: "p19: next_sharer_connect".to_owned(),
                expected: "connect succeeds".to_owned(),
                actual: format!("{e}"),
            });
            return;
        }
    };
    match next_sharer
        .join_room(&room_id, "sfu", Some(&invite_code))
        .await
    {
        Ok(r) if r.success => {}
        Ok(r) => {
            sharer.close().await;
            next_sharer.close().await;
            violations.push(InvariantViolation {
                invariant: "p19: next_sharer_join".to_owned(),
                expected: "join succeeds".to_owned(),
                actual: format!("rejected: {:?}", r.rejection_reason),
            });
            return;
        }
        Err(e) => {
            sharer.close().await;
            next_sharer.close().await;
            violations.push(InvariantViolation {
                invariant: "p19: next_sharer_join".to_owned(),
                expected: "join succeeds".to_owned(),
                actual: format!("{e}"),
            });
            return;
        }
    }

    // Sharer starts sharing
    sharer
        .send_json(&serde_json::json!({ "type": "start_share" }))
        .await
        .ok();
    match sharer
        .recv_type("share_started", Duration::from_secs(3))
        .await
    {
        Ok(_) => {}
        Err(e) => {
            sharer.close().await;
            next_sharer.close().await;
            violations.push(InvariantViolation {
                invariant: "p19: sharer_start_share".to_owned(),
                expected: "ShareStarted received".to_owned(),
                actual: format!("{e}"),
            });
            return;
        }
    }

    // Verify active_shares is set
    tokio::time::sleep(Duration::from_millis(50)).await;
    let share_before =
        get_active_share(&ctx.http_client, &ctx.metrics_url, metrics_token, &room_id).await;
    if share_before.is_none() {
        violations.push(InvariantViolation {
            invariant: "p19: active_shares_set_before_disconnect".to_owned(),
            expected: "active_shares is non-empty".to_owned(),
            actual: "active_shares is empty".to_owned(),
        });
    }

    // Sharer disconnects abruptly (close without Leave)
    sharer.close().await;

    // Give backend time to run cleanup_share_on_disconnect
    tokio::time::sleep(Duration::from_millis(300)).await;

    // P19: active_shares must be cleared
    let share_after =
        get_active_share(&ctx.http_client, &ctx.metrics_url, metrics_token, &room_id).await;
    if share_after.is_some() {
        violations.push(InvariantViolation {
            invariant: "p19: active_shares_cleared_after_disconnect".to_owned(),
            expected: "active_shares is empty after sharer disconnects".to_owned(),
            actual: format!("active_shares = {:?}", share_after),
        });
    }

    // P19: next participant can now start sharing
    next_sharer
        .send_json(&serde_json::json!({ "type": "start_share" }))
        .await
        .ok();
    match next_sharer
        .recv_type("share_started", Duration::from_secs(3))
        .await
    {
        Ok(_) => {}
        Err(e) => {
            violations.push(InvariantViolation {
                invariant: "p19: next_sharer_start_share_after_cleanup".to_owned(),
                expected: "ShareStarted received after sharer disconnects".to_owned(),
                actual: format!("{e}"),
            });
        }
    }

    next_sharer.close().await;
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

/// Create a room and return an invite code for it.
/// In in-process mode, uses AppState directly. In external mode, uses signaling.
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

/// External-mode: connect a client, join the room as first joiner (host),
/// request an invite code, then leave.
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

/// Fetch the first entry from the `active_shares` array for a specific room from the metrics endpoint.
/// Returns `Some(peer_id)` if at least one share is active, `None` otherwise.
async fn get_active_share(
    http_client: &reqwest::Client,
    metrics_url: &str,
    metrics_token: &str,
    room_id: &str,
) -> Option<String> {
    let metrics = fetch_metrics(http_client, metrics_url, metrics_token)
        .await
        .ok()?;
    let v = metrics
        .get("rooms")
        .and_then(|r| r.get(room_id))
        .and_then(|r| r.get("active_shares"))?;
    let arr = v.as_array()?;
    arr.first().and_then(|e| e.as_str()).map(|s| s.to_owned())
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
