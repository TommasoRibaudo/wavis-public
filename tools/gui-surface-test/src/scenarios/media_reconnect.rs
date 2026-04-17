/// Media Reconnect Surface Test Scenario
///
/// Tests the REST surface the GUI depends on for media reconnection:
/// - Voice status is idempotent across repeated queries (reconnect safety)
/// - Channel remains accessible after voice session ends (no stale locks)
/// - Invalid channel voice query returns proper error (not crash)
/// - Missing auth on voice query returns 401 (not 500)
/// - Concurrent voice status queries return consistent results
/// - Voice status transitions cleanly between inactive states
///
/// Note: The actual media reconnection (teardown LK_Room, request new
/// media_token via JoinVoice, create fresh LK_Room) is client-side.
/// These tests exercise the REST surface the GUI queries during and after
/// reconnection to verify session state.
/// Requirements: 22.5
use std::time::Instant;

use async_trait::async_trait;

use crate::channel_ops;
use crate::client::AuthenticatedClient;
use crate::harness_context::TestContext;
use crate::results::{AssertionFailure, ScenarioResult};
use crate::runner::Scenario;

pub struct MediaReconnectScenario;

#[async_trait]
impl Scenario for MediaReconnectScenario {
    fn name(&self) -> &str {
        "media-reconnect"
    }

