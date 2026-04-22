//! Voice channel join/leave orchestration.
//!
//! **Owns:** the bridge between channel membership (Postgres) and room
//! participation (in-memory state). Handles: channel-role to
//! participant-role mapping, active-room creation or reuse for a channel,
//! SFU room join with token issuance, stale-session eviction (same user
//! rejoining), voice leave, banned-user ejection, and voice-status queries.
//!
//! **Does not own:** channel CRUD or membership management (that is
//! `domain::channel`), low-level room mutations (that is
//! `domain::sfu_relay`), or WebSocket dispatch (that is `handlers::ws`).
//!
//! **Key invariants:**
//! - `active_room_map` lock (position 0) is acquired before room locks
//!   (position 1) — see lock ordering in `app_state.rs`.
//! - A channel has at most one active room at any time.
//! - Stale sessions (same user_id already in the room) are evicted
//!   atomically during join, returning the old peer_id for handler-level
//!   WebSocket cleanup.
//!
//! **Layering:** domain layer. Called by `handlers::channel_routes` and
//! `handlers::ws`. Depends on `state::InMemoryRoomState`,
//! `domain::sfu_relay`, `domain::sfu_bridge`, `domain::jwt`, and Postgres.

use crate::app_state::ActiveRoomMap;
use crate::auth::jwt::{sign_livekit_token, sign_media_token};
use crate::channel::channel_models::ChannelRole;
use crate::state::{
    InMemoryRoomState, PassthroughPair, RoomInfo, SubRoomInfo, SubRoomMembershipSource,
    SubRoomState,
};
use crate::voice::sfu_bridge::SfuRoomManager;
use crate::voice::sfu_relay::{OutboundSignal, ParticipantRole, TokenMode};
use chrono::{DateTime, Utc};
use rand::Rng;
use shared::signaling::{
    MediaTokenPayload, ParticipantInfo, ParticipantJoinedPayload, ParticipantLeftPayload,
    PassthroughStatePayload,
    RoomStatePayload, SignalingMessage, SubRoomCreatedPayload, SubRoomDeletedPayload,
    SubRoomInfoPayload, SubRoomJoinedPayload, SubRoomLeftPayload, SubRoomStatePayload,
    WireSubRoomMembershipSource,
};
use sqlx::PgPool;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};
use tracing::{error, warn};
use uuid::Uuid;

pub const DEFAULT_SUB_ROOM_ID: &str = "room-1";
pub const SUB_ROOM_DELETE_AFTER: Duration = Duration::from_secs(60);

/// Result of a successful voice join.
#[derive(Debug)]
pub struct VoiceJoinResult {
    pub room_id: String,
    pub participant_role: ParticipantRole,
    pub signals: Vec<OutboundSignal>,
    pub channel_id: String,
    /// If a stale session for the same user was evicted, contains the old peer_id.
    /// The handler should close the stale WebSocket connection after dispatching signals.
    pub evicted_peer_id: Option<String>,
    pub sub_room_expiry: Option<PendingSubRoomExpiry>,
}

#[derive(Debug, Clone)]
pub struct PendingSubRoomExpiry {
    pub sub_room_id: String,
    pub delete_at: Instant,
}

#[derive(Debug, Default)]
pub struct SubRoomActionResult {
    pub signals: Vec<OutboundSignal>,
    pub expiry: Option<PendingSubRoomExpiry>,
}

/// Errors from voice orchestration.
/// Wire-level: all channel-level rejections map to JoinRejectionReason::NotAuthorized.
/// Server logs carry the specific variant for operational debugging.
///
/// Note: `ChannelNotFound` is intentionally absent. The membership query
/// (SELECT FROM channel_memberships) returns None for both "channel doesn't exist"
/// and "not a member" — these are indistinguishable without an extra query.
/// Both cases log as "not_channel_member" and map to NotAuthorized on wire.
#[derive(Debug, thiserror::Error)]
pub enum VoiceJoinError {
    #[error("not authorized")]
    NotChannelMember,
    #[error("not authorized")]
    ChannelBanned,
    #[error("invalid channel ID format")]
    InvalidChannelId,
    #[error("room full")]
    RoomFull,
    #[error("database error: {0}")]
    DatabaseError(String),
    #[error("sfu error: {0}")]
    SfuError(String),
    #[error("internal error: {0}")]
    InternalError(String),
}

/// Map a Channel_Role to a Room ParticipantRole.
/// Owner/Admin → Host, Member → Guest.
pub fn map_channel_role(role: ChannelRole) -> ParticipantRole {
    match role {
        ChannelRole::Owner | ChannelRole::Admin => ParticipantRole::Host,
        ChannelRole::Member => ParticipantRole::Guest,
    }
}

/// Generate a 6-character alphanumeric suffix using CSPRNG.
fn generate_room_suffix() -> String {
    const CHARSET: &[u8] = b"abcdefghijklmnopqrstuvwxyz0123456789";
    let mut rng = rand::thread_rng();
    (0..6)
        .map(|_| {
            let idx = rng.gen_range(0..CHARSET.len());
            CHARSET[idx] as char
        })
        .collect()
}

fn wire_membership_source(source: SubRoomMembershipSource) -> WireSubRoomMembershipSource {
    match source {
        SubRoomMembershipSource::Explicit => WireSubRoomMembershipSource::Explicit,
        SubRoomMembershipSource::LegacyRoomOneFallback => WireSubRoomMembershipSource::LegacyRoomOne,
    }
}

fn delete_at_epoch_ms(delete_at: Instant) -> u64 {
    let now_instant = Instant::now();
    let now_epoch_ms = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as u64;
    let remaining_ms = delete_at
        .checked_duration_since(now_instant)
        .unwrap_or_default()
        .as_millis() as u64;
    now_epoch_ms.saturating_add(remaining_ms)
}

fn sub_room_info_payload(room: &SubRoomInfo) -> SubRoomInfoPayload {
    SubRoomInfoPayload {
        sub_room_id: room.sub_room_id.clone(),
        room_number: room.room_number,
        is_default: room.is_default,
        participant_ids: room.participant_ids.clone(),
        delete_at_ms: room.delete_at.map(delete_at_epoch_ms),
    }
}

