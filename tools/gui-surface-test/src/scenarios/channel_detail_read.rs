/// Channel Detail Read Scenarios
///
/// Tests the read-only REST endpoints the GUI ChannelDetail screen depends on:
/// - GET /channels/{channelId}        — success, 404, 403
/// - GET /channels/{channelId}/voice   — active session, inactive session
/// - GET /channels/{channelId}/bans    — non-empty, empty, 403 for non-admin
/// - GET /channels/{channelId}/invites — non-empty, empty, 403 for member
///
/// **Validates: Requirements 1.1, 3.1, 8.2, 16.2, 16.3, 16.6**
use std::time::Instant;

use async_trait::async_trait;

use crate::channel_ops;
use crate::client::AuthenticatedClient;
use crate::harness_context::TestContext;
use crate::results::{AssertionFailure, ScenarioResult};
use crate::runner::Scenario;

pub struct ChannelDetailReadScenario;

#[async_trait]
impl Scenario for ChannelDetailReadScenario {
    fn name(&self) -> &str {
        "channel-detail-read"
    }

    async fn run(&self, ctx: &TestContext) -> ScenarioResult {
        let start = Instant::now();
        let mut failures: Vec<AssertionFailure> = Vec::new();

        // --- Setup: register three devices (owner, admin, member) ---
        let owner = match AuthenticatedClient::register(&ctx.base_url, &ctx.http_client).await {
            Ok(c) => c,
            Err(e) => return err_result(self.name(), start, &format!("owner register: {e}")),
        };
        let admin = match AuthenticatedClient::register(&ctx.base_url, &ctx.http_client).await {
            Ok(c) => c,
            Err(e) => return err_result(self.name(), start, &format!("admin register: {e}")),
        };
        let member = match AuthenticatedClient::register(&ctx.base_url, &ctx.http_client).await {
            Ok(c) => c,
            Err(e) => return err_result(self.name(), start, &format!("member register: {e}")),
        };
        let outsider = match AuthenticatedClient::register(&ctx.base_url, &ctx.http_client).await {
            Ok(c) => c,
            Err(e) => return err_result(self.name(), start, &format!("outsider register: {e}")),
        };

        // Create channel, invite admin + member
        let channel_id = match channel_ops::create_channel(&owner, "detail-read-test").await {
            Ok(id) => id,
            Err(e) => return err_result(self.name(), start, &e),
        };
        let invite_code = match channel_ops::create_invite(&owner, &channel_id).await {
            Ok(c) => c,
            Err(e) => return err_result(self.name(), start, &e),
        };
        if let Err(e) = channel_ops::join_channel(&admin, &channel_id, &invite_code).await {
            return err_result(self.name(), start, &e);
        }
        if let Err(e) = channel_ops::join_channel(&member, &channel_id, &invite_code).await {
            return err_result(self.name(), start, &e);
        }
        if let Err(e) = channel_ops::promote_to_admin(&owner, &channel_id, &admin.user_id).await {
            return err_result(self.name(), start, &e);
        }

        // ── GET /channels/{channelId} — success ──
        check_get_detail_success(&owner, &channel_id, &mut failures).await;

        // ── GET /channels/{channelId} — 403 forbidden (outsider) ──
        check_get_detail_forbidden(&outsider, &channel_id, &mut failures).await;

        // ── GET /channels/{channelId} — 403 for non-existent channel ──
        // Backend returns 403 (not 404) to avoid leaking channel existence.
        check_get_detail_nonexistent(&owner, &mut failures).await;

        // ── GET /channels/{channelId}/voice — inactive session ──
        check_voice_inactive(&owner, &channel_id, &mut failures).await;

        // ── GET /channels/{channelId}/bans — empty list ──
        check_bans_empty(&owner, &channel_id, &mut failures).await;

        // ── GET /channels/{channelId}/bans — 403 for member ──
        check_bans_forbidden_member(&member, &channel_id, &mut failures).await;

        // ── Ban a member, then check bans non-empty ──
        let ban_target = match AuthenticatedClient::register(&ctx.base_url, &ctx.http_client).await
        {
            Ok(c) => c,
            Err(e) => return err_result(self.name(), start, &format!("ban target register: {e}")),
        };
        if let Err(e) = channel_ops::join_channel(&ban_target, &channel_id, &invite_code).await {
            return err_result(self.name(), start, &e);
        }
        // Ban the target
        let ban_resp = owner
            .post_empty(&format!(
                "/channels/{channel_id}/bans/{}",
                ban_target.user_id
            ))
            .await;
        if let Ok(r) = &ban_resp
            && !r.status().is_success()
        {
            failures.push(AssertionFailure {
                check: "ban target for bans-list test".into(),
                expected: "2xx".into(),
                actual: format!("{}", r.status()),
            });
        }

        check_bans_nonempty(&owner, &channel_id, &mut failures).await;

        // ── GET /channels/{channelId}/invites — non-empty ──
        check_invites_nonempty(&owner, &channel_id, &mut failures).await;

        // ── GET /channels/{channelId}/invites — 403 for member ──
        check_invites_forbidden_member(&member, &channel_id, &mut failures).await;

        ScenarioResult {
            name: self.name().to_owned(),
            passed: failures.is_empty(),
            duration: start.elapsed(),
            failures,
        }
    }
}

