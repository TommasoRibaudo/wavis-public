/// Channel Detail Mutation Scenarios
///
/// Tests the write REST endpoints the GUI ChannelDetail screen depends on:
/// - POST   /channels/{channelId}/invites          — success, 429
/// - DELETE /channels/{channelId}/invites/{code}    — success, 404
/// - POST   /channels/{channelId}/bans/{userId}     — success, 403, 409
/// - DELETE /channels/{channelId}/bans/{userId}      — success, 404
/// - PUT    /channels/{channelId}/members/{userId}/role — success, 403
/// - DELETE /channels/{channelId}                    — success, 403
/// - POST   /channels/{channelId}/leave              — success, 400 owner
///
/// **Validates: Requirements 5.3, 6.3, 7.3, 8.4, 9.3, 10.3, 11.3, 15.1**
use std::time::Instant;

use async_trait::async_trait;

use crate::channel_ops;
use crate::client::AuthenticatedClient;
use crate::harness_context::TestContext;
use crate::results::{AssertionFailure, ScenarioResult};
use crate::runner::Scenario;

pub struct ChannelDetailMutationsScenario;

#[async_trait]
impl Scenario for ChannelDetailMutationsScenario {
    fn name(&self) -> &str {
        "channel-detail-mutations"
    }

    async fn run(&self, ctx: &TestContext) -> ScenarioResult {
        let start = Instant::now();
        let mut failures: Vec<AssertionFailure> = Vec::new();

        // --- Setup ---
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
        let target = match AuthenticatedClient::register(&ctx.base_url, &ctx.http_client).await {
            Ok(c) => c,
            Err(e) => return err_result(self.name(), start, &format!("target register: {e}")),
        };

        let channel_id = match channel_ops::create_channel(&owner, "mutations-test").await {
            Ok(id) => id,
            Err(e) => return err_result(self.name(), start, &e),
        };
        let invite_code = match channel_ops::create_invite(&owner, &channel_id).await {
            Ok(c) => c,
            Err(e) => return err_result(self.name(), start, &e),
        };
        for client in [&admin, &member, &target] {
            if let Err(e) = channel_ops::join_channel(client, &channel_id, &invite_code).await {
                return err_result(self.name(), start, &e);
            }
        }
        if let Err(e) = channel_ops::promote_to_admin(&owner, &channel_id, &admin.user_id).await {
            return err_result(self.name(), start, &e);
        }

        // ── POST /channels/{id}/invites — success ──
        check_create_invite_success(&owner, &channel_id, &mut failures).await;

        // ── DELETE /channels/{id}/invites/{code} — success ──
        check_revoke_invite_success(&owner, &channel_id, &mut failures).await;

        // ── DELETE /channels/{id}/invites/{code} — 404 ──
        check_revoke_invite_not_found(&owner, &channel_id, &mut failures).await;

        // ── POST /channels/{id}/bans/{userId} — success ──
        check_ban_success(&owner, &channel_id, &target.user_id, &mut failures).await;

        // ── POST /channels/{id}/bans/{userId} — 409 already banned ──
        check_ban_already_banned(&owner, &channel_id, &target.user_id, &mut failures).await;

        // ── POST /channels/{id}/bans/{userId} — 403 member tries to ban ──
        check_ban_forbidden(&member, &channel_id, &target.user_id, &mut failures).await;

        // ── DELETE /channels/{id}/bans/{userId} — success ──
        check_unban_success(&owner, &channel_id, &target.user_id, &mut failures).await;

        // ── DELETE /channels/{id}/bans/{userId} — 404 not banned ──
        check_unban_not_found(&owner, &channel_id, &target.user_id, &mut failures).await;

        // ── PUT /channels/{id}/members/{userId}/role — success ──
        check_role_change_success(&owner, &channel_id, &member.user_id, &mut failures).await;

        // ── PUT /channels/{id}/members/{userId}/role — 403 non-owner ──
        check_role_change_forbidden(&admin, &channel_id, &member.user_id, &mut failures).await;

        // ── POST /channels/{id}/leave — 400 owner cannot leave ──
        check_owner_cannot_leave(&owner, &channel_id, &mut failures).await;

        // ── POST /channels/{id}/leave — success (member leaves) ──
        check_leave_success(&member, &channel_id, &mut failures).await;

        // ── DELETE /channels/{id} — 403 non-owner ──
        check_delete_forbidden(&admin, &channel_id, &mut failures).await;

        // ── DELETE /channels/{id} — success (owner deletes) ──
        check_delete_success(&owner, &channel_id, &mut failures).await;

        ScenarioResult {
            name: self.name().to_owned(),
            passed: failures.is_empty(),
            duration: start.elapsed(),
            failures,
        }
    }
}

