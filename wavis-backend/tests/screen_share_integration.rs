#![cfg(feature = "test-support")]
//! Property-based integration tests for multi-share screen sharing.
//!
//! Exercises domain functions from `screen_share.rs` and `sfu_relay.rs`
//! using `proptest` with real `InMemoryRoomState`. Verifies:
//!   - Start share adds participant and broadcasts (Property 2)
//!   - Start share idempotence (Property 3)
//!   - Stop share removes only target (Property 4)
//!   - Disconnect cleanup (Property 5)
//!   - Non-host moderation rejected (Property 6)
//!   - Host stop-all clears set (Property 7)
//!   - Join produces share_state snapshot (Property 8)
//!   - Active_Shares_Set subset invariant (Property 9)

use proptest::prelude::*;
use std::collections::HashSet;

use shared::signaling::{
    ShareStartedPayload, ShareStatePayload, ShareStoppedPayload, SignalingMessage,
};
use wavis_backend::state::{InMemoryRoomState, RoomInfo};
use wavis_backend::voice::screen_share::{
    ShareResult, cleanup_share_on_disconnect, handle_start_share, handle_stop_all_shares,
    handle_stop_share,
};
use wavis_backend::voice::sfu_bridge::SfuRoomHandle;
use wavis_backend::voice::sfu_relay::{OutboundSignal, ParticipantRole, SignalTarget};

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

