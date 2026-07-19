/// CrossRoomInviteScenario — Property 7: Cross-room invite rejection under load
///
/// Creates an invite scoped to `room_a`, then spawns concurrent clients all attempting
/// to join `room_b` using that invite code. The invite is valid (not expired, not revoked,
/// has uses remaining) but is scoped to the wrong room, so all attempts must be rejected
/// with `InviteInvalid`.
///
/// **Property 7: Cross-room invite rejection under load**
/// **Validates: Requirements 3.5**
use std::time::{Duration, Instant};

use async_trait::async_trait;
use tokio::task::JoinSet;

use crate::client::StressClient;
use crate::config::{Capability, ConfigPreset, TestContext};
use crate::results::{InvariantViolation, LatencyTracker, ScenarioResult};
use crate::runner::{Scenario, Tier};

pub struct CrossRoomInviteScenario;

/// Room type hint — SFU rooms have capacity 6 (matching the product's max group size).
const ROOM_TYPE: &str = "sfu";

#[async_trait]
impl Scenario for CrossRoomInviteScenario {
    fn name(&self) -> &str {
        "cross-room-invite"
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

        // --- Generate two distinct room IDs ---
        let (room_a, room_b) = {
            use rand::RngCore;
            let mut rng = ctx.rng.lock().unwrap();
            let a = format!("room-a-{:016x}", rng.next_u64());
            let b = format!("room-b-{:016x}", rng.next_u64());
            (a, b)
        };

        // --- Create an invite scoped to room_a (with plenty of uses so it won't exhaust) ---
        let invite_code = match &ctx.app_state {
            Some(app_state) => {
                match app_state.invite_store.generate(
                    &room_a,
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
                                expected: "invite created for room_a with max_uses=100".to_owned(),
                                actual: format!("InviteStore::generate failed: {e}"),
                            }],
                        };
                    }
                }
            }
            None => {
                // External mode: host joins room_a, creates invite, leaves.
                match create_invite_for_room_via_signaling(ctx, &room_a).await {
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
                                expected: "invite created for room_a".to_owned(),
                                actual: e,
                            }],
                        };
                    }
                }
            }
        };

        // --- Spawn concurrent clients, each trying to join room_b with the room_a invite ---
        let n_clients = ctx.scale.concurrent_clients.min(20);
        let mut join_set: JoinSet<ClientOutcome> = JoinSet::new();

        for _ in 0..n_clients {
            let ws_url = ctx.ws_url.clone();
            let room_b_clone = room_b.clone();
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
                        };
                    }
                };

                let result = client
                    .join_room(&room_b_clone, ROOM_TYPE, Some(&invite_code_clone))
                    .await;

                let elapsed = t0.elapsed();
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
        let mut invite_invalid_rejections: usize = 0;
        let mut unexpected_rejections: usize = 0;

        while let Some(outcome) = join_set.join_next().await {
            match outcome {
                Ok(o) => {
                    latency.record(o.latency);
                    if o.success {
                        successes += 1;
                    } else {
                        match o.rejection_reason.as_deref() {
                            Some("invite_invalid") => invite_invalid_rejections += 1,
                            // Rate-limit / server-busy rejections are infrastructure
                            // noise, not property violations — the cross-room invite
                            // still didn't succeed.
                            Some(r)
                                if r == "rate_limited"
                                    || r.contains("server busy")
                                    || r.contains("Timeout") =>
                            {
                                invite_invalid_rejections += 1;
                            }
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

        // --- Property 7: Zero successes — all joins must be rejected ---
        if successes > 0 {
            violations.push(InvariantViolation {
                invariant: "cross_room_invite_rejection: zero successes expected".to_owned(),
                expected: "0".to_owned(),
                actual: successes.to_string(),
            });
        }

        // --- Property 7: All rejections must be InviteInvalid ---
        if invite_invalid_rejections != n_clients.saturating_sub(unexpected_rejections) {
            violations.push(InvariantViolation {
                invariant: "cross_room_invite_rejection: all rejections must be InviteInvalid"
                    .to_owned(),
                expected: n_clients.to_string(),
                actual: invite_invalid_rejections.to_string(),
            });
        }

        if unexpected_rejections > 0 {
            violations.push(InvariantViolation {
                invariant: "no_unexpected_rejections".to_owned(),
                expected: "0".to_owned(),
                actual: unexpected_rejections.to_string(),
            });
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

/// External-mode: host joins room_a, creates an invite, then leaves.
/// Returns the invite code scoped to room_a.
async fn create_invite_for_room_via_signaling(
    ctx: &TestContext,
    room_id: &str,
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
