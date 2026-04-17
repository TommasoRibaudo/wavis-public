//! Screen share session lifecycle: start, stop, permission, and cleanup.
//!
//! **Owns:** the business logic for multi-share screen sharing in SFU rooms.
//! Handles: starting a share (with role-based permission checks), stopping
//! individual or all shares, changing share permissions per-room, and
//! cleaning up shares on disconnect. Produces `OutboundSignal` values for
//! handler dispatch.
//!
//! **Does not own:** WebSocket framing or message dispatch (that is
//! `handlers::ws`), SFU media-track management, or room lifecycle.
//!
//! **Key invariants:**
//! - All mutations acquire the room write lock atomically — precondition
//!   checks (membership, room type, permission policy, active-share status)
//!   and state updates happen in a single critical section to prevent
//!   TOCTOU races.
//! - Screen sharing is SFU-only; P2P rooms reject share requests.
//! - Share permission changes apply to future share attempts; existing
//!   active shares are not retroactively stopped.
//!
//! **Platform strategy (§9.3):** this module is entirely platform-agnostic.
//! It tracks share state and produces signaling messages — it does not
//! perform screen capture. Platform-specific capture logic lives on the
//! client side (`wavis-gui/src-tauri/src/share_sources.rs`), behind
//! `#[cfg(target_os)]` gates. No caller of this module branches on platform.
//!
//! **Layering:** domain layer. Called by `handlers::ws`. Depends on
//! `state::InMemoryRoomState` for room data and `domain::sfu_relay` for
//! signal types.

use crate::state::{InMemoryRoomState, RoomType, SharePermission};
use crate::voice::sfu_relay::{OutboundSignal, ParticipantRole};
use shared::signaling::{
    ErrorPayload, SharePermissionChangedPayload, ShareStartedPayload, ShareStatePayload,
    ShareStoppedPayload, SignalingMessage,
};

/// Look up a participant's display name from the room's participant list.
/// Falls back to the participant_id if not found.
fn lookup_display_name(
    participants: &[shared::signaling::ParticipantInfo],
    participant_id: &str,
) -> String {
    participants
        .iter()
        .find(|p| p.participant_id == participant_id)
        .map(|p| p.display_name.clone())
        .unwrap_or_else(|| participant_id.to_string())
}

/// Unified result type for share operations (replaces StartShareResult and StopShareResult).
pub enum ShareResult {
    /// Operation succeeded, dispatch these signals.
    Ok(Vec<OutboundSignal>),
    /// Operation was idempotent (no-op), no signals to dispatch.
    Noop,
    /// Error to send back to the requesting peer.
    Error(SignalingMessage),
}

/// Attempt to start a screen share in `room_id` on behalf of `sender_id`.
///
/// Atomically checks all preconditions under the room write lock (TOCTOU prevention):
/// - Sender is a member of the room
/// - Room type is SFU (screen sharing is SFU-only)
/// - Share permission policy allows this role to share
/// - Sender is not already sharing
///
/// On success, inserts sender into `active_shares` and returns a `BroadcastAll` `ShareStarted` signal.
pub fn handle_start_share(
    state: &InMemoryRoomState,
    room_id: &str,
    sender_id: &str,
    role: ParticipantRole,
) -> ShareResult {
    let result = state.with_room_write(room_id, |members| {
        // Check room type
        if members.info.room_type != RoomType::Sfu {
            return ShareResult::Error(SignalingMessage::Error(ErrorPayload {
                message: "screen sharing unavailable in P2P mode".to_string(),
            }));
        }

        // Future-proofing: enforce share permission policy
        if members.info.share_permission == SharePermission::HostOnly
            && role != ParticipantRole::Host
        {
            return ShareResult::Error(SignalingMessage::Error(ErrorPayload {
                message: "permission denied: only host can share in this room".to_string(),
            }));
        }

        // Check sender is a member
        if !members.peers.contains(&sender_id.to_string()) {
            return ShareResult::Error(SignalingMessage::Error(ErrorPayload {
                message: "not in room".to_string(),
            }));
        }

        // Check sender not already sharing
        if members.info.active_shares.contains(sender_id) {
            return ShareResult::Noop;
        }

        // All preconditions pass — atomically insert into active_shares
        members.info.active_shares.insert(sender_id.to_string());

        let display_name = lookup_display_name(&members.info.participants, sender_id);
        let signal =
            OutboundSignal::broadcast_all(SignalingMessage::ShareStarted(ShareStartedPayload {
                participant_id: sender_id.to_string(),
                display_name,
            }));
        ShareResult::Ok(vec![signal])
    });

    match result {
        Ok(r) => r,
        Err(_room_not_found) => ShareResult::Error(SignalingMessage::Error(ErrorPayload {
            message: "not in room".to_string(),
        })),
    }
}