fn make_sfu_room(state: &InMemoryRoomState, room_id: &str, peers: &[&str]) {
    let info = RoomInfo::new_sfu(6, SfuRoomHandle(format!("{room_id}-handle")));
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

fn get_active_shares(state: &InMemoryRoomState, room_id: &str) -> HashSet<String> {
    state
        .get_room_info(room_id)
        .map(|i| i.active_shares.clone())
        .unwrap_or_default()
}

fn peer_ids(count: usize) -> Vec<String> {
    (0..count).map(|i| format!("peer-{i}")).collect()
}

fn signals_contain_share_started(signals: &[OutboundSignal], participant_id: &str) -> bool {
    signals.iter().any(|s| {
        matches!(
            &s.msg,
            SignalingMessage::ShareStarted(ShareStartedPayload { participant_id: pid, .. })
            if pid == participant_id
        )
    })
}

fn signals_contain_share_stopped(signals: &[OutboundSignal], participant_id: &str) -> bool {
    signals.iter().any(|s| {
        matches!(
            &s.msg,
            SignalingMessage::ShareStopped(ShareStoppedPayload { participant_id: pid, .. })
            if pid == participant_id
        )
    })
}

// ---------------------------------------------------------------------------
// Property 2: Start share adds participant and broadcasts
// Validates: Requirements 1.2, 2.1
// ---------------------------------------------------------------------------

proptest! {
    #![proptest_config(ProptestConfig::with_cases(100))]

    #[test]
    fn prop_start_share_adds_and_broadcasts(n in 1usize..=6, picker in 0usize..6) {
        // **Validates: Requirements 1.2, 2.1**
        let peers = peer_ids(n);
        let state = InMemoryRoomState::new();
        let peer_refs: Vec<&str> = peers.iter().map(|s| s.as_str()).collect();
        make_sfu_room(&state, "room-1", &peer_refs);

        // Pick a non-sharing participant (none are sharing initially)
        let idx = picker % n;
        let target = &peers[idx];

        let result = handle_start_share(&state, "room-1", target, ParticipantRole::Guest);

        // Verify: target is now in active_shares
        let shares = get_active_shares(&state, "room-1");
        prop_assert!(shares.contains(target), "target should be in active_shares");

        // Verify: result is Ok with exactly one ShareStarted broadcast signal
        match result {
            ShareResult::Ok(signals) => {
                prop_assert_eq!(signals.len(), 1, "should produce exactly one signal");
                prop_assert!(
                    signals_contain_share_started(&signals, target),
                    "signal should be ShareStarted for target"
                );
                prop_assert!(
                    matches!(signals[0].target, SignalTarget::BroadcastAll),
                    "signal should be BroadcastAll"
                );
            }
            other => prop_assert!(false, "expected ShareResult::Ok, got {:?}", match other {
                ShareResult::Noop => "Noop",
                ShareResult::Error(_) => "Error",
                _ => unreachable!(),
            }),
        }
    }
}

// ---------------------------------------------------------------------------
// Property 3: Start share idempotence
// Validates: Requirements 1.3, 2.2
// ---------------------------------------------------------------------------

proptest! {
    #![proptest_config(ProptestConfig::with_cases(100))]

    #[test]
    fn prop_start_share_idempotence(
        n in 1usize..=6,
        num_sharers in 1usize..=6,
        picker in 0usize..6,
    ) {
        // **Validates: Requirements 1.3, 2.2**
        let n = n.max(1);
        let peers = peer_ids(n);
        let state = InMemoryRoomState::new();
        let peer_refs: Vec<&str> = peers.iter().map(|s| s.as_str()).collect();
        make_sfu_room(&state, "room-1", &peer_refs);

        // Mark some participants as sharing
        let actual_sharers = num_sharers.min(n);
        for peer in peers.iter().take(actual_sharers) {
            set_active_share(&state, "room-1", peer);
        }

        // Pick an already-sharing participant
        let idx = picker % actual_sharers;
        let target = &peers[idx];

        let shares_before = get_active_shares(&state, "room-1");

        let result = handle_start_share(&state, "room-1", target, ParticipantRole::Guest);

        let shares_after = get_active_shares(&state, "room-1");

        // Verify: Noop returned, set unchanged
        prop_assert!(matches!(result, ShareResult::Noop), "should be Noop for already-sharing participant");
        prop_assert_eq!(shares_before, shares_after, "active_shares should be unchanged");
    }
}

// ---------------------------------------------------------------------------
// Property 4: Stop share removes only target
// Validates: Requirements 1.4, 3.1
// ---------------------------------------------------------------------------

proptest! {
    #![proptest_config(ProptestConfig::with_cases(100))]

    #[test]
    fn prop_stop_share_removes_only_target(
        n in 2usize..=6,
        num_sharers in 2usize..=6,
        picker in 0usize..6,
    ) {
        // **Validates: Requirements 1.4, 3.1**
        let peers = peer_ids(n);
        let state = InMemoryRoomState::new();
        let peer_refs: Vec<&str> = peers.iter().map(|s| s.as_str()).collect();
        make_sfu_room(&state, "room-1", &peer_refs);

        let actual_sharers = num_sharers.min(n).max(2);
        for peer in peers.iter().take(actual_sharers) {
            set_active_share(&state, "room-1", peer);
        }

        let idx = picker % actual_sharers;
        let target = &peers[idx];

        let shares_before = get_active_shares(&state, "room-1");

        let result = handle_stop_share(&state, "room-1", target, None, ParticipantRole::Guest);

        let shares_after = get_active_shares(&state, "room-1");

        // Verify: target removed
        prop_assert!(!shares_after.contains(target), "target should be removed");

        // Verify: all other sharers remain
        for sharer in &shares_before {
            if sharer != target {
                prop_assert!(shares_after.contains(sharer), "other sharer {} should remain", sharer);
            }
        }

        // Verify: exactly one ShareStopped signal
        match result {
            ShareResult::Ok(signals) => {
                prop_assert_eq!(signals.len(), 1);
                prop_assert!(signals_contain_share_stopped(&signals, target));
                prop_assert!(matches!(signals[0].target, SignalTarget::BroadcastAll));
            }
            other => prop_assert!(false, "expected Ok, got {:?}", match other {
                ShareResult::Noop => "Noop",
                ShareResult::Error(_) => "Error",
                _ => unreachable!(),
            }),
        }
    }
}

// ---------------------------------------------------------------------------
// Property 5: Disconnect cleanup emits share_stopped only for active sharers
// Validates: Requirements 1.5, 9.1, 9.2, 9.3
// ---------------------------------------------------------------------------

proptest! {
    #![proptest_config(ProptestConfig::with_cases(100))]

    #[test]
    fn prop_disconnect_cleanup(
        n in 2usize..=6,
        num_sharers in 0usize..=6,
        picker in 0usize..6,
    ) {
        // **Validates: Requirements 1.5, 9.1, 9.2, 9.3**
        let peers = peer_ids(n);
        let state = InMemoryRoomState::new();
        let peer_refs: Vec<&str> = peers.iter().map(|s| s.as_str()).collect();
        make_sfu_room(&state, "room-1", &peer_refs);

        let actual_sharers = num_sharers.min(n);
        let sharer_set: HashSet<String> = peers[..actual_sharers].iter().cloned().collect();
        for sharer in &sharer_set {
            set_active_share(&state, "room-1", sharer);
        }

        // Pick any participant to disconnect
        let idx = picker % n;
        let disconnecting = &peers[idx];
        let was_sharing = sharer_set.contains(disconnecting);

        let result = cleanup_share_on_disconnect(&state, "room-1", disconnecting);

        let shares_after = get_active_shares(&state, "room-1");

        // Verify: disconnecting peer is no longer in active_shares
        prop_assert!(!shares_after.contains(disconnecting));

        if was_sharing {
            // Should have produced ShareStopped signals
            let signals = result.expect("should produce signals for active sharer");
            prop_assert!(signals_contain_share_stopped(&signals, disconnecting));
        } else {
            // Should produce no signals
            prop_assert!(result.is_none(), "should produce no signals for non-sharer");
        }

        // Verify: other sharers remain
        for sharer in &sharer_set {
            if sharer != disconnecting {
                prop_assert!(shares_after.contains(sharer), "other sharer {} should remain", sharer);
            }
        }
    }
}

// ---------------------------------------------------------------------------
// Property 6: Non-host moderation actions are rejected
// Validates: Requirements 3.4, 4.2
// ---------------------------------------------------------------------------

proptest! {
    #![proptest_config(ProptestConfig::with_cases(100))]

    #[test]
    fn prop_non_host_moderation_rejected(
        n in 2usize..=6,
        num_sharers in 1usize..=6,
        guest_picker in 0usize..6,
        target_picker in 0usize..6,
    ) {
        // **Validates: Requirements 3.4, 4.2**
        let peers = peer_ids(n);
        let state = InMemoryRoomState::new();
        let peer_refs: Vec<&str> = peers.iter().map(|s| s.as_str()).collect();
        make_sfu_room(&state, "room-1", &peer_refs);

        let actual_sharers = num_sharers.min(n);
        for peer in peers.iter().take(actual_sharers) {
            set_active_share(&state, "room-1", peer);
        }

        let shares_before = get_active_shares(&state, "room-1");

        // Pick a non-host guest and a different target
        let guest_idx = guest_picker % n;
        let guest = &peers[guest_idx];

        // Pick a target that is different from the guest
        let target_idx = if n > 1 {
            (target_picker % (n - 1) + guest_idx + 1) % n
        } else {
            0
        };
        let target = &peers[target_idx];

        // Test 1: Non-host targeted stop → permission error
        let result = handle_stop_share(
            &state, "room-1", guest, Some(target), ParticipantRole::Guest,
        );
        match &result {
            ShareResult::Error(SignalingMessage::Error(e)) => {
                prop_assert!(e.message.contains("permission denied"));
            }
            _ => prop_assert!(false, "expected permission error for non-host targeted stop"),
        }

        // Test 2: Non-host stop-all → permission error
        let result2 = handle_stop_all_shares(&state, "room-1", guest, ParticipantRole::Guest);
        match &result2 {
            ShareResult::Error(SignalingMessage::Error(e)) => {
                prop_assert!(e.message.contains("permission denied"));
            }
            _ => prop_assert!(false, "expected permission error for non-host stop-all"),
        }

        // Verify: active_shares unchanged
        let shares_after = get_active_shares(&state, "room-1");
        prop_assert_eq!(shares_before, shares_after, "active_shares should be unchanged after rejected moderation");
    }
}

// ---------------------------------------------------------------------------
// Property 7: Host stop-all clears the entire Active_Shares_Set
// Validates: Requirements 4.1, 4.3
// ---------------------------------------------------------------------------

proptest! {
    #![proptest_config(ProptestConfig::with_cases(100))]

    #[test]
    fn prop_host_stop_all_clears_set(
        n in 1usize..=6,
        num_sharers in 0usize..=6,
    ) {
        // **Validates: Requirements 4.1, 4.3**
        let peers = peer_ids(n);
        let state = InMemoryRoomState::new();
        let peer_refs: Vec<&str> = peers.iter().map(|s| s.as_str()).collect();
        make_sfu_room(&state, "room-1", &peer_refs);

        let actual_sharers = num_sharers.min(n);
        for peer in peers.iter().take(actual_sharers) {
            set_active_share(&state, "room-1", peer);
        }

        let host = &peers[0];
        let result = handle_stop_all_shares(&state, "room-1", host, ParticipantRole::Host);

        let shares_after = get_active_shares(&state, "room-1");
        prop_assert!(shares_after.is_empty(), "active_shares should be empty after stop-all");

        if actual_sharers == 0 {
            prop_assert!(matches!(result, ShareResult::Noop), "should be Noop when no sharers");
        } else {
            match result {
                ShareResult::Ok(signals) => {
                    // Should have exactly one ShareStopped per removed sharer
                    prop_assert_eq!(signals.len(), actual_sharers, "should produce one signal per sharer");
                    for signal in &signals {
                        prop_assert!(matches!(signal.target, SignalTarget::BroadcastAll));
                        prop_assert!(matches!(&signal.msg, SignalingMessage::ShareStopped(_)));
                    }
                }
                _ => prop_assert!(false, "expected Ok for host stop-all with active sharers"),
            }
        }
    }
}

// ---------------------------------------------------------------------------
// Property 8: Join produces share_state snapshot in correct signal order
// Validates: Requirements 5.1, 5.2, 5.3
// ---------------------------------------------------------------------------

proptest! {
    #![proptest_config(ProptestConfig::with_cases(100))]

    #[test]
    fn prop_join_produces_share_state_snapshot(
        n in 1usize..=5,
        num_sharers in 0usize..=5,
    ) {
        // **Validates: Requirements 5.1, 5.2, 5.3**
        use wavis_backend::voice::mock_sfu_bridge::MockSfuBridge;
        use wavis_backend::voice::sfu_relay::{handle_sfu_join, TokenMode};
        use wavis_backend::channel::invite::{InviteStore, InviteStoreConfig};

        let rt = tokio::runtime::Runtime::new().unwrap();
        let result: Result<(), proptest::test_runner::TestCaseError> = rt.block_on(async {
            let peers = peer_ids(n);
            let state = InMemoryRoomState::new();
            let bridge = MockSfuBridge::new();
            let invite_store = InviteStore::new(InviteStoreConfig::default());
            let secret = b"integration-test-secret-32bytes!";
            let token_mode = TokenMode::Custom {
                jwt_secret: secret,
                issuer: wavis_backend::auth::jwt::DEFAULT_JWT_ISSUER,
                ttl_secs: wavis_backend::auth::jwt::TOKEN_TTL_SECS,
            };

            // Create room and add existing participants via handle_sfu_join
            for peer in &peers {
                let _ = handle_sfu_join(
                    &bridge, &state, "room-1", peer, peer,
                    None, &token_mode, "sfu://test", 6, &invite_store, None,
                ).await;
            }

            // Mark some as sharing
            let actual_sharers = num_sharers.min(n);
            let expected_sharer_set: HashSet<String> = peers[..actual_sharers].iter().cloned().collect();
            for sharer in &expected_sharer_set {
                set_active_share(&state, "room-1", sharer);
            }

            // New participant joins
            let joiner = "late-joiner".to_string();
            let signals = handle_sfu_join(
                &bridge, &state, "room-1", &joiner, &joiner,
                None, &token_mode, "sfu://test", 6, &invite_store, None,
            ).await.expect("join should succeed");

            // Find the ShareState signal targeted to the joiner
            let share_state_signals: Vec<&OutboundSignal> = signals.iter().filter(|s| {
                matches!(&s.msg, SignalingMessage::ShareState(_))
                    && matches!(&s.target, SignalTarget::Peer(pid) if pid == &joiner)
            }).collect();

            prop_assert_eq!(share_state_signals.len(), 1, "should have exactly one ShareState signal for joiner");

            // Verify content matches active shares
            if let SignalingMessage::ShareState(ShareStatePayload { participant_ids }) = &share_state_signals[0].msg {
                let received_set: HashSet<String> = participant_ids.iter().cloned().collect();
                prop_assert_eq!(received_set, expected_sharer_set, "ShareState should match active shares");
            } else {
                prop_assert!(false, "expected ShareState message");
            }

            // Verify signal ordering: Joined, MediaToken come before ShareState
            let joined_pos = signals.iter().position(|s| matches!(&s.msg, SignalingMessage::Joined(_)));
            let media_token_pos = signals.iter().position(|s| matches!(&s.msg, SignalingMessage::MediaToken(_)));
            let share_state_pos = signals.iter().position(|s| matches!(&s.msg, SignalingMessage::ShareState(_)));

            let j = joined_pos.expect("should have Joined signal");
            let m = media_token_pos.expect("should have MediaToken signal");
            let ss = share_state_pos.expect("should have ShareState signal");
            prop_assert!(j < ss, "Joined should come before ShareState");
            prop_assert!(m < ss, "MediaToken should come before ShareState");

            Ok(())
        });
        result?;
    }
}

// ---------------------------------------------------------------------------
// Property 9: Active_Shares_Set is always a subset of room participants
// Validates: Requirements 10.1, 10.2, 10.3, 10.4
// ---------------------------------------------------------------------------

/// Operations that can be applied to a room.
#[derive(Debug, Clone)]
enum RoomOp {
    Join(String),
    Leave(String),
    StartShare(String),
    StopShare(String),
    StopAll,
}

fn arb_room_op(max_peers: usize) -> impl Strategy<Value = RoomOp> {
    let peer_idx = 0..max_peers;
    prop_oneof![
        peer_idx
            .clone()
            .prop_map(|i| RoomOp::Join(format!("peer-{i}"))),
        peer_idx
            .clone()
            .prop_map(|i| RoomOp::Leave(format!("peer-{i}"))),
        peer_idx
            .clone()
            .prop_map(|i| RoomOp::StartShare(format!("peer-{i}"))),
        peer_idx.prop_map(|i| RoomOp::StopShare(format!("peer-{i}"))),
        Just(RoomOp::StopAll),
    ]
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(100))]

    #[test]
    fn prop_active_shares_subset_invariant(
        ops in proptest::collection::vec(arb_room_op(6), 1..50),
    ) {
        // **Validates: Requirements 10.1, 10.2, 10.3, 10.4**
        let state = InMemoryRoomState::new();
        let info = RoomInfo::new_sfu(6, SfuRoomHandle("room-1-handle".to_string()));
        state.create_room("room-1".to_string(), info);

        // Track which peers are currently in the room
        let mut participants: HashSet<String> = HashSet::new();
        // The first joiner is the host
        let mut host: Option<String> = None;

        for op in &ops {
            match op {
                RoomOp::Join(peer) => {
                    if !participants.contains(peer) && participants.len() < 6 {
                        state.add_peer(peer.clone(), "room-1".to_string());
                        participants.insert(peer.clone());
                        if host.is_none() {
                            host = Some(peer.clone());
                        }
                    }
                }
                RoomOp::Leave(peer) => {
                    if participants.contains(peer) {
                        // Clean up share state before removing peer
                        let _ = cleanup_share_on_disconnect(&state, "room-1", peer);
                        state.remove_peer(peer);
                        participants.remove(peer);

                        // If room was destroyed (last peer left), recreate it
                        if participants.is_empty() {
                            let info = RoomInfo::new_sfu(6, SfuRoomHandle("room-1-handle".to_string()));
                            state.create_room("room-1".to_string(), info);
                            host = None;
                        }

                        if host.as_deref() == Some(peer) {
                            host = participants.iter().next().cloned();
                        }
                    }
                }
                RoomOp::StartShare(peer) => {
                    if participants.contains(peer) {
                        let _ = handle_start_share(&state, "room-1", peer, ParticipantRole::Guest);
                    }
                }
                RoomOp::StopShare(peer) => {
                    if participants.contains(peer) {
                        let _ = handle_stop_share(
                            &state, "room-1", peer, None, ParticipantRole::Guest,
                        );
                    }
                }
                RoomOp::StopAll => {
                    if let Some(ref h) = host {
                        let _ = handle_stop_all_shares(&state, "room-1", h, ParticipantRole::Host);
                    }
                }
            }

            // INVARIANT: active_shares ⊆ participants after every operation
            let shares = get_active_shares(&state, "room-1");
            for sharer in &shares {
                prop_assert!(
                    participants.contains(sharer),
                    "active_shares contains '{}' which is not a participant. shares={:?}, participants={:?}, op={:?}",
                    sharer, shares, participants, op
                );
            }
        }
    }
}

