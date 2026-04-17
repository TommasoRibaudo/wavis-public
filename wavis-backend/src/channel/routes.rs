//! HTTP REST endpoints for channel lifecycle and membership management.
//!
//! **Owns:** request parsing, response formatting, and per-user rate limiting
//! for: channel CRUD, invite creation/revocation, join/leave, ban/unban,
//! role changes, voice status queries, and ban/invite listing.
//!
//! **Does not own:** channel business rules, membership policy, or voice
//! orchestration logic. All decisions are delegated to `domain::channel`
//! and `domain::voice_orchestrator`. This module never queries the database
//! directly.
//!
//! **Key invariants:**
//! - Every endpoint requires a valid [`AuthenticatedUser`].
//! - Channel rate limiting is checked before any domain call.
//! - Permission errors from domain are mapped to appropriate HTTP status
//!   codes (403 for forbidden, 404 for not-found, 409 for conflict).
//!
//! **Layering:** handlers → domain → state. This module dispatches to domain
//! functions and translates `ChannelError` variants into HTTP responses.

use axum::Json;
use axum::extract::{Path, State};
use axum::http::StatusCode;
use serde::{Deserialize, Serialize};
use std::time::Instant;
use uuid::Uuid;

use crate::app_state::AppState;
use crate::auth::extractor::AuthenticatedUser;
use crate::channel::channel;
use crate::channel::channel_models::{ChannelError, ChannelRole};
use crate::error::ErrorResponse;
use crate::voice::voice_orchestrator;
use crate::ws::ws_dispatch::{dispatch_signals, schedule_sub_room_expiry};

// ---------------------------------------------------------------------------
// Request / Response types
// ---------------------------------------------------------------------------

#[derive(Deserialize)]
pub struct CreateChannelRequest {
    pub name: String,
}

#[derive(Serialize)]
pub struct CreateChannelResponse {
    pub channel_id: String,
    pub name: String,
    pub owner_user_id: String,
    pub created_at: String,
}

#[derive(Serialize)]
pub struct ChannelListItemResponse {
    pub channel_id: String,
    pub name: String,
    pub owner_user_id: String,
    pub created_at: String,
    pub role: String,
}

#[derive(Serialize)]
pub struct ChannelDetailResponse {
    pub channel_id: String,
    pub name: String,
    pub owner_user_id: String,
    pub created_at: String,
    pub role: String,
    pub members: Vec<MemberResponse>,
}

#[derive(Serialize)]
pub struct MemberResponse {
    pub user_id: String,
    pub role: String,
    pub joined_at: String,
    pub display_name: String,
}

#[derive(Deserialize)]
pub struct CreateInviteRequest {
    pub expires_in_secs: Option<i64>,
    pub max_uses: Option<i32>,
}

#[derive(Serialize)]
pub struct InviteResponse {
    pub code: String,
    pub channel_id: String,
    pub expires_at: Option<String>,
    pub max_uses: Option<i32>,
    pub uses: i32,
}

#[derive(Deserialize)]
pub struct JoinChannelRequest {
    pub code: String,
}

#[derive(Serialize)]
pub struct JoinChannelResponse {
    pub channel_id: String,
    pub name: String,
    pub role: String,
}

#[derive(Serialize)]
pub struct BanResponse {
    pub channel_id: String,
    pub user_id: String,
    pub banned_at: String,
}

#[derive(Deserialize)]
pub struct ChangeRoleRequest {
    pub role: String,
}

#[derive(Serialize)]
pub struct ChangeRoleResponse {
    pub channel_id: String,
    pub user_id: String,
    pub role: String,
}

#[derive(Serialize)]
pub struct VoiceStatusResponse {
    pub active: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub participant_count: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub participants: Option<Vec<VoiceParticipantInfo>>,
}

#[derive(Serialize)]
pub struct VoiceParticipantInfo {
    pub display_name: String,
}

#[derive(Serialize)]
pub struct BanListResponse {
    pub banned: Vec<BanListItem>,
}

#[derive(Serialize)]
pub struct BanListItem {
    pub user_id: String,
    pub banned_at: String,
}

// ---------------------------------------------------------------------------
// Error mapping
// ---------------------------------------------------------------------------

fn role_to_string(role: ChannelRole) -> String {
    match role {
        ChannelRole::Owner => "owner".to_string(),
        ChannelRole::Admin => "admin".to_string(),
        ChannelRole::Member => "member".to_string(),
    }
}

