/// Channel Lifecycle Scenarios
///
/// Tests the REST endpoints the GUI channels screens depend on:
/// - POST /channels              — create channel
/// - GET  /channels              — list channels
/// - POST /channels/join         — join by invite code
/// - POST /channels/{id}/leave   — leave channel
/// - DELETE /channels/{id}       — delete channel (owner only)
/// - GET /channels/{id}          — 403 for non-member after leave
/// - Error cases: invalid invite, already member, non-owner delete
use std::time::Instant;

use async_trait::async_trait;

use crate::channel_ops;
use crate::client::AuthenticatedClient;
use crate::harness_context::TestContext;
use crate::results::{AssertionFailure, ScenarioResult};
use crate::runner::Scenario;

pub struct ChannelLifecycleScenario;

#[async_trait]
impl Scenario for ChannelLifecycleScenario {
    fn name(&self) -> &str {
        "channel-lifecycle"
    }

    async fn run(&self, ctx: &TestContext) -> ScenarioResult {
        let start = Instant::now();
        let mut failures: Vec<AssertionFailure> = Vec::new();

        // --- Setup: register owner + joiner ---
        let owner = match AuthenticatedClient::register(&ctx.base_url, &ctx.http_client).await {
            Ok(c) => c,
            Err(e) => return err_result(self.name(), start, &format!("owner register: {e}")),
        };
        let joiner = match AuthenticatedClient::register(&ctx.base_url, &ctx.http_client).await {
            Ok(c) => c,
            Err(e) => return err_result(self.name(), start, &format!("joiner register: {e}")),
        };

        // ── Create channel ──
        let channel_id = check_create_channel(&owner, &mut failures).await;
        let channel_id = match channel_id {
            Some(id) => id,
            None => return err_result(self.name(), start, "create channel failed"),
        };

        // ── List channels — owner sees it ──
        check_list_channels(&owner, &channel_id, true, &mut failures).await;

        // ── List channels — joiner does NOT see it yet ──
        check_list_channels(&joiner, &channel_id, false, &mut failures).await;

        // ── Create invite + join ──
        let invite_code = match channel_ops::create_invite(&owner, &channel_id).await {
            Ok(c) => c,
            Err(e) => return err_result(self.name(), start, &e),
        };
        check_join_by_invite(&joiner, &invite_code, &mut failures).await;

        // ── Joiner now sees channel in list ──
        check_list_channels(&joiner, &channel_id, true, &mut failures).await;

        // ── Join with invalid invite code ──
        check_join_invalid_invite(&joiner, &mut failures).await;

        // ── Join again (already member) ──
        check_join_already_member(&joiner, &invite_code, &mut failures).await;

        // ── Non-owner delete — 403 ──
        check_delete_forbidden(&joiner, &channel_id, &mut failures).await;

        // ── Joiner leaves ──
        check_leave(&joiner, &channel_id, &mut failures).await;

        // ── After leave, joiner gets 403 on channel detail ──
        check_detail_forbidden_after_leave(&joiner, &channel_id, &mut failures).await;

        // ── Owner deletes channel ──
        check_delete_success(&owner, &channel_id, &mut failures).await;

        // ── After delete, owner's list no longer contains it ──
        check_list_channels(&owner, &channel_id, false, &mut failures).await;

        ScenarioResult {
            name: self.name().to_owned(),
            passed: failures.is_empty(),
            duration: start.elapsed(),
            failures,
        }
    }
}

// ─── Check helpers ─────────────────────────────────────────────────

async fn check_create_channel(
    owner: &AuthenticatedClient,
    failures: &mut Vec<AssertionFailure>,
) -> Option<String> {
    let resp = match owner
        .post(
            "/channels",
            &serde_json::json!({ "name": "lifecycle-test" }),
        )
        .await
    {
        Ok(r) => r,
        Err(e) => {
            failures.push(AssertionFailure {
                check: "POST /channels create".into(),
                expected: "response".into(),
                actual: format!("error: {e}"),
            });
            return None;
        }
    };

    if resp.status() != reqwest::StatusCode::CREATED {
        failures.push(AssertionFailure {
            check: "POST /channels status".into(),
            expected: "201".into(),
            actual: format!("{}", resp.status()),
        });
        return None;
    }

    let body: serde_json::Value = match resp.json().await {
        Ok(v) => v,
        Err(e) => {
            failures.push(AssertionFailure {
                check: "create channel parse".into(),
                expected: "valid JSON".into(),
                actual: format!("{e}"),
            });
            return None;
        }
    };

    // Verify response shape matches GUI expectations
    for field in ["channel_id", "name", "owner_user_id", "created_at"] {
        if body.get(field).is_none() {
            failures.push(AssertionFailure {
                check: format!("create channel has {field}"),
                expected: format!("{field} present"),
                actual: "missing".into(),
            });
        }
    }

    body.get("channel_id")
        .and_then(|v| v.as_str())
        .map(|s| s.to_owned())
}

