//! Channel business logic and database operations.
//!
//! **Owns:** all channel orchestration — creation, deletion, membership
//! management (join, leave, ban, unban, role changes), invite lifecycle
//! (generation, redemption, revocation, expiry sweep), channel listing
//! and detail queries, and name/role validation.
//!
//! **Does not own:** type definitions or error enums (those are in
//! `domain::channel_models`), HTTP routing (that is `handlers::channel`),
//! or rate limiting (that is `domain::channel_rate_limiter`).

use base64::{Engine as _, engine::general_purpose::URL_SAFE_NO_PAD};
use chrono::{DateTime, Utc};
use rand::RngCore;
use sqlx::{PgPool, Row};
use uuid::Uuid;

use crate::channel::channel_models::*;

/// Validate channel name: non-empty, ≤100 UTF-8 characters.
pub fn validate_channel_name(name: &str) -> Result<(), ChannelError> {
    if name.is_empty() || name.chars().count() > 100 {
        return Err(ChannelError::InvalidName);
    }
    Ok(())
}

/// Generate a CSPRNG invite code (≥128-bit entropy, base64url, 22 chars).
pub fn generate_invite_code() -> String {
    let mut bytes = [0u8; 16];
    rand::thread_rng().fill_bytes(&mut bytes);
    URL_SAFE_NO_PAD.encode(bytes)
}

/// Create a channel + owner membership in a single transaction.
pub async fn create_channel(
    pool: &PgPool,
    user_id: Uuid,
    name: &str,
) -> Result<Channel, ChannelError> {
    validate_channel_name(name)?;

    let mut tx = pool
        .begin()
        .await
        .map_err(|e| ChannelError::DatabaseError(e.to_string()))?;

    let row = sqlx::query(
        "INSERT INTO channels (name, owner_user_id) VALUES ($1, $2) \
         RETURNING channel_id, name, owner_user_id, created_at",
    )
    .bind(name)
    .bind(user_id)
    .fetch_one(&mut *tx)
    .await
    .map_err(|e| ChannelError::DatabaseError(e.to_string()))?;

    let channel_id: Uuid = row.get("channel_id");

    sqlx::query(
        "INSERT INTO channel_memberships (channel_id, user_id, role) VALUES ($1, $2, 'owner')",
    )
    .bind(channel_id)
    .bind(user_id)
    .execute(&mut *tx)
    .await
    .map_err(|e| ChannelError::DatabaseError(e.to_string()))?;

    tx.commit()
        .await
        .map_err(|e| ChannelError::DatabaseError(e.to_string()))?;

    Ok(Channel {
        channel_id,
        name: row.get("name"),
        owner_user_id: row.get("owner_user_id"),
        created_at: row.get("created_at"),
    })
}

/// List channels where user_id is a non-banned member, ordered by joined_at DESC.
pub async fn list_channels(
    pool: &PgPool,
    user_id: Uuid,
) -> Result<Vec<ChannelListItem>, ChannelError> {
    let rows = sqlx::query(
        "SELECT c.channel_id, c.name, c.owner_user_id, c.created_at, cm.role \
         FROM channels c \
         JOIN channel_memberships cm ON c.channel_id = cm.channel_id \
         WHERE cm.user_id = $1 AND cm.banned_at IS NULL \
         ORDER BY cm.joined_at DESC",
    )
    .bind(user_id)
    .fetch_all(pool)
    .await
    .map_err(|e| ChannelError::DatabaseError(e.to_string()))?;

    let items = rows
        .iter()
        .map(|row| {
            let role_str: String = row.get("role");
            ChannelListItem {
                channel_id: row.get("channel_id"),
                name: row.get("name"),
                owner_user_id: row.get("owner_user_id"),
                created_at: row.get("created_at"),
                role: parse_role(&role_str),
            }
        })
        .collect();

    Ok(items)
}

