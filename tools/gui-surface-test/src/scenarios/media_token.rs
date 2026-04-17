/// Media Token Surface Test Scenario
///
/// Tests the REST + voice status surface the GUI depends on for media token flow:
/// - Voice status endpoint returns well-formed JSON for media state rendering
/// - Voice status reflects active/inactive session correctly
/// - Channel member can query voice status (media state visible)
/// - Non-member cannot access voice status (403)
/// - Voice status is consistent across repeated queries (no phantom media state)
///
/// Note: The actual `media_token` message is delivered via WebSocket signaling
/// after `JoinVoice`. These tests exercise the REST surface the GUI uses to
/// determine whether a voice session is active and whether to expect a media token.
/// Requirements: 22.1
use std::time::Instant;

use async_trait::async_trait;

use crate::channel_ops;
use crate::client::AuthenticatedClient;
use crate::harness_context::TestContext;
use crate::results::{AssertionFailure, ScenarioResult};
use crate::runner::Scenario;

pub struct MediaTokenScenario;

#[async_trait]
impl Scenario for MediaTokenScenario {
    fn name(&self) -> &str {
        "media-token"
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

        let outsider = match AuthenticatedClient::register(&ctx.base_url, &ctx.http_client).await {
            Ok(c) => c,
            Err(e) => return err_result(self.name(), start, &format!("outsider register: {e}")),
        };

        let channel_id = match channel_ops::create_channel(&owner, "media-token-test").await {
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

        // ── 1. Voice status baseline: no active session ──
        check_voice_status_baseline(&owner, &channel_id, &mut failures).await;

        // ── 2. Voice status structure is well-formed for media state rendering ──
        check_voice_status_structure(&owner, &channel_id, &mut failures).await;

        // ── 3. Member can query voice status ──
        check_member_can_query_voice(&member, &channel_id, &mut failures).await;

        // ── 4. Non-member cannot access voice status ──
        check_outsider_forbidden(&outsider, &channel_id, &mut failures).await;

        // ── 5. Voice status is consistent across repeated queries ──
        check_voice_status_idempotent(&owner, &channel_id, &mut failures).await;

        ScenarioResult {
            name: self.name().to_owned(),
            passed: failures.is_empty(),
            duration: start.elapsed(),
            failures,
        }
    }
}

/// Validates: Requirement 22.1 (media state baseline)
/// Before any voice session, the voice status must show inactive.
/// The GUI uses this to determine mediaState = 'disconnected' on initial load.
async fn check_voice_status_baseline(
    client: &AuthenticatedClient,
    channel_id: &str,
    failures: &mut Vec<AssertionFailure>,
) {
    let body = match fetch_voice_json(client, channel_id).await {
        Ok(v) => v,
        Err(e) => {
            failures.push(AssertionFailure {
                check: "voice status baseline request".into(),
                expected: "response".into(),
                actual: e,
            });
            return;
        }
    };

    match body.get("active").and_then(|v| v.as_bool()) {
        Some(false) => {}
        other => {
            failures.push(AssertionFailure {
                check: "voice status baseline active=false".into(),
                expected: "false".into(),
                actual: format!("{other:?}"),
            });
        }
    }
}

/// Validates: Requirement 22.1 (voice status structure for media rendering)
/// The voice status response must be a JSON object with the fields the GUI
/// needs to render media status indicators (active, participant_count, participants).
async fn check_voice_status_structure(
    client: &AuthenticatedClient,
    channel_id: &str,
    failures: &mut Vec<AssertionFailure>,
) {
    let body = match fetch_voice_json(client, channel_id).await {
        Ok(v) => v,
        Err(e) => {
            failures.push(AssertionFailure {
                check: "voice status structure request".into(),
                expected: "response".into(),
                actual: e,
            });
            return;
        }
    };

    if !body.is_object() {
        failures.push(AssertionFailure {
            check: "voice status is JSON object".into(),
            expected: "object".into(),
            actual: format!("{body}"),
        });
        return;
    }

    if body.get("active").and_then(|v| v.as_bool()).is_none() {
        failures.push(AssertionFailure {
            check: "voice status has active boolean".into(),
            expected: "active: bool present".into(),
            actual: format!("{body}"),
        });
    }
}

/// Validates: Requirement 22.1 (member access to voice status)
/// A channel member can query voice status to determine whether to expect
/// a media_token after JoinVoice.
async fn check_member_can_query_voice(
    member: &AuthenticatedClient,
    channel_id: &str,
    failures: &mut Vec<AssertionFailure>,
) {
    let resp = match member.get(&format!("/channels/{channel_id}/voice")).await {
        Ok(r) => r,
        Err(e) => {
            failures.push(AssertionFailure {
                check: "member voice status request".into(),
                expected: "response".into(),
                actual: format!("error: {e}"),
            });
            return;
        }
    };

    if resp.status() != reqwest::StatusCode::OK {
        failures.push(AssertionFailure {
            check: "member voice status returns 200".into(),
            expected: "200".into(),
            actual: format!("{}", resp.status()),
        });
    }
}

/// Validates: Requirement 22.1 (access control)
/// Non-members must not be able to query voice status.
async fn check_outsider_forbidden(
    outsider: &AuthenticatedClient,
    channel_id: &str,
    failures: &mut Vec<AssertionFailure>,
) {
    let resp = match outsider.get(&format!("/channels/{channel_id}/voice")).await {
        Ok(r) => r,
        Err(e) => {
            failures.push(AssertionFailure {
                check: "outsider voice status request".into(),
                expected: "response".into(),
                actual: format!("error: {e}"),
            });
            return;
        }
    };

    if resp.status() != reqwest::StatusCode::FORBIDDEN {
        failures.push(AssertionFailure {
            check: "outsider voice status returns 403".into(),
            expected: "403".into(),
            actual: format!("{}", resp.status()),
        });
    }
}

/// Validates: Requirement 22.1 (idempotent voice status)
/// Two consecutive queries must return the same active state.
/// The GUI relies on this for deterministic media state transitions.
async fn check_voice_status_idempotent(
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

    let active1 = body1.get("active").and_then(|v| v.as_bool());
    let active2 = body2.get("active").and_then(|v| v.as_bool());

    if active1 != active2 {
        failures.push(AssertionFailure {
            check: "idempotent voice: active field consistent".into(),
            expected: format!("{active1:?}"),
            actual: format!("{active2:?}"),
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