fn passthrough_label(a: u32, b: u32) -> String {
    format!("{a} - {b}")
}

fn active_passthrough_payload(state: &SubRoomState) -> Option<PassthroughStatePayload> {
    let pair = state.active_passthrough.as_ref()?;
    let source_room = state
        .rooms
        .iter()
        .find(|room| room.sub_room_id == pair.source_sub_room_id)?;
    let target_room = state
        .rooms
        .iter()
        .find(|room| room.sub_room_id == pair.target_sub_room_id)?;
    Some(PassthroughStatePayload {
        source_sub_room_id: pair.source_sub_room_id.clone(),
        target_sub_room_id: pair.target_sub_room_id.clone(),
        label: passthrough_label(source_room.room_number, target_room.room_number),
    })
}

fn normalize_passthrough_pair(
    sub_rooms: &SubRoomState,
    source_sub_room_id: &str,
    target_sub_room_id: &str,
) -> Result<PassthroughPair, String> {
    if source_sub_room_id == target_sub_room_id {
        return Err("passthrough requires a different target room".to_string());
    }

    let source_room = sub_rooms
        .rooms
        .iter()
        .find(|room| room.sub_room_id == source_sub_room_id)
        .ok_or_else(|| "source sub-room not found".to_string())?;
    let target_room = sub_rooms
        .rooms
        .iter()
        .find(|room| room.sub_room_id == target_sub_room_id)
        .ok_or_else(|| "target sub-room not found".to_string())?;

    if source_room.room_number <= target_room.room_number {
        Ok(PassthroughPair {
            source_sub_room_id: source_room.sub_room_id.clone(),
            target_sub_room_id: target_room.sub_room_id.clone(),
        })
    } else {
        Ok(PassthroughPair {
            source_sub_room_id: target_room.sub_room_id.clone(),
            target_sub_room_id: source_room.sub_room_id.clone(),
        })
    }
}

fn clear_invalid_passthrough_locked(sub_rooms: &mut SubRoomState) {
    let Some(pair) = sub_rooms.active_passthrough.as_ref() else {
        return;
    };
    let has_source = sub_rooms
        .rooms
        .iter()
        .any(|room| room.sub_room_id == pair.source_sub_room_id);
    let has_target = sub_rooms
        .rooms
        .iter()
        .any(|room| room.sub_room_id == pair.target_sub_room_id);
    if !has_source || !has_target || pair.source_sub_room_id == pair.target_sub_room_id {
        sub_rooms.active_passthrough = None;
    }
}

fn sub_room_state_payload(state: &SubRoomState) -> SubRoomStatePayload {
    SubRoomStatePayload {
        rooms: state.rooms.iter().map(sub_room_info_payload).collect(),
        passthrough: active_passthrough_payload(state),
    }
}

fn sub_room_state_signals(room_state: &InMemoryRoomState, room_id: &str) -> Vec<OutboundSignal> {
    room_state
        .get_room_info(room_id)
        .and_then(|info| info.sub_room_state.as_ref().map(sub_room_state_payload))
        .map(|payload| vec![OutboundSignal::broadcast_all(SignalingMessage::SubRoomState(payload))])
        .unwrap_or_default()
}

fn ensure_room_sub_rooms(info: &mut RoomInfo) {
    if info.sub_room_state.is_none() {
        info.sub_room_state = Some(SubRoomState::new(DEFAULT_SUB_ROOM_ID.to_string()));
    }
}

fn schedule_empty_room_if_needed(
    sub_rooms: &mut SubRoomState,
    sub_room_id: &str,
) -> Option<PendingSubRoomExpiry> {
    let room = sub_rooms
        .rooms
        .iter_mut()
        .find(|room| room.sub_room_id == sub_room_id)?;
    if room.is_default || !room.participant_ids.is_empty() {
        room.delete_at = None;
        return None;
    }

    let delete_at = Instant::now() + SUB_ROOM_DELETE_AFTER;
    room.delete_at = Some(delete_at);
    Some(PendingSubRoomExpiry {
        sub_room_id: room.sub_room_id.clone(),
        delete_at,
    })
}

/// Ensure an active room exists for a channel. Atomic get-or-create.
///
/// Acquires `active_room_map` write lock, checks for existing entry,
/// creates room if absent (via `InMemoryRoomState::create_room` + `SfuRoomManager::create_room`),
/// inserts mapping. All under a single write lock hold to prevent duplicate room creation.
///
/// **Contention note (future optimization):** Holding the write lock across SFU
/// room creation blocks voice-status queries and JoinVoice calls for *other*
/// channels while the SFU/create path runs. For MVP (single-instance, fast
/// MockSfuBridge / LiveKit create_room), this is acceptable. If SFU latency
/// becomes a concern, the standard mitigation is:
///   1. Insert a placeholder "creating" entry (or per-channel Mutex) under lock 0
///   2. Release lock 0
///   3. Perform room creation
///   4. Reacquire lock 0 and finalize the mapping (or remove placeholder on failure)
///      This is explicitly out of scope for MVP.
///
/// Returns `(room_id, created)` where `created` is true if a new room was created.
pub async fn ensure_active_room(
    active_room_map: &ActiveRoomMap,
    room_state: &InMemoryRoomState,
    sfu_room_manager: &dyn SfuRoomManager,
    _token_mode: &TokenMode<'_>,
    _sfu_url: &str,
    channel_id: &Uuid,
    max_participants: u8,
) -> Result<(String, bool), VoiceJoinError> {
    let mut map = active_room_map.write().await;

    // If an active room already exists for this channel, verify it's still live
    // in InMemoryRoomState. If the room was auto-cleaned (all peers left) but the
    // map entry survived a cleanup race, evict the stale entry and fall through to
    // create a fresh room.
    if let Some(room_id) = map.get(channel_id) {
        if room_state.get_room_info(room_id).is_some() {
            return Ok((room_id.clone(), false));
        }
        // Stale mapping — room no longer exists in state. Remove and recreate.
        warn!(
            channel_id = %channel_id,
            stale_room_id = %room_id,
            "evicting stale active_room_map entry — room no longer in state"
        );
        map.remove(channel_id);
    }

    // Generate room_id: "channel-{channel_id}-{6-char alphanumeric suffix}"
    let suffix = generate_room_suffix();
    let room_id = format!("channel-{channel_id}-{suffix}");

    // Create SFU room handle via the bridge.
    let sfu_handle = sfu_room_manager
        .create_room(&room_id)
        .await
        .map_err(|e| VoiceJoinError::SfuError(e.to_string()))?;

    // Create room in InMemoryRoomState with SFU type.
    let mut info = RoomInfo::new_sfu(max_participants, sfu_handle);
    info.sub_room_state = Some(SubRoomState::new(DEFAULT_SUB_ROOM_ID.to_string()));
    let created = room_state.create_room(room_id.clone(), info);
    if !created {
        // Room already exists in state (should not happen under write lock, but guard defensively).
        return Err(VoiceJoinError::InternalError(
            "room already exists in state".to_string(),
        ));
    }

    // Insert channel_id → room_id mapping.
    map.insert(*channel_id, room_id.clone());

    // Write lock released here on drop.
    Ok((room_id, true))
}

