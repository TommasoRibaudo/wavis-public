//! Channel domain types and error definitions.
//!
//! **Owns:** all structs and enums used by `domain::channel` — database-mapped
//! records (`Channel`, `ChannelMembership`, `ChannelInvite`), API response
//! types (`ChannelListItem`, `ChannelDetail`, `ChannelMemberInfo`), operation
//! results (`JoinResult`, `BanResult`, `RoleChangeResult`), and the
//! `ChannelError` enum.
//!
//! **Does not own:** business logic, database queries, or authorization checks
//! (those are in `domain::channel`).

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

/// Channel roles — independent from Room ParticipantRole.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, sqlx::Type)]
#[sqlx(type_name = "text", rename_all = "lowercase")]
pub enum ChannelRole {
    Owner,
    Admin,
    Member,
}

/// A channel record from the `channels` table.
#[derive(Debug, Clone)]
pub struct Channel {
    pub channel_id: Uuid,
    pub name: String,
    pub owner_user_id: Uuid,
    pub created_at: DateTime<Utc>,
}

/// A membership record from the `channel_memberships` table.
#[derive(Debug, Clone)]
#[allow(dead_code)]
pub struct ChannelMembership {
    pub channel_id: Uuid,
    pub user_id: Uuid,
    pub role: ChannelRole,
    pub banned_at: Option<DateTime<Utc>>,
    pub joined_at: DateTime<Utc>,
}

/// An invite record from the `channel_invites` table.
#[derive(Debug, Clone)]
#[allow(dead_code)]
pub struct ChannelInvite {
    pub code: String,
    pub channel_id: Uuid,
    pub expires_at: Option<DateTime<Utc>>,
    pub max_uses: Option<i32>,
    pub uses: i32,
    pub created_at: DateTime<Utc>,
}

/// Channel listing item (includes the requesting user's role).
#[derive(Debug, Clone, Serialize)]
pub struct ChannelListItem {
    pub channel_id: Uuid,
    pub name: String,
    pub owner_user_id: Uuid,
    pub created_at: DateTime<Utc>,
    pub role: ChannelRole,
}

/// Channel detail response (includes member list).
#[derive(Debug, Clone, Serialize)]
pub struct ChannelDetail {
    pub channel_id: Uuid,
    pub name: String,
    pub owner_user_id: Uuid,
    pub created_at: DateTime<Utc>,
    pub role: ChannelRole,
    pub members: Vec<ChannelMemberInfo>,
}

/// Member info within a channel detail response.
#[derive(Debug, Clone, Serialize)]
pub struct ChannelMemberInfo {
    pub user_id: Uuid,
    pub role: ChannelRole,
    pub joined_at: DateTime<Utc>,
    pub display_name: String,
}

/// Result of joining a channel via invite.
#[derive(Debug, Clone)]
pub struct JoinResult {
    pub channel_id: Uuid,
    pub name: String,
    pub role: ChannelRole,
}

/// Result of banning a member.
#[derive(Debug, Clone)]
pub struct BanResult {
    pub channel_id: Uuid,
    pub user_id: Uuid,
    pub banned_at: DateTime<Utc>,
}

/// Result of changing a member's role.
#[derive(Debug, Clone)]
pub struct RoleChangeResult {
    pub channel_id: Uuid,
    pub user_id: Uuid,
    pub role: ChannelRole,
}

/// Banned member info for the bans list endpoint.
#[derive(Debug, Clone, Serialize)]
pub struct BannedMemberInfo {
    pub user_id: Uuid,
    pub banned_at: DateTime<Utc>,
}

#[derive(Debug, thiserror::Error)]
pub enum ChannelError {
    #[error("invalid channel name")]
    InvalidName,
    #[error("channel not found")]
    ChannelNotFound,
    #[error("not a member")]
    NotMember,
    #[error("banned")]
    Banned,
    #[error("forbidden")]
    Forbidden,
    #[error("already a member")]
    AlreadyMember,
    #[error("invalid invite")]
    InvalidInvite,
    #[error("already banned")]
    AlreadyBanned,
    #[error("cannot ban owner")]
    CannotBanOwner,
    #[error("insufficient privileges")]
    InsufficientPrivileges,
    #[error("cannot ban yourself")]
    SelfBan,
    #[error("owner cannot leave")]
    OwnerCannotLeave,
    #[error("target not member")]
    TargetNotMember,
    #[error("target not banned")]
    TargetNotBanned,
    #[error("invalid role")]
    InvalidRole,
    #[error("cannot change owner role")]
    CannotChangeOwnerRole,
    #[error("cannot change banned member role")]
    CannotChangeBannedRole,
    #[error("invite not found")]
    InviteNotFound,
    #[error("owner consistency violation")]
    OwnerConsistencyViolation,
    #[error("database error: {0}")]
    DatabaseError(String),
}
