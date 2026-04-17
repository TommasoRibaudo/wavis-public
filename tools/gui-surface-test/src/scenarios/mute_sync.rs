/// Mute Sync Surface Test Scenario
///
/// Tests the REST surface the GUI depends on for mute synchronization:
/// - Owner (host) role enables mute controls in the GUI
/// - Member (guest) role disables mute controls
/// - Host-mute is a WS signaling operation (MuteParticipant), but the GUI
///   gates the /mute button on the caller's role from GET /channels/{id}
/// - After host-mute, the participant's muted state is enforced server-side;
///   the GUI blocks unmute attempts locally when isHostMuted is true
///
/// Note: The actual mute toggle (setMicEnabled) is client-side LiveKit SDK.
/// Host-mute (MuteParticipant) is WS signaling. These tests exercise the
/// REST role surface the GUI uses to determine mute control visibility.
/// Requirements: 22.2
use std::time::Instant;

use async_trait::async_trait;

use crate::channel_ops;
use crate::client::AuthenticatedClient;
use crate::harness_context::TestContext;
use crate::results::{AssertionFailure, ScenarioResult};
use crate::runner::Scenario;

pub struct MuteSyncScenario;

#[async_trait]
impl Scenario for MuteSyncScenario {
    fn name(&self) -> &str {
        "mute-sync"
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

        let channel_id = match channel_ops::create_channel(&owner, "mute-sync-test").await {
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

        // ── 1. Owner role maps to host (mute control visible) ──
        check_owner_has_mute_control(&owner, &channel_id, &mut failures).await;

        // ── 2. Member role maps to guest (no mute control) ──
        check_member_no_mute_control(&member, &channel_id, &mut failures).await;

        // ── 3. Voice status accessible for mute state rendering ──
        check_voice_status_for_mute(&owner, &channel_id, &mut failures).await;

        // ── 4. Member voice status accessible (sees own mute state) ──
        check_member_voice_status(&member, &channel_id, &mut failures).await;

        ScenarioResult {
            name: self.name().to_owned(),
            passed: failures.is_empty(),
            duration: start.elapsed(),
            failures,
        }
    }
}

/// Validates: Requirement 22.2 (host mute control gating)
/// Owner role → host → /mute button visible in the GUI participant list.
async fn check_owner_has_mute_control(
    owner: &AuthenticatedClient,
    channel_id: &str,
    failures: &mut Vec<AssertionFailure>,
) {
    let body = match fetch_channel_json(owner, channel_id).await {
        Ok(v) => v,
        Err(e) => {
            failures.push(AssertionFailure {
                check: "owner mute control check".into(),
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
                check: "owner role is 'owner' (host for mute controls)".into(),
                expected: "owner".into(),
                actual: format!("{other:?}"),
            });
        }
    }
}

/// Validates: Requirement 22.2 (guest cannot host-mute)
/// Member role → guest → /mute button NOT visible.
async fn check_member_no_mute_control(
    member: &AuthenticatedClient,
    channel_id: &str,
    failures: &mut Vec<AssertionFailure>,
) {
    let body = match fetch_channel_json(member, channel_id).await {
        Ok(v) => v,
        Err(e) => {
            failures.push(AssertionFailure {
                check: "member mute control check".into(),
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
                check: "member role is 'member' (guest, no mute control)".into(),
                expected: "member".into(),
                actual: format!("{other:?}"),
            });
        }
    }
}

/// Validates: Requirement 22.2 (voice status for mute state)
/// Voice status endpoint must be accessible for the GUI to render
/// participant mute indicators alongside media state.
async fn check_voice_status_for_mute(
    client: &AuthenticatedClient,
    channel_id: &str,
    failures: &mut Vec<AssertionFailure>,
) {
    let resp = match client.get(&format!("/channels/{channel_id}/voice")).await {
        Ok(r) => r,
        Err(e) => {
            failures.push(AssertionFailure {
                check: "voice status for mute request".into(),
                expected: "response".into(),
                actual: format!("error: {e}"),
            });
            return;
        }
    };

    if resp.status() != reqwest::StatusCode::OK {
        failures.push(AssertionFailure {
            check: "voice status for mute returns 200".into(),
            expected: "200".into(),
            actual: format!("{}", resp.status()),
        });
    }
}

/// Validates: Requirement 22.2 (member sees own mute state)
/// Member can query voice status to see their own mute state after host-mute.
async fn check_member_voice_status(
    member: &AuthenticatedClient,
    channel_id: &str,
    failures: &mut Vec<AssertionFailure>,
) {
    let resp = match member.get(&format!("/channels/{channel_id}/voice")).await {
        Ok(r) => r,
        Err(e) => {
            failures.push(AssertionFailure {
                check: "member voice status for mute request".into(),
                expected: "response".into(),
                actual: format!("error: {e}"),
            });
            return;
        }
    };

    if resp.status() != reqwest::StatusCode::OK {
        failures.push(AssertionFailure {
            check: "member voice status for mute returns 200".into(),
            expected: "200".into(),
            actual: format!("{}", resp.status()),
        });
    }
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
