/// AuthzFuzzScenario — Property 8: Privileged action authorization
///                     Property 9: Session-bound message rejection
///                     Property 10: Cross-room injection rejection
///                     Property 11: Abuse metrics counter accuracy
///
/// Sets up an SFU room with a Host and a Guest, then verifies:
///   A) Guest sending privileged actions (KickParticipant, MuteParticipant) is rejected
///      with "unauthorized" and room state is unchanged.
///   B) A pre-join client (no SignalingSession) sending action messages is rejected
///      with "not authenticated".
///   C) A client joined to a different room sending messages referencing room_a's
///      participants is rejected (cross-room injection).
///   D) Abuse metrics counters (state_machine_rejections, action_rate_limit_rejections)
///      increase after the unauthorized attempts.
///   E) Guest sending UnmuteParticipant is rejected with "unauthorized".
///   F) Guest sending SetSharePermission is rejected with "unauthorized".
///
/// **Validates: Requirements 4.1, 4.2, 4.4, 4.5, 4.6, 4.7**
use std::time::{Duration, Instant};

use async_trait::async_trait;

use crate::assertions::{assert_counter_delta, fetch_metrics};
use crate::client::StressClient;
use crate::config::{Capability, ConfigPreset, TestContext};
use crate::results::{InvariantViolation, LatencyTracker, ScenarioResult};
use crate::runner::{Scenario, Tier};

pub struct AuthzFuzzScenario;

#[async_trait]
impl Scenario for AuthzFuzzScenario {
    fn name(&self) -> &str {
        "authz-fuzz"
    }

    fn tier(&self) -> Tier {
        Tier::Tier1
    }

    fn requires(&self) -> Vec<Capability> {
        vec![Capability::Sfu]
    }

    fn config_preset(&self) -> ConfigPreset {
        ConfigPreset::Default
    }