/// Get channel details including non-banned member list.
/// Validates membership, ban state, and owner consistency invariant.
pub async fn get_channel_detail(
    pool: &PgPool,
    channel_id: Uuid,
    user_id: Uuid,
) -> Result<ChannelDetail, ChannelError> {
    // Check requester membership
    let membership_row = sqlx::query(
        "SELECT role, banned_at FROM channel_memberships \
         WHERE channel_id = $1 AND user_id = $2",
    )
    .bind(channel_id)
    .bind(user_id)
    .fetch_optional(pool)
    .await
    .map_err(|e| ChannelError::DatabaseError(e.to_string()))?;

    let membership_row = membership_row.ok_or(ChannelError::NotMember)?;

    let banned_at: Option<DateTime<Utc>> = membership_row.get("banned_at");
    if banned_at.is_some() {
        return Err(ChannelError::Banned);
    }

    let requester_role_str: String = membership_row.get("role");
    let requester_role = parse_role(&requester_role_str);

    // Query channel
    let channel_row = sqlx::query(
        "SELECT channel_id, name, owner_user_id, created_at FROM channels WHERE channel_id = $1",
    )
    .bind(channel_id)
    .fetch_optional(pool)
    .await
    .map_err(|e| ChannelError::DatabaseError(e.to_string()))?;

    let channel_row = channel_row.ok_or(ChannelError::ChannelNotFound)?;

    let ch_owner_user_id: Uuid = channel_row.get("owner_user_id");

    // Owner consistency check: query the membership row where role='owner'
    let owner_membership_row = sqlx::query(
        "SELECT user_id FROM channel_memberships \
         WHERE channel_id = $1 AND role = 'owner'",
    )
    .bind(channel_id)
    .fetch_optional(pool)
    .await
    .map_err(|e| ChannelError::DatabaseError(e.to_string()))?;

    match owner_membership_row {
        None => {
            tracing::error!(
                channel_id = %channel_id,
                "owner consistency violation: no owner membership row found"
            );
            return Err(ChannelError::OwnerConsistencyViolation);
        }
        Some(row) => {
            let membership_owner_id: Uuid = row.get("user_id");
            if membership_owner_id != ch_owner_user_id {
                tracing::error!(
                    channel_id = %channel_id,
                    channels_owner = %ch_owner_user_id,
                    membership_owner = %membership_owner_id,
                    "owner consistency violation: channels.owner_user_id != owner membership user_id"
                );
                return Err(ChannelError::OwnerConsistencyViolation);
            }
        }
    }

    // Query all non-banned members with display name from their most recent active device
    let member_rows = sqlx::query(
        "SELECT cm.user_id, cm.role, cm.joined_at, \
                COALESCE(d.device_name, '') AS display_name \
         FROM channel_memberships cm \
         LEFT JOIN LATERAL ( \
             SELECT device_name FROM devices \
             WHERE devices.user_id = cm.user_id AND revoked_at IS NULL \
             ORDER BY created_at DESC LIMIT 1 \
         ) d ON true \
         WHERE cm.channel_id = $1 AND cm.banned_at IS NULL",
    )
    .bind(channel_id)
    .fetch_all(pool)
    .await
    .map_err(|e| ChannelError::DatabaseError(e.to_string()))?;

    let members = member_rows
        .iter()
        .map(|row| {
            let role_str: String = row.get("role");
            let display_name: String = row.get("display_name");
            ChannelMemberInfo {
                user_id: row.get("user_id"),
                role: parse_role(&role_str),
                joined_at: row.get("joined_at"),
                display_name,
            }
        })
        .collect();

    Ok(ChannelDetail {
        channel_id: channel_row.get("channel_id"),
        name: channel_row.get("name"),
        owner_user_id: ch_owner_user_id,
        created_at: channel_row.get("created_at"),
        role: requester_role,
        members,
    })
}