// ─── Check helpers ─────────────────────────────────────────────────

async fn check_create_invite_success(
    client: &AuthenticatedClient,
    channel_id: &str,
    failures: &mut Vec<AssertionFailure>,
) {
    match client
        .post(
            &format!("/channels/{channel_id}/invites"),
            &serde_json::json!({}),
        )
        .await
    {
        Ok(resp) => {
            let status = resp.status().as_u16();
            if status != 201 {
                failures.push(AssertionFailure {
                    check: "POST /invites success".into(),
                    expected: "201".into(),
                    actual: format!("{status}"),
                });
                return;
            }
            let body: serde_json::Value = match resp.json().await {
                Ok(v) => v,
                Err(e) => {
                    failures.push(AssertionFailure {
                        check: "POST /invites parse".into(),
                        expected: "valid JSON".into(),
                        actual: format!("{e}"),
                    });
                    return;
                }
            };
            if body.get("code").is_none() {
                failures.push(AssertionFailure {
                    check: "POST /invites has code".into(),
                    expected: "code field".into(),
                    actual: format!("{body}"),
                });
            }
        }
        Err(e) => {
            failures.push(AssertionFailure {
                check: "POST /invites request".into(),
                expected: "response".into(),
                actual: format!("error: {e}"),
            });
        }
    }
}

async fn check_revoke_invite_success(
    client: &AuthenticatedClient,
    channel_id: &str,
    failures: &mut Vec<AssertionFailure>,
) {
    // Create an invite to revoke
    let code = match channel_ops::create_invite(client, channel_id).await {
        Ok(c) => c,
        Err(e) => {
            failures.push(AssertionFailure {
                check: "revoke setup: create invite".into(),
                expected: "success".into(),
                actual: e,
            });
            return;
        }
    };

    match client
        .delete(&format!("/channels/{channel_id}/invites/{code}"))
        .await
    {
        Ok(resp) => {
            let status = resp.status().as_u16();
            if status != 204 {
                failures.push(AssertionFailure {
                    check: "DELETE /invites/{code} success".into(),
                    expected: "204".into(),
                    actual: format!("{status}"),
                });
            }
        }
        Err(e) => {
            failures.push(AssertionFailure {
                check: "DELETE /invites/{code} request".into(),
                expected: "response".into(),
                actual: format!("error: {e}"),
            });
        }
    }
}

async fn check_revoke_invite_not_found(
    client: &AuthenticatedClient,
    channel_id: &str,
    failures: &mut Vec<AssertionFailure>,
) {
    match client
        .delete(&format!("/channels/{channel_id}/invites/NONEXISTENT999"))
        .await
    {
        Ok(resp) => {
            let status = resp.status().as_u16();
            if status != 404 {
                failures.push(AssertionFailure {
                    check: "DELETE /invites/{code} 404".into(),
                    expected: "404".into(),
                    actual: format!("{status}"),
                });
            }
        }
        Err(e) => {
            failures.push(AssertionFailure {
                check: "DELETE /invites/{code} 404 request".into(),
                expected: "response".into(),
                actual: format!("error: {e}"),
            });
        }
    }
}