async fn check_list_channels(
    client: &AuthenticatedClient,
    channel_id: &str,
    should_contain: bool,
    failures: &mut Vec<AssertionFailure>,
) {
    let resp = match client.get("/channels").await {
        Ok(r) => r,
        Err(e) => {
            failures.push(AssertionFailure {
                check: "GET /channels list".into(),
                expected: "response".into(),
                actual: format!("error: {e}"),
            });
            return;
        }
    };

    if resp.status() != reqwest::StatusCode::OK {
        failures.push(AssertionFailure {
            check: "GET /channels status".into(),
            expected: "200".into(),
            actual: format!("{}", resp.status()),
        });
        return;
    }

    let body: serde_json::Value = match resp.json().await {
        Ok(v) => v,
        Err(e) => {
            failures.push(AssertionFailure {
                check: "list channels parse".into(),
                expected: "valid JSON".into(),
                actual: format!("{e}"),
            });
            return;
        }
    };

    let arr = body.as_array();
    let contains = arr
        .map(|a| {
            a.iter()
                .any(|item| item.get("channel_id").and_then(|v| v.as_str()) == Some(channel_id))
        })
        .unwrap_or(false);

    if should_contain && !contains {
        failures.push(AssertionFailure {
            check: "list channels contains expected channel".into(),
            expected: format!("channel {channel_id} in list"),
            actual: "not found".into(),
        });
    } else if !should_contain && contains {
        failures.push(AssertionFailure {
            check: "list channels does NOT contain channel".into(),
            expected: format!("channel {channel_id} absent"),
            actual: "still present".into(),
        });
    }

    // Verify list item shape (if non-empty)
    if let Some(arr) = arr {
        for item in arr {
            for field in ["channel_id", "name", "role", "owner_user_id", "created_at"] {
                if item.get(field).is_none() {
                    failures.push(AssertionFailure {
                        check: format!("list item has {field}"),
                        expected: format!("{field} present"),
                        actual: format!("missing in {item}"),
                    });
                    break; // one failure per item is enough
                }
            }
        }
    }
}

async fn check_join_by_invite(
    joiner: &AuthenticatedClient,
    invite_code: &str,
    failures: &mut Vec<AssertionFailure>,
) {
    let resp = match joiner
        .post(
            "/channels/join",
            &serde_json::json!({ "code": invite_code }),
        )
        .await
    {
        Ok(r) => r,
        Err(e) => {
            failures.push(AssertionFailure {
                check: "POST /channels/join".into(),
                expected: "response".into(),
                actual: format!("error: {e}"),
            });
            return;
        }
    };

    if !resp.status().is_success() {
        failures.push(AssertionFailure {
            check: "POST /channels/join success".into(),
            expected: "2xx".into(),
            actual: format!("{}", resp.status()),
        });
        return;
    }

    let body: serde_json::Value = match resp.json().await {
        Ok(v) => v,
        Err(e) => {
            failures.push(AssertionFailure {
                check: "join response parse".into(),
                expected: "valid JSON".into(),
                actual: format!("{e}"),
            });
            return;
        }
    };

    // GUI expects channel_id, name, role
    for field in ["channel_id", "name", "role"] {
        if body.get(field).is_none() {
            failures.push(AssertionFailure {
                check: format!("join response has {field}"),
                expected: format!("{field} present"),
                actual: "missing".into(),
            });
        }
    }
}