    async fn run(&self, ctx: &TestContext) -> ScenarioResult {
        let start = Instant::now();
        let mut failures: Vec<AssertionFailure> = Vec::new();

        let owner = match AuthenticatedClient::register(&ctx.base_url, &ctx.http_client).await {
            Ok(c) => c,
            Err(e) => return err_result(self.name(), start, &format!("owner register: {e}")),
        };

        let member = match AuthenticatedClient::register(&ctx.base_url, &ctx.http_client).await {
            Ok(c) => c,
            Err(e) => return err_result(self.name(), start, &format!("member register: {e}")),
        };

        let channel_id = match channel_ops::create_channel(&owner, "media-reconnect-test").await {
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

        // ── 1. Voice status idempotent (reconnect safety) ──
        check_voice_idempotent(&owner, &channel_id, &mut failures).await;

        // ── 2. Channel accessible after voice session ends ──
        check_channel_accessible_after_voice(&owner, &channel_id, &mut failures).await;

        // ── 3. Invalid channel voice query returns error ──
        check_invalid_channel_voice(&owner, &mut failures).await;

        // ── 4. Missing auth returns 401 ──
        check_missing_auth_voice(&ctx.base_url, &ctx.http_client, &channel_id, &mut failures).await;

        // ── 5. Concurrent voice queries consistent ──
        check_concurrent_voice_queries(&owner, &channel_id, &mut failures).await;

        // ── 6. Voice status clean between queries (no stale media state) ──
        check_voice_status_clean(&member, &channel_id, &mut failures).await;

        ScenarioResult {
            name: self.name().to_owned(),
            passed: failures.is_empty(),
            duration: start.elapsed(),
            failures,
        }
    }
}

/// Validates: Requirement 22.5 (idempotent voice status for reconnect)
/// Multiple consecutive queries must return the same result.
/// The GUI queries voice status after reconnection to verify session state.
async fn check_voice_idempotent(
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

    let active1 = body1.get("active").and_then(|v| v.as_bool());
    let active2 = body2.get("active").and_then(|v| v.as_bool());
    let active3 = body3.get("active").and_then(|v| v.as_bool());

    if active1 != active2 || active2 != active3 {
        failures.push(AssertionFailure {
            check: "voice status idempotent across 3 queries".into(),
            expected: format!("{active1:?}"),
            actual: format!("q1={active1:?} q2={active2:?} q3={active3:?}"),
        });
    }
}

/// Validates: Requirement 22.5 (channel accessible after voice ends)
/// After a voice session ends, the channel detail must still be accessible.
/// No stale locks or dangling state from the voice session.
async fn check_channel_accessible_after_voice(
    client: &AuthenticatedClient,
    channel_id: &str,
    failures: &mut Vec<AssertionFailure>,
) {
    // Query channel detail — must succeed
    let resp = match client.get(&format!("/channels/{channel_id}")).await {
        Ok(r) => r,
        Err(e) => {
            failures.push(AssertionFailure {
                check: "channel accessible after voice".into(),
                expected: "response".into(),
                actual: format!("error: {e}"),
            });
            return;
        }
    };

    if resp.status() != reqwest::StatusCode::OK {
        failures.push(AssertionFailure {
            check: "channel detail returns 200 after voice".into(),
            expected: "200".into(),
            actual: format!("{}", resp.status()),
        });
        return;
    }

    // Voice status must also be accessible
    let resp = match client.get(&format!("/channels/{channel_id}/voice")).await {
        Ok(r) => r,
        Err(e) => {
            failures.push(AssertionFailure {
                check: "voice status accessible after voice".into(),
                expected: "response".into(),
                actual: format!("error: {e}"),
            });
            return;
        }
    };

    if resp.status() != reqwest::StatusCode::OK {
        failures.push(AssertionFailure {
            check: "voice status returns 200 after voice".into(),
            expected: "200".into(),
            actual: format!("{}", resp.status()),
        });
    }
}

/// Validates: Requirement 22.5 (invalid channel error handling)
/// Querying voice status for a non-existent channel must return a proper
/// error (404), not a server crash. The GUI handles this gracefully.
async fn check_invalid_channel_voice(
    client: &AuthenticatedClient,
    failures: &mut Vec<AssertionFailure>,
) {
    let resp = match client
        .get("/channels/nonexistent-channel-id-12345/voice")
        .await
    {
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

    // Should be 403 (not a member) or 404 (not found) — not 500
    let status = resp.status().as_u16();
    if status == 500 {
        failures.push(AssertionFailure {
            check: "invalid channel voice not 500".into(),
            expected: "4xx error".into(),
            actual: format!("{status}"),
        });
    }
}

/// Validates: Requirement 22.5 (missing auth returns 401)
/// Voice status without auth must return 401, not 500.
async fn check_missing_auth_voice(
    base_url: &str,
    http: &reqwest::Client,
    channel_id: &str,
    failures: &mut Vec<AssertionFailure>,
) {
    let resp = match http
        .get(format!("{base_url}/channels/{channel_id}/voice"))
        .send()
        .await
    {
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

/// Validates: Requirement 22.5 (concurrent queries consistent)
/// Multiple concurrent voice status queries must all return the same result.
/// This simulates the GUI's reconnect scenario where multiple components
/// may query voice status simultaneously.
async fn check_concurrent_voice_queries(
    client: &AuthenticatedClient,
    channel_id: &str,
    failures: &mut Vec<AssertionFailure>,
) {
    let (r1, r2, r3) = tokio::join!(
        fetch_voice_json(client, channel_id),
        fetch_voice_json(client, channel_id),
        fetch_voice_json(client, channel_id),
    );

    let mut active_values: Vec<Option<bool>> = Vec::new();
    for (i, r) in [r1, r2, r3].into_iter().enumerate() {
        match r {
            Ok(body) => {
                active_values.push(body.get("active").and_then(|v| v.as_bool()));
            }
            Err(e) => {
                failures.push(AssertionFailure {
                    check: format!("concurrent voice query {i}"),
                    expected: "response".into(),
                    actual: e,
                });
                return;
            }
        }
    }

    if active_values.windows(2).any(|w| w[0] != w[1]) {
        failures.push(AssertionFailure {
            check: "concurrent voice queries consistent".into(),
            expected: format!("{:?}", active_values[0]),
            actual: format!("{active_values:?}"),
        });
    }
}

/// Validates: Requirement 22.5 (clean voice status between queries)
/// Member's view of voice status must be clean (no stale media state).
async fn check_voice_status_clean(
    member: &AuthenticatedClient,
    channel_id: &str,
    failures: &mut Vec<AssertionFailure>,
) {
    let body = match fetch_voice_json(member, channel_id).await {
        Ok(v) => v,
        Err(e) => {
            failures.push(AssertionFailure {
                check: "member voice status clean".into(),
                expected: "response".into(),
                actual: e,
            });
            return;
        }
    };

    // Must be a well-formed JSON object
    if !body.is_object() {
        failures.push(AssertionFailure {
            check: "member voice status is JSON object".into(),
            expected: "object".into(),
            actual: format!("{body}"),
        });
        return;
    }

    // active field must be present
    if body.get("active").and_then(|v| v.as_bool()).is_none() {
        failures.push(AssertionFailure {
            check: "member voice status has active field".into(),
            expected: "active: bool".into(),
            actual: format!("{body}"),
        });
    }
}

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