// ===========================================================================
// Integration tests for full share lifecycle
// Validates: Requirements 1.2, 1.4, 1.5, 4.1, 5.1, 9.1
// ===========================================================================

// ---------------------------------------------------------------------------
// Test: Full share lifecycle — 3 join, 2 share, 1 stops
// Validates: Requirements 1.2, 1.4
// ---------------------------------------------------------------------------

#[test]
fn integration_full_share_lifecycle() {
    let state = InMemoryRoomState::new();
    make_sfu_room(&state, "room-1", &["peer-0", "peer-1", "peer-2"]);

    // peer-0 starts sharing
    let r0 = handle_start_share(&state, "room-1", "peer-0", ParticipantRole::Guest);
    match r0 {
        ShareResult::Ok(signals) => {
            assert_eq!(signals.len(), 1);
            assert!(signals_contain_share_started(&signals, "peer-0"));
        }
        _ => panic!("expected Ok for peer-0 start_share"),
    }

    // peer-1 starts sharing
    let r1 = handle_start_share(&state, "room-1", "peer-1", ParticipantRole::Guest);
    match r1 {
        ShareResult::Ok(signals) => {
            assert_eq!(signals.len(), 1);
            assert!(signals_contain_share_started(&signals, "peer-1"));
        }
        _ => panic!("expected Ok for peer-1 start_share"),
    }

    // Verify both are in active_shares
    let shares = get_active_shares(&state, "room-1");
    assert!(shares.contains("peer-0"));
    assert!(shares.contains("peer-1"));
    assert_eq!(shares.len(), 2);

    // peer-0 stops sharing
    let r2 = handle_stop_share(&state, "room-1", "peer-0", None, ParticipantRole::Guest);
    match r2 {
        ShareResult::Ok(signals) => {
            assert_eq!(signals.len(), 1);
            assert!(signals_contain_share_stopped(&signals, "peer-0"));
        }
        _ => panic!("expected Ok for peer-0 stop_share"),
    }

    // Verify only peer-1 remains
    let shares = get_active_shares(&state, "room-1");
    assert!(!shares.contains("peer-0"));
    assert!(shares.contains("peer-1"));
    assert_eq!(shares.len(), 1);
}

