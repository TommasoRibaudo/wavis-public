/// Screen Share Lifecycle Surface Test Scenario
///
/// Tests the REST surface the GUI depends on for screen share lifecycle:
/// - Voice status structure supports share state rendering
/// - Owner (host) can stop others' shares (role gating)
/// - Member (guest) cannot stop others' shares
/// - Non-member cannot access share state
/// - Share state is deterministic across queries (no phantom shares)
/// - Channel detail includes member list for share participant resolution
///
/// Note: StartShare/StopShare/ShareStarted/ShareStopped are WS signaling.
/// The actual screen share track is published via LiveKit SDK client-side.
/// These tests exercise the REST surface the GUI uses to gate share controls
/// and resolve participant identities for share display.
/// Requirements: 22.3
use std::time::Instant;

use async_trait::async_trait;

use crate::channel_ops;
use crate::client::AuthenticatedClient;
use crate::harness_context::TestContext;
use crate::results::{AssertionFailure, ScenarioResult};
use crate::runner::Scenario;

pub struct ScreenShareLifecycleScenario;

#[async_trait]
impl Scenario for ScreenShareLifecycleScenario {
    fn name(&self) -> &str {
        "screen-share-lifecycle"
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

        let outsider = match AuthenticatedClient::register(&ctx.base_url, &ctx.http_client).await {
            Ok(c) => c,
            Err(e) => return err_result(self.name(), start, &format!("outsider register: {e}")),
        };

        let channel_id = match channel_ops::create_channel(&owner, "share-lifecycle-test").await {
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

        // ── 1. Voice status baseline: no active shares ──
        check_no_active_shares(&owner, &channel_id, &mut failures).await;

        // ── 2. Owner role enables stop-share control ──
        check_owner_stop_share_gating(&owner, &channel_id, &mut failures).await;

        // ── 3. Member role disables stop-share control ──
        check_member_no_stop_share(&member, &channel_id, &mut failures).await;

        // ── 4. Non-member cannot access voice/share state ──
        check_outsider_no_access(&outsider, &channel_id, &mut failures).await;

        // ── 5. Channel detail has members for share participant resolution ──
        check_members_for_share_resolution(&owner, &channel_id, &mut failures).await;

        // ── 6. Share state deterministic across queries ──
        check_share_state_deterministic(&owner, &channel_id, &mut failures).await;

        ScenarioResult {
            name: self.name().to_owned(),
            passed: failures.is_empty(),
            duration: start.elapsed(),
            failures,
        }
    }
}

/// Validates: Requirement 22.3 (share baseline)
/// No active session means no share state. The GUI clears all isSharing
/// flags on session start before applying ShareState from the server.
async fn check_no_active_shares(
    client: &AuthenticatedClient,
    channel_id: &str,
    failures: &mut Vec<AssertionFailure>,
) {
    let body = match fetch_voice_json(client, channel_id).await {
        Ok(v) => v,
        Err(e) => {
            failures.push(AssertionFailure {
                check: "share baseline request".into(),
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
                check: "share baseline active=false".into(),
                expected: "false".into(),
                actual: format!("{other:?}"),
            });
        }
    }
}

/// Validates: Requirement 22.3 (host stop-share gating)
/// Owner role → host → /stop-share and /stop-all-shares visible.
async fn check_owner_stop_share_gating(
    owner: &AuthenticatedClient,
    channel_id: &str,
    failures: &mut Vec<AssertionFailure>,
) {
    let body = match fetch_channel_json(owner, channel_id).await {
        Ok(v) => v,
        Err(e) => {
            failures.push(AssertionFailure {
                check: "owner stop-share gating".into(),
                expected: "response".into(),
                actual: e,
            });
            return;
        }
    };

    match body.get("role").and_then(|v| v.as_str()) {
        Some("owner") => {}
        other => {
            failures.push(AssertionFailure {
                check: "owner role for stop-share".into(),
                expected: "owner".into(),
                actual: format!("{other:?}"),
            });
        }
    }
}

/// Validates: Requirement 22.3 (guest no stop-share)
/// Member role → guest → no stop-share controls.
async fn check_member_no_stop_share(
    member: &AuthenticatedClient,
    channel_id: &str,
    failures: &mut Vec<AssertionFailure>,
) {
    let body = match fetch_channel_json(member, channel_id).await {
        Ok(v) => v,
        Err(e) => {
            failures.push(AssertionFailure {
                check: "member no stop-share".into(),
                expected: "response".into(),
                actual: e,
            });
            return;
        }
    };

    match body.get("role").and_then(|v| v.as_str()) {
        Some("member") => {}
        other => {
            failures.push(AssertionFailure {
                check: "member role for no stop-share".into(),
                expected: "member".into(),
                actual: format!("{other:?}"),
            });
        }
    }
}

/// Validates: Requirement 22.3 (access control)
/// Non-members cannot access voice/share state.
async fn check_outsider_no_access(
    outsider: &AuthenticatedClient,
    channel_id: &str,
    failures: &mut Vec<AssertionFailure>,
) {
    let resp = match outsider.get(&format!("/channels/{channel_id}/voice")).await {
        Ok(r) => r,
        Err(e) => {
            failures.push(AssertionFailure {
                check: "outsider share access".into(),
                expected: "response".into(),
                actual: format!("error: {e}"),
            });
            return;
        }
    };

    if resp.status() != reqwest::StatusCode::FORBIDDEN {
        failures.push(AssertionFailure {
            check: "outsider share access returns 403".into(),
            expected: "403".into(),
            actual: format!("{}", resp.status()),
        });
    }
}

/// Validates: Requirement 22.3 (participant resolution for share display)
/// Channel detail must include members array so the GUI can resolve
/// participant identities from ShareStarted/ShareState messages.
async fn check_members_for_share_resolution(
    owner: &AuthenticatedClient,
    channel_id: &str,
    failures: &mut Vec<AssertionFailure>,
) {
    let body = match fetch_channel_json(owner, channel_id).await {
        Ok(v) => v,
        Err(e) => {
            failures.push(AssertionFailure {
                check: "members for share resolution".into(),
                expected: "response".into(),
                actual: e,
            });
            return;
        }
    };

    match body.get("members").and_then(|v| v.as_array()) {
        Some(arr) if !arr.is_empty() => {
            // Verify each member has user_id for identity resolution
            for (i, m) in arr.iter().enumerate() {
                if m.get("user_id").and_then(|v| v.as_str()).is_none() {
                    failures.push(AssertionFailure {
                        check: format!("member[{i}] has user_id for share resolution"),
                        expected: "user_id present".into(),
                        actual: format!("{m}"),
                    });
                }
            }
        }
        _ => {
            failures.push(AssertionFailure {
                check: "channel has members array".into(),
                expected: "non-empty members array".into(),
                actual: "absent or empty".into(),
            });
        }
    }
}

/// Validates: Requirement 22.3 (deterministic share state)
/// Two consecutive queries return the same share baseline.
async fn check_share_state_deterministic(
    client: &AuthenticatedClient,
    channel_id: &str,
    failures: &mut Vec<AssertionFailure>,
) {
    let body1 = match fetch_voice_json(client, channel_id).await {
        Ok(v) => v,
        Err(e) => {
            failures.push(AssertionFailure {
                check: "deterministic share query 1".into(),
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
                check: "deterministic share query 2".into(),
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
            check: "deterministic share: active consistent".into(),
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

async fn fetch_channel_json(
    client: &AuthenticatedClient,
    channel_id: &str,
) -> Result<serde_json::Value, String> {
    let resp = client
        .get(&format!("/channels/{channel_id}"))
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