/// Join voice in a channel. Full orchestration:
/// 1. UUID validation on channel_id
/// 2. DB membership check (non-banned member required)
/// 3. Role mapping (Channel_Role → ParticipantRole)
/// 4. ensure_active_room (atomic get-or-create)
/// 5. Add participant to room, sign token, build signals
///
/// Returns VoiceJoinResult with signals for the handler to dispatch.
/// On channel-level rejection, returns VoiceJoinError (mapped to NotAuthorized on wire).
///
/// Note: We replicate the join logic from handle_sfu_join directly rather than
/// delegating to it, because handle_sfu_join has first-joiner-is-Host logic and
/// invite validation that don't apply to channel-based voice.
/// Evict a stale session for the same user from a room.
///
/// When a user reconnects after an abrupt disconnect (network drop, crash, sleep),
/// the old WebSocket may not have been detected as dead yet. This function checks
/// whether the same `user_id` already has a participant entry in the room and, if so,
/// removes the stale session: SFU removal (best-effort), share cleanup, state removal,
/// and `ParticipantLeft` broadcast.
///
/// Returns `Ok(Some((evicted_peer_id, signals)))` if a stale session was evicted,
/// `Ok(None)` if no stale session existed.
///
/// This is NOT a kick — the evicted peer is NOT added to `revoked_participants`,
/// since the user is about to rejoin immediately.
async fn evict_stale_session(
    room_state: &InMemoryRoomState,
    sfu_room_manager: &dyn SfuRoomManager,
    room_id: &str,
    user_id: &Uuid,
) -> Result<Option<(String, Vec<OutboundSignal>, Option<PendingSubRoomExpiry>)>, VoiceJoinError> {
    let user_id_str = user_id.to_string();

    // Check if this user already has a participant in the room.
    let stale_peer_id = room_state.get_room_info(room_id).and_then(|info| {
        info.participants
            .iter()
            .find(|p| p.user_id.as_deref() == Some(&user_id_str))
            .map(|p| p.participant_id.clone())
    });

    let stale_peer_id = match stale_peer_id {
        Some(id) => id,
        None => return Ok(None),
    };

    warn!(
        user_id = %user_id,
        stale_peer_id = %stale_peer_id,
        room_id = %room_id,
        "evicting stale session for same user on rejoin"
    );

    // 1. Best-effort SFU removal.
    let sfu_handle = room_state
        .get_room_info(room_id)
        .and_then(|info| info.sfu_handle);
    if let Some(ref handle) = sfu_handle
        && let Err(e) = sfu_room_manager
            .remove_participant(handle, &stale_peer_id)
            .await
    {
        warn!(
            stale_peer_id = %stale_peer_id,
            error = %e,
            "SFU remove_participant failed during stale session eviction (best-effort)"
        );
    }

    // 2. Clean up any active screen share owned by the stale peer.
    let share_signals =
        super::screen_share::cleanup_share_on_disconnect(room_state, room_id, &stale_peer_id);

    // 3. Remove stale peer from state (peer list + peer_to_room reverse index).
    room_state.remove_peer_preserve_room(&stale_peer_id);

    // 4. Remove from participant info list.
    room_state.update_room_info(room_id, |info| {
        info.participants
            .retain(|p| p.participant_id != stale_peer_id);
    });

    // 5. Build eviction signals.
    let mut signals = vec![OutboundSignal::broadcast_except(
        &stale_peer_id,
        SignalingMessage::ParticipantLeft(ParticipantLeftPayload {
            participant_id: stale_peer_id.clone(),
        }),
    )];
    let sub_room_result = remove_participant_from_sub_room(room_state, room_id, &stale_peer_id);
    signals.extend(sub_room_result.signals);

    if let Some(share_sigs) = share_signals {
        signals.extend(share_sigs);
    }

    Ok(Some((stale_peer_id, signals, sub_room_result.expiry)))
}

pub fn sync_sub_room_state_on_voice_join(
    room_state: &InMemoryRoomState,
    room_id: &str,
    participant_id: &str,
    supports_sub_rooms: bool,
) -> SubRoomActionResult {
    let mut joined_event = None;

    room_state.update_room_info(room_id, |info| {
        ensure_room_sub_rooms(info);
        let Some(sub_rooms) = info.sub_room_state.as_mut() else {
            return;
        };
        clear_invalid_passthrough_locked(sub_rooms);

        if supports_sub_rooms {
            sub_rooms.participant_assignments.remove(participant_id);
            sub_rooms.membership_sources.remove(participant_id);
            for room in &mut sub_rooms.rooms {
                room.participant_ids.retain(|id| id != participant_id);
            }
            return;
        }

        let source = SubRoomMembershipSource::LegacyRoomOneFallback;
        let default_room = sub_rooms
            .rooms
            .iter_mut()
            .find(|room| room.is_default)
            .expect("sub-room state always contains ROOM 1");
        if !default_room.participant_ids.iter().any(|id| id == participant_id) {
            default_room.participant_ids.push(participant_id.to_string());
        }
        default_room.delete_at = None;
        sub_rooms
            .participant_assignments
            .insert(participant_id.to_string(), default_room.sub_room_id.clone());
        sub_rooms
            .membership_sources
            .insert(participant_id.to_string(), source);
        joined_event = Some(SignalingMessage::SubRoomJoined(SubRoomJoinedPayload {
            participant_id: participant_id.to_string(),
            sub_room_id: default_room.sub_room_id.clone(),
            source: wire_membership_source(source),
        }));
    });

    let mut signals = vec![];
    if let Some(event) = joined_event {
        signals.push(OutboundSignal::broadcast_all(event));
    }
    signals.extend(sub_room_state_signals(room_state, room_id));
    SubRoomActionResult {
        signals,
        expiry: None,
    }
}