// ---------------------------------------------------------------------------
// Test: Late joiner receives share_state snapshot
// Validates: Requirements 5.1
// ---------------------------------------------------------------------------

#[tokio::test]
async fn integration_late_joiner_receives_share_state() {
    use wavis_backend::channel::invite::{InviteStore, InviteStoreConfig};
    use wavis_backend::voice::mock_sfu_bridge::MockSfuBridge;
    use wavis_backend::voice::sfu_relay::{TokenMode, handle_sfu_join};

    let state = InMemoryRoomState::new();
    let bridge = MockSfuBridge::new();
    let invite_store = InviteStore::new(InviteStoreConfig::default());
    let secret = b"integration-test-secret-32bytes!";
    let token_mode = TokenMode::Custom {
        jwt_secret: secret,
        issuer: wavis_backend::auth::jwt::DEFAULT_JWT_ISSUER,
        ttl_secs: wavis_backend::auth::jwt::TOKEN_TTL_SECS,
    };

    // peer-0 and peer-1 join
    let _ = handle_sfu_join(
        &bridge,
        &state,
        "room-1",
        "peer-0",
        "peer-0",
        None,
        &token_mode,
        "sfu://test",
        6,
        &invite_store,
        None,
    )
    .await
    .unwrap();
    let _ = handle_sfu_join(
        &bridge,
        &state,
        "room-1",
        "peer-1",
        "peer-1",
        None,
        &token_mode,
        "sfu://test",
        6,
        &invite_store,
        None,
    )
    .await
    .unwrap();

    // peer-0 and peer-1 start sharing
    handle_start_share(&state, "room-1", "peer-0", ParticipantRole::Guest);
    handle_start_share(&state, "room-1", "peer-1", ParticipantRole::Guest);

    // peer-2 joins late
    let signals = handle_sfu_join(
        &bridge,
        &state,
        "room-1",
        "peer-2",
        "peer-2",
        None,
        &token_mode,
        "sfu://test",
        6,
        &invite_store,
        None,
    )
    .await
    .unwrap();

    // Find the ShareState signal targeted to peer-2
    let share_state_signals: Vec<&OutboundSignal> = signals
        .iter()
        .filter(|s| {
            matches!(&s.msg, SignalingMessage::ShareState(_))
                && matches!(&s.target, SignalTarget::Peer(pid) if pid == "peer-2")
        })
        .collect();

    assert_eq!(
        share_state_signals.len(),
        1,
        "should have exactly one ShareState for late joiner"
    );

    if let SignalingMessage::ShareState(ShareStatePayload { participant_ids }) =
        &share_state_signals[0].msg
    {
        let received_set: HashSet<String> = participant_ids.iter().cloned().collect();
        let mut expected = HashSet::new();
        expected.insert("peer-0".to_string());
        expected.insert("peer-1".to_string());
        assert_eq!(
            received_set, expected,
            "ShareState should contain both active sharers"
        );
    } else {
        panic!("expected ShareState message");
    }
}

