/// Voice Room Host Control Scenarios
///
/// Tests the REST surface the GUI depends on for host control enforcement:
/// - Role-based access: only owner/admin can change roles, ban members
/// - Member role verification: GET /channels/{id} returns role per member
/// - Admin promotion: PUT /channels/{id}/members/{userId}/role
/// - Ban (REST kick equivalent): POST /channels/{id}/bans/{userId}
/// - Banned member loses voice status access (403)
/// - Regular members cannot perform host actions
///
/// Note: Kick, mute, and stop-share are WebSocket signaling operations.
/// These tests exercise the REST role/permission surface the GUI uses to
/// determine host status and enforce host-only actions.
/// Requirements: 8.1, 8.2, 8.3, 8.4, 8.5, 9.1, 9.2, 9.3, 9.4, 9.5, 9.6, 9.7
use std::time::Instant;

use async_trait::async_trait;

use crate::channel_ops;
use crate::client::AuthenticatedClient;
use crate::harness_context::TestContext;
use crate::results::{AssertionFailure, ScenarioResult};
use crate::runner::Scenario;

pub struct VoiceRoomHostScenario;

#[async_trait]
impl Scenario for VoiceRoomHostScenario {
    fn name(&self) -> &str {
        "voice-room-host"
    }

    async fn run(&self, ctx: &TestContext) -> ScenarioResult {
        let start = Instant::now();
        let mut failures: Vec<AssertionFailure> = Vec::new();

        // --- Setup: owner creates channel, two members join ---
        let owner = match AuthenticatedClient::register(&ctx.base_url, &ctx.http_client).await {
            Ok(c) => c,
            Err(e) => return err_result(self.name(), start, &format!("owner register: {e}")),
        };

        let member_a = match AuthenticatedClient::register(&ctx.base_url, &ctx.http_client).await {
            Ok(c) => c,
            Err(e) => return err_result(self.name(), start, &format!("member_a register: {e}")),
        };

        let member_b = match AuthenticatedClient::register(&ctx.base_url, &ctx.http_client).await {
            Ok(c) => c,
            Err(e) => return err_result(self.name(), start, &format!("member_b register: {e}")),
        };

        let channel_id = match channel_ops::create_channel(&owner, "host-control-test").await {
            Ok(id) => id,
            Err(e) => return err_result(self.name(), start, &e),
        };

        let invite_code = match channel_ops::create_invite(&owner, &channel_id).await {
            Ok(code) => code,
            Err(e) => return err_result(self.name(), start, &e),
        };

        if let Err(e) = channel_ops::join_channel(&member_a, &channel_id, &invite_code).await {
            return err_result(self.name(), start, &e);
        }
        if let Err(e) = channel_ops::join_channel(&member_b, &channel_id, &invite_code).await {
            return err_result(self.name(), start, &e);
        }

        // ── 1. Owner sees own role as "owner" (host gating basis) ──
        check_owner_role_is_host(&owner, &channel_id, &mut failures).await;

        // ── 2. Member sees own role as "member" (non-host) ──
        check_member_role_is_guest(&member_a, &channel_id, &mut failures).await;

        // ── 3. Member cannot change roles (host control gating) ──
        check_member_cannot_change_role(&member_a, &channel_id, &member_b.user_id, &mut failures)
            .await;

        // ── 4. Member cannot ban (kick equivalent gating) ──
        check_member_cannot_ban(&member_a, &channel_id, &member_b.user_id, &mut failures).await;

        // ── 5. Owner can promote member to admin (host role grant) ──
        check_owner_can_promote_admin(&owner, &channel_id, &member_a.user_id, &mut failures).await;

        // ── 6. Admin (promoted) can ban another member (host action) ──
        check_admin_can_ban(&member_a, &channel_id, &member_b.user_id, &mut failures).await;

        // ── 7. Banned member loses voice status access (kicked state) ──
        check_banned_member_forbidden(&member_b, &channel_id, &mut failures).await;

        // ── 8. Admin role visible in channel detail members list ──
        check_admin_role_in_members(&owner, &channel_id, &member_a.user_id, &mut failures).await;

        ScenarioResult {
            name: self.name().to_owned(),
            passed: failures.is_empty(),
            duration: start.elapsed(),
            failures,
        }
    }
}