pub fn create_sub_room(room_state: &InMemoryRoomState, room_id: &str) -> Result<SubRoomActionResult, String> {
    let mut created_room = None;
    let mut expiry = None;

    room_state.update_room_info(room_id, |info| {
        ensure_room_sub_rooms(info);
        let Some(sub_rooms) = info.sub_room_state.as_mut() else {
            return;
        };
        clear_invalid_passthrough_locked(sub_rooms);

        let next_room_number = sub_rooms
            .rooms
            .iter()
            .map(|room| room.room_number)
            .max()
            .unwrap_or(0)
            + 1;
        let sub_room_id = format!("room-{next_room_number}");
        let mut room = SubRoomInfo {
            sub_room_id,
            room_number: next_room_number,
            is_default: false,
            participant_ids: vec![],
            delete_at: None,
        };
        let delete_at = Instant::now() + SUB_ROOM_DELETE_AFTER;
        room.delete_at = Some(delete_at);
        expiry = Some(PendingSubRoomExpiry {
            sub_room_id: room.sub_room_id.clone(),
            delete_at,
        });
        created_room = Some(sub_room_info_payload(&room));
        sub_rooms.rooms.push(room);
    });

    let Some(created_room) = created_room else {
        return Err("sub-room state unavailable".to_string());
    };

    let mut signals = vec![OutboundSignal::broadcast_all(SignalingMessage::SubRoomCreated(
        SubRoomCreatedPayload { room: created_room },
    ))];
    signals.extend(sub_room_state_signals(room_state, room_id));
    Ok(SubRoomActionResult { signals, expiry })
}

pub fn join_sub_room(
    room_state: &InMemoryRoomState,
    voice_room_id: &str,
    sub_room_id: &str,
    participant_id: &str,
) -> Result<SubRoomActionResult, String> {
    let mut joined = None;
    let mut expiry = None;

    room_state.with_room_write(voice_room_id, |members| {
        ensure_room_sub_rooms(&mut members.info);
        let Some(sub_rooms) = members.info.sub_room_state.as_mut() else {
            return Err("sub-room state unavailable".to_string());
        };
        clear_invalid_passthrough_locked(sub_rooms);

        let target_room_id = sub_room_id.to_string();
        let target_idx = sub_rooms
            .rooms
            .iter()
            .position(|room| room.sub_room_id == target_room_id)
            .ok_or_else(|| "sub-room not found".to_string())?;

        let previous_room_id = sub_rooms.participant_assignments.get(participant_id).cloned();
        if previous_room_id.as_deref() == Some(target_room_id.as_str()) {
            return Ok(());
        }

        if let Some(ref old_room_id) = previous_room_id {
            if let Some(old_room) = sub_rooms
                .rooms
                .iter_mut()
                .find(|room| room.sub_room_id == *old_room_id)
            {
                old_room.participant_ids.retain(|id| id != participant_id);
            }
            expiry = schedule_empty_room_if_needed(sub_rooms, old_room_id);
        }

        let target_room = &mut sub_rooms.rooms[target_idx];
        if !target_room.participant_ids.iter().any(|id| id == participant_id) {
            target_room.participant_ids.push(participant_id.to_string());
        }
        target_room.delete_at = None;
        sub_rooms
            .participant_assignments
            .insert(participant_id.to_string(), target_room.sub_room_id.clone());
        sub_rooms
            .membership_sources
            .insert(participant_id.to_string(), SubRoomMembershipSource::Explicit);
        joined = Some(SignalingMessage::SubRoomJoined(SubRoomJoinedPayload {
            participant_id: participant_id.to_string(),
            sub_room_id: target_room.sub_room_id.clone(),
            source: WireSubRoomMembershipSource::Explicit,
        }));
        Ok(())
    })
    .map_err(|_| "voice session not found".to_string())??;

    let mut signals = vec![];
    if let Some(event) = joined {
        signals.push(OutboundSignal::broadcast_all(event));
    }
    signals.extend(sub_room_state_signals(room_state, voice_room_id));
    Ok(SubRoomActionResult { signals, expiry })
}

pub fn leave_sub_room(
    room_state: &InMemoryRoomState,
    room_id: &str,
    participant_id: &str,
) -> Result<SubRoomActionResult, String> {
    let mut left_sub_room_id = None;
    let mut expiry = None;

    room_state
        .with_room_write(room_id, |members| {
            let Some(sub_rooms) = members.info.sub_room_state.as_mut() else {
                return Ok::<(), String>(());
            };
            clear_invalid_passthrough_locked(sub_rooms);

            let Some(current_room_id) = sub_rooms.participant_assignments.remove(participant_id) else {
                sub_rooms.membership_sources.remove(participant_id);
                return Ok::<(), String>(());
            };
            sub_rooms.membership_sources.remove(participant_id);
            if let Some(current_room) = sub_rooms
                .rooms
                .iter_mut()
                .find(|room| room.sub_room_id == current_room_id)
            {
                current_room.participant_ids.retain(|id| id != participant_id);
            }
            expiry = schedule_empty_room_if_needed(sub_rooms, &current_room_id);
            left_sub_room_id = Some(current_room_id);
            Ok::<(), String>(())
        })
        .map_err(|_| "voice session not found".to_string())??;

    let mut signals = vec![];
    if let Some(sub_room_id) = left_sub_room_id {
        signals.push(OutboundSignal::broadcast_all(SignalingMessage::SubRoomLeft(
            SubRoomLeftPayload {
                participant_id: participant_id.to_string(),
                sub_room_id,
            },
        )));
    }
    signals.extend(sub_room_state_signals(room_state, room_id));
    Ok(SubRoomActionResult { signals, expiry })
}