// ---------------------------------------------------------------------------
// Test: Disconnect cleans up shares
// Validates: Requirements 1.5, 9.1
// ---------------------------------------------------------------------------

#[test]
fn integration_disconnect_cleans_up_shares() {
    let state = InMemoryRoomState::new();
    make_sfu_room(&state, "room-1", &["peer-0", "peer-1", "peer-2"]);

    // peer-1 starts sharing
    handle_start_share(&state, "room-1", "peer-1", ParticipantRole::Guest);

    let shares = get_active_shares(&state, "room-1");
    assert!(shares.contains("peer-1"));

    // peer-1 disconnects
    let signals = cleanup_share_on_disconnect(&state, "room-1", "peer-1");
    assert!(
        signals.is_some(),
        "should produce ShareStopped for active sharer"
    );
    let signals = signals.unwrap();
    assert!(signals_contain_share_stopped(&signals, "peer-1"));

    // Verify peer-1 is no longer in active_shares
    let shares = get_active_shares(&state, "room-1");
    assert!(!shares.contains("peer-1"));

    // Non-sharer disconnect produces no signals
    let signals2 = cleanup_share_on_disconnect(&state, "room-1", "peer-2");
    assert!(
        signals2.is_none(),
        "should produce no signals for non-sharer disconnect"
    );
}

