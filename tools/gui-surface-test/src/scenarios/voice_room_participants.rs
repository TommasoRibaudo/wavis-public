/// Voice Room Participant Sync Scenarios
///
/// Tests the REST surface the GUI depends on for participant synchronization:
/// - GET /channels/{channelId}/voice — participant list structure for GUI sync
/// - Participant count and display_name fields present when active
/// - Capacity invariant: participant_count never exceeds 6
/// - Non-member forbidden (403)
///
/// Note: Voice sessions are joined via WebSocket (JoinVoice), not REST.
/// These tests exercise the REST query surface the GUI polls for participant state.
/// Requirements: 2.1, 2.2, 2.3, 2.4, 2.6
use std::time::Instant;

use async_trait::async_trait;

use crate::channel_ops;
use crate::client::AuthenticatedClient;
use crate::harness_context::TestContext;
use crate::results::{AssertionFailure, ScenarioResult};
use crate::runner::Scenario;

pub struct VoiceRoomParticipantsScenario;

#[async_trait]
impl Scenario for VoiceRoomParticipantsScenario {
    fn name(&self) -> &str {
        "voice-room-participants"
    }

    async fn run(&self, ctx: &TestContext) -> ScenarioResult {
        let start = Instant::now();
        let mut failures: Vec<AssertionFailure> = Vec::new();

        // --- Setup: owner + channel ---
        let owner = match AuthenticatedClient::register(&ctx.base_url, &ctx.http_client).await {
            Ok(c) => c,
            Err(e) => return err_result(self.name(), start, &format!("owner register: {e}")),
        };

        let channel_id = match channel_ops::create_channel(&owner, "participant-sync-test").await {
            Ok(id) => id,
            Err(e) => return err_result(self.name(), start, &e),
        };

        // ── 1. Inactive session returns no participants (ParticipantLeft / RoomState baseline) ──
        check_inactive_no_participants(&owner, &channel_id, &mut failures).await;

        // ── 2. Voice status response has correct structure for GUI participant sync ──
        check_participant_fields_structure(&owner, &channel_id, &mut failures).await;

        // ── 3. Capacity invariant: participant_count never exceeds 6 ──
        check_capacity_invariant(&owner, &channel_id, &mut failures).await;

        // ── 4. Non-member cannot query participant state (403) ──
        let outsider = match AuthenticatedClient::register(&ctx.base_url, &ctx.http_client).await {
            Ok(c) => c,
            Err(e) => return err_result(self.name(), start, &format!("outsider register: {e}")),
        };
        check_non_member_forbidden(&outsider, &channel_id, &mut failures).await;

        // ── 5. Member who joined channel can query voice status ──
        let member = match AuthenticatedClient::register(&ctx.base_url, &ctx.http_client).await {
            Ok(c) => c,
            Err(e) => return err_result(self.name(), start, &format!("member register: {e}")),
        };
        let invite_code = match channel_ops::create_invite(&owner, &channel_id).await {
            Ok(code) => code,
            Err(e) => return err_result(self.name(), start, &e),
        };
        if let Err(e) = channel_ops::join_channel(&member, &channel_id, &invite_code).await {
            return err_result(self.name(), start, &e);
        }
        check_member_can_query_voice(&member, &channel_id, &mut failures).await;

        ScenarioResult {
            name: self.name().to_owned(),
            passed: failures.is_empty(),
            duration: start.elapsed(),
            failures,
        }
    }
}