pub fn remove_participant_from_sub_room(
    room_state: &InMemoryRoomState,
    room_id: &str,
    participant_id: &str,
) -> SubRoomActionResult {
    leave_sub_room(room_state, room_id, participant_id).unwrap_or_default()
}

pub fn set_passthrough(
    room_state: &InMemoryRoomState,
    room_id: &str,
    participant_id: &str,
    target_sub_room_id: &str,
) -> Result<SubRoomActionResult, String> {
    let changed = room_state
        .with_room_write(room_id, |members| {
            let Some(sub_rooms) = members.info.sub_room_state.as_mut() else {
                return Err("sub-room state unavailable".to_string());
            };
            clear_invalid_passthrough_locked(sub_rooms);

            let source_sub_room_id = sub_rooms
                .participant_assignments
                .get(participant_id)
                .cloned()
                .ok_or_else(|| "join a room to use passthrough".to_string())?;

            if let Some(active_pair) = sub_rooms.active_passthrough.as_ref() {
                let caller_involved = active_pair.source_sub_room_id == source_sub_room_id
                    || active_pair.target_sub_room_id == source_sub_room_id;
                if !caller_involved {
                    return Err("passthrough is controlled by the active room pair".to_string());
                }
            }

            let next_pair =
                normalize_passthrough_pair(sub_rooms, &source_sub_room_id, target_sub_room_id)?;
            let changed = sub_rooms.active_passthrough.as_ref() != Some(&next_pair);
            sub_rooms.active_passthrough = Some(next_pair);
            Ok(changed)
        })
        .map_err(|_| "voice session not found".to_string())??;

    if !changed {
        return Ok(SubRoomActionResult::default());
    }

    Ok(SubRoomActionResult {
        signals: sub_room_state_signals(room_state, room_id),
        expiry: None,
    })
}

pub fn clear_passthrough(
    room_state: &InMemoryRoomState,
    room_id: &str,
    participant_id: &str,
) -> Result<SubRoomActionResult, String> {
    let changed = room_state
        .with_room_write(room_id, |members| {
            let Some(sub_rooms) = members.info.sub_room_state.as_mut() else {
                return Err("sub-room state unavailable".to_string());
            };
            clear_invalid_passthrough_locked(sub_rooms);

            let source_sub_room_id = sub_rooms
                .participant_assignments
                .get(participant_id)
                .cloned()
                .ok_or_else(|| "join a room to use passthrough".to_string())?;
            let Some(active_pair) = sub_rooms.active_passthrough.as_ref() else {
                return Ok(false);
            };
            let caller_involved = active_pair.source_sub_room_id == source_sub_room_id
                || active_pair.target_sub_room_id == source_sub_room_id;
            if !caller_involved {
                return Err("passthrough is controlled by the active room pair".to_string());
            }

            sub_rooms.active_passthrough = None;
            Ok(true)
        })
        .map_err(|_| "voice session not found".to_string())??;

    if !changed {
        return Ok(SubRoomActionResult::default());
    }

    Ok(SubRoomActionResult {
        signals: sub_room_state_signals(room_state, room_id),
        expiry: None,
    })
}

pub fn expire_sub_room(
    room_state: &InMemoryRoomState,
    room_id: &str,
    sub_room_id: &str,
    expected_delete_at: Instant,
) -> SubRoomActionResult {
    let mut deleted = false;

    let _ = room_state.with_room_write(room_id, |members| {
        let Some(sub_rooms) = members.info.sub_room_state.as_mut() else {
            return;
        };
        clear_invalid_passthrough_locked(sub_rooms);
        let Some(idx) = sub_rooms
            .rooms
            .iter()
            .position(|room| room.sub_room_id == sub_room_id)
        else {
            return;
        };
        let room = &sub_rooms.rooms[idx];
        if room.is_default || !room.participant_ids.is_empty() || room.delete_at != Some(expected_delete_at) {
            return;
        }
        sub_rooms.rooms.remove(idx);
        clear_invalid_passthrough_locked(sub_rooms);
        deleted = true;
    });

    if !deleted {
        return SubRoomActionResult::default();
    }

    let mut signals = vec![OutboundSignal::broadcast_all(SignalingMessage::SubRoomDeleted(
        SubRoomDeletedPayload {
            sub_room_id: sub_room_id.to_string(),
        },
    ))];
    signals.extend(sub_room_state_signals(room_state, room_id));
    SubRoomActionResult {
        signals,
        expiry: None,
    }
}