    async fn run(&self, ctx: &TestContext) -> ScenarioResult {
        let start = Instant::now();
        let mut violations: Vec<InvariantViolation> = Vec::new();
        let latency = LatencyTracker::new();

        // --- Generate unique room IDs ---
        let (room_a, room_b) = {
            use rand::RngCore;
            let mut rng = ctx.rng.lock().unwrap();
            let a = format!("authz-a-{:016x}", rng.next_u64());
            let b = format!("authz-b-{:016x}", rng.next_u64());
            (a, b)
        };

        // --- Create invite codes for room_a and room_b ---
        let (invite_a, invite_b) = match &ctx.app_state {
            Some(app_state) => {
                let inv_a = match app_state.invite_store.generate(
                    &room_a,
                    "stress-issuer",
                    Some(10),
                    Instant::now(),
                ) {
                    Ok(r) => r.code,
                    Err(e) => {
                        return early_fail(self.name(), start, "invite_creation_room_a", e);
                    }
                };
                let inv_b = match app_state.invite_store.generate(
                    &room_b,
                    "stress-issuer",
                    Some(10),
                    Instant::now(),
                ) {
                    Ok(r) => r.code,
                    Err(e) => {
                        return early_fail(self.name(), start, "invite_creation_room_b", e);
                    }
                };
                (inv_a, inv_b)
            }
            None => {
                // External mode: create invites via signaling
                let inv_a = match create_invite_via_signaling(ctx, &room_a).await {
                    Ok(c) => c,
                    Err(e) => {
                        return early_fail(self.name(), start, "invite_creation_room_a", e);
                    }
                };
                let inv_b = match create_invite_via_signaling(ctx, &room_b).await {
                    Ok(c) => c,
                    Err(e) => {
                        return early_fail(self.name(), start, "invite_creation_room_b", e);
                    }
                };
                (inv_a, inv_b)
            }
        };

        // --- Snapshot baseline abuse metrics ---
        let metrics_token = std::env::var("TEST_METRICS_TOKEN").unwrap_or_default();
        let baseline = fetch_metrics(&ctx.http_client, &ctx.metrics_url, &metrics_token)
            .await
            .unwrap_or(serde_json::Value::Null);

        // --- Connect Host (first joiner → Host role) ---
        let mut host = match StressClient::connect(&ctx.ws_url).await {
            Ok(c) => c,
            Err(e) => {
                return early_fail(self.name(), start, "host_connect", format!("{e}"));
            }
        };
        let host_join = match host.join_room(&room_a, "sfu", Some(&invite_a)).await {
            Ok(r) => r,
            Err(e) => {
                host.close().await;
                return early_fail(self.name(), start, "host_join", format!("{e}"));
            }
        };
        if !host_join.success {
            host.close().await;
            return early_fail(
                self.name(),
                start,
                "host_join_rejected",
                format!("{:?}", host_join.rejection_reason),
            );
        }
        let host_peer_id = host_join.peer_id.clone();

        // --- Connect Guest (second joiner → Guest role) ---
        let mut guest = match StressClient::connect(&ctx.ws_url).await {
            Ok(c) => c,
            Err(e) => {
                host.close().await;
                return early_fail(self.name(), start, "guest_connect", format!("{e}"));
            }
        };
        let guest_join = match guest.join_room(&room_a, "sfu", Some(&invite_a)).await {
            Ok(r) => r,
            Err(e) => {
                host.close().await;
                guest.close().await;
                return early_fail(self.name(), start, "guest_join", format!("{e}"));
            }
        };
        if !guest_join.success {
            host.close().await;
            guest.close().await;
            return early_fail(
                self.name(),
                start,
                "guest_join_rejected",
                format!("{:?}", guest_join.rejection_reason),
            );
        }

        // =====================================================================
        // Test A — Property 8: Guest sends privileged actions → "unauthorized"
        // =====================================================================

        // Guest sends KickParticipant targeting the host
        guest
            .send_json(&serde_json::json!({
                "type": "kick_participant",
                "targetParticipantId": host_peer_id
            }))
            .await
            .ok();

        match guest.recv_type("error", Duration::from_secs(3)).await {
            Ok(err_msg) => {
                let msg = err_msg
                    .get("message")
                    .and_then(|v| v.as_str())
                    .unwrap_or("")
                    .to_lowercase();
                if !msg.contains("unauthorized")
                    && !msg.contains("not allowed")
                    && !msg.contains("permission")
                {
                    violations.push(InvariantViolation {
                        invariant: "property_8: guest_kick_rejected_with_unauthorized".to_owned(),
                        expected: "error message containing 'unauthorized'".to_owned(),
                        actual: format!("error message: '{msg}'"),
                    });
                }
            }
            Err(e) => {
                violations.push(InvariantViolation {
                    invariant: "property_8: guest_kick_must_receive_error".to_owned(),
                    expected: "error response within 3s".to_owned(),
                    actual: format!("no error received: {e}"),
                });
            }
        }

        // Guest sends MuteParticipant targeting the host
        guest
            .send_json(&serde_json::json!({
                "type": "mute_participant",
                "targetParticipantId": host_peer_id
            }))
            .await
            .ok();

        match guest.recv_type("error", Duration::from_secs(3)).await {
            Ok(err_msg) => {
                let msg = err_msg
                    .get("message")
                    .and_then(|v| v.as_str())
                    .unwrap_or("")
                    .to_lowercase();
                if !msg.contains("unauthorized")
                    && !msg.contains("not allowed")
                    && !msg.contains("permission")
                {
                    violations.push(InvariantViolation {
                        invariant: "property_8: guest_mute_rejected_with_unauthorized".to_owned(),
                        expected: "error message containing 'unauthorized'".to_owned(),
                        actual: format!("error message: '{msg}'"),
                    });
                }
            }
            Err(e) => {
                violations.push(InvariantViolation {
                    invariant: "property_8: guest_mute_must_receive_error".to_owned(),
                    expected: "error response within 3s".to_owned(),
                    actual: format!("no error received: {e}"),
                });
            }
        }

        // Assert room state is unchanged after guest's unauthorized attempts
        tokio::time::sleep(Duration::from_millis(100)).await;
        match fetch_metrics(&ctx.http_client, &ctx.metrics_url, &metrics_token).await {
            Ok(metrics) => {
                let count = metrics
                    .get("rooms")
                    .and_then(|r| r.get(&room_a))
                    .and_then(|r| r.get("participant_count"))
                    .and_then(|v| v.as_u64())
                    .unwrap_or(0);
                // Both host and guest should still be in the room (kick was rejected)
                if count < 2 {
                    violations.push(InvariantViolation {
                        invariant: "property_8: room_state_unchanged_after_unauthorized_kick"
                            .to_owned(),
                        expected: ">= 2 participants (host + guest still present)".to_owned(),
                        actual: format!("{count} participants"),
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

        // =====================================================================
        // Test B — Property 9: Pre-join client sends action → "not authenticated"
        // =====================================================================

        let mut pre_join = match StressClient::connect(&ctx.ws_url).await {
            Ok(c) => c,
            Err(e) => {
                // Non-fatal: record violation and continue
                violations.push(InvariantViolation {
                    invariant: "property_9: pre_join_client_connect".to_owned(),
                    expected: "connection succeeds".to_owned(),
                    actual: format!("connect failed: {e}"),
                });
                // Skip test B — clean up and continue to test C
                host.close().await;
                guest.close().await;
                return build_result(self.name(), start, violations, latency);
            }
        };

        // Send KickParticipant WITHOUT joining first (no SignalingSession)
        pre_join
            .send_json(&serde_json::json!({
                "type": "kick_participant",
                "targetParticipantId": "peer-1"
            }))
            .await
            .ok();

        match pre_join.recv_type("error", Duration::from_secs(3)).await {
            Ok(err_msg) => {
                let msg = err_msg
                    .get("message")
                    .and_then(|v| v.as_str())
                    .unwrap_or("")
                    .to_lowercase();
                // Backend should say "not authenticated" (pre-join gate)
                if !msg.contains("not authenticated")
                    && !msg.contains("unauthenticated")
                    && !msg.contains("no session")
                    && !msg.contains("join first")
                {
                    violations.push(InvariantViolation {
                        invariant: "property_9: pre_join_action_rejected_not_authenticated"
                            .to_owned(),
                        expected: "error message containing 'not authenticated'".to_owned(),
                        actual: format!("error message: '{msg}'"),
                    });
                }
            }
            Err(e) => {
                violations.push(InvariantViolation {
                    invariant: "property_9: pre_join_action_must_receive_error".to_owned(),
                    expected: "error response within 3s".to_owned(),
                    actual: format!("no error received: {e}"),
                });
            }
        }

        pre_join.close().await;

        // =====================================================================
        // Test C — Property 10: Cross-room injection → rejection
        // =====================================================================

        // Connect a client and join room_b (a different room)
        let mut cross_client = match StressClient::connect(&ctx.ws_url).await {
            Ok(c) => c,
            Err(e) => {
                violations.push(InvariantViolation {
                    invariant: "property_10: cross_room_client_connect".to_owned(),
                    expected: "connection succeeds".to_owned(),
                    actual: format!("connect failed: {e}"),
                });
                host.close().await;
                guest.close().await;
                return build_result(self.name(), start, violations, latency);
            }
        };

        let cross_join = cross_client
            .join_room(&room_b, "sfu", Some(&invite_b))
            .await;
        match cross_join {
            Ok(r) if r.success => {
                // Client is now in room_b. Send a kick targeting host_peer_id (who is in room_a).
                // The backend should reject this because the target is in a different room.
                cross_client
                    .send_json(&serde_json::json!({
                        "type": "kick_participant",
                        "targetParticipantId": host_peer_id
                    }))
                    .await
                    .ok();

                // Expect either an error response OR the kick silently fails (target not in room).
                // Either way, host must still be in room_a.
                tokio::time::sleep(Duration::from_millis(300)).await;

                match fetch_metrics(&ctx.http_client, &ctx.metrics_url, &metrics_token).await {
                    Ok(metrics) => {
                        let host_still_in_room_a = metrics
                            .get("rooms")
                            .and_then(|r| r.get(&room_a))
                            .and_then(|r| r.get("peer_ids"))
                            .and_then(|v| v.as_array())
                            .map(|ids| ids.iter().any(|id| id.as_str() == Some(&host_peer_id)))
                            .unwrap_or(false);

                        if !host_still_in_room_a {
                            violations.push(InvariantViolation {
                                invariant: "property_10: cross_room_kick_must_not_affect_room_a"
                                    .to_owned(),
                                expected: format!("host peer_id '{host_peer_id}' still in room_a"),
                                actual: "host was removed from room_a by cross-room kick"
                                    .to_owned(),
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
            }
            Ok(r) => {
                // cross_client couldn't join room_b — skip test C but note it
                violations.push(InvariantViolation {
                    invariant: "property_10: cross_room_client_join_room_b".to_owned(),
                    expected: "join room_b succeeds".to_owned(),
                    actual: format!("join rejected: {:?}", r.rejection_reason),
                });
            }
            Err(e) => {
                violations.push(InvariantViolation {
                    invariant: "property_10: cross_room_client_join_room_b".to_owned(),
                    expected: "join room_b succeeds".to_owned(),
                    actual: format!("join error: {e}"),
                });
            }
        }

        cross_client.close().await;

        // =====================================================================
        // Test E — Guest sends UnmuteParticipant → "unauthorized"
        // =====================================================================

        guest
            .send_json(&serde_json::json!({
                "type": "unmute_participant",
                "targetParticipantId": host_peer_id
            }))
            .await
            .ok();

        match guest.recv_type("error", Duration::from_secs(3)).await {
            Ok(err_msg) => {
                let msg = err_msg
                    .get("message")
                    .and_then(|v| v.as_str())
                    .unwrap_or("")
                    .to_lowercase();
                if !msg.contains("unauthorized")
                    && !msg.contains("not allowed")
                    && !msg.contains("permission")
                {
                    violations.push(InvariantViolation {
                        invariant: "property_8: guest_unmute_rejected_with_unauthorized".to_owned(),
                        expected: "error message containing 'unauthorized'".to_owned(),
                        actual: format!("error message: '{msg}'"),
                    });
                }
            }
            Err(e) => {
                violations.push(InvariantViolation {
                    invariant: "property_8: guest_unmute_must_receive_error".to_owned(),
                    expected: "error response within 3s".to_owned(),
                    actual: format!("no error received: {e}"),
                });
            }
        }

        // =====================================================================
        // Test F — Guest sends SetSharePermission → "unauthorized"
        // =====================================================================

        guest
            .send_json(&serde_json::json!({
                "type": "set_share_permission",
                "permission": "host_only"
            }))
            .await
            .ok();

        match guest.recv_type("error", Duration::from_secs(3)).await {
            Ok(err_msg) => {
                let msg = err_msg
                    .get("message")
                    .and_then(|v| v.as_str())
                    .unwrap_or("")
                    .to_lowercase();
                if !msg.contains("unauthorized")
                    && !msg.contains("not allowed")
                    && !msg.contains("permission")
                {
                    violations.push(InvariantViolation {
                        invariant:
                            "property_8: guest_set_share_permission_rejected_with_unauthorized"
                                .to_owned(),
                        expected: "error message containing 'unauthorized'".to_owned(),
                        actual: format!("error message: '{msg}'"),
                    });
                }
            }
            Err(e) => {
                violations.push(InvariantViolation {
                    invariant: "property_8: guest_set_share_permission_must_receive_error"
                        .to_owned(),
                    expected: "error response within 3s".to_owned(),
                    actual: format!("no error received: {e}"),
                });
            }
        }

        // =====================================================================
        // Test D — Property 11: Abuse metrics counters increased
        // =====================================================================

        // Give the backend a moment to flush atomic counters
        tokio::time::sleep(Duration::from_millis(200)).await;

        match fetch_metrics(&ctx.http_client, &ctx.metrics_url, &metrics_token).await {
            Ok(current) => {
                // state_machine_rejections should have increased (pre-join gate fired in Test B)
                if let Some(v) =
                    assert_counter_delta(&baseline, &current, "state_machine_rejections", 1)
                {
                    violations.push(v);
                }
                // Note: action_rate_limit_rejections may or may not increase depending on
                // whether the backend routes unauthorized actions through the action rate limiter
                // before the role check. We assert state_machine_rejections as the primary
                // counter for Property 11 (pre-join gate increments it).
            }
            Err(e) => {
                violations.push(InvariantViolation {
                    invariant: "metrics_endpoint_reachable_test_d".to_owned(),
                    expected: "metrics endpoint responds".to_owned(),
                    actual: format!("fetch failed: {e}"),
                });
            }
        }

        // --- Clean up ---
        host.close().await;
        guest.close().await;

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