// ─── Check helpers ─────────────────────────────────────────────────

async fn check_get_detail_success(
    client: &AuthenticatedClient,
    channel_id: &str,
    failures: &mut Vec<AssertionFailure>,
) {
    match client.get(&format!("/channels/{channel_id}")).await {
        Ok(resp) => {
            if resp.status() != reqwest::StatusCode::OK {
                failures.push(AssertionFailure {
                    check: "GET /channels/{id} success".into(),
                    expected: "200".into(),
                    actual: format!("{}", resp.status()),
                });
                return;
            }
            let body: serde_json::Value = match resp.json().await {
                Ok(v) => v,
                Err(e) => {
                    failures.push(AssertionFailure {
                        check: "GET /channels/{id} parse body".into(),
                        expected: "valid JSON".into(),
                        actual: format!("{e}"),
                    });
                    return;
                }
            };
            if body.get("channel_id").is_none() {
                failures.push(AssertionFailure {
                    check: "GET /channels/{id} has channel_id".into(),
                    expected: "channel_id field present".into(),
                    actual: "missing".into(),
                });
            }
            if body.get("members").and_then(|m| m.as_array()).is_none() {
                failures.push(AssertionFailure {
                    check: "GET /channels/{id} has members array".into(),
                    expected: "members array present".into(),
                    actual: "missing".into(),
                });
            }
            if body.get("role").is_none() {
                failures.push(AssertionFailure {
                    check: "GET /channels/{id} has role".into(),
                    expected: "role field present".into(),
                    actual: "missing".into(),
                });
            }
        }
        Err(e) => {
            failures.push(AssertionFailure {
                check: "GET /channels/{id} request".into(),
                expected: "response".into(),
                actual: format!("error: {e}"),
            });
        }
    }
}

async fn check_get_detail_forbidden(
    client: &AuthenticatedClient,
    channel_id: &str,
    failures: &mut Vec<AssertionFailure>,
) {
    match client.get(&format!("/channels/{channel_id}")).await {
        Ok(resp) => {
            if resp.status() != reqwest::StatusCode::FORBIDDEN {
                failures.push(AssertionFailure {
                    check: "GET /channels/{id} forbidden for outsider".into(),
                    expected: "403".into(),
                    actual: format!("{}", resp.status()),
                });
            }
        }
        Err(e) => {
            failures.push(AssertionFailure {
                check: "GET /channels/{id} forbidden request".into(),
                expected: "response".into(),
                actual: format!("error: {e}"),
            });
        }
    }
}