#[allow(clippy::too_many_arguments)]
pub async fn join_voice(
    pool: &PgPool,
    room_state: &InMemoryRoomState,
    active_room_map: &ActiveRoomMap,
    sfu_room_manager: &dyn SfuRoomManager,
    token_mode: &TokenMode<'_>,
    sfu_url: &str,
    channel_id_str: &str,
    user_id: &Uuid,
    peer_id: &str,
    display_name: &str,
    profile_color: Option<&str>,
    supports_sub_rooms: bool,
    max_participants: u8,
) -> Result<VoiceJoinResult, VoiceJoinError> {
    // 1. Parse channel_id as UUID.
    let channel_id = Uuid::parse_str(channel_id_str).map_err(|_| {
        warn!(
            channel_id = %channel_id_str,
            reason = "invalid_channel_id",
            "voice join rejected"
        );
        VoiceJoinError::InvalidChannelId
    })?;

    // 2. DB membership check — must be non-banned member.
    let membership: Option<(ChannelRole, Option<DateTime<Utc>>)> = sqlx::query_as(
        "SELECT role, banned_at FROM channel_memberships WHERE channel_id = $1 AND user_id = $2",
    )
    .bind(channel_id)
    .bind(user_id)
    .fetch_optional(pool)
    .await
    .map_err(|e| {
        error!(
            user_id = %user_id,
            channel_id = %channel_id,
            error = %e,
            "voice join DB error during membership check"
        );
        VoiceJoinError::DatabaseError(e.to_string())
    })?;

    let (role, banned_at) = match membership {
        Some(row) => row,
        None => {
            warn!(
                user_id = %user_id,
                channel_id = %channel_id,
                reason = "not_channel_member",
                "voice join rejected"
            );
            return Err(VoiceJoinError::NotChannelMember);
        }
    };

    if banned_at.is_some() {
        warn!(
            user_id = %user_id,
            channel_id = %channel_id,
            reason = "channel_banned",
            "voice join rejected"
        );
        return Err(VoiceJoinError::ChannelBanned);
    }

    // 3. Map channel role → participant role.
    let participant_role = map_channel_role(role);

    // 4. Get or create active room for this channel.
    let (room_id, created) = ensure_active_room(
        active_room_map,
        room_state,
        sfu_room_manager,
        token_mode,
        sfu_url,
        &channel_id,
        max_participants,
    )
    .await?;

    // 5. Join user to room — replicate handle_sfu_join logic without
    //    first-joiner-is-Host and without invite validation.

    // 5a. Evict stale session for the same user (ghost duplicate prevention).
    // Must happen before capacity check so the freed slot is available.
    let (evicted_peer_id, eviction_signals, eviction_sub_room_expiry) =
        match evict_stale_session(room_state, sfu_room_manager, &room_id, user_id).await? {
            Some((peer_id_evicted, sigs, expiry)) => (Some(peer_id_evicted), sigs, expiry),
            None => (None, vec![], None),
        };

    // Get SFU handle from room state.
    let sfu_handle = room_state
        .get_room_info(&room_id)
        .and_then(|info| info.sfu_handle)
        .ok_or_else(|| VoiceJoinError::InternalError("room missing SFU handle".to_string()))?;

    // Register participant with SFU.
    if let Err(e) = sfu_room_manager.add_participant(&sfu_handle, peer_id).await {
        // Rollback if room was newly created.
        if created {
            rollback_room(room_state, active_room_map, &room_id, &channel_id).await;
        }
        return Err(VoiceJoinError::SfuError(e.to_string()));
    }

    // Snapshot existing participants BEFORE adding the new one.
    let existing_participants = room_state
        .get_room_info(&room_id)
        .map(|info| info.participants.clone())
        .unwrap_or_default();
    let is_late_joiner = !existing_participants.is_empty();

    // Atomic capacity check via try_add_peer_with (no invite for channel voice).
    let _count = match room_state.try_add_peer_with(peer_id.to_string(), &room_id, || Ok(())) {
        Ok(count) => count,
        Err(_) => {
            // Room full — rollback if newly created.
            if created {
                rollback_room(room_state, active_room_map, &room_id, &channel_id).await;
            }
            return Err(VoiceJoinError::RoomFull);
        }
    };

    // Update participant list and record token issuance.
    let new_participant = ParticipantInfo {
        participant_id: peer_id.to_string(),
        display_name: display_name.to_string(),
        user_id: Some(user_id.to_string()),
        profile_color: profile_color.map(|s| s.to_string()),
    };
    room_state.update_room_info(&room_id, |info| {
        info.participants.push(new_participant.clone());
        info.record_token_issued(peer_id);
    });

    let sub_room_sync = sync_sub_room_state_on_voice_join(
        room_state,
        &room_id,
        peer_id,
        supports_sub_rooms,
    );

    // Sign media token.
    let token = match token_mode {
        TokenMode::Custom {
            jwt_secret,
            issuer,
            ttl_secs,
        } => sign_media_token(&room_id, peer_id, jwt_secret, issuer, *ttl_secs)
            .map_err(|e| VoiceJoinError::SfuError(e.to_string()))?,
        TokenMode::LiveKit {
            api_key,
            api_secret,
            ttl_secs,
        } => sign_livekit_token(
            &room_id,
            peer_id,
            display_name,
            api_key,
            api_secret,
            *ttl_secs,
        )
        .map_err(|e| VoiceJoinError::SfuError(e.to_string()))?,
    };

    let peer_count = room_state.peer_count(&room_id) as u32;

    // Read current share permission to include in the Joined message.
    let share_permission = room_state
        .get_room_info(&room_id)
        .map(|info| shared::signaling::WireSharePermission::from(info.share_permission.clone()));

    // Build all participants list (existing + new joiner).
    let mut all_participants = existing_participants.clone();
    all_participants.push(new_participant);

    // Build outbound signals.
    let mut signals = Vec::new();

    // 1. Joined → joiner
    signals.push(OutboundSignal::to_peer(
        peer_id,
        SignalingMessage::Joined(shared::signaling::JoinedPayload {
            room_id: room_id.clone(),
            peer_id: peer_id.to_string(),
            peer_count,
            participants: all_participants,
            ice_config: None,
            share_permission,
        }),
    ));

    // 2. MediaToken → joiner
    signals.push(OutboundSignal::to_peer(
        peer_id,
        SignalingMessage::MediaToken(MediaTokenPayload {
            token,
            sfu_url: sfu_url.to_string(),
        }),
    ));

    // 3. RoomState → joiner (only if late joiner)
    if is_late_joiner {
        signals.push(OutboundSignal::to_peer(
            peer_id,
            SignalingMessage::RoomState(RoomStatePayload {
                participants: existing_participants,
            }),
        ));
    }

    // 4. ShareState → joiner (snapshot of active screen shares for consistent client init)
    signals.push(super::screen_share::share_state_snapshot(
        room_state, &room_id, peer_id,
    ));

    // 5. ParticipantJoined → broadcast to existing peers (exclude joiner)
    signals.push(OutboundSignal::broadcast_except(
        peer_id,
        SignalingMessage::ParticipantJoined(ParticipantJoinedPayload {
            participant_id: peer_id.to_string(),
            display_name: display_name.to_string(),
            user_id: Some(user_id.to_string()),
            profile_color: profile_color.map(|s| s.to_string()),
        }),
    ));

    Ok(VoiceJoinResult {
        room_id,
        participant_role,
        signals: {
            // Prepend eviction signals so ParticipantLeft for the ghost
            // is dispatched before ParticipantJoined for the new session.
            let mut all = eviction_signals;
            all.extend(signals);
            all.extend(sub_room_sync.signals);
            all
        },
        channel_id: channel_id.to_string(),
        evicted_peer_id,
        sub_room_expiry: eviction_sub_room_expiry,
    })
}