// ---------------------------------------------------------------------------
// Test: Host stop-all lifecycle
// Validates: Requirements 4.1
// ---------------------------------------------------------------------------

#[test]
fn integration_host_stop_all_lifecycle() {
    let state = InMemoryRoomState::new();
    make_sfu_room(&state, "room-1", &["host", "peer-1", "peer-2"]);

    // peer-1 and peer-2 start sharing
    handle_start_share(&state, "room-1", "peer-1", ParticipantRole::Guest);
    handle_start_share(&state, "room-1", "peer-2", ParticipantRole::Guest);

    let shares = get_active_shares(&state, "room-1");
    assert_eq!(shares.len(), 2);

    // Host stops all shares
    let result = handle_stop_all_shares(&state, "room-1", "host", ParticipantRole::Host);
    match result {
        ShareResult::Ok(signals) => {
            assert_eq!(
                signals.len(),
                2,
                "should produce one ShareStopped per sharer"
            );
            assert!(signals_contain_share_stopped(&signals, "peer-1"));
            assert!(signals_contain_share_stopped(&signals, "peer-2"));
            // All signals should be BroadcastAll
            for signal in &signals {
                assert!(matches!(signal.target, SignalTarget::BroadcastAll));
            }
        }
        _ => panic!("expected Ok for host stop-all"),
    }

    // Verify all shares cleared
    let shares = get_active_shares(&state, "room-1");
    assert!(
        shares.is_empty(),
        "active_shares should be empty after stop-all"
    );
}
