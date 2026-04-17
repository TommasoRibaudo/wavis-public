/// Voice Room Reconnection & Leave Scenarios
///
/// Tests the REST surface the GUI depends on for reconnection and leave lifecycle:
/// - Voice status returns inactive after no active session (leave/disconnect cleanup)
/// - Idempotent voice status queries return consistent results (mirrors idempotent leave)
/// - Channel detail still accessible after voice leave (leave doesn't affect membership)
/// - Error handling: invalid channel IDs, missing auth for voice endpoints
/// - Concurrent voice status queries handled gracefully
///
/// Note: Reconnection, leave, and peer-left are WebSocket signaling operations.
/// These tests exercise the REST query and error surface the GUI depends on
/// for session lifecycle state and error handling.
/// Requirements: 14.2, 14.3, 14.5, 16.1, 16.2, 16.3, 16.4, 17.1, 17.3
use std::time::Instant;

use async_trait::async_trait;

use crate::channel_ops;
use crate::client::AuthenticatedClient;
use crate::harness_context::TestContext;
use crate::results::{AssertionFailure, ScenarioResult};
use crate::runner::Scenario;

pub struct VoiceRoomReconnectScenario;

#[async_trait]
impl Scenario for VoiceRoomReconnectScenario {
    fn name(&self) -> &str {
        "voice-room-reconnect"
    }

    async fn run(&self, ctx: &TestContext) -> ScenarioResult {
        let start = Instant::now();
        let mut failures: Vec<AssertionFailure> = Vec::new();

        // --- Setup: owner creates channel, member joins ---
        let owner = match AuthenticatedClient::register(&ctx.base_url, &ctx.http_client).await {
            Ok(c) => c,
            Err(e) => return err_result(self.name(), start, &format!("owner register: {e}")),
        };

        let member = match AuthenticatedClient::register(&ctx.base_url, &ctx.http_client).await {
            Ok(c) => c,
            Err(e) => return err_result(self.name(), start, &format!("member register: {e}")),
        };

        let channel_id = match channel_ops::create_channel(&owner, "reconnect-leave-test").await {
            Ok(id) => id,
            Err(e) => return err_result(self.name(), start, &e),
        };

        let invite_code = match channel_ops::create_invite(&owner, &channel_id).await {
            Ok(code) => code,
            Err(e) => return err_result(self.name(), start, &e),
        };

        if let Err(e) = channel_ops::join_channel(&member, &channel_id, &invite_code).await {
            return err_result(self.name(), start, &e);
        }

        // ── 1. Voice status inactive after no session (leave/disconnect cleanup) ──
        check_voice_inactive_after_no_session(&owner, &channel_id, &mut failures).await;

        // ── 2. Idempotent voice status queries (mirrors idempotent leave) ──
        check_idempotent_voice_queries(&owner, &channel_id, &mut failures).await;

        // ── 3. Channel detail still accessible (leave doesn't affect membership) ──
        check_channel_accessible_after_leave(&member, &channel_id, &mut failures).await;

        // ── 4. Error: invalid channel ID returns 403 for voice endpoint ──
        check_invalid_channel_voice_error(&owner, &mut failures).await;

        // ── 5. Error: missing auth returns 401 for voice endpoint ──
        check_missing_auth_voice_error(ctx, &channel_id, &mut failures).await;

        // ── 6. Concurrent voice status queries return consistent results ──
        check_concurrent_voice_queries(&owner, &channel_id, &mut failures).await;

        ScenarioResult {
            name: self.name().to_owned(),
            passed: failures.is_empty(),
            duration: start.elapsed(),
            failures,
        }
    }
}