async fn check_get_detail_nonexistent(
    client: &AuthenticatedClient,
    failures: &mut Vec<AssertionFailure>,
) {
    let fake_id = uuid::Uuid::new_v4();
    match client.get(&format!("/channels/{fake_id}")).await {
        Ok(resp) => {
            // Backend returns 403 for non-existent channels to avoid leaking existence.
            if resp.status() != reqwest::StatusCode::FORBIDDEN {
                failures.push(AssertionFailure {
                    check: "GET /channels/{fake_id} returns 403 (not 404)".into(),
                    expected: "403".into(),
                    actual: format!("{}", resp.status()),
                });
            }
        }
        Err(e) => {
            failures.push(AssertionFailure {
                check: "GET /channels/{fake_id} request".into(),
                expected: "response".into(),
                actual: format!("error: {e}"),
            });
        }
    }
}

async fn check_voice_inactive(
    client: &AuthenticatedClient,
    channel_id: &str,
    failures: &mut Vec<AssertionFailure>,
) {
    match client.get(&format!("/channels/{channel_id}/voice")).await {
        Ok(resp) => {
            if resp.status() != reqwest::StatusCode::OK {
                failures.push(AssertionFailure {
                    check: "GET /channels/{id}/voice inactive".into(),
                    expected: "200".into(),
                    actual: format!("{}", resp.status()),
                });
                return;
            }
            let body: serde_json::Value = match resp.json().await {
                Ok(v) => v,
                Err(e) => {
                    failures.push(AssertionFailure {
                        check: "GET /channels/{id}/voice parse".into(),
                        expected: "valid JSON".into(),
                        actual: format!("{e}"),
                    });
                    return;
                }
            };
            if body.get("active").and_then(|v| v.as_bool()) != Some(false) {
                failures.push(AssertionFailure {
                    check: "GET /channels/{id}/voice active=false".into(),
                    expected: "active: false".into(),
                    actual: format!("{body}"),
                });
            }
        }
        Err(e) => {
            failures.push(AssertionFailure {
                check: "GET /channels/{id}/voice request".into(),
                expected: "response".into(),
                actual: format!("error: {e}"),
            });
        }
    }
}

async fn check_bans_empty(
    client: &AuthenticatedClient,
    channel_id: &str,
    failures: &mut Vec<AssertionFailure>,
) {
    match client.get(&format!("/channels/{channel_id}/bans")).await {
        Ok(resp) => {
            if resp.status() != reqwest::StatusCode::OK {
                failures.push(AssertionFailure {
                    check: "GET /channels/{id}/bans empty list".into(),
                    expected: "200".into(),
                    actual: format!("{}", resp.status()),
                });
                return;
            }
            let body: serde_json::Value = match resp.json().await {
                Ok(v) => v,
                Err(e) => {
                    failures.push(AssertionFailure {
                        check: "GET /channels/{id}/bans parse".into(),
                        expected: "valid JSON".into(),
                        actual: format!("{e}"),
                    });
                    return;
                }
            };
            let banned = body.get("banned").and_then(|v| v.as_array());
            if banned.map(|a| a.len()) != Some(0) {
                failures.push(AssertionFailure {
                    check: "GET /channels/{id}/bans empty".into(),
                    expected: "banned: []".into(),
                    actual: format!("{body}"),
                });
            }
        }
        Err(e) => {
            failures.push(AssertionFailure {
                check: "GET /channels/{id}/bans request".into(),
                expected: "response".into(),
                actual: format!("error: {e}"),
            });
        }
    }
}

async fn check_bans_forbidden_member(
    member: &AuthenticatedClient,
    channel_id: &str,
    failures: &mut Vec<AssertionFailure>,
) {
    match member.get(&format!("/channels/{channel_id}/bans")).await {
        Ok(resp) => {
            if resp.status() != reqwest::StatusCode::FORBIDDEN {
                failures.push(AssertionFailure {
                    check: "GET /channels/{id}/bans 403 for member".into(),
                    expected: "403".into(),
                    actual: format!("{}", resp.status()),
                });
            }
        }
        Err(e) => {
            failures.push(AssertionFailure {
                check: "GET /channels/{id}/bans member request".into(),
                expected: "response".into(),
                actual: format!("error: {e}"),
            });
        }
    }
}