/// Validates: Requirements 8.1, 8.2 (host control gating)
/// The GUI determines host status from the channel role returned by
/// GET /channels/{id}. Owner role maps to host → kick/mute/stop-share visible.
async fn check_owner_role_is_host(
    owner: &AuthenticatedClient,
    channel_id: &str,
    failures: &mut Vec<AssertionFailure>,
) {
    let resp = match owner.get(&format!("/channels/{channel_id}")).await {
        Ok(r) => r,
        Err(e) => {
            failures.push(AssertionFailure {
                check: "owner channel detail request".into(),
                expected: "response".into(),
                actual: format!("error: {e}"),
            });
            return;
        }
    };

    if resp.status() != reqwest::StatusCode::OK {
        failures.push(AssertionFailure {
            check: "owner channel detail status".into(),
            expected: "200".into(),
            actual: format!("{}", resp.status()),
        });
        return;
    }

    let body: serde_json::Value = match resp.json().await {
        Ok(v) => v,
        Err(e) => {
            failures.push(AssertionFailure {
                check: "owner channel detail parse".into(),
                expected: "valid JSON".into(),
                actual: format!("{e}"),
            });
            return;
        }
    };

    // The "role" field on the channel detail is the caller's own role
    match body.get("role").and_then(|v| v.as_str()) {
        Some("owner") => {} // owner maps to host in GUI
        other => {
            failures.push(AssertionFailure {
                check: "owner role is 'owner' (maps to host)".into(),
                expected: "owner".into(),
                actual: format!("{other:?}"),
            });
        }
    }
}

/// Validates: Requirements 9.5, 9.7 (host control gating — non-host)
/// A regular member's role is "member" which maps to guest in the GUI.
/// Kick/mute/stop-share controls must NOT be rendered for guests.
async fn check_member_role_is_guest(
    member: &AuthenticatedClient,
    channel_id: &str,
    failures: &mut Vec<AssertionFailure>,
) {
    let resp = match member.get(&format!("/channels/{channel_id}")).await {
        Ok(r) => r,
        Err(e) => {
            failures.push(AssertionFailure {
                check: "member channel detail request".into(),
                expected: "response".into(),
                actual: format!("error: {e}"),
            });
            return;
        }
    };

    if resp.status() != reqwest::StatusCode::OK {
        failures.push(AssertionFailure {
            check: "member channel detail status".into(),
            expected: "200".into(),
            actual: format!("{}", resp.status()),
        });
        return;
    }

    let body: serde_json::Value = match resp.json().await {
        Ok(v) => v,
        Err(e) => {
            failures.push(AssertionFailure {
                check: "member channel detail parse".into(),
                expected: "valid JSON".into(),
                actual: format!("{e}"),
            });
            return;
        }
    };

    match body.get("role").and_then(|v| v.as_str()) {
        Some("member") => {} // member maps to guest in GUI — no host controls
        other => {
            failures.push(AssertionFailure {
                check: "member role is 'member' (maps to guest)".into(),
                expected: "member".into(),
                actual: format!("{other:?}"),
            });
        }
    }
}

/// Validates: Requirements 8.1, 9.1 (host control gating)
/// A regular member cannot change another member's role.
/// This enforces that only hosts (owner/admin) can perform role mutations.
async fn check_member_cannot_change_role(
    member: &AuthenticatedClient,
    channel_id: &str,
    target_user_id: &str,
    failures: &mut Vec<AssertionFailure>,
) {
    let resp = match member
        .put(
            &format!("/channels/{channel_id}/members/{target_user_id}/role"),
            &serde_json::json!({ "role": "admin" }),
        )
        .await
    {
        Ok(r) => r,
        Err(e) => {
            failures.push(AssertionFailure {
                check: "member change role request".into(),
                expected: "response".into(),
                actual: format!("error: {e}"),
            });
            return;
        }
    };

    if resp.status() != reqwest::StatusCode::FORBIDDEN {
        failures.push(AssertionFailure {
            check: "member cannot change role (403)".into(),
            expected: "403".into(),
            actual: format!("{}", resp.status()),
        });
    }
}

/// Validates: Requirements 8.1, 8.2, 8.3 (kick gating)
/// A regular member cannot ban (REST kick equivalent) another member.
/// The GUI's /kick button sends KickParticipant over WS, but the server
/// enforces Host role. This REST ban test validates the same permission model.
async fn check_member_cannot_ban(
    member: &AuthenticatedClient,
    channel_id: &str,
    target_user_id: &str,
    failures: &mut Vec<AssertionFailure>,
) {
    let resp = match member
        .post_empty(&format!("/channels/{channel_id}/bans/{target_user_id}"))
        .await
    {
        Ok(r) => r,
        Err(e) => {
            failures.push(AssertionFailure {
                check: "member ban request".into(),
                expected: "response".into(),
                actual: format!("error: {e}"),
            });
            return;
        }
    };

    if resp.status() != reqwest::StatusCode::FORBIDDEN {
        failures.push(AssertionFailure {
            check: "member cannot ban (403)".into(),
            expected: "403".into(),
            actual: format!("{}", resp.status()),
        });
    }
}

/// Validates: Requirements 9.1, 9.2 (admin promotion — host role grant)
/// Owner can promote a member to admin. Admin maps to host in the GUI,
/// granting kick/mute/stop-share controls.
async fn check_owner_can_promote_admin(
    owner: &AuthenticatedClient,
    channel_id: &str,
    target_user_id: &str,
    failures: &mut Vec<AssertionFailure>,
) {
    if let Err(e) = channel_ops::promote_to_admin(owner, channel_id, target_user_id).await {
        failures.push(AssertionFailure {
            check: "owner promotes member to admin".into(),
            expected: "success".into(),
            actual: e,
        });
    }
}

