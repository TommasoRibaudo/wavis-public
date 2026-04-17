/// Volume Control Surface Test Scenario
///
/// Tests the REST surface the GUI depends on for volume control rendering:
/// - Voice status endpoint accessible for volume UI rendering
/// - Channel detail includes participant list for per-participant volume sliders
/// - Member can access voice status (sees participants for volume control)
/// - Non-member cannot access voice/participant data
/// - Participant list structure supports volume control rendering
///
/// Note: Actual volume control (setParticipantVolume, setMasterVolume) is
/// client-side LiveKit SDK via GainNode. No REST endpoint for volume changes.
/// These tests exercise the REST surface the GUI uses to render the volume
/// control UI (participant list, voice status).
/// Requirements: 22.4
use std::time::Instant;

use async_trait::async_trait;

use crate::channel_ops;
use crate::client::AuthenticatedClient;
use crate::harness_context::TestContext;
use crate::results::{AssertionFailure, ScenarioResult};
use crate::runner::Scenario;

pub struct VolumeControlScenario;

#[async_trait]
impl Scenario for VolumeControlScenario {
    fn name(&self) -> &str {
        "volume-control"
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

        let channel_id = match channel_ops::create_channel(&owner, "volume-control-test").await {
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

        // ── 1. Voice status accessible for volume UI ──
        check_voice_status_accessible(&owner, &channel_id, &mut failures).await;

        // ── 2. Channel detail has participant list for volume sliders ──
        check_participant_list_structure(&owner, &channel_id, &mut failures).await;

        // ── 3. Member can access voice status ──
        check_member_voice_access(&member, &channel_id, &mut failures).await;

        // ── 4. Non-member cannot access participant data ──
        check_outsider_no_access(&outsider, &channel_id, &mut failures).await;

        // ── 5. Channel detail members have identity fields for volume mapping ──
        check_member_identity_fields(&owner, &channel_id, &mut failures).await;

        ScenarioResult {
            name: self.name().to_owned(),
            passed: failures.is_empty(),
            duration: start.elapsed(),
            failures,
        }
    }
}

/// Validates: Requirement 22.4 (voice status for volume UI)
async fn check_voice_status_accessible(
    client: &AuthenticatedClient,
    channel_id: &str,
    failures: &mut Vec<AssertionFailure>,
) {
    let resp = match client.get(&format!("/channels/{channel_id}/voice")).await {
        Ok(r) => r,
        Err(e) => {
            failures.push(AssertionFailure {
                check: "voice status for volume UI".into(),
                expected: "response".into(),
                actual: format!("error: {e}"),
            });
            return;
        }
    };

    if resp.status() != reqwest::StatusCode::OK {
        failures.push(AssertionFailure {
            check: "voice status returns 200".into(),
            expected: "200".into(),
            actual: format!("{}", resp.status()),
        });
    }
}

/// Validates: Requirement 22.4 (participant list for volume sliders)
/// Channel detail must include members array. The GUI renders a volume
/// slider per participant using identity from this list.
async fn check_participant_list_structure(
    client: &AuthenticatedClient,
    channel_id: &str,
    failures: &mut Vec<AssertionFailure>,
) {
    let body = match fetch_channel_json(client, channel_id).await {
        Ok(v) => v,
        Err(e) => {
            failures.push(AssertionFailure {
                check: "participant list for volume".into(),
                expected: "response".into(),
                actual: e,
            });
            return;
        }
    };

    match body.get("members").and_then(|v| v.as_array()) {
        Some(arr) if arr.len() >= 2 => {} // owner + member
        Some(arr) => {
            failures.push(AssertionFailure {
                check: "channel has ≥2 members for volume test".into(),
                expected: "≥2 members".into(),
                actual: format!("{} members", arr.len()),
            });
        }
        None => {
            failures.push(AssertionFailure {
                check: "channel has members array".into(),
                expected: "members array present".into(),
                actual: "absent".into(),
            });
        }
    }
}

/// Validates: Requirement 22.4 (member voice access)
async fn check_member_voice_access(
    member: &AuthenticatedClient,
    channel_id: &str,
    failures: &mut Vec<AssertionFailure>,
) {
    let resp = match member.get(&format!("/channels/{channel_id}/voice")).await {
        Ok(r) => r,
        Err(e) => {
            failures.push(AssertionFailure {
                check: "member voice access for volume".into(),
                expected: "response".into(),
                actual: format!("error: {e}"),
            });
            return;
        }
    };

    if resp.status() != reqwest::StatusCode::OK {
        failures.push(AssertionFailure {
            check: "member voice access returns 200".into(),
            expected: "200".into(),
            actual: format!("{}", resp.status()),
        });
    }
}

/// Validates: Requirement 22.4 (access control)
async fn check_outsider_no_access(
    outsider: &AuthenticatedClient,
    channel_id: &str,
    failures: &mut Vec<AssertionFailure>,
) {
    let resp = match outsider.get(&format!("/channels/{channel_id}/voice")).await {
        Ok(r) => r,
        Err(e) => {
            failures.push(AssertionFailure {
                check: "outsider volume access".into(),
                expected: "response".into(),
                actual: format!("error: {e}"),
            });
            return;
        }
    };

    if resp.status() != reqwest::StatusCode::FORBIDDEN {
        failures.push(AssertionFailure {
            check: "outsider volume access returns 403".into(),
            expected: "403".into(),
            actual: format!("{}", resp.status()),
        });
    }
}

/// Validates: Requirement 22.4 (identity fields for volume mapping)
/// Each member must have user_id so the GUI can map participant.identity
/// to the correct volume slider / GainNode.
async fn check_member_identity_fields(
    client: &AuthenticatedClient,
    channel_id: &str,
    failures: &mut Vec<AssertionFailure>,
) {
    let body = match fetch_channel_json(client, channel_id).await {
        Ok(v) => v,
        Err(e) => {
            failures.push(AssertionFailure {
                check: "member identity fields".into(),
                expected: "response".into(),
                actual: e,
            });
            return;
        }
    };

    if let Some(members) = body.get("members").and_then(|v| v.as_array()) {
        for (i, m) in members.iter().enumerate() {
            if m.get("user_id").and_then(|v| v.as_str()).is_none() {
                failures.push(AssertionFailure {
                    check: format!("member[{i}] has user_id for volume mapping"),
                    expected: "user_id present".into(),
                    actual: format!("{m}"),
                });
            }
        }
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