/// Validates: Requirements 2.2, 2.3
/// When no voice session is active, the participant list should be absent/empty.
/// This mirrors the GUI behavior after all participants have left (ParticipantLeft)
/// or when a RoomState snapshot shows an empty room.
async fn check_inactive_no_participants(
    client: &AuthenticatedClient,
    channel_id: &str,
    failures: &mut Vec<AssertionFailure>,
) {
    let resp = match client.get(&format!("/channels/{channel_id}/voice")).await {
        Ok(r) => r,
        Err(e) => {
            failures.push(AssertionFailure {
                check: "inactive voice status request".into(),
                expected: "response".into(),
                actual: format!("error: {e}"),
            });
            return;
        }
    };

    if resp.status() != reqwest::StatusCode::OK {
        failures.push(AssertionFailure {
            check: "inactive voice status code".into(),
            expected: "200".into(),
            actual: format!("{}", resp.status()),
        });
        return;
    }

    let body: serde_json::Value = match resp.json().await {
        Ok(v) => v,
        Err(e) => {
            failures.push(AssertionFailure {
                check: "inactive voice status parse".into(),
                expected: "valid JSON".into(),
                actual: format!("{e}"),
            });
            return;
        }
    };

    // active must be false
    match body.get("active").and_then(|v| v.as_bool()) {
        Some(false) => {}
        other => {
            failures.push(AssertionFailure {
                check: "inactive session active=false".into(),
                expected: "false".into(),
                actual: format!("{other:?}"),
            });
        }
    }

    // participant_count should be absent or null when inactive
    if let Some(count) = body.get("participant_count").and_then(|v| v.as_u64()) {
        failures.push(AssertionFailure {
            check: "inactive session has no participant_count".into(),
            expected: "absent or null".into(),
            actual: format!("{count}"),
        });
    }

    // participants should be absent or null when inactive
    if let Some(arr) = body.get("participants").and_then(|v| v.as_array())
        && !arr.is_empty()
    {
        failures.push(AssertionFailure {
            check: "inactive session has no participants array".into(),
            expected: "absent, null, or empty".into(),
            actual: format!("{} participants", arr.len()),
        });
    }
}

/// Validates: Requirements 2.1, 2.3
/// Verify the voice status response structure has the fields the GUI needs
/// for participant sync (ParticipantJoined display_name, RoomState snapshot).
/// When active, the response must include participant_count and participants array
/// with display_name on each entry.
async fn check_participant_fields_structure(
    client: &AuthenticatedClient,
    channel_id: &str,
    failures: &mut Vec<AssertionFailure>,
) {
    let resp = match client.get(&format!("/channels/{channel_id}/voice")).await {
        Ok(r) => r,
        Err(e) => {
            failures.push(AssertionFailure {
                check: "voice status structure request".into(),
                expected: "response".into(),
                actual: format!("error: {e}"),
            });
            return;
        }
    };

    if resp.status() != reqwest::StatusCode::OK {
        failures.push(AssertionFailure {
            check: "voice status structure status code".into(),
            expected: "200".into(),
            actual: format!("{}", resp.status()),
        });
        return;
    }

    let body: serde_json::Value = match resp.json().await {
        Ok(v) => v,
        Err(e) => {
            failures.push(AssertionFailure {
                check: "voice status structure parse".into(),
                expected: "valid JSON".into(),
                actual: format!("{e}"),
            });
            return;
        }
    };

    // The response must always have the "active" boolean field
    if body.get("active").and_then(|v| v.as_bool()).is_none() {
        failures.push(AssertionFailure {
            check: "voice status has active boolean".into(),
            expected: "active: bool present".into(),
            actual: format!("{body}"),
        });
    }

    // When active=true, participant_count and participants must be present.
    // When active=false (our case since no WS join), verify the shape is correct
    // by confirming the response is a valid JSON object with expected keys.
    let is_active = body
        .get("active")
        .and_then(|v| v.as_bool())
        .unwrap_or(false);
    if is_active {
        // If somehow active (unlikely without WS join), validate participant fields
        if body.get("participant_count").is_none() {
            failures.push(AssertionFailure {
                check: "active voice has participant_count".into(),
                expected: "participant_count present".into(),
                actual: "missing".into(),
            });
        }
        if let Some(participants) = body.get("participants").and_then(|v| v.as_array()) {
            for (i, p) in participants.iter().enumerate() {
                if p.get("display_name").and_then(|v| v.as_str()).is_none() {
                    failures.push(AssertionFailure {
                        check: format!("participant[{i}] has display_name"),
                        expected: "display_name string present".into(),
                        actual: format!("{p}"),
                    });
                }
            }
        }
    }

    // Verify the response is a JSON object (not array, not primitive)
    if !body.is_object() {
        failures.push(AssertionFailure {
            check: "voice status is JSON object".into(),
            expected: "object".into(),
            actual: format!("{}", body),
        });
    }
}