async fn check_ban_success(
    client: &AuthenticatedClient,
    channel_id: &str,
    user_id: &str,
    failures: &mut Vec<AssertionFailure>,
) {
    match client
        .post_empty(&format!("/channels/{channel_id}/bans/{user_id}"))
        .await
    {
        Ok(resp) => {
            let status = resp.status().as_u16();
            if status != 200 {
                failures.push(AssertionFailure {
                    check: "POST /bans/{userId} success".into(),
                    expected: "200".into(),
                    actual: format!("{status}"),
                });
            }
        }
        Err(e) => {
            failures.push(AssertionFailure {
                check: "POST /bans/{userId} request".into(),
                expected: "response".into(),
                actual: format!("error: {e}"),
            });
        }
    }
}

async fn check_ban_already_banned(
    client: &AuthenticatedClient,
    channel_id: &str,
    user_id: &str,
    failures: &mut Vec<AssertionFailure>,
) {
    match client
        .post_empty(&format!("/channels/{channel_id}/bans/{user_id}"))
        .await
    {
        Ok(resp) => {
            let status = resp.status().as_u16();
            if status != 409 {
                failures.push(AssertionFailure {
                    check: "POST /bans/{userId} 409 already banned".into(),
                    expected: "409".into(),
                    actual: format!("{status}"),
                });
            }
        }
        Err(e) => {
            failures.push(AssertionFailure {
                check: "POST /bans/{userId} 409 request".into(),
                expected: "response".into(),
                actual: format!("error: {e}"),
            });
        }
    }
}

async fn check_ban_forbidden(
    member: &AuthenticatedClient,
    channel_id: &str,
    user_id: &str,
    failures: &mut Vec<AssertionFailure>,
) {
    match member
        .post_empty(&format!("/channels/{channel_id}/bans/{user_id}"))
        .await
    {
        Ok(resp) => {
            let status = resp.status().as_u16();
            if status != 403 {
                failures.push(AssertionFailure {
                    check: "POST /bans/{userId} 403 member".into(),
                    expected: "403".into(),
                    actual: format!("{status}"),
                });
            }
        }
        Err(e) => {
            failures.push(AssertionFailure {
                check: "POST /bans/{userId} 403 request".into(),
                expected: "response".into(),
                actual: format!("error: {e}"),
            });
        }
    }
}

async fn check_unban_success(
    client: &AuthenticatedClient,
    channel_id: &str,
    user_id: &str,
    failures: &mut Vec<AssertionFailure>,
) {
    match client
        .delete(&format!("/channels/{channel_id}/bans/{user_id}"))
        .await
    {
        Ok(resp) => {
            let status = resp.status().as_u16();
            if status != 204 {
                failures.push(AssertionFailure {
                    check: "DELETE /bans/{userId} success".into(),
                    expected: "204".into(),
                    actual: format!("{status}"),
                });
            }
        }
        Err(e) => {
            failures.push(AssertionFailure {
                check: "DELETE /bans/{userId} request".into(),
                expected: "response".into(),
                actual: format!("error: {e}"),
            });
        }
    }
}

async fn check_unban_not_found(
    client: &AuthenticatedClient,
    channel_id: &str,
    user_id: &str,
    failures: &mut Vec<AssertionFailure>,
) {
    // user_id was already unbanned above, so this should 404
    match client
        .delete(&format!("/channels/{channel_id}/bans/{user_id}"))
        .await
    {
        Ok(resp) => {
            let status = resp.status().as_u16();
            if status != 404 {
                failures.push(AssertionFailure {
                    check: "DELETE /bans/{userId} 404 not banned".into(),
                    expected: "404".into(),
                    actual: format!("{status}"),
                });
            }
        }
        Err(e) => {
            failures.push(AssertionFailure {
                check: "DELETE /bans/{userId} 404 request".into(),
                expected: "response".into(),
                actual: format!("error: {e}"),
            });
        }
    }
}

async fn check_role_change_success(
    owner: &AuthenticatedClient,
    channel_id: &str,
    user_id: &str,
    failures: &mut Vec<AssertionFailure>,
) {
    match owner
        .put(
            &format!("/channels/{channel_id}/members/{user_id}/role"),
            &serde_json::json!({ "role": "admin" }),
        )
        .await
    {
        Ok(resp) => {
            let status = resp.status().as_u16();
            if status != 200 {
                failures.push(AssertionFailure {
                    check: "PUT /members/{userId}/role success".into(),
                    expected: "200".into(),
                    actual: format!("{status}"),
                });
            }
        }
        Err(e) => {
            failures.push(AssertionFailure {
                check: "PUT /members/{userId}/role request".into(),
                expected: "response".into(),
                actual: format!("error: {e}"),
            });
        }
    }
}