fn map_channel_error(
    err: &ChannelError,
    user_id: Uuid,
    channel_id: Option<Uuid>,
) -> (StatusCode, Json<ErrorResponse>) {
    // Log at error level for internal failures, warn for everything else
    match err {
        ChannelError::DatabaseError(_) | ChannelError::OwnerConsistencyViolation => {
            tracing::error!(
                user_id = %user_id,
                channel_id = ?channel_id,
                error = %err,
                "channel operation failed"
            );
        }
        _ => {
            tracing::warn!(
                user_id = %user_id,
                channel_id = ?channel_id,
                error = %err,
                "channel operation rejected"
            );
        }
    }

    let (status, message) = match err {
        // Opaque errors — prevent information leakage
        ChannelError::ChannelNotFound => (StatusCode::NOT_FOUND, "not found"),
        ChannelError::NotMember => (StatusCode::FORBIDDEN, "forbidden"),
        ChannelError::Banned => (StatusCode::FORBIDDEN, "forbidden"),
        ChannelError::Forbidden => (StatusCode::FORBIDDEN, "forbidden"),
        ChannelError::TargetNotMember => (StatusCode::NOT_FOUND, "not found"),
        ChannelError::TargetNotBanned => (StatusCode::NOT_FOUND, "not found"),
        ChannelError::InviteNotFound => (StatusCode::NOT_FOUND, "not found"),
        ChannelError::InvalidInvite => (StatusCode::BAD_REQUEST, "invalid invite"),

        // Specific reasons — caller is confirmed member, no info leak
        ChannelError::InvalidName => (
            StatusCode::BAD_REQUEST,
            "invalid channel name: must be 1-100 characters",
        ),
        ChannelError::AlreadyMember => (StatusCode::CONFLICT, "already a member"),
        ChannelError::AlreadyBanned => (StatusCode::CONFLICT, "user is already banned"),
        ChannelError::CannotBanOwner => (StatusCode::FORBIDDEN, "cannot ban the channel owner"),
        ChannelError::InsufficientPrivileges => (StatusCode::FORBIDDEN, "insufficient privileges"),
        ChannelError::SelfBan => (StatusCode::BAD_REQUEST, "cannot ban yourself"),
        ChannelError::OwnerCannotLeave => (
            StatusCode::BAD_REQUEST,
            "owner cannot leave; delete the channel instead",
        ),
        ChannelError::InvalidRole => (
            StatusCode::BAD_REQUEST,
            "invalid role: must be 'admin' or 'member'",
        ),
        ChannelError::CannotChangeOwnerRole => {
            (StatusCode::BAD_REQUEST, "cannot change the owner's role")
        }
        ChannelError::CannotChangeBannedRole => (
            StatusCode::BAD_REQUEST,
            "cannot change a banned member's role",
        ),

        // Internal errors
        ChannelError::OwnerConsistencyViolation => {
            (StatusCode::INTERNAL_SERVER_ERROR, "internal error")
        }
        ChannelError::DatabaseError(_) => (StatusCode::INTERNAL_SERVER_ERROR, "internal error"),
    };

    (
        status,
        Json(ErrorResponse {
            error: message.to_string(),
        }),
    )
}

// ---------------------------------------------------------------------------
// Rate limiting helper
// ---------------------------------------------------------------------------

fn check_rate_limit(
    state: &AppState,
    user_id: Uuid,
) -> Result<(), (StatusCode, Json<ErrorResponse>)> {
    let now = Instant::now();
    if !state.channel_rate_limiter.check(user_id, now) {
        tracing::warn!(user_id = %user_id, "channel rate limit exceeded");
        return Err((
            StatusCode::TOO_MANY_REQUESTS,
            Json(ErrorResponse {
                error: "too many requests".to_string(),
            }),
        ));
    }
    state.channel_rate_limiter.record(user_id, now);
    Ok(())
}

// ---------------------------------------------------------------------------
// Handlers
// ---------------------------------------------------------------------------

/// POST /channels
pub async fn create_channel(
    State(state): State<AppState>,
    user: AuthenticatedUser,
    Json(body): Json<CreateChannelRequest>,
) -> Result<(StatusCode, Json<CreateChannelResponse>), (StatusCode, Json<ErrorResponse>)> {
    check_rate_limit(&state, user.user_id)?;

    let ch = channel::create_channel(&state.db_pool, user.user_id, &body.name)
        .await
        .map_err(|e| map_channel_error(&e, user.user_id, None))?;

    Ok((
        StatusCode::CREATED,
        Json(CreateChannelResponse {
            channel_id: ch.channel_id.to_string(),
            name: ch.name,
            owner_user_id: ch.owner_user_id.to_string(),
            created_at: ch.created_at.to_rfc3339(),
        }),
    ))
}