/// Validates: Requirements 8.1, 8.3, 8.4 (admin can kick via ban)
/// An admin (host) can ban another member, which is the REST equivalent
/// of the WS KickParticipant action. After ban, the target is removed
/// from the channel — mirroring the "target removed" behavior of kick.
async fn check_admin_can_ban(
    admin: &AuthenticatedClient,
    channel_id: &str,
    target_user_id: &str,
    failures: &mut Vec<AssertionFailure>,
) {
    let resp = match admin
        .post_empty(&format!("/channels/{channel_id}/bans/{target_user_id}"))
        .await
    {
        Ok(r) => r,
        Err(e) => {
            failures.push(AssertionFailure {
                check: "admin ban request".into(),
                expected: "response".into(),
                actual: format!("error: {e}"),
            });
            return;
        }
    };

    if !resp.status().is_success() {
        let status = resp.status();
        let body = resp.text().await.unwrap_or_default();
        failures.push(AssertionFailure {
            check: "admin can ban member (host action)".into(),
            expected: "2xx".into(),
            actual: format!("{status} {body}"),
        });
    }
}

/// Validates: Requirements 8.4, 8.5 (kicked/banned state)
/// After being banned, the member loses access to voice status (403).
/// This mirrors the GUI's "You were kicked" state — the banned user
/// can no longer interact with the channel's voice session.
async fn check_banned_member_forbidden(
    banned: &AuthenticatedClient,
    channel_id: &str,
    failures: &mut Vec<AssertionFailure>,
) {
    // Voice status should be forbidden for banned member
    let resp = match banned.get(&format!("/channels/{channel_id}/voice")).await {
        Ok(r) => r,
        Err(e) => {
            failures.push(AssertionFailure {
                check: "banned member voice status request".into(),
                expected: "response".into(),
                actual: format!("error: {e}"),
            });
            return;
        }
    };

    if resp.status() != reqwest::StatusCode::FORBIDDEN {
        failures.push(AssertionFailure {
            check: "banned member voice status returns 403".into(),
            expected: "403".into(),
            actual: format!("{}", resp.status()),
        });
    }

    // Channel detail should also be forbidden for banned member
    let resp = match banned.get(&format!("/channels/{channel_id}")).await {
        Ok(r) => r,
        Err(e) => {
            failures.push(AssertionFailure {
                check: "banned member channel detail request".into(),
                expected: "response".into(),
                actual: format!("error: {e}"),
            });
            return;
        }
    };

    if resp.status() != reqwest::StatusCode::FORBIDDEN {
        failures.push(AssertionFailure {
            check: "banned member channel detail returns 403".into(),
            expected: "403".into(),
            actual: format!("{}", resp.status()),
        });
    }
}

/// Validates: Requirements 9.3, 9.4 (admin role visible in member list)
/// After promotion, the admin role is reflected in the channel detail
/// members list. The GUI uses this to determine which participants
/// have host privileges (owner/admin → host controls visible).
async fn check_admin_role_in_members(
    owner: &AuthenticatedClient,
    channel_id: &str,
    admin_user_id: &str,
    failures: &mut Vec<AssertionFailure>,
) {
    let resp = match owner.get(&format!("/channels/{channel_id}")).await {
        Ok(r) => r,
        Err(e) => {
            failures.push(AssertionFailure {
                check: "channel detail for admin role check".into(),
                expected: "response".into(),
                actual: format!("error: {e}"),
            });
            return;
        }
    };

    if resp.status() != reqwest::StatusCode::OK {
        failures.push(AssertionFailure {
            check: "channel detail status for admin role check".into(),
            expected: "200".into(),
            actual: format!("{}", resp.status()),
        });
        return;
    }

    let body: serde_json::Value = match resp.json().await {
        Ok(v) => v,
        Err(e) => {
            failures.push(AssertionFailure {
                check: "channel detail parse for admin role check".into(),
                expected: "valid JSON".into(),
                actual: format!("{e}"),
            });
            return;
        }
    };

    let members = match body.get("members").and_then(|v| v.as_array()) {
        Some(arr) => arr,
        None => {
            failures.push(AssertionFailure {
                check: "channel detail has members array".into(),
                expected: "members array present".into(),
                actual: format!("{body}"),
            });
            return;
        }
    };

    // Find the promoted admin in the members list
    let admin_member = members.iter().find(|m| {
        m.get("user_id")
            .and_then(|v| v.as_str())
            .map(|id| id == admin_user_id)
            .unwrap_or(false)
    });

    match admin_member {
        Some(m) => {
            let role = m.get("role").and_then(|v| v.as_str());
            if role != Some("admin") {
                failures.push(AssertionFailure {
                    check: "promoted member has admin role".into(),
                    expected: "admin".into(),
                    actual: format!("{role:?}"),
                });
            }
        }
        None => {
            failures.push(AssertionFailure {
                check: "admin user found in members list".into(),
                expected: format!("user_id={admin_user_id}"),
                actual: "not found".into(),
            });
        }
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