/// Look up a user's active voice session in a channel.
/// Used by the ban eject path and the voice query endpoint.
/// Returns `(room_id, participant_id)` if the user is in the channel's active room.
pub async fn find_user_in_voice(
    active_room_map: &ActiveRoomMap,
    room_state: &InMemoryRoomState,
    channel_id: &Uuid,
    user_id: &Uuid,
) -> Option<(String, String)> {
    // 1. Read active_room_map for channel_id → room_id.
    let room_id = {
        let map = active_room_map.read().await;
        map.get(channel_id)?.clone()
    };

    // 2. Get room info and scan participant list for matching user_id.
    let info = room_state.get_room_info(&room_id)?;
    let user_id_str = user_id.to_string();
    let participant = info
        .participants
        .iter()
        .find(|p| p.user_id.as_deref() == Some(&user_id_str))?;

    Some((room_id, participant.participant_id.clone()))
}

/// Eject a user from voice after a ban. Server-initiated kick.
///
/// Calls `handle_kick` with a synthetic server identity and Host role,
/// producing `ParticipantKicked` broadcast signals. Does NOT require the
/// banned peer to send a Leave message — removal is server-authoritative.
///
/// If the room becomes empty after eject, cleans up the `active_room_map` entry.
/// LOCK ORDERING: `handle_kick` releases all room locks before returning.
/// The `active_room_map` write lock is acquired only after `handle_kick` completes,
/// as an independent post-cleanup step. See design Property 9.
pub async fn eject_banned_user(
    room_state: &InMemoryRoomState,
    active_room_map: &ActiveRoomMap,
    sfu_room_manager: &dyn SfuRoomManager,
    room_id: &str,
    peer_id: &str,
    channel_id: &Uuid,
) -> Result<Vec<OutboundSignal>, VoiceJoinError> {
    use crate::voice::sfu_relay;

    // Server-initiated kick: use synthetic kicker identity with Host role.
    // handle_kick validates Host role, removes peer from SFU (best-effort),
    // removes from state, adds to revoked_participants, broadcasts ParticipantKicked.
    let signals = sfu_relay::handle_kick(
        sfu_room_manager,
        room_state,
        room_id,
        "__server__",
        ParticipantRole::Host,
        peer_id,
    )
    .await
    .map_err(|e| VoiceJoinError::InternalError(e.to_string()))?;

    // Check if room is now empty after the kick.
    // All room-level locks (rooms, per-room, peer_to_room) are released at this point.
    let remaining = room_state.peer_count(room_id);

    if remaining == 0 {
        // Room is empty — destroy SFU room (best-effort) and clean up active_room_map.
        let sfu_handle = room_state
            .get_room_info(room_id)
            .and_then(|info| info.sfu_handle);
        if let Some(ref handle) = sfu_handle {
            let _ = sfu_room_manager.destroy_room(handle).await;
        }

        // Remove the now-empty room from in-memory state.
        room_state.remove_empty_room(room_id);

        // LOCK ORDERING: All room locks (rooms, per-room, peer_to_room) are released above.
        // active_room_map write lock acquired here as an independent post-cleanup step.
        // See design Property 9.
        let mut map = active_room_map.write().await;
        // Only remove if the mapping still points to this room
        // (guards against a race where a new room was already created).
        if map.get(channel_id).map(|r| r.as_str()) == Some(room_id) {
            map.remove(channel_id);
        }
    }

    Ok(signals)
}