/// Validates: Requirements 14.2, 14.3, 16.1
/// When no voice session is active (after leave/disconnect), the voice status
/// endpoint must return inactive state. This mirrors the GUI's state after
/// leaveRoom() sends Leave, disconnects, and clears local state.
async fn check_voice_inactive_after_no_session(
    client: &AuthenticatedClient,
    channel_id: &str,
    failures: &mut Vec<AssertionFailure>,
) {
    let resp = match client.get(&format!("/channels/{channel_id}/voice")).await {
        Ok(r) => r,
        Err(e) => {
            failures.push(AssertionFailure {
                check: "voice inactive after no session request".into(),
                expected: "response".into(),
                actual: format!("error: {e}"),
            });
            return;
        }
    };

    if resp.status() != reqwest::StatusCode::OK {
        failures.push(AssertionFailure {
            check: "voice inactive after no session status".into(),
            expected: "200".into(),
            actual: format!("{}", resp.status()),
        });
        return;
    }

    let body: serde_json::Value = match resp.json().await {
        Ok(v) => v,
        Err(e) => {
            failures.push(AssertionFailure {
                check: "voice inactive after no session parse".into(),
                expected: "valid JSON".into(),
                actual: format!("{e}"),
            });
            return;
        }
    };

    // active must be false — no session means leave/disconnect cleanup is complete
    match body.get("active").and_then(|v| v.as_bool()) {
        Some(false) => {}
        other => {
            failures.push(AssertionFailure {
                check: "voice inactive after no session active=false".into(),
                expected: "false".into(),
                actual: format!("{other:?}"),
            });
        }
    }

    // No participants should be present after leave
    if let Some(count) = body.get("participant_count").and_then(|v| v.as_u64())
        && count > 0
    {
        failures.push(AssertionFailure {
            check: "no participants after leave/disconnect".into(),
            expected: "0 or absent".into(),
            actual: format!("{count}"),
        });
    }
}

/// Validates: Requirements 14.5, 16.3
/// Multiple queries to the voice status endpoint must return consistent results.
/// This mirrors the GUI's idempotent leave behavior — calling leaveRoom() when
/// already disconnected completes without error, and querying state after leave
/// always returns the same inactive baseline.
async fn check_idempotent_voice_queries(
    client: &AuthenticatedClient,
    channel_id: &str,
    failures: &mut Vec<AssertionFailure>,
) {
    let body1 = match fetch_voice_json(client, channel_id).await {
        Ok(v) => v,
        Err(e) => {
            failures.push(AssertionFailure {
                check: "idempotent voice query 1".into(),
                expected: "response".into(),
                actual: e,
            });
            return;
        }
    };

    let body2 = match fetch_voice_json(client, channel_id).await {
        Ok(v) => v,
        Err(e) => {
            failures.push(AssertionFailure {
                check: "idempotent voice query 2".into(),
                expected: "response".into(),
                actual: e,
            });
            return;
        }
    };

    let body3 = match fetch_voice_json(client, channel_id).await {
        Ok(v) => v,
        Err(e) => {
            failures.push(AssertionFailure {
                check: "idempotent voice query 3".into(),
                expected: "response".into(),
                actual: e,
            });
            return;
        }
    };

    // All three queries must return the same active state
    let active1 = body1.get("active").and_then(|v| v.as_bool());
    let active2 = body2.get("active").and_then(|v| v.as_bool());
    let active3 = body3.get("active").and_then(|v| v.as_bool());

    if active1 != active2 || active2 != active3 {
        failures.push(AssertionFailure {
            check: "idempotent voice queries: active field consistent across 3 calls".into(),
            expected: format!("{active1:?}"),
            actual: format!("q1={active1:?}, q2={active2:?}, q3={active3:?}"),
        });
    }

    // participant_count must also be consistent
    let count1 = body1.get("participant_count").and_then(|v| v.as_u64());
    let count2 = body2.get("participant_count").and_then(|v| v.as_u64());
    let count3 = body3.get("participant_count").and_then(|v| v.as_u64());

    if count1 != count2 || count2 != count3 {
        failures.push(AssertionFailure {
            check: "idempotent voice queries: participant_count consistent".into(),
            expected: format!("{count1:?}"),
            actual: format!("q1={count1:?}, q2={count2:?}, q3={count3:?}"),
        });
    }
}

