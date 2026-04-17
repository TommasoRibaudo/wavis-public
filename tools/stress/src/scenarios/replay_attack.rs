/// ReplayAttackScenario — Property 9: Session-bound message rejection
///
/// Validates that the backend's pre-join gate rejects ALL non-Join messages when no
/// `SignalingSession` exists. The scenario:
///
///   Phase 1 — Capture: Connect client A, join the room, record the raw JSON of action
///             messages that would be valid in-session.
///   Phase 2 — Disconnect: Client A closes the WebSocket (session is destroyed).
///   Phase 3 — Replay: Connect a NEW client B (fresh connection, no session), replay
///             the captured raw JSON messages. Assert ALL are rejected.
///
/// The key insight: the backend's pre-join gate rejects ALL non-Join messages when no
/// `SignalingSession` exists. So replaying any action message on a fresh connection
/// (before joining) must be rejected with "not authenticated" or the connection is closed.
///
/// **Validates: Requirements 4.4**
use std::time::{Duration, Instant};

use async_trait::async_trait;

use crate::client::{StressClient, StressError};
use crate::config::{ConfigPreset, TestContext};
use crate::results::{InvariantViolation, LatencyTracker, ScenarioResult};
use crate::runner::{Scenario, Tier};

pub struct ReplayAttackScenario;

#[async_trait]
impl Scenario for ReplayAttackScenario {
    fn name(&self) -> &str {
        "replay-attack"
    }

    fn tier(&self) -> Tier {
        Tier::Tier1
    }

    fn config_preset(&self) -> ConfigPreset {
        ConfigPreset::Default
    }