/// Delete a channel (CASCADE removes memberships + invites). Owner-only.
pub async fn delete_channel(
    pool: &PgPool,
    channel_id: Uuid,
    user_id: Uuid,
) -> Result<(), ChannelError> {
    // Verify requester is the owner
    let channel_row = sqlx::query("SELECT owner_user_id FROM channels WHERE channel_id = $1")
        .bind(channel_id)
        .fetch_optional(pool)
        .await
        .map_err(|e| ChannelError::DatabaseError(e.to_string()))?;

    let channel_row = channel_row.ok_or(ChannelError::ChannelNotFound)?;
    let owner_user_id: Uuid = channel_row.get("owner_user_id");

    if owner_user_id != user_id {
        return Err(ChannelError::Forbidden);
    }

    let mut tx = pool
        .begin()
        .await
        .map_err(|e| ChannelError::DatabaseError(e.to_string()))?;

    sqlx::query("DELETE FROM channels WHERE channel_id = $1")
        .bind(channel_id)
        .execute(&mut *tx)
        .await
        .map_err(|e| ChannelError::DatabaseError(e.to_string()))?;

    tx.commit()
        .await
        .map_err(|e| ChannelError::DatabaseError(e.to_string()))?;

    Ok(())
}

/// Create a channel invite. Requires owner or admin role.
pub async fn create_invite(
    pool: &PgPool,
    channel_id: Uuid,
    user_id: Uuid,
    expires_in_secs: Option<i64>,
    max_uses: Option<i32>,
) -> Result<ChannelInvite, ChannelError> {
    // Check membership + role
    let membership = sqlx::query(
        "SELECT role, banned_at FROM channel_memberships WHERE channel_id = $1 AND user_id = $2",
    )
    .bind(channel_id)
    .bind(user_id)
    .fetch_optional(pool)
    .await
    .map_err(|e| ChannelError::DatabaseError(e.to_string()))?;

    let membership = membership.ok_or(ChannelError::Forbidden)?;
    let banned_at: Option<DateTime<Utc>> = membership.get("banned_at");
    if banned_at.is_some() {
        return Err(ChannelError::Forbidden);
    }

    let role_str: String = membership.get("role");
    let role = parse_role(&role_str);
    if role == ChannelRole::Member {
        return Err(ChannelError::Forbidden);
    }

    let code = generate_invite_code();
    let expires_at = expires_in_secs.map(|secs| Utc::now() + chrono::Duration::seconds(secs));

    let row = sqlx::query(
        "INSERT INTO channel_invites (code, channel_id, expires_at, max_uses) \
         VALUES ($1, $2, $3, $4) \
         RETURNING code, channel_id, expires_at, max_uses, uses, created_at",
    )
    .bind(&code)
    .bind(channel_id)
    .bind(expires_at)
    .bind(max_uses)
    .fetch_one(pool)
    .await
    .map_err(|e| ChannelError::DatabaseError(e.to_string()))?;

    Ok(ChannelInvite {
        code: row.get("code"),
        channel_id: row.get("channel_id"),
        expires_at: row.get("expires_at"),
        max_uses: row.get("max_uses"),
        uses: row.get("uses"),
        created_at: row.get("created_at"),
    })
}