/// GET /channels
pub async fn list_channels(
    State(state): State<AppState>,
    user: AuthenticatedUser,
) -> Result<Json<Vec<ChannelListItemResponse>>, (StatusCode, Json<ErrorResponse>)> {
    let items = channel::list_channels(&state.db_pool, user.user_id)
        .await
        .map_err(|e| map_channel_error(&e, user.user_id, None))?;

    let response: Vec<ChannelListItemResponse> = items
        .into_iter()
        .map(|item| ChannelListItemResponse {
            channel_id: item.channel_id.to_string(),
            name: item.name,
            owner_user_id: item.owner_user_id.to_string(),
            created_at: item.created_at.to_rfc3339(),
            role: role_to_string(item.role),
        })
        .collect();

    Ok(Json(response))
}

/// GET /channels/:channel_id
pub async fn get_channel(
    State(state): State<AppState>,
    user: AuthenticatedUser,
    Path(channel_id): Path<Uuid>,
) -> Result<Json<ChannelDetailResponse>, (StatusCode, Json<ErrorResponse>)> {
    let detail = channel::get_channel_detail(&state.db_pool, channel_id, user.user_id)
        .await
        .map_err(|e| map_channel_error(&e, user.user_id, Some(channel_id)))?;

    let members: Vec<MemberResponse> = detail
        .members
        .into_iter()
        .map(|m| MemberResponse {
            user_id: m.user_id.to_string(),
            role: role_to_string(m.role),
            joined_at: m.joined_at.to_rfc3339(),
            display_name: m.display_name,
        })
        .collect();

    Ok(Json(ChannelDetailResponse {
        channel_id: detail.channel_id.to_string(),
        name: detail.name,
        owner_user_id: detail.owner_user_id.to_string(),
        created_at: detail.created_at.to_rfc3339(),
        role: role_to_string(detail.role),
        members,
    }))
}

/// DELETE /channels/:channel_id
pub async fn delete_channel(
    State(state): State<AppState>,
    user: AuthenticatedUser,
    Path(channel_id): Path<Uuid>,
) -> Result<StatusCode, (StatusCode, Json<ErrorResponse>)> {
    check_rate_limit(&state, user.user_id)?;

    channel::delete_channel(&state.db_pool, channel_id, user.user_id)
        .await
        .map_err(|e| map_channel_error(&e, user.user_id, Some(channel_id)))?;

    Ok(StatusCode::NO_CONTENT)
}

/// POST /channels/:channel_id/invites
pub async fn create_invite(
    State(state): State<AppState>,
    user: AuthenticatedUser,
    Path(channel_id): Path<Uuid>,
    Json(body): Json<CreateInviteRequest>,
) -> Result<(StatusCode, Json<InviteResponse>), (StatusCode, Json<ErrorResponse>)> {
    check_rate_limit(&state, user.user_id)?;

    let invite = channel::create_invite(
        &state.db_pool,
        channel_id,
        user.user_id,
        body.expires_in_secs,
        body.max_uses,
    )
    .await
    .map_err(|e| map_channel_error(&e, user.user_id, Some(channel_id)))?;

    Ok((
        StatusCode::CREATED,
        Json(InviteResponse {
            code: invite.code,
            channel_id: invite.channel_id.to_string(),
            expires_at: invite.expires_at.map(|t| t.to_rfc3339()),
            max_uses: invite.max_uses,
            uses: invite.uses,
        }),
    ))
}

/// POST /channels/join
pub async fn join_channel(
    State(state): State<AppState>,
    user: AuthenticatedUser,
    Json(body): Json<JoinChannelRequest>,
) -> Result<Json<JoinChannelResponse>, (StatusCode, Json<ErrorResponse>)> {
    check_rate_limit(&state, user.user_id)?;

    let result = channel::join_channel_by_invite(&state.db_pool, user.user_id, &body.code)
        .await
        .map_err(|e| map_channel_error(&e, user.user_id, None))?;

    Ok(Json(JoinChannelResponse {
        channel_id: result.channel_id.to_string(),
        name: result.name,
        role: role_to_string(result.role),
    }))
}