/// Attempt to stop the active screen share in `room_id`.
///
/// Allowed when:
/// - `sender_id` is in `active_shares` (self-stop), OR
/// - `sender_role` is `Host` and there is at least one active share (host override)
///
/// Idempotent: returns `NoOp` if sender is not sharing or has no authority.
/// Stop a screen share in `room_id`.
///
/// Self-stop: `target_participant_id` is `None` → stops the caller's own share.
/// Host-directed stop: `target_participant_id` is `Some(target)` where target != peer_id
///   → requires `role == Host`, otherwise returns permission error.
///
/// Atomically checks all preconditions under the room write lock (TOCTOU prevention):
/// - Room type is SFU (screen sharing is SFU-only)
/// - Target is still a participant (guards against disconnect race)
/// - Target is in `active_shares`
///
/// On success, removes target from `active_shares` and returns a `BroadcastAll` `ShareStopped` signal.
pub fn handle_stop_share(
    state: &InMemoryRoomState,
    room_id: &str,
    peer_id: &str,
    target_participant_id: Option<&str>,
    role: ParticipantRole,
) -> ShareResult {
    let effective_target = target_participant_id.unwrap_or(peer_id);

    // Permission check: only host can stop another participant's share
    if effective_target != peer_id && role != ParticipantRole::Host {
        return ShareResult::Error(SignalingMessage::Error(ErrorPayload {
            message: "permission denied: only host can stop another participant's share"
                .to_string(),
        }));
    }

    let result = state.with_room_write(room_id, |members| {
        // Check room type
        if members.info.room_type != RoomType::Sfu {
            return ShareResult::Error(SignalingMessage::Error(ErrorPayload {
                message: "screen sharing unavailable in P2P mode".to_string(),
            }));
        }

        // Re-verify target is still a participant (guards against disconnect race)
        if !members.peers.contains(&effective_target.to_string()) {
            return ShareResult::Noop;
        }

        // Check target is in active_shares
        if !members.info.active_shares.contains(effective_target) {
            return ShareResult::Noop;
        }

        // All preconditions pass — atomically remove from active_shares
        members.info.active_shares.remove(effective_target);

        let display_name = lookup_display_name(&members.info.participants, effective_target);
        let signal =
            OutboundSignal::broadcast_all(SignalingMessage::ShareStopped(ShareStoppedPayload {
                participant_id: effective_target.to_string(),
                display_name,
            }));
        ShareResult::Ok(vec![signal])
    });

    match result {
        Ok(r) => r,
        Err(_room_not_found) => ShareResult::Noop,
    }
}

/// Host stops all active screen shares in `room_id`.
///
/// Requires `role == ParticipantRole::Host` — returns permission error otherwise.
/// If no shares are active, returns `Noop`.
///
/// Atomically clears `active_shares` under the room write lock and returns
/// one `BroadcastAll` `ShareStopped` signal per removed sharer.
pub fn handle_stop_all_shares(
    state: &InMemoryRoomState,
    room_id: &str,
    _peer_id: &str,
    role: ParticipantRole,
) -> ShareResult {
    // Permission check: only host can stop all shares
    if role != ParticipantRole::Host {
        return ShareResult::Error(SignalingMessage::Error(ErrorPayload {
            message: "permission denied: only host can stop all shares".to_string(),
        }));
    }

    let result = state.with_room_write(room_id, |members| {
        // Check room type
        if members.info.room_type != RoomType::Sfu {
            return ShareResult::Error(SignalingMessage::Error(ErrorPayload {
                message: "screen sharing unavailable in P2P mode".to_string(),
            }));
        }

        // If no active shares, nothing to do
        if members.info.active_shares.is_empty() {
            return ShareResult::Noop;
        }

        // Collect all current sharers, then clear
        let sharers: Vec<String> = members.info.active_shares.drain().collect();

        let signals = sharers
            .into_iter()
            .map(|participant_id| {
                let display_name = lookup_display_name(&members.info.participants, &participant_id);
                OutboundSignal::broadcast_all(SignalingMessage::ShareStopped(ShareStoppedPayload {
                    participant_id,
                    display_name,
                }))
            })
            .collect();

        ShareResult::Ok(signals)
    });

    match result {
        Ok(r) => r,
        Err(_room_not_found) => ShareResult::Noop,
    }
}