/// Join a channel via invite code. Atomic: validate invite + create membership + increment uses.
pub async fn join_channel_by_invite(
    pool: &PgPool,
    user_id: Uuid,
    code: &str,
) -> Result<JoinResult, ChannelError> {
    let mut tx = pool
        .begin()
        .await
        .map_err(|e| ChannelError::DatabaseError(e.to_string()))?;

    // Atomic invite consumption: UPDATE ... RETURNING
    let invite_row = sqlx::query(
        "UPDATE channel_invites SET uses = uses + 1 \
         WHERE code = $1 \
           AND (expires_at IS NULL OR expires_at > now()) \
           AND (max_uses IS NULL OR uses < max_uses) \
         RETURNING channel_id",
    )
    .bind(code)
    .fetch_optional(&mut *tx)
    .await
    .map_err(|e| ChannelError::DatabaseError(e.to_string()))?;

    let invite_row = invite_row.ok_or(ChannelError::InvalidInvite)?;
    let channel_id: Uuid = invite_row.get("channel_id");

    // Check existing membership
    let existing = sqlx::query(
        "SELECT banned_at FROM channel_memberships WHERE channel_id = $1 AND user_id = $2",
    )
    .bind(channel_id)
    .bind(user_id)
    .fetch_optional(&mut *tx)
    .await
    .map_err(|e| ChannelError::DatabaseError(e.to_string()))?;

    if let Some(row) = existing {
        let banned_at: Option<DateTime<Utc>> = row.get("banned_at");
        if banned_at.is_some() {
            // Drop tx → implicit rollback (uses not committed)
            return Err(ChannelError::Banned);
        }
        return Err(ChannelError::AlreadyMember);
    }

    // INSERT membership
    let result = sqlx::query(
        "INSERT INTO channel_memberships (channel_id, user_id, role) \
         VALUES ($1, $2, 'member') ON CONFLICT DO NOTHING",
    )
    .bind(channel_id)
    .bind(user_id)
    .execute(&mut *tx)
    .await
    .map_err(|e| ChannelError::DatabaseError(e.to_string()))?;

    if result.rows_affected() == 0 {
        // Concurrent race — someone else inserted first
        return Err(ChannelError::AlreadyMember);
    }

    // Get channel name for response
    let channel_row = sqlx::query("SELECT name FROM channels WHERE channel_id = $1")
        .bind(channel_id)
        .fetch_one(&mut *tx)
        .await
        .map_err(|e| ChannelError::DatabaseError(e.to_string()))?;

    tx.commit()
        .await
        .map_err(|e| ChannelError::DatabaseError(e.to_string()))?;

    Ok(JoinResult {
        channel_id,
        name: channel_row.get("name"),
        role: ChannelRole::Member,
    })
}

/// Leave a channel. Owner cannot leave (must delete channel).
/// Banned members cannot leave (must be unbanned first).
pub async fn leave_channel(
    pool: &PgPool,
    channel_id: Uuid,
    user_id: Uuid,
) -> Result<(), ChannelError> {
    let membership = sqlx::query(
        "SELECT role, banned_at FROM channel_memberships WHERE channel_id = $1 AND user_id = $2",
    )
    .bind(channel_id)
    .bind(user_id)
    .fetch_optional(pool)
    .await
    .map_err(|e| ChannelError::DatabaseError(e.to_string()))?;

    let membership = membership.ok_or(ChannelError::NotMember)?;

    let banned_at: Option<DateTime<Utc>> = membership.get("banned_at");
    if banned_at.is_some() {
        return Err(ChannelError::Banned);
    }

    let role_str: String = membership.get("role");
    if parse_role(&role_str) == ChannelRole::Owner {
        return Err(ChannelError::OwnerCannotLeave);
    }

    sqlx::query("DELETE FROM channel_memberships WHERE channel_id = $1 AND user_id = $2")
        .bind(channel_id)
        .bind(user_id)
        .execute(pool)
        .await
        .map_err(|e| ChannelError::DatabaseError(e.to_string()))?;

    Ok(())
}