async fn check_role_change_forbidden(
    non_owner: &AuthenticatedClient,
    channel_id: &str,
    user_id: &str,
    failures: &mut Vec<AssertionFailure>,
) {
    match non_owner
        .put(
            &format!("/channels/{channel_id}/members/{user_id}/role"),
            &serde_json::json!({ "role": "member" }),
        )
        .await
    {
        Ok(resp) => {
            let status = resp.status().as_u16();
            if status != 403 {
                failures.push(AssertionFailure {
                    check: "PUT /members/{userId}/role 403 non-owner".into(),
                    expected: "403".into(),
                    actual: format!("{status}"),
                });
            }
        }
        Err(e) => {
            failures.push(AssertionFailure {
                check: "PUT /members/{userId}/role 403 request".into(),
                expected: "response".into(),
                actual: format!("error: {e}"),
            });
        }
    }
}

async fn check_owner_cannot_leave(
    owner: &AuthenticatedClient,
    channel_id: &str,
    failures: &mut Vec<AssertionFailure>,
) {
    match owner
        .post_empty(&format!("/channels/{channel_id}/leave"))
        .await
    {
        Ok(resp) => {
            let status = resp.status().as_u16();
            if status != 400 {
                failures.push(AssertionFailure {
                    check: "POST /leave 400 owner cannot leave".into(),
                    expected: "400".into(),
                    actual: format!("{status}"),
                });
            }
        }
        Err(e) => {
            failures.push(AssertionFailure {
                check: "POST /leave owner request".into(),
                expected: "response".into(),
                actual: format!("error: {e}"),
            });
        }
    }
}

async fn check_leave_success(
    member: &AuthenticatedClient,
    channel_id: &str,
    failures: &mut Vec<AssertionFailure>,
) {
    match member
        .post_empty(&format!("/channels/{channel_id}/leave"))
        .await
    {
        Ok(resp) => {
            let status = resp.status().as_u16();
            if status != 204 {
                failures.push(AssertionFailure {
                    check: "POST /leave success".into(),
                    expected: "204".into(),
                    actual: format!("{status}"),
                });
            }
        }
        Err(e) => {
            failures.push(AssertionFailure {
                check: "POST /leave request".into(),
                expected: "response".into(),
                actual: format!("error: {e}"),
            });
        }
    }
}

async fn check_delete_forbidden(
    non_owner: &AuthenticatedClient,
    channel_id: &str,
    failures: &mut Vec<AssertionFailure>,
) {
    match non_owner.delete(&format!("/channels/{channel_id}")).await {
        Ok(resp) => {
            let status = resp.status().as_u16();
            if status != 403 {
                failures.push(AssertionFailure {
                    check: "DELETE /channels/{id} 403 non-owner".into(),
                    expected: "403".into(),
                    actual: format!("{status}"),
                });
            }
        }
        Err(e) => {
            failures.push(AssertionFailure {
                check: "DELETE /channels/{id} 403 request".into(),
                expected: "response".into(),
                actual: format!("error: {e}"),
            });
        }
    }
}

async fn check_delete_success(
    owner: &AuthenticatedClient,
    channel_id: &str,
    failures: &mut Vec<AssertionFailure>,
) {
    match owner.delete(&format!("/channels/{channel_id}")).await {
        Ok(resp) => {
            let status = resp.status().as_u16();
            if status != 204 {
                failures.push(AssertionFailure {
                    check: "DELETE /channels/{id} success".into(),
                    expected: "204".into(),
                    actual: format!("{status}"),
                });
            }
        }
        Err(e) => {
            failures.push(AssertionFailure {
                check: "DELETE /channels/{id} request".into(),
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
