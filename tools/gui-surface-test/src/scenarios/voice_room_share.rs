/// Voice Room Screen Share Scenarios
///
/// Tests the REST surface the GUI depends on for screen share lifecycle:
/// - Voice status response structure supports share state (active field, participants)
/// - Role-based gating: only owner/admin (host) can stop others' shares or stop-all
/// - Member can query voice status (share state visible via participant data)
/// - Non-member cannot access voice/share state (403)
/// - Share state baseline: inactive session has no stale share data
///
/// Note: StartShare, StopShare, StopAllShares, and ShareState are WebSocket
/// signaling operations. These tests exercise the REST permission and query
/// surface the GUI uses to determine share visibility and host control access.
/// Requirements: 10.3, 10.4, 10.5, 10.6, 10.7
use std::time::Instant;

use async_trait::async_trait;

use crate::channel_ops;
use crate::client::AuthenticatedClient;
use crate::harness_context::TestContext;
use crate::results::{AssertionFailure, ScenarioResult};
use crate::runner::Scenario;

pub struct VoiceRoomShareScenario;

#[async_trait]
impl Scenario for VoiceRoomShareScenario {
    fn name(&self) -> &str {
        "voice-room-share"
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

        let channel_id = match channel_ops::create_channel(&owner, "share-test").await {
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

        // ── 1. Inactive session has clean share baseline (no stale share data) ──
        check_inactive_share_baseline(&owner, &channel_id, &mut failures).await;

        // ── 2. Voice status structure supports share state visibility ──
        check_voice_status_share_structure(&owner, &channel_id, &mut failures).await;

        // ── 3. Owner role maps to host (stop-share/stop-all-shares gating) ──
        check_owner_is_host_for_share(&owner, &channel_id, &mut failures).await;

        // ── 4. Member role maps to guest (no stop-share/stop-all-shares) ──
        check_member_is_guest_for_share(&member, &channel_id, &mut failures).await;

        // ── 5. Non-member cannot access share state (403) ──
        check_outsider_no_share_access(&outsider, &channel_id, &mut failures).await;

        // ── 6. Re-query after baseline: share state deterministic (no drift) ──
        check_share_state_deterministic(&owner, &channel_id, &mut failures).await;

        ScenarioResult {
            name: self.name().to_owned(),
            passed: failures.is_empty(),
            duration: start.elapsed(),
            failures,
        }
    }
}

/// Validates: Requirements 10.5, 10.7
/// When no voice session is active, the voice status must not contain stale
/// share data. This mirrors the GUI's deterministic reset on reconnect —
/// all isSharing flags must be cleared before ShareState is re-applied.
/// An inactive session is the baseline: no participants, no share state.
async fn check_inactive_share_baseline(
    client: &AuthenticatedClient,
    channel_id: &str,
    failures: &mut Vec<AssertionFailure>,
) {
    let resp = match client.get(&format!("/channels/{channel_id}/voice")).await {
        Ok(r) => r,
        Err(e) => {
            failures.push(AssertionFailure {
                check: "inactive share baseline request".into(),
                expected: "response".into(),
                actual: format!("error: {e}"),
            });
            return;
        }
    };

    if resp.status() != reqwest::StatusCode::OK {
        failures.push(AssertionFailure {
            check: "inactive share baseline status".into(),
            expected: "200".into(),
            actual: format!("{}", resp.status()),
        });
        return;
    }

    let body: serde_json::Value = match resp.json().await {
        Ok(v) => v,
        Err(e) => {
            failures.push(AssertionFailure {
                check: "inactive share baseline parse".into(),
                expected: "valid JSON".into(),
                actual: format!("{e}"),
            });
            return;
        }
    };

    // active must be false — no session means no share state
    match body.get("active").and_then(|v| v.as_bool()) {
        Some(false) => {}
        other => {
            failures.push(AssertionFailure {
                check: "inactive share baseline active=false".into(),
                expected: "false".into(),
                actual: format!("{other:?}"),
            });
        }
    }

    // No participants means no share flags can exist
    if let Some(arr) = body.get("participants").and_then(|v| v.as_array())
        && !arr.is_empty()
    {
        failures.push(AssertionFailure {
            check: "inactive share baseline no participants".into(),
            expected: "absent or empty".into(),
            actual: format!("{} participants", arr.len()),
        });
    }
}

/// Validates: Requirements 10.3, 10.6
/// The voice status response structure must be well-formed for the GUI to
/// render share indicators. When active, each participant entry should be
/// a valid object the GUI can extend with isSharing from ShareState/ShareStarted.
/// The response must be a JSON object with the expected top-level fields.
async fn check_voice_status_share_structure(
    client: &AuthenticatedClient,
    channel_id: &str,
    failures: &mut Vec<AssertionFailure>,
) {
    let resp = match client.get(&format!("/channels/{channel_id}/voice")).await {
        Ok(r) => r,
        Err(e) => {
            failures.push(AssertionFailure {
                check: "share structure request".into(),
                expected: "response".into(),
                actual: format!("error: {e}"),
            });
            return;
        }
    };

    if resp.status() != reqwest::StatusCode::OK {
        failures.push(AssertionFailure {
            check: "share structure status".into(),
            expected: "200".into(),
            actual: format!("{}", resp.status()),
        });
        return;
    }

    let body: serde_json::Value = match resp.json().await {
        Ok(v) => v,
        Err(e) => {
            failures.push(AssertionFailure {
                check: "share structure parse".into(),
                expected: "valid JSON".into(),
                actual: format!("{e}"),
            });
            return;
        }
    };

    // Must be a JSON object
    if !body.is_object() {
        failures.push(AssertionFailure {
            check: "share structure is JSON object".into(),
            expected: "object".into(),
            actual: format!("{body}"),
        });
        return;
    }

    // Must have the "active" boolean — GUI uses this to decide whether to
    // render share indicators at all
    if body.get("active").and_then(|v| v.as_bool()).is_none() {
        failures.push(AssertionFailure {
            check: "share structure has active boolean".into(),
            expected: "active: bool present".into(),
            actual: format!("{body}"),
        });
    }

    // When active, participants array entries must have display_name
    // (the GUI maps participantId from ShareStarted/ShareState to these entries)
    let is_active = body
        .get("active")
        .and_then(|v| v.as_bool())
        .unwrap_or(false);
    if is_active && let Some(participants) = body.get("participants").and_then(|v| v.as_array()) {
        for (i, p) in participants.iter().enumerate() {
            if p.get("display_name").and_then(|v| v.as_str()).is_none() {
                failures.push(AssertionFailure {
                    check: format!("share participant[{i}] has display_name"),
                    expected: "display_name string present".into(),
                    actual: format!("{p}"),
                });
            }
        }
    }
}

/// Validates: Requirements 10.4, 10.5 (host gating for stop-share/stop-all-shares)
/// The GUI renders /stop-share and /stop-all-shares buttons only when the
/// self participant is host. Owner role maps to host in the GUI.
async fn check_owner_is_host_for_share(
    owner: &AuthenticatedClient,
    channel_id: &str,
    failures: &mut Vec<AssertionFailure>,
) {
    let resp = match owner.get(&format!("/channels/{channel_id}")).await {
        Ok(r) => r,
        Err(e) => {
            failures.push(AssertionFailure {
                check: "owner share host check request".into(),
                expected: "response".into(),
                actual: format!("error: {e}"),
            });
            return;
        }
    };

    if resp.status() != reqwest::StatusCode::OK {
        failures.push(AssertionFailure {
            check: "owner share host check status".into(),
            expected: "200".into(),
            actual: format!("{}", resp.status()),
        });
        return;
    }

    let body: serde_json::Value = match resp.json().await {
        Ok(v) => v,
        Err(e) => {
            failures.push(AssertionFailure {
                check: "owner share host check parse".into(),
                expected: "valid JSON".into(),
                actual: format!("{e}"),
            });
            return;
        }
    };

    // Owner role → host → /stop-share and /stop-all-shares visible
    match body.get("role").and_then(|v| v.as_str()) {
        Some("owner") => {}
        other => {
            failures.push(AssertionFailure {
                check: "owner role is 'owner' (host for share controls)".into(),
                expected: "owner".into(),
                actual: format!("{other:?}"),
            });
        }
    }
}

/// Validates: Requirements 10.4, 10.5 (guest cannot stop others' shares)
/// A regular member's role is "member" → guest in the GUI.
/// /stop-share and /stop-all-shares must NOT be rendered for guests.
async fn check_member_is_guest_for_share(
    member: &AuthenticatedClient,
    channel_id: &str,
    failures: &mut Vec<AssertionFailure>,
) {
    let resp = match member.get(&format!("/channels/{channel_id}")).await {
        Ok(r) => r,
        Err(e) => {
            failures.push(AssertionFailure {
                check: "member share guest check request".into(),
                expected: "response".into(),
                actual: format!("error: {e}"),
            });
            return;
        }
    };

    if resp.status() != reqwest::StatusCode::OK {
        failures.push(AssertionFailure {
            check: "member share guest check status".into(),
            expected: "200".into(),
            actual: format!("{}", resp.status()),
        });
        return;
    }

    let body: serde_json::Value = match resp.json().await {
        Ok(v) => v,
        Err(e) => {
            failures.push(AssertionFailure {
                check: "member share guest check parse".into(),
                expected: "valid JSON".into(),
                actual: format!("{e}"),
            });
            return;
        }
    };

    // Member role → guest → no stop-share/stop-all-shares controls
    match body.get("role").and_then(|v| v.as_str()) {
        Some("member") => {}
        other => {
            failures.push(AssertionFailure {
                check: "member role is 'member' (guest, no share controls)".into(),
                expected: "member".into(),
                actual: format!("{other:?}"),
            });
        }
    }
}

/// Validates: Requirements 10.3, 10.6 (access control for share state)
/// Non-members must not be able to query voice/share state.
/// This ensures ShareState and ShareStarted data is not leaked to outsiders.
async fn check_outsider_no_share_access(
    outsider: &AuthenticatedClient,
    channel_id: &str,
    failures: &mut Vec<AssertionFailure>,
) {
    let resp = match outsider.get(&format!("/channels/{channel_id}/voice")).await {
        Ok(r) => r,
        Err(e) => {
            failures.push(AssertionFailure {
                check: "outsider share access request".into(),
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

/// Validates: Requirement 10.7 (deterministic share state on reconnect)
/// Two consecutive queries to voice status must return the same share baseline.
/// This mirrors the GUI's reconnect behavior: after reconnect, all isSharing
/// flags are cleared before ShareState is re-applied. The REST endpoint must
/// return consistent state across queries (no phantom share data).
async fn check_share_state_deterministic(
    client: &AuthenticatedClient,
    channel_id: &str,
    failures: &mut Vec<AssertionFailure>,
) {
    // Query twice and compare active + participant state
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
            check: "deterministic share: active field consistent".into(),
            expected: format!("{active1:?}"),
            actual: format!("{active2:?}"),
        });
    }

    let count1 = body1.get("participant_count").and_then(|v| v.as_u64());
    let count2 = body2.get("participant_count").and_then(|v| v.as_u64());

    if count1 != count2 {
        failures.push(AssertionFailure {
            check: "deterministic share: participant_count consistent".into(),
            expected: format!("{count1:?}"),
            actual: format!("{count2:?}"),
        });
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