/// Revoke an invite code. Requires owner or admin role.
pub async fn revoke_invite(
    pool: &PgPool,
    channel_id: Uuid,
    user_id: Uuid,
    code: &str,
) -> Result<(), ChannelError> {
    // Check membership + role
    let membership = sqlx::query(
        "SELECT role, banned_at FROM channel_memberships WHERE channel_id = $1 AND user_id = $2",
    )
    .bind(channel_id)
    .bind(user_id)
    .fetch_optional(pool)
    .await
    .map_err(|e| ChannelError::DatabaseError(e.to_string()))?;

    let membership = membership.ok_or(ChannelError::Forbidden)?;
    let banned_at: Option<DateTime<Utc>> = membership.get("banned_at");
    if banned_at.is_some() {
        return Err(ChannelError::Forbidden);
    }

    let role_str: String = membership.get("role");
    if parse_role(&role_str) == ChannelRole::Member {
        return Err(ChannelError::Forbidden);
    }

    let result = sqlx::query("DELETE FROM channel_invites WHERE code = $1 AND channel_id = $2")
        .bind(code)
        .bind(channel_id)
        .execute(pool)
        .await
        .map_err(|e| ChannelError::DatabaseError(e.to_string()))?;

    if result.rows_affected() == 0 {
        return Err(ChannelError::InviteNotFound);
    }

    Ok(())
}

/// Ban a member. Requires owner or admin. Admins cannot ban other admins.
/// Self-ban prevented. Owner cannot be banned.
pub async fn ban_member(
    pool: &PgPool,
    channel_id: Uuid,
    requester_user_id: Uuid,
    target_user_id: Uuid,
) -> Result<BanResult, ChannelError> {
    // Self-ban check
    if requester_user_id == target_user_id {
        return Err(ChannelError::SelfBan);
    }

    let mut tx = pool
        .begin()
        .await
        .map_err(|e| ChannelError::DatabaseError(e.to_string()))?;

    // Check requester membership + role
    let requester = sqlx::query(
        "SELECT role, banned_at FROM channel_memberships WHERE channel_id = $1 AND user_id = $2",
    )
    .bind(channel_id)
    .bind(requester_user_id)
    .fetch_optional(&mut *tx)
    .await
    .map_err(|e| ChannelError::DatabaseError(e.to_string()))?;

    let requester = requester.ok_or(ChannelError::Forbidden)?;
    let req_banned: Option<DateTime<Utc>> = requester.get("banned_at");
    if req_banned.is_some() {
        return Err(ChannelError::Forbidden);
    }
    let req_role_str: String = requester.get("role");
    let req_role = parse_role(&req_role_str);
    if req_role == ChannelRole::Member {
        return Err(ChannelError::Forbidden);
    }

    // Check target membership
    let target = sqlx::query(
        "SELECT role, banned_at FROM channel_memberships WHERE channel_id = $1 AND user_id = $2",
    )
    .bind(channel_id)
    .bind(target_user_id)
    .fetch_optional(&mut *tx)
    .await
    .map_err(|e| ChannelError::DatabaseError(e.to_string()))?;

    let target = target.ok_or(ChannelError::TargetNotMember)?;
    let target_role_str: String = target.get("role");
    let target_role = parse_role(&target_role_str);
    let target_banned: Option<DateTime<Utc>> = target.get("banned_at");

    // Cannot ban owner
    if target_role == ChannelRole::Owner {
        return Err(ChannelError::CannotBanOwner);
    }
    // Admin cannot ban admin
    if req_role == ChannelRole::Admin && target_role == ChannelRole::Admin {
        return Err(ChannelError::InsufficientPrivileges);
    }
    // Already banned
    if target_banned.is_some() {
        return Err(ChannelError::AlreadyBanned);
    }

    // Ban the member
    let row = sqlx::query(
        "UPDATE channel_memberships SET banned_at = now() \
         WHERE channel_id = $1 AND user_id = $2 RETURNING banned_at",
    )
    .bind(channel_id)
    .bind(target_user_id)
    .fetch_one(&mut *tx)
    .await
    .map_err(|e| ChannelError::DatabaseError(e.to_string()))?;

    tx.commit()
        .await
        .map_err(|e| ChannelError::DatabaseError(e.to_string()))?;

    Ok(BanResult {
        channel_id,
        user_id: target_user_id,
        banned_at: row.get("banned_at"),
    })
}