/// POST /channels/:channel_id/leave
pub async fn leave_channel(
    State(state): State<AppState>,
    user: AuthenticatedUser,
    Path(channel_id): Path<Uuid>,
) -> Result<StatusCode, (StatusCode, Json<ErrorResponse>)> {
    check_rate_limit(&state, user.user_id)?;

    channel::leave_channel(&state.db_pool, channel_id, user.user_id)
        .await
        .map_err(|e| map_channel_error(&e, user.user_id, Some(channel_id)))?;

    Ok(StatusCode::NO_CONTENT)
}

/// DELETE /channels/:channel_id/invites/:code
pub async fn revoke_invite(
    State(state): State<AppState>,
    user: AuthenticatedUser,
    Path((channel_id, code)): Path<(Uuid, String)>,
) -> Result<StatusCode, (StatusCode, Json<ErrorResponse>)> {
    check_rate_limit(&state, user.user_id)?;

    channel::revoke_invite(&state.db_pool, channel_id, user.user_id, &code)
        .await
        .map_err(|e| map_channel_error(&e, user.user_id, Some(channel_id)))?;

    Ok(StatusCode::NO_CONTENT)
}

/// POST /channels/:channel_id/bans/:user_id
pub async fn ban_member(
    State(state): State<AppState>,
    user: AuthenticatedUser,
    Path((channel_id, target_user_id)): Path<(Uuid, Uuid)>,
) -> Result<Json<BanResponse>, (StatusCode, Json<ErrorResponse>)> {
    check_rate_limit(&state, user.user_id)?;

    let result = channel::ban_member(&state.db_pool, channel_id, user.user_id, target_user_id)
        .await
        .map_err(|e| map_channel_error(&e, user.user_id, Some(channel_id)))?;

    // Voice eject (best-effort) — Req 6.1, 6.3, 6.4, 6.6
    // Ban is already persisted in DB. Eject from active voice session if present.
    if let Some((room_id, peer_id)) = voice_orchestrator::find_user_in_voice(
        &state.active_room_map,
        state.room_state.as_ref(),
        &channel_id,
        &target_user_id,
    )
    .await
    {
        match voice_orchestrator::eject_banned_user(
            state.room_state.as_ref(),
            &state.active_room_map,
            state.sfu_room_manager.as_ref(),
            &room_id,
            &peer_id,
            &channel_id,
        )
        .await
        {
            Ok(mut signals) => {
                let sub_room_result = voice_orchestrator::remove_participant_from_sub_room(
                    state.room_state.as_ref(),
                    &room_id,
                    &peer_id,
                );
                signals.extend(sub_room_result.signals);
                dispatch_signals(
                    signals,
                    &room_id,
                    state.room_state.as_ref(),
                    state.connections.as_ref(),
                );
                if let Some(expiry) = sub_room_result.expiry {
                    schedule_sub_room_expiry(&state, &room_id, &expiry.sub_room_id, expiry.delete_at);
                }
            }
            Err(e) => {
                tracing::error!(
                    channel_id = %channel_id,
                    target = %target_user_id,
                    error = %e,
                    "voice eject after ban failed"
                );
            }
        }
    }

    Ok(Json(BanResponse {
        channel_id: result.channel_id.to_string(),
        user_id: result.user_id.to_string(),
        banned_at: result.banned_at.to_rfc3339(),
    }))
}

/// DELETE /channels/:channel_id/bans/:user_id
pub async fn unban_member(
    State(state): State<AppState>,
    user: AuthenticatedUser,
    Path((channel_id, target_user_id)): Path<(Uuid, Uuid)>,
) -> Result<StatusCode, (StatusCode, Json<ErrorResponse>)> {
    check_rate_limit(&state, user.user_id)?;

    channel::unban_member(&state.db_pool, channel_id, user.user_id, target_user_id)
        .await
        .map_err(|e| map_channel_error(&e, user.user_id, Some(channel_id)))?;

    Ok(StatusCode::NO_CONTENT)
}

/// PUT /channels/:channel_id/members/:user_id/role
pub async fn change_role(
    State(state): State<AppState>,
    user: AuthenticatedUser,
    Path((channel_id, target_user_id)): Path<(Uuid, Uuid)>,
    Json(body): Json<ChangeRoleRequest>,
) -> Result<Json<ChangeRoleResponse>, (StatusCode, Json<ErrorResponse>)> {
    check_rate_limit(&state, user.user_id)?;

    let result = channel::change_role(
        &state.db_pool,
        channel_id,
        user.user_id,
        target_user_id,
        &body.role,
    )
    .await
    .map_err(|e| map_channel_error(&e, user.user_id, Some(channel_id)))?;

    Ok(Json(ChangeRoleResponse {
        channel_id: result.channel_id.to_string(),
        user_id: result.user_id.to_string(),
        role: role_to_string(result.role),
    }))
}