    async fn run(&self, ctx: &TestContext) -> ScenarioResult {
        let start = Instant::now();
        let mut violations: Vec<InvariantViolation> = Vec::new();
        let latency = LatencyTracker::new();

        // --- Generate a unique room ID ---
        let room_id = {
            use rand::RngCore;
            let mut rng = ctx.rng.lock().unwrap();
            format!("replay-{:016x}", rng.next_u64())
        };

        // --- Create invite code ---
        let invite_code = match &ctx.app_state {
            Some(app_state) => {
                match app_state.invite_store.generate(
                    &room_id,
                    "stress-issuer",
                    Some(10),
                    Instant::now(),
                ) {
                    Ok(r) => r.code,
                    Err(e) => {
                        return early_fail(self.name(), start, "invite_creation", format!("{e}"));
                    }
                }
            }
            None => {
                // External mode: create invite via signaling
                match create_invite_via_signaling(ctx, &room_id).await {
                    Ok(c) => c,
                    Err(e) => {
                        return early_fail(self.name(), start, "invite_creation", e);
                    }
                }
            }
        };

        // =====================================================================
        // Phase 1 — Capture: Connect client A and join the room
        // =====================================================================

        let mut client_a = match StressClient::connect(&ctx.ws_url).await {
            Ok(c) => c,
            Err(e) => {
                return early_fail(self.name(), start, "client_a_connect", format!("{e}"));
            }
        };

        let join = match client_a
            .join_room(&room_id, "sfu", Some(&invite_code))
            .await
        {
            Ok(r) => r,
            Err(e) => {
                client_a.close().await;
                return early_fail(self.name(), start, "client_a_join", format!("{e}"));
            }
        };

        if !join.success {
            client_a.close().await;
            return early_fail(
                self.name(),
                start,
                "client_a_join_rejected",
                format!("{:?}", join.rejection_reason),
            );
        }

        // These are the action messages that would be valid in-session.
        // We capture them as raw JSON strings to replay verbatim on the new connection.
        let messages_to_replay = [
            serde_json::json!({"type": "kick_participant", "targetParticipantId": "peer-1"}),
            serde_json::json!({"type": "mute_participant", "targetParticipantId": "peer-1"}),
            serde_json::json!({"type": "start_share"}),
            serde_json::json!({"type": "leave"}),
            serde_json::json!({"type": "invite_create", "maxUses": 5}),
        ];

        // Serialize to raw strings now (simulating "captured" messages)
        let raw_messages: Vec<String> = messages_to_replay
            .iter()
            .map(|m| serde_json::to_string(m).unwrap())
            .collect();

        // =====================================================================
        // Phase 2 — Disconnect: Close client A (destroys the SignalingSession)
        // =====================================================================

        client_a.close().await;

        // Brief pause to let the backend process the disconnect
        tokio::time::sleep(Duration::from_millis(100)).await;

        // =====================================================================
        // Phase 3 — Replay: Connect client B (fresh connection, no session)
        //           and replay the captured messages
        // =====================================================================

        let mut client_b = match StressClient::connect(&ctx.ws_url).await {
            Ok(c) => c,
            Err(e) => {
                return early_fail(self.name(), start, "client_b_connect", format!("{e}"));
            }
        };

        for raw_msg in &raw_messages {
            // Send the raw captured message on the new connection (no session)
            if let Err(e) = client_b.send_raw(raw_msg).await {
                // If the connection was already closed by the backend, that's a valid rejection
                if matches!(e, StressError::Closed) {
                    // Connection closed = rejection. Reconnect for remaining messages.
                    client_b = match StressClient::connect(&ctx.ws_url).await {
                        Ok(c) => c,
                        Err(_) => break, // Can't reconnect — stop replaying
                    };
                    continue;
                }
                // Other send errors are unexpected
                violations.push(InvariantViolation {
                    invariant: "property_9: replay_send_must_not_error_unexpectedly".to_owned(),
                    expected: "send succeeds or connection closed".to_owned(),
                    actual: format!("unexpected send error: {e}"),
                });
                continue;
            }

            // Wait for an error response (timeout 3s)
            match client_b.recv_type("error", Duration::from_secs(3)).await {
                Ok(err_msg) => {
                    let msg = err_msg
                        .get("message")
                        .and_then(|v| v.as_str())
                        .unwrap_or("")
                        .to_lowercase();

                    // The backend should say "not authenticated" (pre-join gate)
                    if !msg.contains("not authenticated")
                        && !msg.contains("unauthenticated")
                        && !msg.contains("no session")
                        && !msg.contains("join first")
                        && !msg.contains("unauthorized")
                    {
                        violations.push(InvariantViolation {
                            invariant: format!(
                                "property_9: replayed_message_rejected_not_authenticated (msg={})",
                                raw_msg
                            ),
                            expected: "error containing 'not authenticated'".to_owned(),
                            actual: format!("error message: '{msg}'"),
                        });
                    }
                }
                Err(StressError::Closed) => {
                    // Connection closed by backend = valid rejection (pre-join gate closed it)
                    // Reconnect for remaining messages
                    client_b = match StressClient::connect(&ctx.ws_url).await {
                        Ok(c) => c,
                        Err(_) => break,
                    };
                }
                Err(StressError::Timeout(_)) => {
                    // No response within 3s — this is a violation: the backend must reject
                    violations.push(InvariantViolation {
                        invariant: format!(
                            "property_9: replayed_message_must_be_rejected (msg={})",
                            raw_msg
                        ),
                        expected: "error response or connection close within 3s".to_owned(),
                        actual: "no response (timeout)".to_owned(),
                    });
                }
                Err(e) => {
                    violations.push(InvariantViolation {
                        invariant: format!(
                            "property_9: replay_recv_unexpected_error (msg={})",
                            raw_msg
                        ),
                        expected: "error response or connection close".to_owned(),
                        actual: format!("unexpected error: {e}"),
                    });
                }
            }
        }

        client_b.close().await;

        build_result(self.name(), start, violations, latency)
    }
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

fn early_fail(
    name: &str,
    start: Instant,
    invariant: impl Into<String>,
    actual: impl std::fmt::Display,
) -> ScenarioResult {
    ScenarioResult {
        name: name.to_owned(),
        passed: false,
        duration: start.elapsed(),
        actions_per_second: 0.0,
        p95_latency: Duration::ZERO,
        p99_latency: Duration::ZERO,
        violations: vec![InvariantViolation {
            invariant: invariant.into(),
            expected: "success".to_owned(),
            actual: actual.to_string(),
        }],
    }
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

    host.send_json(&serde_json::json!({ "type": "invite_create", "maxUses": 10 }))
        .await
        .map_err(|e| format!("invite_create send failed: {e}"))?;

    let msg = host
        .recv_type("invite_created", Duration::from_secs(5))
        .await
        .map_err(|e| format!("invite_created recv failed: {e}"))?;

    let code = msg
        .get("inviteCode")
        .and_then(|v| v.as_str())
        .ok_or_else(|| "invite_created missing inviteCode".to_owned())?
        .to_owned();

    host.send_json(&serde_json::json!({ "type": "leave" }))
        .await
        .ok();
    host.close().await;

    Ok(code)
}