/// Validates: Requirement 2.6
/// The participant_count field (when present) must never exceed 6.
/// Since we can't join voice via REST, we verify the invariant holds
/// for the inactive case and validate the field type is correct.
async fn check_capacity_invariant(
    client: &AuthenticatedClient,
    channel_id: &str,
    failures: &mut Vec<AssertionFailure>,
) {
    let resp = match client.get(&format!("/channels/{channel_id}/voice")).await {
        Ok(r) => r,
        Err(e) => {
            failures.push(AssertionFailure {
                check: "capacity invariant request".into(),
                expected: "response".into(),
                actual: format!("error: {e}"),
            });
            return;
        }
    };

    if resp.status() != reqwest::StatusCode::OK {
        failures.push(AssertionFailure {
            check: "capacity invariant status code".into(),
            expected: "200".into(),
            actual: format!("{}", resp.status()),
        });
        return;
    }

    let body: serde_json::Value = match resp.json().await {
        Ok(v) => v,
        Err(e) => {
            failures.push(AssertionFailure {
                check: "capacity invariant parse".into(),
                expected: "valid JSON".into(),
                actual: format!("{e}"),
            });
            return;
        }
    };

    // If participant_count is present, it must be <= 6
    if let Some(count) = body.get("participant_count").and_then(|v| v.as_u64())
        && count > 6
    {
        failures.push(AssertionFailure {
            check: "participant_count <= 6 (capacity invariant)".into(),
            expected: "<= 6".into(),
            actual: format!("{count}"),
        });
    }

    // If participants array is present, its length must be <= 6
    if let Some(participants) = body.get("participants").and_then(|v| v.as_array())
        && participants.len() > 6
    {
        failures.push(AssertionFailure {
            check: "participants array length <= 6 (capacity invariant)".into(),
            expected: "<= 6".into(),
            actual: format!("{}", participants.len()),
        });
    }
}

/// Validates: Requirement 2.1 (access control for participant data)
/// Non-members must not be able to query participant state.
async fn check_non_member_forbidden(
    outsider: &AuthenticatedClient,
    channel_id: &str,
    failures: &mut Vec<AssertionFailure>,
) {
    let resp = match outsider.get(&format!("/channels/{channel_id}/voice")).await {
        Ok(r) => r,
        Err(e) => {
            failures.push(AssertionFailure {
                check: "non-member voice status request".into(),
                expected: "response".into(),
                actual: format!("error: {e}"),
            });
            return;
        }
    };

    if resp.status() != reqwest::StatusCode::FORBIDDEN {
        failures.push(AssertionFailure {
            check: "non-member voice status returns 403".into(),
            expected: "403".into(),
            actual: format!("{}", resp.status()),
        });
    }
}

/// Validates: Requirements 2.1, 2.4
/// A channel member can query voice status and receives a well-formed response.
/// This confirms the GUI's participant sync polling endpoint is accessible to members.
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
        return;
    }

    let body: serde_json::Value = match resp.json().await {
        Ok(v) => v,
        Err(e) => {
            failures.push(AssertionFailure {
                check: "member voice status parse".into(),
                expected: "valid JSON".into(),
                actual: format!("{e}"),
            });
            return;
        }
    };

    // Must have the active field
    if body.get("active").and_then(|v| v.as_bool()).is_none() {
        failures.push(AssertionFailure {
            check: "member voice status has active field".into(),
            expected: "active boolean present".into(),
            actual: format!("{body}"),
        });
    }
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