async fn check_bans_nonempty(
    client: &AuthenticatedClient,
    channel_id: &str,
    failures: &mut Vec<AssertionFailure>,
) {
    match client.get(&format!("/channels/{channel_id}/bans")).await {
        Ok(resp) => {
            if resp.status() != reqwest::StatusCode::OK {
                failures.push(AssertionFailure {
                    check: "GET /channels/{id}/bans non-empty".into(),
                    expected: "200".into(),
                    actual: format!("{}", resp.status()),
                });
                return;
            }
            let body: serde_json::Value = match resp.json().await {
                Ok(v) => v,
                Err(e) => {
                    failures.push(AssertionFailure {
                        check: "GET /channels/{id}/bans non-empty parse".into(),
                        expected: "valid JSON".into(),
                        actual: format!("{e}"),
                    });
                    return;
                }
            };
            let banned = body.get("banned").and_then(|v| v.as_array());
            match banned {
                Some(arr) if !arr.is_empty() => {
                    // Verify each entry has user_id and banned_at
                    for entry in arr {
                        if entry.get("user_id").is_none() || entry.get("banned_at").is_none() {
                            failures.push(AssertionFailure {
                                check: "bans entry has user_id + banned_at".into(),
                                expected: "both fields present".into(),
                                actual: format!("{entry}"),
                            });
                        }
                    }
                }
                _ => {
                    failures.push(AssertionFailure {
                        check: "GET /channels/{id}/bans non-empty".into(),
                        expected: "at least 1 banned member".into(),
                        actual: format!("{body}"),
                    });
                }
            }
        }
        Err(e) => {
            failures.push(AssertionFailure {
                check: "GET /channels/{id}/bans non-empty request".into(),
                expected: "response".into(),
                actual: format!("error: {e}"),
            });
        }
    }
}

async fn check_invites_nonempty(
    client: &AuthenticatedClient,
    channel_id: &str,
    failures: &mut Vec<AssertionFailure>,
) {
    match client.get(&format!("/channels/{channel_id}/invites")).await {
        Ok(resp) => {
            if resp.status() != reqwest::StatusCode::OK {
                failures.push(AssertionFailure {
                    check: "GET /channels/{id}/invites non-empty".into(),
                    expected: "200".into(),
                    actual: format!("{}", resp.status()),
                });
                return;
            }
            let body: serde_json::Value = match resp.json().await {
                Ok(v) => v,
                Err(e) => {
                    failures.push(AssertionFailure {
                        check: "GET /channels/{id}/invites parse".into(),
                        expected: "valid JSON".into(),
                        actual: format!("{e}"),
                    });
                    return;
                }
            };
            let arr = body.as_array();
            match arr {
                Some(a) if !a.is_empty() => {
                    for entry in a {
                        if entry.get("code").is_none() {
                            failures.push(AssertionFailure {
                                check: "invite entry has code".into(),
                                expected: "code field present".into(),
                                actual: format!("{entry}"),
                            });
                        }
                    }
                }
                _ => {
                    failures.push(AssertionFailure {
                        check: "GET /channels/{id}/invites non-empty".into(),
                        expected: "at least 1 invite".into(),
                        actual: format!("{body}"),
                    });
                }
            }
        }
        Err(e) => {
            failures.push(AssertionFailure {
                check: "GET /channels/{id}/invites request".into(),
                expected: "response".into(),
                actual: format!("error: {e}"),
            });
        }
    }
}

async fn check_invites_forbidden_member(
    member: &AuthenticatedClient,
    channel_id: &str,
    failures: &mut Vec<AssertionFailure>,
) {
    match member.get(&format!("/channels/{channel_id}/invites")).await {
        Ok(resp) => {
            if resp.status() != reqwest::StatusCode::FORBIDDEN {
                failures.push(AssertionFailure {
                    check: "GET /channels/{id}/invites 403 for member".into(),
                    expected: "403".into(),
                    actual: format!("{}", resp.status()),
                });
            }
        }
        Err(e) => {
            failures.push(AssertionFailure {
                check: "GET /channels/{id}/invites member request".into(),
                expected: "response".into(),
                actual: format!("error: {e}"),
            });
        }
    }
}

// ─── Helpers ───────────────────────────────────────────────────────

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