/// GET /channels/:channel_id/voice
/// Returns voice session status for a channel.
/// Requires authenticated, non-banned channel member.
/// Requirements: 9.1, 9.2, 9.3, 9.4, 9.5, 9.6
pub async fn get_voice_status(
    State(state): State<AppState>,
    user: AuthenticatedUser,
    Path(channel_id): Path<Uuid>,
) -> Result<Json<VoiceStatusResponse>, (StatusCode, Json<ErrorResponse>)> {
    // 1. Verify non-banned membership
    let membership = sqlx::query_as::<_, (String, Option<chrono::DateTime<chrono::Utc>>)>(
        "SELECT role, banned_at FROM channel_memberships WHERE channel_id = $1 AND user_id = $2",
    )
    .bind(channel_id)
    .bind(user.user_id)
    .fetch_optional(&state.db_pool)
    .await
    .map_err(|e| {
        tracing::error!(
            user_id = %user.user_id,
            channel_id = %channel_id,
            error = %e,
            "voice status DB error"
        );
        (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(ErrorResponse {
                error: "internal error".to_string(),
            }),
        )
    })?;

    match membership {
        // Not found → 403 opaque (indistinguishable from banned)
        None => {
            return Err((
                StatusCode::FORBIDDEN,
                Json(ErrorResponse {
                    error: "forbidden".to_string(),
                }),
            ));
        }
        // Banned → 403 opaque (indistinguishable from not found)
        Some((_, Some(_))) => {
            return Err((
                StatusCode::FORBIDDEN,
                Json(ErrorResponse {
                    error: "forbidden".to_string(),
                }),
            ));
        }
        // Non-banned member → proceed
        Some((_, None)) => {}
    }

    // 2. Check active room
    let map = state.active_room_map.read().await;
    match map.get(&channel_id) {
        Some(room_id) => {
            let room_id = room_id.clone();
            drop(map);

            // 3. Read participant list from room state.
            // Race note: the room may have been destroyed between the map read
            // and this room state read. If the room no longer exists, treat as
            // inactive (active: false) rather than returning stale data.
            match state.room_state.get_room_info(&room_id) {
                Some(room_info) => {
                    let participants: Vec<VoiceParticipantInfo> = room_info
                        .participants
                        .iter()
                        .map(|p| VoiceParticipantInfo {
                            display_name: p.display_name.clone(),
                        })
                        .collect();
                    Ok(Json(VoiceStatusResponse {
                        active: true,
                        participant_count: Some(participants.len() as u32),
                        participants: Some(participants),
                    }))
                }
                None => Ok(Json(VoiceStatusResponse {
                    active: false,
                    participant_count: None,
                    participants: None,
                })),
            }
        }
        None => {
            drop(map);
            Ok(Json(VoiceStatusResponse {
                active: false,
                participant_count: None,
                participants: None,
            }))
        }
    }
}

/// GET /channels/:channel_id/bans
pub async fn list_bans(
    State(state): State<AppState>,
    user: AuthenticatedUser,
    Path(channel_id): Path<Uuid>,
) -> Result<Json<BanListResponse>, (StatusCode, Json<ErrorResponse>)> {
    let bans = channel::list_bans(&state.db_pool, channel_id, user.user_id)
        .await
        .map_err(|e| map_channel_error(&e, user.user_id, Some(channel_id)))?;

    Ok(Json(BanListResponse {
        banned: bans
            .into_iter()
            .map(|b| BanListItem {
                user_id: b.user_id.to_string(),
                banned_at: b.banned_at.to_rfc3339(),
            })
            .collect(),
    }))
}
/// GET /channels/:channel_id/invites
pub async fn list_invites(
    State(state): State<AppState>,
    user: AuthenticatedUser,
    Path(channel_id): Path<Uuid>,
) -> Result<Json<Vec<InviteResponse>>, (StatusCode, Json<ErrorResponse>)> {
    let invites = channel::list_invites(&state.db_pool, channel_id, user.user_id)
        .await
        .map_err(|e| map_channel_error(&e, user.user_id, Some(channel_id)))?;

    let response: Vec<InviteResponse> = invites
        .into_iter()
        .map(|inv| InviteResponse {
            code: inv.code,
            channel_id: inv.channel_id.to_string(),
            expires_at: inv.expires_at.map(|t| t.to_rfc3339()),
            max_uses: inv.max_uses,
            uses: inv.uses,
        })
        .collect();

    Ok(Json(response))
}