/// Rollback a newly created room: remove from InMemoryRoomState and active_room_map.
/// Called when join fails after room creation (e.g., SFU add_participant error, room full).
async fn rollback_room(
    room_state: &InMemoryRoomState,
    active_room_map: &ActiveRoomMap,
    room_id: &str,
    channel_id: &Uuid,
) {
    // Remove the empty room from state.
    room_state.remove_empty_room(room_id);
    // Remove the active_room_map entry.
    let mut map = active_room_map.write().await;
    if map.get(channel_id).map(|r| r.as_str()) == Some(room_id) {
        map.remove(channel_id);
    }
}
/// Re-query a user's current Channel_Role from the database.
/// Used for lazy role enforcement on moderation actions in channel-based sessions.
/// Returns `Ok(Some(ChannelRole))` if the user is a non-banned member,
/// `Ok(None)` if not found or banned.
pub async fn get_current_channel_role(
    pool: &PgPool,
    channel_id: &str,
    user_id: &Uuid,
) -> Result<Option<ChannelRole>, VoiceJoinError> {
    // Parse channel_id as UUID — this should never fail since it was validated at join time.
    let channel_uuid = Uuid::parse_str(channel_id)
        .map_err(|_| VoiceJoinError::InternalError("invalid channel_id in session".to_string()))?;

    let role: Option<ChannelRole> = sqlx::query_scalar(
        "SELECT role FROM channel_memberships WHERE channel_id = $1 AND user_id = $2 AND banned_at IS NULL",
    )
    .bind(channel_uuid)
    .bind(user_id)
    .fetch_optional(pool)
    .await
    .map_err(|e| {
        error!(
            user_id = %user_id,
            channel_id = %channel_id,
            error = %e,
            "failed to query channel role for lazy enforcement"
        );
        VoiceJoinError::DatabaseError(e.to_string())
    })?;

    Ok(role)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::voice::sfu_bridge::SfuRoomHandle;
    use proptest::prelude::*;

    #[test]
    fn owner_maps_to_host() {
        assert_eq!(map_channel_role(ChannelRole::Owner), ParticipantRole::Host);
    }

    #[test]
    fn admin_maps_to_host() {
        assert_eq!(map_channel_role(ChannelRole::Admin), ParticipantRole::Host);
    }

    #[test]
    fn member_maps_to_guest() {
        assert_eq!(
            map_channel_role(ChannelRole::Member),
            ParticipantRole::Guest
        );
    }

    fn sub_room_test_state() -> InMemoryRoomState {
        let state = InMemoryRoomState::new();
        let mut info = RoomInfo::new_sfu(6, SfuRoomHandle("handle-1".to_string()));
        info.sub_room_state = Some(SubRoomState::new(DEFAULT_SUB_ROOM_ID.to_string()));
        info.participants = vec![
            ParticipantInfo {
                participant_id: "peer-a".to_string(),
                display_name: "Peer A".to_string(),
                user_id: None,
                profile_color: None,
            },
            ParticipantInfo {
                participant_id: "peer-b".to_string(),
                display_name: "Peer B".to_string(),
                user_id: None,
                profile_color: None,
            },
        ];
        state.create_room("voice-room".to_string(), info);
        state.add_peer("peer-a".to_string(), "voice-room".to_string());
        state.add_peer("peer-b".to_string(), "voice-room".to_string());
        state
    }

    #[test]
    fn legacy_join_assigns_room_one() {
        let state = sub_room_test_state();

        let result = sync_sub_room_state_on_voice_join(&state, "voice-room", "peer-a", false);

        assert!(result.expiry.is_none());
        let info = state.get_room_info("voice-room").expect("room exists");
        let sub_rooms = info.sub_room_state.expect("sub rooms");
        assert_eq!(
            sub_rooms.participant_assignments.get("peer-a"),
            Some(&DEFAULT_SUB_ROOM_ID.to_string())
        );
        assert_eq!(
            sub_rooms.membership_sources.get("peer-a"),
            Some(&SubRoomMembershipSource::LegacyRoomOneFallback)
        );
        assert_eq!(result.signals.len(), 2);
    }

    #[test]
    fn create_and_expire_non_default_sub_room() {
        let state = sub_room_test_state();

        let created = create_sub_room(&state, "voice-room").expect("create sub room");
        let expiry = created.expiry.expect("expiry scheduled");
        assert_eq!(expiry.sub_room_id, "room-2");

        let expired = expire_sub_room(&state, "voice-room", &expiry.sub_room_id, expiry.delete_at);
        let info = state.get_room_info("voice-room").expect("room exists");
        let sub_rooms = info.sub_room_state.expect("sub rooms");

        assert!(
            sub_rooms
                .rooms
                .iter()
                .all(|room| room.sub_room_id != expiry.sub_room_id)
        );
        assert_eq!(expired.signals.len(), 2);
    }

    #[test]
    fn leaving_last_member_schedules_non_default_room_deletion() {
        let state = sub_room_test_state();
        let _ = create_sub_room(&state, "voice-room").expect("create sub room");
        let _ = join_sub_room(&state, "voice-room", "room-2", "peer-a");

        let result = leave_sub_room(&state, "voice-room", "peer-a").expect("leave sub room");

        assert!(result.expiry.is_some());
        let info = state.get_room_info("voice-room").expect("room exists");
        let sub_rooms = info.sub_room_state.expect("sub rooms");
        assert!(!sub_rooms.participant_assignments.contains_key("peer-a"));
        let room = sub_rooms
            .rooms
            .iter()
            .find(|room| room.sub_room_id == "room-2")
            .expect("room-2 exists");
        assert!(room.delete_at.is_some());
    }

    fn arb_channel_role() -> impl Strategy<Value = ChannelRole> {
        prop_oneof![
            Just(ChannelRole::Owner),
            Just(ChannelRole::Admin),
            Just(ChannelRole::Member),
        ]
    }

    // Feature: channel-voice-orchestration, Property 1: Channel_Role → ParticipantRole mapping correctness
    // Validates: Requirements 4.1
    proptest! {
        #![proptest_config(ProptestConfig::with_cases(256))]
        #[test]
        fn prop_channel_role_mapping_correctness(role in arb_channel_role()) {
            let result = map_channel_role(role);
            match role {
                ChannelRole::Owner => prop_assert_eq!(result, ParticipantRole::Host),
                ChannelRole::Admin => prop_assert_eq!(result, ParticipantRole::Host),
                ChannelRole::Member => prop_assert_eq!(result, ParticipantRole::Guest),
            }
        }
    }

    fn arb_uuid_bytes() -> impl Strategy<Value = [u8; 16]> {
        proptest::array::uniform16(proptest::num::u8::ANY)
    }

    // Feature: channel-voice-orchestration, Property 13: Room ID generation format
    // Validates: Requirements 1.4
    proptest! {
        #![proptest_config(ProptestConfig::with_cases(256))]
        #[test]
        fn prop_room_id_generation_format(uuid_bytes in arb_uuid_bytes()) {
            let channel_id = Uuid::from_bytes(uuid_bytes);
            let suffix = generate_room_suffix();
            let room_id = format!("channel-{channel_id}-{suffix}");

            // Assert format: "channel-{uuid}-{6 alphanumeric chars}"
            let expected_prefix = format!("channel-{channel_id}-");
            prop_assert!(room_id.starts_with(&expected_prefix),
                "room_id should start with 'channel-{{uuid}}-', got: {}", room_id);

            let suffix_part = &room_id[expected_prefix.len()..];
            prop_assert_eq!(suffix_part.len(), 6,
                "suffix should be 6 chars, got: {} ('{}')", suffix_part.len(), suffix_part);
            prop_assert!(suffix_part.chars().all(|c| c.is_ascii_alphanumeric()),
                "suffix should be alphanumeric, got: '{}'", suffix_part);

            // Generate 100 room IDs and assert no duplicates
            let mut ids = std::collections::HashSet::new();
            for _ in 0..100 {
                let s = generate_room_suffix();
                let id = format!("channel-{channel_id}-{s}");
                ids.insert(id);
            }
            prop_assert_eq!(ids.len(), 100,
                "100 generated room IDs should all be unique, got {} unique", ids.len());
        }
    }
}