/// Unban a member. Requires owner or admin.
pub async fn unban_member(
    pool: &PgPool,
    channel_id: Uuid,
    requester_user_id: Uuid,
    target_user_id: Uuid,
) -> Result<(), ChannelError> {
    let mut tx = pool
        .begin()
        .await
        .map_err(|e| ChannelError::DatabaseError(e.to_string()))?;

    // Check requester
    let requester = sqlx::query(
        "SELECT role, banned_at FROM channel_memberships WHERE channel_id = $1 AND user_id = $2",
    )
    .bind(channel_id)
    .bind(requester_user_id)
    .fetch_optional(&mut *tx)
    .await
    .map_err(|e| ChannelError::DatabaseError(e.to_string()))?;

    let requester = requester.ok_or(ChannelError::Forbidden)?;
    let req_banned: Option<DateTime<Utc>> = requester.get("banned_at");
    if req_banned.is_some() {
        return Err(ChannelError::Forbidden);
    }
    let req_role_str: String = requester.get("role");
    if parse_role(&req_role_str) == ChannelRole::Member {
        return Err(ChannelError::Forbidden);
    }

    // Check target
    let target = sqlx::query(
        "SELECT banned_at FROM channel_memberships WHERE channel_id = $1 AND user_id = $2",
    )
    .bind(channel_id)
    .bind(target_user_id)
    .fetch_optional(&mut *tx)
    .await
    .map_err(|e| ChannelError::DatabaseError(e.to_string()))?;

    let target = target.ok_or(ChannelError::TargetNotMember)?;
    let target_banned: Option<DateTime<Utc>> = target.get("banned_at");
    if target_banned.is_none() {
        return Err(ChannelError::TargetNotBanned);
    }

    sqlx::query(
        "UPDATE channel_memberships SET banned_at = NULL \
         WHERE channel_id = $1 AND user_id = $2",
    )
    .bind(channel_id)
    .bind(target_user_id)
    .execute(&mut *tx)
    .await
    .map_err(|e| ChannelError::DatabaseError(e.to_string()))?;

    tx.commit()
        .await
        .map_err(|e| ChannelError::DatabaseError(e.to_string()))?;

    Ok(())
}