async fn check_join_invalid_invite(
    client: &AuthenticatedClient,
    failures: &mut Vec<AssertionFailure>,
) {
    let resp = match client
        .post(
            "/channels/join",
            &serde_json::json!({ "code": "INVALID-CODE-XYZ" }),
        )
        .await
    {
        Ok(r) => r,
        Err(e) => {
            failures.push(AssertionFailure {
                check: "join invalid invite request".into(),
                expected: "response".into(),
                actual: format!("error: {e}"),
            });
            return;
        }
    };

    if resp.status() != reqwest::StatusCode::BAD_REQUEST {
        failures.push(AssertionFailure {
            check: "join invalid invite returns 400".into(),
            expected: "400".into(),
            actual: format!("{}", resp.status()),
        });
    }
}

async fn check_join_already_member(
    client: &AuthenticatedClient,
    invite_code: &str,
    failures: &mut Vec<AssertionFailure>,
) {
    let resp = match client
        .post(
            "/channels/join",
            &serde_json::json!({ "code": invite_code }),
        )
        .await
    {
        Ok(r) => r,
        Err(e) => {
            failures.push(AssertionFailure {
                check: "join already member request".into(),
                expected: "response".into(),
                actual: format!("error: {e}"),
            });
            return;
        }
    };

    if resp.status() != reqwest::StatusCode::CONFLICT {
        failures.push(AssertionFailure {
            check: "join already member returns 409".into(),
            expected: "409".into(),
            actual: format!("{}", resp.status()),
        });
    }
}

async fn check_delete_forbidden(
    non_owner: &AuthenticatedClient,
    channel_id: &str,
    failures: &mut Vec<AssertionFailure>,
) {
    let resp = match non_owner.delete(&format!("/channels/{channel_id}")).await {
        Ok(r) => r,
        Err(e) => {
            failures.push(AssertionFailure {
                check: "DELETE /channels/{id} non-owner".into(),
                expected: "response".into(),
                actual: format!("error: {e}"),
            });
            return;
        }
    };

    if resp.status() != reqwest::StatusCode::FORBIDDEN {
        failures.push(AssertionFailure {
            check: "DELETE /channels/{id} non-owner returns 403".into(),
            expected: "403".into(),
            actual: format!("{}", resp.status()),
        });
    }
}

async fn check_leave(
    client: &AuthenticatedClient,
    channel_id: &str,
    failures: &mut Vec<AssertionFailure>,
) {
    let resp = match client
        .post_empty(&format!("/channels/{channel_id}/leave"))
        .await
    {
        Ok(r) => r,
        Err(e) => {
            failures.push(AssertionFailure {
                check: "POST /channels/{id}/leave".into(),
                expected: "response".into(),
                actual: format!("error: {e}"),
            });
            return;
        }
    };

    if !resp.status().is_success() {
        failures.push(AssertionFailure {
            check: "POST /channels/{id}/leave success".into(),
            expected: "2xx".into(),
            actual: format!("{}", resp.status()),
        });
    }
}

async fn check_detail_forbidden_after_leave(
    client: &AuthenticatedClient,
    channel_id: &str,
    failures: &mut Vec<AssertionFailure>,
) {
    let resp = match client.get(&format!("/channels/{channel_id}")).await {
        Ok(r) => r,
        Err(e) => {
            failures.push(AssertionFailure {
                check: "GET /channels/{id} after leave".into(),
                expected: "response".into(),
                actual: format!("error: {e}"),
            });
            return;
        }
    };

    if resp.status() != reqwest::StatusCode::FORBIDDEN {
        failures.push(AssertionFailure {
            check: "GET /channels/{id} after leave returns 403".into(),
            expected: "403".into(),
            actual: format!("{}", resp.status()),
        });
    }
}

async fn check_delete_success(
    owner: &AuthenticatedClient,
    channel_id: &str,
    failures: &mut Vec<AssertionFailure>,
) {
    let resp = match owner.delete(&format!("/channels/{channel_id}")).await {
        Ok(r) => r,
        Err(e) => {
            failures.push(AssertionFailure {
                check: "DELETE /channels/{id} owner".into(),
                expected: "response".into(),
                actual: format!("error: {e}"),
            });
            return;
        }
    };

    if !resp.status().is_success() {
        failures.push(AssertionFailure {
            check: "DELETE /channels/{id} owner success".into(),
            expected: "2xx".into(),
            actual: format!("{}", resp.status()),
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