/// Build a `ShareState` snapshot signal targeted to a specific peer.
///
/// Returns an `OutboundSignal` addressed to `target_peer_id` containing the
/// current `active_shares` set as a `ShareState` message. If the room does
/// not exist, returns an empty snapshot.
pub fn share_state_snapshot(
    state: &InMemoryRoomState,
    room_id: &str,
    target_peer_id: &str,
) -> OutboundSignal {
    let participant_ids: Vec<String> = state
        .get_room_info(room_id)
        .map(|info| info.active_shares.iter().cloned().collect())
        .unwrap_or_default();

    OutboundSignal::to_peer(
        target_peer_id,
        SignalingMessage::ShareState(ShareStatePayload { participant_ids }),
    )
}

/// Clean up a screen share when `peer_id` disconnects.
///
/// If `peer_id` is in `active_shares`, removes them and returns
/// a `BroadcastAll` `ShareStopped` signal. Otherwise returns `None`.
pub fn cleanup_share_on_disconnect(
    state: &InMemoryRoomState,
    room_id: &str,
    peer_id: &str,
) -> Option<Vec<OutboundSignal>> {
    let result = state.with_room_write(room_id, |members| {
        if !members.info.active_shares.remove(peer_id) {
            return None;
        }

        let display_name = lookup_display_name(&members.info.participants, peer_id);
        let signal =
            OutboundSignal::broadcast_all(SignalingMessage::ShareStopped(ShareStoppedPayload {
                participant_id: peer_id.to_string(),
                display_name,
            }));
        Some(vec![signal])
    });

    result.ok().flatten()
}