/// List banned members for a channel. Requires owner or admin role.
pub async fn list_bans(
    pool: &PgPool,
    channel_id: Uuid,
    user_id: Uuid,
) -> Result<Vec<BannedMemberInfo>, ChannelError> {
    // 1. Check requester membership + role
    let membership = sqlx::query(
        "SELECT role, banned_at FROM channel_memberships \
         WHERE channel_id = $1 AND user_id = $2",
    )
    .bind(channel_id)
    .bind(user_id)
    .fetch_optional(pool)
    .await
    .map_err(|e| ChannelError::DatabaseError(e.to_string()))?;

    let membership = membership.ok_or(ChannelError::Forbidden)?;
    let banned_at: Option<DateTime<Utc>> = membership.get("banned_at");
    if banned_at.is_some() {
        return Err(ChannelError::Forbidden);
    }

    let role_str: String = membership.get("role");
    let role = parse_role(&role_str);
    if role == ChannelRole::Member {
        return Err(ChannelError::Forbidden);
    }

    // 2. Query banned members, ordered by banned_at DESC
    let rows = sqlx::query(
        "SELECT user_id, banned_at FROM channel_memberships \
         WHERE channel_id = $1 AND banned_at IS NOT NULL \
         ORDER BY banned_at DESC",
    )
    .bind(channel_id)
    .fetch_all(pool)
    .await
    .map_err(|e| ChannelError::DatabaseError(e.to_string()))?;

    Ok(rows
        .iter()
        .map(|row| BannedMemberInfo {
            user_id: row.get("user_id"),
            banned_at: row.get("banned_at"),
        })
        .collect())
}
/// List active (non-expired, non-exhausted) invites for a channel.
/// Requires owner or admin role.
pub async fn list_invites(
    pool: &PgPool,
    channel_id: Uuid,
    user_id: Uuid,
) -> Result<Vec<ChannelInvite>, ChannelError> {
    // 1. Check requester membership + role (same pattern as list_bans)
    let membership = sqlx::query(
        "SELECT role, banned_at FROM channel_memberships \
         WHERE channel_id = $1 AND user_id = $2",
    )
    .bind(channel_id)
    .bind(user_id)
    .fetch_optional(pool)
    .await
    .map_err(|e| ChannelError::DatabaseError(e.to_string()))?;

    let membership = membership.ok_or(ChannelError::Forbidden)?;
    let banned_at: Option<DateTime<Utc>> = membership.get("banned_at");
    if banned_at.is_some() {
        return Err(ChannelError::Forbidden);
    }

    let role_str: String = membership.get("role");
    let role = parse_role(&role_str);
    if role == ChannelRole::Member {
        return Err(ChannelError::Forbidden);
    }

    // 2. Query active invites (not expired, not exhausted)
    let rows = sqlx::query(
        "SELECT code, channel_id, expires_at, max_uses, uses, created_at \
         FROM channel_invites \
         WHERE channel_id = $1 \
           AND (expires_at IS NULL OR expires_at > now()) \
           AND (max_uses IS NULL OR uses < max_uses) \
         ORDER BY created_at DESC",
    )
    .bind(channel_id)
    .fetch_all(pool)
    .await
    .map_err(|e| ChannelError::DatabaseError(e.to_string()))?;

    Ok(rows
        .iter()
        .map(|row| ChannelInvite {
            code: row.get("code"),
            channel_id: row.get("channel_id"),
            expires_at: row.get("expires_at"),
            max_uses: row.get("max_uses"),
            uses: row.get("uses"),
            created_at: row.get("created_at"),
        })
        .collect())
}

/// Parse a role string for assignment. Only "admin" and "member" are valid targets.
/// Returns `Err(ChannelError::InvalidRole)` for "owner" or unrecognized strings.
pub fn parse_assignable_role(role: &str) -> Result<ChannelRole, ChannelError> {
    match role {
        "admin" => Ok(ChannelRole::Admin),
        "member" => Ok(ChannelRole::Member),
        _ => Err(ChannelError::InvalidRole),
    }
}

/// Change a member's role. Owner-only. Cannot set role to 'owner'.
/// Cannot change the owner's own role. Cannot change a banned member's role.
pub async fn change_role(
    pool: &PgPool,
    channel_id: Uuid,
    requester_user_id: Uuid,
    target_user_id: Uuid,
    new_role_str: &str,
) -> Result<RoleChangeResult, ChannelError> {
    let new_role = parse_assignable_role(new_role_str)?;

    let mut tx = pool
        .begin()
        .await
        .map_err(|e| ChannelError::DatabaseError(e.to_string()))?;

    // Check requester is owner
    let requester = sqlx::query(
        "SELECT role, banned_at FROM channel_memberships WHERE channel_id = $1 AND user_id = $2",
    )
    .bind(channel_id)
    .bind(requester_user_id)
    .fetch_optional(&mut *tx)
    .await
    .map_err(|e| ChannelError::DatabaseError(e.to_string()))?;

    let requester = requester.ok_or(ChannelError::Forbidden)?;
    let req_banned: Option<DateTime<Utc>> = requester.get("banned_at");
    if req_banned.is_some() {
        return Err(ChannelError::Forbidden);
    }
    let req_role_str: String = requester.get("role");
    if parse_role(&req_role_str) != ChannelRole::Owner {
        return Err(ChannelError::Forbidden);
    }

    // Cannot change owner's own role
    if requester_user_id == target_user_id {
        return Err(ChannelError::CannotChangeOwnerRole);
    }

    // Check target
    let target = sqlx::query(
        "SELECT role, banned_at FROM channel_memberships WHERE channel_id = $1 AND user_id = $2",
    )
    .bind(channel_id)
    .bind(target_user_id)
    .fetch_optional(&mut *tx)
    .await
    .map_err(|e| ChannelError::DatabaseError(e.to_string()))?;

    let target = target.ok_or(ChannelError::TargetNotMember)?;
    let target_role_str: String = target.get("role");
    let target_banned: Option<DateTime<Utc>> = target.get("banned_at");

    // Cannot change owner's role (defensive — target is somehow owner)
    if parse_role(&target_role_str) == ChannelRole::Owner {
        return Err(ChannelError::CannotChangeOwnerRole);
    }
    // Cannot change banned member's role
    if target_banned.is_some() {
        return Err(ChannelError::CannotChangeBannedRole);
    }

    let role_str = match new_role {
        ChannelRole::Admin => "admin",
        ChannelRole::Member => "member",
        ChannelRole::Owner => unreachable!(), // already checked above
    };

    sqlx::query(
        "UPDATE channel_memberships SET role = $1 \
         WHERE channel_id = $2 AND user_id = $3",
    )
    .bind(role_str)
    .bind(channel_id)
    .bind(target_user_id)
    .execute(&mut *tx)
    .await
    .map_err(|e| ChannelError::DatabaseError(e.to_string()))?;

    tx.commit()
        .await
        .map_err(|e| ChannelError::DatabaseError(e.to_string()))?;

    Ok(RoleChangeResult {
        channel_id,
        user_id: target_user_id,
        role: new_role,
    })
}