/// Validates: Requirements 14.3, 14.5
/// After leaving voice (or never joining), the member can still access channel
/// detail. Leave only disconnects the voice session — it does not affect channel
/// membership. The GUI navigates to /channel/{channelId} after leave, so the
/// channel detail endpoint must remain accessible.
async fn check_channel_accessible_after_leave(
    member: &AuthenticatedClient,
    channel_id: &str,
    failures: &mut Vec<AssertionFailure>,
) {
    // Channel detail must still be accessible
    let resp = match member.get(&format!("/channels/{channel_id}")).await {
        Ok(r) => r,
        Err(e) => {
            failures.push(AssertionFailure {
                check: "channel detail after leave request".into(),
                expected: "response".into(),
                actual: format!("error: {e}"),
            });
            return;
        }
    };

    if resp.status() != reqwest::StatusCode::OK {
        failures.push(AssertionFailure {
            check: "channel detail after leave returns 200".into(),
            expected: "200".into(),
            actual: format!("{}", resp.status()),
        });
        return;
    }

    let body: serde_json::Value = match resp.json().await {
        Ok(v) => v,
        Err(e) => {
            failures.push(AssertionFailure {
                check: "channel detail after leave parse".into(),
                expected: "valid JSON".into(),
                actual: format!("{e}"),
            });
            return;
        }
    };

    // Channel name must be present (GUI displays it after navigating back)
    if body.get("name").and_then(|v| v.as_str()).is_none() {
        failures.push(AssertionFailure {
            check: "channel detail has name after leave".into(),
            expected: "name string present".into(),
            actual: format!("{body}"),
        });
    }

    // Member role must still be present (membership not affected by voice leave)
    if body.get("role").and_then(|v| v.as_str()).is_none() {
        failures.push(AssertionFailure {
            check: "channel detail has role after leave".into(),
            expected: "role string present".into(),
            actual: format!("{body}"),
        });
    }

    // Voice status must also still be queryable by the member
    let voice_resp = match member.get(&format!("/channels/{channel_id}/voice")).await {
        Ok(r) => r,
        Err(e) => {
            failures.push(AssertionFailure {
                check: "voice status after leave request".into(),
                expected: "response".into(),
                actual: format!("error: {e}"),
            });
            return;
        }
    };

    if voice_resp.status() != reqwest::StatusCode::OK {
        failures.push(AssertionFailure {
            check: "voice status after leave returns 200".into(),
            expected: "200".into(),
            actual: format!("{}", voice_resp.status()),
        });
    }
}

/// Validates: Requirements 17.1, 17.3
/// Invalid/nonexistent channel IDs must return an error response for the voice
/// endpoint. The GUI displays signaling errors as opaque user-facing messages.
/// The backend returns 403 (not 404) to avoid leaking channel existence.
async fn check_invalid_channel_voice_error(
    client: &AuthenticatedClient,
    failures: &mut Vec<AssertionFailure>,
) {
    let fake_id = uuid::Uuid::new_v4();
    let resp = match client.get(&format!("/channels/{fake_id}/voice")).await {
        Ok(r) => r,
        Err(e) => {
            failures.push(AssertionFailure {
                check: "invalid channel voice request".into(),
                expected: "response".into(),
                actual: format!("error: {e}"),
            });
            return;
        }
    };

    // Backend returns 403 to hide channel existence (same as channel detail)
    if resp.status() != reqwest::StatusCode::FORBIDDEN {
        failures.push(AssertionFailure {
            check: "invalid channel voice returns 403".into(),
            expected: "403".into(),
            actual: format!("{}", resp.status()),
        });
    }

    // Also test channel detail with invalid ID
    let resp = match client.get(&format!("/channels/{fake_id}")).await {
        Ok(r) => r,
        Err(e) => {
            failures.push(AssertionFailure {
                check: "invalid channel detail request".into(),
                expected: "response".into(),
                actual: format!("error: {e}"),
            });
            return;
        }
    };

    if resp.status() != reqwest::StatusCode::FORBIDDEN {
        failures.push(AssertionFailure {
            check: "invalid channel detail returns 403".into(),
            expected: "403".into(),
            actual: format!("{}", resp.status()),
        });
    }
}