/// Change the share permission policy for a room.
///
/// Host-only action. Validates the permission, updates state atomically,
/// and broadcasts `SharePermissionChanged` to all participants.
/// Returns `Noop` if the permission is already set to the requested value.
pub fn handle_set_share_permission(
    state: &InMemoryRoomState,
    room_id: &str,
    sender_id: &str,
    role: ParticipantRole,
    permission: shared::signaling::WireSharePermission,
) -> ShareResult {
    if role != ParticipantRole::Host {
        return ShareResult::Error(SignalingMessage::Error(ErrorPayload {
            message: "unauthorized".to_string(),
        }));
    }

    let new_perm: SharePermission = permission.into();

    let result = state.with_room_write(room_id, |members| {
        if !members.peers.contains(&sender_id.to_string()) {
            return ShareResult::Error(SignalingMessage::Error(ErrorPayload {
                message: "not in room".to_string(),
            }));
        }

        if members.info.room_type != RoomType::Sfu {
            return ShareResult::Error(SignalingMessage::Error(ErrorPayload {
                message: "not supported in P2P mode".to_string(),
            }));
        }

        if members.info.share_permission == new_perm {
            return ShareResult::Noop;
        }

        members.info.share_permission = new_perm;

        let signal = OutboundSignal::broadcast_all(SignalingMessage::SharePermissionChanged(
            SharePermissionChangedPayload {
                permission,
            },
        ));
        ShareResult::Ok(vec![signal])
    });

    match result {
        Ok(r) => r,
        Err(_room_not_found) => ShareResult::Error(SignalingMessage::Error(ErrorPayload {
            message: "not in room".to_string(),
        })),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::state::RoomInfo;
    use crate::voice::sfu_bridge::SfuRoomHandle;
    use proptest::prelude::*;

    // ── Helpers ──────────────────────────────────────────────────────────────

    fn make_sfu_room(state: &InMemoryRoomState, room_id: &str, peers: &[&str]) {
        let info = RoomInfo::new_sfu(6, SfuRoomHandle(format!("{room_id}-handle")));
        state.create_room(room_id.to_string(), info);
        for peer in peers {
            state.add_peer(peer.to_string(), room_id.to_string());
        }
    }

    fn make_p2p_room(state: &InMemoryRoomState, room_id: &str, peers: &[&str]) {
        let info = RoomInfo::new_p2p();
        state.create_room(room_id.to_string(), info);
        for peer in peers {
            state.add_peer(peer.to_string(), room_id.to_string());
        }
    }

    fn set_active_share(state: &InMemoryRoomState, room_id: &str, owner: &str) {
        state
            .with_room_write(room_id, |m| {
                m.info.active_shares.insert(owner.to_string());
            })
            .unwrap();
    }

    fn get_active_share(state: &InMemoryRoomState, room_id: &str) -> Option<String> {
        state
            .get_room_info(room_id)
            .and_then(|i| i.active_shares.iter().next().cloned())
    }

    fn get_active_shares(
        state: &InMemoryRoomState,
        room_id: &str,
    ) -> std::collections::HashSet<String> {
        state
            .get_room_info(room_id)
            .map(|i| i.active_shares.clone())
            .unwrap_or_default()
    }

    fn signals_contain_share_started(signals: &[OutboundSignal], participant_id: &str) -> bool {
        signals.iter().any(|s| {
            matches!(&s.msg, SignalingMessage::ShareStarted(p) if p.participant_id == participant_id)
                && s.target == crate::voice::sfu_relay::SignalTarget::BroadcastAll
        })
    }

    fn signals_contain_share_stopped(signals: &[OutboundSignal], participant_id: &str) -> bool {
        signals.iter().any(|s| {
            matches!(&s.msg, SignalingMessage::ShareStopped(p) if p.participant_id == participant_id)
                && s.target == crate::voice::sfu_relay::SignalTarget::BroadcastAll
        })
    }

    // ── Unit tests ────────────────────────────────────────────────────────────

    #[test]
    fn start_share_succeeds_in_sfu_room() {
        let state = InMemoryRoomState::new();
        make_sfu_room(&state, "room-1", &["peer-a"]);

        let result = handle_start_share(&state, "room-1", "peer-a", ParticipantRole::Guest);
        assert!(matches!(result, ShareResult::Ok(_)));
        assert_eq!(
            get_active_share(&state, "room-1"),
            Some("peer-a".to_string())
        );
    }

    #[test]
    fn start_share_fails_in_p2p_room() {
        let state = InMemoryRoomState::new();
        make_p2p_room(&state, "room-1", &["peer-a"]);

        let result = handle_start_share(&state, "room-1", "peer-a", ParticipantRole::Guest);
        assert!(matches!(result, ShareResult::Error(_)));
        assert_eq!(get_active_share(&state, "room-1"), None);
    }

    #[test]
    fn start_share_noop_when_already_sharing() {
        let state = InMemoryRoomState::new();
        make_sfu_room(&state, "room-1", &["peer-a", "peer-b"]);
        set_active_share(&state, "room-1", "peer-a");

        let result = handle_start_share(&state, "room-1", "peer-a", ParticipantRole::Guest);
        assert!(matches!(result, ShareResult::Noop));
        // State unchanged
        assert!(get_active_shares(&state, "room-1").contains("peer-a"));
    }

    #[test]
    fn start_share_fails_when_sender_not_in_room() {
        let state = InMemoryRoomState::new();
        make_sfu_room(&state, "room-1", &["peer-a"]);

        let result = handle_start_share(&state, "room-1", "outsider", ParticipantRole::Guest);
        assert!(matches!(result, ShareResult::Error(_)));
    }

    #[test]
    fn start_share_fails_for_nonexistent_room() {
        let state = InMemoryRoomState::new();
        let result = handle_start_share(&state, "ghost-room", "peer-a", ParticipantRole::Guest);
        assert!(matches!(result, ShareResult::Error(_)));
    }

    #[test]
    fn stop_share_by_owner_clears_share() {
        let state = InMemoryRoomState::new();
        make_sfu_room(&state, "room-1", &["peer-a"]);
        set_active_share(&state, "room-1", "peer-a");

        let result = handle_stop_share(&state, "room-1", "peer-a", None, ParticipantRole::Guest);
        assert!(matches!(result, ShareResult::Ok(_)));
        assert_eq!(get_active_share(&state, "room-1"), None);
    }

    #[test]
    fn stop_share_by_host_clears_another_participants_share() {
        let state = InMemoryRoomState::new();
        make_sfu_room(&state, "room-1", &["host", "sharer"]);
        set_active_share(&state, "room-1", "sharer");

        let result = handle_stop_share(
            &state,
            "room-1",
            "host",
            Some("sharer"),
            ParticipantRole::Host,
        );
        assert!(matches!(result, ShareResult::Ok(_)));
        assert_eq!(get_active_share(&state, "room-1"), None);
    }

    #[test]
    fn stop_share_noop_when_no_active_share() {
        let state = InMemoryRoomState::new();
        make_sfu_room(&state, "room-1", &["peer-a"]);

        let result = handle_stop_share(&state, "room-1", "peer-a", None, ParticipantRole::Guest);
        assert!(matches!(result, ShareResult::Noop));
    }

    #[test]
    fn stop_share_noop_for_non_owner_guest() {
        let state = InMemoryRoomState::new();
        make_sfu_room(&state, "room-1", &["peer-a", "peer-b"]);
        set_active_share(&state, "room-1", "peer-a");

        let result = handle_stop_share(&state, "room-1", "peer-b", None, ParticipantRole::Guest);
        assert!(matches!(result, ShareResult::Noop));
        // State unchanged
        assert_eq!(
            get_active_share(&state, "room-1"),
            Some("peer-a".to_string())
        );
    }

    #[test]
    fn cleanup_on_disconnect_clears_owner_share() {
        let state = InMemoryRoomState::new();
        make_sfu_room(&state, "room-1", &["peer-a", "peer-b"]);
        set_active_share(&state, "room-1", "peer-a");

        let signals = cleanup_share_on_disconnect(&state, "room-1", "peer-a");
        assert!(signals.is_some());
        assert_eq!(get_active_share(&state, "room-1"), None);
    }

    #[test]
    fn cleanup_on_disconnect_noop_for_non_owner() {
        let state = InMemoryRoomState::new();
        make_sfu_room(&state, "room-1", &["peer-a", "peer-b"]);
        set_active_share(&state, "room-1", "peer-a");

        let signals = cleanup_share_on_disconnect(&state, "room-1", "peer-b");
        assert!(signals.is_none());
        // Share still active
        assert_eq!(
            get_active_share(&state, "room-1"),
            Some("peer-a".to_string())
        );
    }

    // ── Multi-share unit tests ────────────────────────────────────────────────

    #[test]
    fn multiple_concurrent_shares() {
        let state = InMemoryRoomState::new();
        make_sfu_room(&state, "room-1", &["peer-a", "peer-b", "peer-c"]);

        // Two participants start sharing simultaneously
        let r1 = handle_start_share(&state, "room-1", "peer-a", ParticipantRole::Guest);
        let r2 = handle_start_share(&state, "room-1", "peer-b", ParticipantRole::Guest);
        assert!(matches!(r1, ShareResult::Ok(_)));
        assert!(matches!(r2, ShareResult::Ok(_)));

        let shares = get_active_shares(&state, "room-1");
        assert_eq!(shares.len(), 2);
        assert!(shares.contains("peer-a"));
        assert!(shares.contains("peer-b"));
    }

    #[test]
    fn non_host_targeted_stop_rejected() {
        let state = InMemoryRoomState::new();
        make_sfu_room(&state, "room-1", &["peer-a", "peer-b"]);
        set_active_share(&state, "room-1", "peer-a");

        // Guest tries to stop another participant's share
        let result = handle_stop_share(
            &state,
            "room-1",
            "peer-b",
            Some("peer-a"),
            ParticipantRole::Guest,
        );
        assert!(matches!(result, ShareResult::Error(_)));
        // Share still active
        assert!(get_active_shares(&state, "room-1").contains("peer-a"));
    }

    #[test]
    fn host_directed_stop_removes_target_only() {
        let state = InMemoryRoomState::new();
        make_sfu_room(&state, "room-1", &["host", "sharer-a", "sharer-b"]);
        set_active_share(&state, "room-1", "sharer-a");
        set_active_share(&state, "room-1", "sharer-b");

        // Host stops only sharer-a
        let result = handle_stop_share(
            &state,
            "room-1",
            "host",
            Some("sharer-a"),
            ParticipantRole::Host,
        );
        assert!(
            matches!(result, ShareResult::Ok(ref sigs) if signals_contain_share_stopped(sigs, "sharer-a"))
        );

        let shares = get_active_shares(&state, "room-1");
        assert!(!shares.contains("sharer-a"), "sharer-a must be removed");
        assert!(shares.contains("sharer-b"), "sharer-b must remain");
    }

    // ── handle_stop_all_shares tests ──────────────────────────────────────────

    #[test]
    fn stop_all_shares_host_clears_all() {
        let state = InMemoryRoomState::new();
        make_sfu_room(&state, "room-1", &["host", "sharer-a", "sharer-b"]);
        set_active_share(&state, "room-1", "sharer-a");
        set_active_share(&state, "room-1", "sharer-b");

        let result = handle_stop_all_shares(&state, "room-1", "host", ParticipantRole::Host);
        match result {
            ShareResult::Ok(signals) => {
                assert_eq!(signals.len(), 2, "one ShareStopped per sharer");
                assert!(signals_contain_share_stopped(&signals, "sharer-a"));
                assert!(signals_contain_share_stopped(&signals, "sharer-b"));
            }
            _ => panic!("expected ShareResult::Ok"),
        }
        assert!(get_active_shares(&state, "room-1").is_empty());
    }

    #[test]
    fn stop_all_shares_non_host_rejected() {
        let state = InMemoryRoomState::new();
        make_sfu_room(&state, "room-1", &["host", "guest", "sharer"]);
        set_active_share(&state, "room-1", "sharer");

        let result = handle_stop_all_shares(&state, "room-1", "guest", ParticipantRole::Guest);
        assert!(matches!(result, ShareResult::Error(_)));
        // Share still active
        assert!(get_active_shares(&state, "room-1").contains("sharer"));
    }

    #[test]
    fn stop_all_shares_empty_set_noop() {
        let state = InMemoryRoomState::new();
        make_sfu_room(&state, "room-1", &["host"]);

        let result = handle_stop_all_shares(&state, "room-1", "host", ParticipantRole::Host);
        assert!(matches!(result, ShareResult::Noop));
    }

    // ── share_state_snapshot tests ────────────────────────────────────────────

    #[test]
    fn share_state_snapshot_returns_active_sharers() {
        let state = InMemoryRoomState::new();
        make_sfu_room(&state, "room-1", &["peer-a", "peer-b", "peer-c"]);
        set_active_share(&state, "room-1", "peer-a");
        set_active_share(&state, "room-1", "peer-b");

        let signal = share_state_snapshot(&state, "room-1", "peer-c");
        assert!(
            matches!(signal.target, crate::voice::sfu_relay::SignalTarget::Peer(ref id) if id == "peer-c")
        );
        match &signal.msg {
            SignalingMessage::ShareState(payload) => {
                let ids: std::collections::HashSet<String> =
                    payload.participant_ids.iter().cloned().collect();
                assert_eq!(ids.len(), 2);
                assert!(ids.contains("peer-a"));
                assert!(ids.contains("peer-b"));
            }
            _ => panic!("expected ShareState message"),
        }
    }

    #[test]
    fn share_state_snapshot_empty_room() {
        let state = InMemoryRoomState::new();
        // Room doesn't exist — should return empty snapshot
        let signal = share_state_snapshot(&state, "nonexistent", "peer-x");
        match &signal.msg {
            SignalingMessage::ShareState(payload) => {
                assert!(payload.participant_ids.is_empty());
            }
            _ => panic!("expected ShareState message"),
        }
    }

    #[test]
    fn share_state_snapshot_no_active_shares() {
        let state = InMemoryRoomState::new();
        make_sfu_room(&state, "room-1", &["peer-a"]);

        let signal = share_state_snapshot(&state, "room-1", "peer-a");
        match &signal.msg {
            SignalingMessage::ShareState(payload) => {
                assert!(payload.participant_ids.is_empty());
            }
            _ => panic!("expected ShareState message"),
        }
    }

    // ── Property tests ────────────────────────────────────────────────────────

    // Feature: phase3-security-hardening, Property 6: StartShare succeeds only when all preconditions hold
    // Validates: Requirements 4.2, 4.3, 4.5, 6.2, 6.3
    proptest! {
        #![proptest_config(ProptestConfig::with_cases(100))]

        #[test]
        fn prop6_start_share_preconditions(
            room_id in "[a-z]{4,8}",
            sender_id in "[a-z]{4,8}",
            other_id in "[a-z]{4,8}",
        ) {
            prop_assume!(sender_id != other_id);

            // Case 1: valid SFU room, sender is member, no active share → Ok
            {
                let state = InMemoryRoomState::new();
                make_sfu_room(&state, &room_id, &[&sender_id]);
                let result = handle_start_share(&state, &room_id, &sender_id, ParticipantRole::Guest);
                prop_assert!(
                    matches!(result, ShareResult::Ok(_)),
                    "should succeed when all preconditions hold"
                );
                prop_assert_eq!(
                    get_active_share(&state, &room_id),
                    Some(sender_id.clone()),
                    "active_share must be set to sender_id"
                );
            }

            // Case 2: P2P room → Error
            {
                let state = InMemoryRoomState::new();
                make_p2p_room(&state, &room_id, &[&sender_id]);
                let result = handle_start_share(&state, &room_id, &sender_id, ParticipantRole::Guest);
                prop_assert!(
                    matches!(result, ShareResult::Error(_)),
                    "P2P room must return Error"
                );
            }

            // Case 3: sender not in room → Error
            {
                let state = InMemoryRoomState::new();
                make_sfu_room(&state, &room_id, &[&other_id]);
                let result = handle_start_share(&state, &room_id, &sender_id, ParticipantRole::Guest);
                prop_assert!(
                    matches!(result, ShareResult::Error(_)),
                    "sender not in room must return Error"
                );
            }

            // Case 4: already sharing → Noop
            {
                let state = InMemoryRoomState::new();
                make_sfu_room(&state, &room_id, &[&sender_id, &other_id]);
                set_active_share(&state, &room_id, &sender_id);
                let result = handle_start_share(&state, &room_id, &sender_id, ParticipantRole::Guest);
                prop_assert!(
                    matches!(result, ShareResult::Noop),
                    "already sharing must return Noop"
                );
            }
        }

        #[test]
        fn prop6_start_share_produces_broadcast_all_signal(
            room_id in "[a-z]{4,8}",
            sender_id in "[a-z]{4,8}",
        ) {
            let state = InMemoryRoomState::new();
            make_sfu_room(&state, &room_id, &[&sender_id]);
            let result = handle_start_share(&state, &room_id, &sender_id, ParticipantRole::Guest);
            if let ShareResult::Ok(signals) = result {
                prop_assert!(
                    signals_contain_share_started(&signals, &sender_id),
                    "must produce BroadcastAll ShareStarted with sender_id"
                );
            } else {
                prop_assert!(false, "expected Ok");
            }
        }
    }

    // Feature: phase3-security-hardening, Property 7: StartShare failure preserves room state
    // Validates: Requirements 4.4
    proptest! {
        #![proptest_config(ProptestConfig::with_cases(100))]

        #[test]
        fn prop7_start_share_failure_preserves_state(
            room_id in "[a-z]{4,8}",
            sender_id in "[a-z]{4,8}",
            owner_id in "[a-z]{4,8}",
        ) {
            prop_assume!(sender_id != owner_id);

            // Noop: sender already sharing — active_shares must still contain sender
            {
                let state = InMemoryRoomState::new();
                make_sfu_room(&state, &room_id, &[&sender_id, &owner_id]);
                set_active_share(&state, &room_id, &sender_id);

                let before = get_active_shares(&state, &room_id);
                let _ = handle_start_share(&state, &room_id, &sender_id, ParticipantRole::Guest);
                let after = get_active_shares(&state, &room_id);

                prop_assert_eq!(before, after, "active_shares must be unchanged on Noop");
            }

            // Failure: sender not in room — active_share must remain None
            {
                let state = InMemoryRoomState::new();
                make_sfu_room(&state, &room_id, &[&owner_id]);

                let before = get_active_share(&state, &room_id);
                let _ = handle_start_share(&state, &room_id, &sender_id, ParticipantRole::Guest);
                let after = get_active_share(&state, &room_id);

                prop_assert_eq!(before, after, "active_share must be unchanged on Error");
            }

            // Failure: P2P room — active_share must remain None
            {
                let state = InMemoryRoomState::new();
                make_p2p_room(&state, &room_id, &[&sender_id]);

                let before = get_active_share(&state, &room_id);
                let _ = handle_start_share(&state, &room_id, &sender_id, ParticipantRole::Guest);
                let after = get_active_share(&state, &room_id);

                prop_assert_eq!(before, after, "active_share must be unchanged on Error");
            }
        }
    }

    // Feature: phase3-security-hardening, Property 8: StopShare by owner or host clears share
    // Validates: Requirements 5.1, 5.4
    proptest! {
        #![proptest_config(ProptestConfig::with_cases(100))]

        #[test]
        fn prop8_stop_share_by_owner_clears(
            room_id in "[a-z]{4,8}",
            owner_id in "[a-z]{4,8}",
        ) {
            let state = InMemoryRoomState::new();
            make_sfu_room(&state, &room_id, &[&owner_id]);
            set_active_share(&state, &room_id, &owner_id);

            let result = handle_stop_share(&state, &room_id, &owner_id, None, ParticipantRole::Guest);

            prop_assert!(
                matches!(result, ShareResult::Ok(_)),
                "owner stop must return Ok"
            );
            prop_assert_eq!(
                get_active_share(&state, &room_id),
                None,
                "active_share must be None after owner stops"
            );

            if let ShareResult::Ok(signals) = result {
                prop_assert!(
                    signals_contain_share_stopped(&signals, &owner_id),
                    "must produce BroadcastAll ShareStopped with owner_id"
                );
            }
        }

        #[test]
        fn prop8_stop_share_by_host_clears_another_share(
            room_id in "[a-z]{4,8}",
            host_id in "[a-z]{4,8}",
            sharer_id in "[a-z]{4,8}",
        ) {
            prop_assume!(host_id != sharer_id);

            let state = InMemoryRoomState::new();
            make_sfu_room(&state, &room_id, &[&host_id, &sharer_id]);
            set_active_share(&state, &room_id, &sharer_id);

            let result = handle_stop_share(&state, &room_id, &host_id, Some(sharer_id.as_str()), ParticipantRole::Host);

            prop_assert!(
                matches!(result, ShareResult::Ok(_)),
                "host override must return Ok"
            );
            prop_assert_eq!(
                get_active_share(&state, &room_id),
                None,
                "active_share must be None after host stops"
            );

            if let ShareResult::Ok(signals) = result {
                prop_assert!(
                    signals_contain_share_stopped(&signals, &sharer_id),
                    "ShareStopped must carry the sharer's participant_id, not the host's"
                );
            }
        }
    }

    // Feature: phase3-security-hardening, Property 9: StopShare is idempotent
    // Validates: Requirements 5.3
    proptest! {
        #![proptest_config(ProptestConfig::with_cases(100))]

        #[test]
        fn prop9_stop_share_noop_when_no_active_share(
            room_id in "[a-z]{4,8}",
            sender_id in "[a-z]{4,8}",
        ) {
            let state = InMemoryRoomState::new();
            make_sfu_room(&state, &room_id, &[&sender_id]);
            // No active share

            let result = handle_stop_share(&state, &room_id, &sender_id, None, ParticipantRole::Guest);
            prop_assert!(
                matches!(result, ShareResult::Noop),
                "no active share must return Noop"
            );
            prop_assert_eq!(get_active_share(&state, &room_id), None, "state must remain None");
        }

        #[test]
        fn prop9_stop_share_noop_for_non_owner_guest(
            room_id in "[a-z]{4,8}",
            owner_id in "[a-z]{4,8}",
            other_id in "[a-z]{4,8}",
        ) {
            prop_assume!(owner_id != other_id);

            let state = InMemoryRoomState::new();
            make_sfu_room(&state, &room_id, &[&owner_id, &other_id]);
            set_active_share(&state, &room_id, &owner_id);

            let result = handle_stop_share(&state, &room_id, &other_id, None, ParticipantRole::Guest);
            prop_assert!(
                matches!(result, ShareResult::Noop),
                "non-owner guest must return Noop"
            );
            prop_assert_eq!(
                get_active_share(&state, &room_id),
                Some(owner_id.clone()),
                "active_share must be unchanged"
            );
        }

        #[test]
        fn prop9_stop_share_idempotent_double_stop(
            room_id in "[a-z]{4,8}",
            owner_id in "[a-z]{4,8}",
        ) {
            let state = InMemoryRoomState::new();
            make_sfu_room(&state, &room_id, &[&owner_id]);
            set_active_share(&state, &room_id, &owner_id);

            // First stop — should succeed
            let r1 = handle_stop_share(&state, &room_id, &owner_id, None, ParticipantRole::Guest);
            prop_assert!(matches!(r1, ShareResult::Ok(_)));

            // Second stop — should be Noop
            let r2 = handle_stop_share(&state, &room_id, &owner_id, None, ParticipantRole::Guest);
            prop_assert!(
                matches!(r2, ShareResult::Noop),
                "second stop must be Noop (idempotent)"
            );
            prop_assert_eq!(get_active_share(&state, &room_id), None);
        }
    }

    // Feature: phase3-security-hardening, Property 10: Disconnect cleanup clears owner's share
    // Validates: Requirements 5.2
    proptest! {
        #![proptest_config(ProptestConfig::with_cases(100))]

        #[test]
        fn prop10_cleanup_clears_owner_share(
            room_id in "[a-z]{4,8}",
            owner_id in "[a-z]{4,8}",
        ) {
            let state = InMemoryRoomState::new();
            make_sfu_room(&state, &room_id, &[&owner_id]);
            set_active_share(&state, &room_id, &owner_id);

            let signals = cleanup_share_on_disconnect(&state, &room_id, &owner_id);

            prop_assert!(signals.is_some(), "owner disconnect must return Some(signals)");
            prop_assert_eq!(
                get_active_share(&state, &room_id),
                None,
                "active_share must be None after owner disconnects"
            );

            if let Some(sigs) = signals {
                prop_assert!(
                    signals_contain_share_stopped(&sigs, &owner_id),
                    "must produce BroadcastAll ShareStopped with owner_id"
                );
            }
        }

        #[test]
        fn prop10_cleanup_noop_for_non_owner(
            room_id in "[a-z]{4,8}",
            owner_id in "[a-z]{4,8}",
            other_id in "[a-z]{4,8}",
        ) {
            prop_assume!(owner_id != other_id);

            let state = InMemoryRoomState::new();
            make_sfu_room(&state, &room_id, &[&owner_id, &other_id]);
            set_active_share(&state, &room_id, &owner_id);

            let signals = cleanup_share_on_disconnect(&state, &room_id, &other_id);

            prop_assert!(signals.is_none(), "non-owner disconnect must return None");
            prop_assert_eq!(
                get_active_share(&state, &room_id),
                Some(owner_id.clone()),
                "active_share must be unchanged when non-owner disconnects"
            );
        }

        #[test]
        fn prop10_cleanup_noop_when_no_active_share(
            room_id in "[a-z]{4,8}",
            peer_id in "[a-z]{4,8}",
        ) {
            let state = InMemoryRoomState::new();
            make_sfu_room(&state, &room_id, &[&peer_id]);
            // No active share

            let signals = cleanup_share_on_disconnect(&state, &room_id, &peer_id);
            prop_assert!(signals.is_none(), "no active share must return None");
        }
    }
}