/// Delete expired channel invites. Called by background sweep.
/// Returns the number of deleted rows.
pub async fn sweep_expired_invites(pool: &PgPool) -> Result<u64, ChannelError> {
    let result = sqlx::query(
        "DELETE FROM channel_invites WHERE expires_at IS NOT NULL AND expires_at < now()",
    )
    .execute(pool)
    .await
    .map_err(|e| ChannelError::DatabaseError(e.to_string()))?;

    Ok(result.rows_affected())
}

/// Parse a role string from the DB into ChannelRole.
pub fn parse_role(role: &str) -> ChannelRole {
    match role {
        "owner" => ChannelRole::Owner,
        "admin" => ChannelRole::Admin,
        _ => ChannelRole::Member,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use base64::{Engine, engine::general_purpose::URL_SAFE_NO_PAD};
    use proptest::prelude::*;
    use std::collections::HashSet;

    // Feature: channel-membership, Property 3: Channel name validation
    // Validates: Requirements 1.4, 15.3
    proptest! {
        #![proptest_config(ProptestConfig::with_cases(1024))]

        #[test]
        fn prop_valid_names_accepted(name in ".{1,100}") {
            // Any non-empty string of 1-100 chars should be accepted
            prop_assert!(validate_channel_name(&name).is_ok());
        }

        #[test]
        fn prop_empty_name_rejected(name in Just(String::new())) {
            prop_assert!(validate_channel_name(&name).is_err());
        }

        #[test]
        fn prop_long_names_rejected(base in ".{101,200}") {
            prop_assert!(validate_channel_name(&base).is_err());
        }
    }

    // Feature: channel-membership, Property 8: Invite code entropy and format
    // Validates: Requirements 5.1, 5.7
    #[test]
    fn prop_invite_code_entropy_and_format() {
        let mut codes = HashSet::new();
        for _ in 0..1000 {
            let code = generate_invite_code();
            // Encoded length must be 22
            assert_eq!(code.len(), 22, "invite code must be 22 chars");
            // Decoded bytes must be 16
            let decoded = URL_SAFE_NO_PAD
                .decode(&code)
                .expect("must be valid base64url");
            assert_eq!(decoded.len(), 16, "decoded invite code must be 16 bytes");
            // No duplicates
            assert!(codes.insert(code), "duplicate invite code detected");
        }
    }
}