/// Validates: Requirement 16.4
/// The voice endpoint must return 401 when no auth header is provided.
/// This mirrors the GUI's error handling: if the access token is missing
/// or expired during reconnection, the auth flow must re-authenticate
/// before querying voice state.
async fn check_missing_auth_voice_error(
    ctx: &TestContext,
    channel_id: &str,
    failures: &mut Vec<AssertionFailure>,
) {
    let url = format!("{}/channels/{channel_id}/voice", ctx.base_url);
    let resp = match ctx.http_client.get(&url).send().await {
        Ok(r) => r,
        Err(e) => {
            failures.push(AssertionFailure {
                check: "missing auth voice request".into(),
                expected: "response".into(),
                actual: format!("error: {e}"),
            });
            return;
        }
    };

    if resp.status() != reqwest::StatusCode::UNAUTHORIZED {
        failures.push(AssertionFailure {
            check: "missing auth voice returns 401".into(),
            expected: "401".into(),
            actual: format!("{}", resp.status()),
        });
    }
}

/// Validates: Requirements 16.1, 16.2
/// Concurrent queries to the voice status endpoint must all succeed and return
/// consistent results. This mirrors the GUI's reconnection scenario where
/// multiple components may query voice state simultaneously after reconnect.
async fn check_concurrent_voice_queries(
    client: &AuthenticatedClient,
    channel_id: &str,
    failures: &mut Vec<AssertionFailure>,
) {
    // Fire 5 concurrent requests using tokio::spawn
    let mut handles = Vec::new();
    for i in 0..5u8 {
        let url = format!("{}/channels/{channel_id}/voice", client.base_url);
        let token = client.access_token.clone();
        let http = client.http.clone();
        handles.push(tokio::spawn(async move {
            let resp = http
                .get(&url)
                .header("Authorization", format!("Bearer {token}"))
                .send()
                .await;
            (i, resp)
        }));
    }

    let mut active_values: Vec<Option<bool>> = Vec::new();
    for handle in handles {
        match handle.await {
            Ok((i, Ok(resp))) => {
                if resp.status() != reqwest::StatusCode::OK {
                    failures.push(AssertionFailure {
                        check: format!("concurrent voice query {i} status"),
                        expected: "200".into(),
                        actual: format!("{}", resp.status()),
                    });
                    continue;
                }
                match resp.json::<serde_json::Value>().await {
                    Ok(body) => {
                        active_values.push(body.get("active").and_then(|v| v.as_bool()));
                    }
                    Err(e) => {
                        failures.push(AssertionFailure {
                            check: format!("concurrent voice query {i} parse"),
                            expected: "valid JSON".into(),
                            actual: format!("{e}"),
                        });
                    }
                }
            }
            Ok((i, Err(e))) => {
                failures.push(AssertionFailure {
                    check: format!("concurrent voice query {i} request"),
                    expected: "response".into(),
                    actual: format!("error: {e}"),
                });
            }
            Err(e) => {
                failures.push(AssertionFailure {
                    check: "concurrent voice query task".into(),
                    expected: "task completed".into(),
                    actual: format!("join error: {e}"),
                });
            }
        }
    }

    // All concurrent responses must agree on the active state
    if !active_values.is_empty() {
        let first = &active_values[0];
        for (i, val) in active_values.iter().enumerate().skip(1) {
            if val != first {
                failures.push(AssertionFailure {
                    check: format!("concurrent voice queries consistent: q0 vs q{i}"),
                    expected: format!("{first:?}"),
                    actual: format!("{val:?}"),
                });
            }
        }
    }
}

/// Helper: fetch voice status as parsed JSON.
async fn fetch_voice_json(
    client: &AuthenticatedClient,
    channel_id: &str,
) -> Result<serde_json::Value, String> {
    let resp = client
        .get(&format!("/channels/{channel_id}/voice"))
        .await
        .map_err(|e| format!("request failed: {e}"))?;

    if resp.status() != reqwest::StatusCode::OK {
        return Err(format!("status {}", resp.status()));
    }

    resp.json().await.map_err(|e| format!("parse failed: {e}"))
}

fn err_result(name: &str, start: Instant, msg: &str) -> ScenarioResult {
    ScenarioResult {
        name: name.to_owned(),
        passed: false,
        duration: start.elapsed(),
        failures: vec![AssertionFailure {
            check: "setup".into(),
            expected: "success".into(),
            actual: msg.to_owned(),
        }],
    }
}
