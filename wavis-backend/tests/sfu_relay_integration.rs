#![cfg(feature = "test-support")]
//! Integration tests: SFU multi-party join/leave flow
//!
//! Exercises `handle_sfu_join` and `handle_sfu_leave` domain functions end-to-end
//! using `MockSfuBridge` and a real `InMemoryRoomState`. Verifies:
//!   - Each joiner receives Joined + MediaToken (+ RoomState if late joiner)
//!   - ParticipantJoined is broadcast to existing participants
//!   - Participants leaving trigger ParticipantLeft broadcasts
//!   - Last leave triggers destroy_room on the SFU bridge
//!   - Join at capacity returns an error
//!
//! Requirements: 2.1, 2.2, 2.3, 2.4, 2.8, 2.10

use shared::signaling::SignalingMessage;
use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use wavis_backend::channel::invite::InviteStore;
use wavis_backend::state::InMemoryRoomState;
use wavis_backend::voice::mock_sfu_bridge::{MockSfuBridge, MockSfuCall};
use wavis_backend::voice::relay::RoomState;
use wavis_backend::voice::sfu_bridge::SfuError;
use wavis_backend::voice::sfu_relay::{
    OutboundSignal, SignalTarget, TokenMode, handle_sfu_join, handle_sfu_leave,
};

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

fn test_secret() -> Vec<u8> {
    b"integration-test-secret-32bytes!".to_vec()
}

fn custom_mode(secret: &[u8]) -> TokenMode<'_> {
    TokenMode::Custom {
        jwt_secret: secret,
        issuer: wavis_backend::auth::jwt::DEFAULT_JWT_ISSUER,
        ttl_secs: wavis_backend::auth::jwt::TOKEN_TTL_SECS,
    }
}

/// Minimal connection map: records messages sent to each peer.
#[derive(Clone)]
struct TestConnections {
    inboxes: Arc<Mutex<HashMap<String, Vec<SignalingMessage>>>>,
}

impl TestConnections {
    fn new() -> Self {
        Self {
            inboxes: Arc::new(Mutex::new(HashMap::new())),
        }
    }

    fn take(&self, peer_id: &str) -> Vec<SignalingMessage> {
        self.inboxes
            .lock()
            .unwrap()
            .remove(peer_id)
            .unwrap_or_default()
    }

    fn all_for(&self, peer_id: &str) -> Vec<SignalingMessage> {
        self.inboxes
            .lock()
            .unwrap()
            .get(peer_id)
            .cloned()
            .unwrap_or_default()
    }
}

/// Dispatch `OutboundSignal`s to the test connection map, mirroring ws.rs logic.
fn dispatch(
    signals: Vec<OutboundSignal>,
    room_id: &str,
    state: &InMemoryRoomState,
    conns: &TestConnections,
) {
    for signal in signals {
        match signal.target {
            SignalTarget::Peer(peer_id) => {
                conns
                    .inboxes
                    .lock()
                    .unwrap()
                    .entry(peer_id)
                    .or_default()
                    .push(signal.msg);
            }
            SignalTarget::Broadcast { exclude } => {
                let peers = state.get_peers_in_room(&room_id.to_string());
                for peer in peers {
                    if peer != exclude {
                        conns
                            .inboxes
                            .lock()
                            .unwrap()
                            .entry(peer)
                            .or_default()
                            .push(signal.msg.clone());
                    }
                }
            }
            SignalTarget::BroadcastAll => {
                let peers = state.get_peers_in_room(&room_id.to_string());
                for peer in peers {
                    conns
                        .inboxes
                        .lock()
                        .unwrap()
                        .entry(peer)
                        .or_default()
                        .push(signal.msg.clone());
                }
            }
        }
    }
}

// ---------------------------------------------------------------------------
// Task 14.1 — Multi-participant join/leave flow
// ---------------------------------------------------------------------------

#[tokio::test]
async fn first_joiner_receives_joined_and_media_token_no_room_state() {
    let bridge = MockSfuBridge::new();
    let state = InMemoryRoomState::new();
    let conns = TestConnections::new();

    let secret = test_secret();
    let signals = handle_sfu_join(
        &bridge,
        &state,
        "room-1",
        "alice",
        "Alice",
        None,
        &custom_mode(&secret),
        "sfu://localhost",
        4,
        &InviteStore::default(),
        None,
    )
    .await
    .unwrap();

    dispatch(signals, "room-1", &state, &conns);

    let msgs = conns.take("alice");
    // First joiner: Joined + MediaToken + ParticipantJoined broadcast (no one to receive it)
    // Direct messages to alice: Joined, MediaToken
    assert!(
        msgs.iter()
            .any(|m| matches!(m, SignalingMessage::Joined(_))),
        "alice should receive Joined"
    );
    assert!(
        msgs.iter()
            .any(|m| matches!(m, SignalingMessage::MediaToken(_))),
        "alice should receive MediaToken"
    );
    // No RoomState for first joiner
    assert!(
        !msgs
            .iter()
            .any(|m| matches!(m, SignalingMessage::RoomState(_))),
        "first joiner should NOT receive RoomState"
    );
}

#[tokio::test]
async fn second_joiner_receives_room_state_and_first_gets_participant_joined() {
    let bridge = MockSfuBridge::new();
    let state = InMemoryRoomState::new();
    let conns = TestConnections::new();

    let secret = test_secret();

    // Alice joins first
    let s1 = handle_sfu_join(
        &bridge,
        &state,
        "room-1",
        "alice",
        "Alice",
        None,
        &custom_mode(&secret),
        "sfu://localhost",
        4,
        &InviteStore::default(),
        None,
    )
    .await
    .unwrap();
    dispatch(s1, "room-1", &state, &conns);
    conns.take("alice"); // clear alice's inbox

    // Bob joins second
    let s2 = handle_sfu_join(
        &bridge,
        &state,
        "room-1",
        "bob",
        "Bob",
        None,
        &custom_mode(&secret),
        "sfu://localhost",
        4,
        &InviteStore::default(),
        None,
    )
    .await
    .unwrap();
    dispatch(s2, "room-1", &state, &conns);

    // Bob should get Joined + MediaToken + RoomState
    let bob_msgs = conns.take("bob");
    assert!(
        bob_msgs
            .iter()
            .any(|m| matches!(m, SignalingMessage::Joined(_))),
        "bob should receive Joined"
    );
    assert!(
        bob_msgs
            .iter()
            .any(|m| matches!(m, SignalingMessage::MediaToken(_))),
        "bob should receive MediaToken"
    );
    assert!(
        bob_msgs
            .iter()
            .any(|m| matches!(m, SignalingMessage::RoomState(_))),
        "late joiner bob should receive RoomState"
    );

    // Alice should get ParticipantJoined for bob
    let alice_msgs = conns.take("alice");
    assert_eq!(
        alice_msgs.len(),
        1,
        "alice should receive exactly one message"
    );
    assert!(
        matches!(&alice_msgs[0], SignalingMessage::ParticipantJoined(p) if p.participant_id == "bob"),
        "alice should receive ParticipantJoined for bob"
    );
}

#[tokio::test]
async fn three_participants_join_sequentially_all_receive_correct_messages() {
    let bridge = MockSfuBridge::new();
    let state = InMemoryRoomState::new();
    let conns = TestConnections::new();

    let secret = test_secret();
    let peers = [("alice", "Alice"), ("bob", "Bob"), ("carol", "Carol")];

    for (peer_id, display_name) in &peers {
        let signals = handle_sfu_join(
            &bridge,
            &state,
            "room-1",
            peer_id,
            display_name,
            None,
            &custom_mode(&secret),
            "sfu://localhost",
            4,
            &InviteStore::default(),
            None,
        )
        .await
        .unwrap();
        dispatch(signals, "room-1", &state, &conns);
    }

    // Alice (first joiner): Joined + MediaToken, no RoomState
    let alice_msgs = conns.take("alice");
    // alice gets: Joined, MediaToken (direct), then ParticipantJoined×2 (broadcast from bob and carol)
    let alice_joined = alice_msgs
        .iter()
        .filter(|m| matches!(m, SignalingMessage::Joined(_)))
        .count();
    let alice_token = alice_msgs
        .iter()
        .filter(|m| matches!(m, SignalingMessage::MediaToken(_)))
        .count();
    let alice_pj = alice_msgs
        .iter()
        .filter(|m| matches!(m, SignalingMessage::ParticipantJoined(_)))
        .count();
    assert_eq!(alice_joined, 1);
    assert_eq!(alice_token, 1);
    assert_eq!(
        alice_pj, 2,
        "alice should receive ParticipantJoined for bob and carol"
    );

    // Bob (second joiner): Joined + MediaToken + RoomState + ParticipantJoined for carol
    let bob_msgs = conns.take("bob");
    assert!(
        bob_msgs
            .iter()
            .any(|m| matches!(m, SignalingMessage::Joined(_)))
    );
    assert!(
        bob_msgs
            .iter()
            .any(|m| matches!(m, SignalingMessage::MediaToken(_)))
    );
    assert!(
        bob_msgs
            .iter()
            .any(|m| matches!(m, SignalingMessage::RoomState(_)))
    );
    let bob_pj = bob_msgs
        .iter()
        .filter(|m| matches!(m, SignalingMessage::ParticipantJoined(_)))
        .count();
    assert_eq!(
        bob_pj, 1,
        "bob should receive ParticipantJoined for carol only"
    );

    // Carol (third joiner): Joined + MediaToken + RoomState, no ParticipantJoined
    let carol_msgs = conns.take("carol");
    assert!(
        carol_msgs
            .iter()
            .any(|m| matches!(m, SignalingMessage::Joined(_)))
    );
    assert!(
        carol_msgs
            .iter()
            .any(|m| matches!(m, SignalingMessage::MediaToken(_)))
    );
    assert!(
        carol_msgs
            .iter()
            .any(|m| matches!(m, SignalingMessage::RoomState(_)))
    );
    let carol_pj = carol_msgs
        .iter()
        .filter(|m| matches!(m, SignalingMessage::ParticipantJoined(_)))
        .count();
    assert_eq!(
        carol_pj, 0,
        "carol should not receive ParticipantJoined (no one joined after her)"
    );
}

#[tokio::test]
async fn participants_leave_one_by_one_broadcast_participant_left() {
    let bridge = MockSfuBridge::new();
    let state = InMemoryRoomState::new();
    let conns = TestConnections::new();

    let secret = test_secret();

    // 3 participants join
    for (id, name) in [("alice", "Alice"), ("bob", "Bob"), ("carol", "Carol")] {
        let s = handle_sfu_join(
            &bridge,
            &state,
            "room-1",
            id,
            name,
            None,
            &custom_mode(&secret),
            "sfu://localhost",
            4,
            &InviteStore::default(),
            None,
        )
        .await
        .unwrap();
        dispatch(s, "room-1", &state, &conns);
    }
    // Clear all inboxes
    conns.take("alice");
    conns.take("bob");
    conns.take("carol");

    // Alice leaves
    let s = handle_sfu_leave(&bridge, &state, "room-1", "alice")
        .await
        .unwrap();
    dispatch(s, "room-1", &state, &conns);

    // Bob and Carol should receive ParticipantLeft for alice
    let bob_msgs = conns.take("bob");
    assert!(
        bob_msgs.iter().any(
            |m| matches!(m, SignalingMessage::ParticipantLeft(p) if p.participant_id == "alice")
        ),
        "bob should receive ParticipantLeft for alice"
    );
    let carol_msgs = conns.take("carol");
    assert!(
        carol_msgs.iter().any(
            |m| matches!(m, SignalingMessage::ParticipantLeft(p) if p.participant_id == "alice")
        ),
        "carol should receive ParticipantLeft for alice"
    );

    // destroy_room should NOT have been called yet (2 remain)
    let calls = bridge.get_calls();
    assert!(
        !calls
            .iter()
            .any(|c| matches!(c, MockSfuCall::DestroyRoom(_))),
        "destroy_room should not be called while participants remain"
    );

    // Bob leaves
    let s = handle_sfu_leave(&bridge, &state, "room-1", "bob")
        .await
        .unwrap();
    dispatch(s, "room-1", &state, &conns);

    let carol_msgs2 = conns.take("carol");
    assert!(
        carol_msgs2.iter().any(
            |m| matches!(m, SignalingMessage::ParticipantLeft(p) if p.participant_id == "bob")
        ),
        "carol should receive ParticipantLeft for bob"
    );

    // Carol leaves last
    let s = handle_sfu_leave(&bridge, &state, "room-1", "carol")
        .await
        .unwrap();
    dispatch(s, "room-1", &state, &conns);

    // destroy_room should now have been called
    let calls = bridge.get_calls();
    assert!(
        calls
            .iter()
            .any(|c| matches!(c, MockSfuCall::DestroyRoom(_))),
        "destroy_room should be called when last participant leaves"
    );

    // Room should be empty
    assert_eq!(state.peer_count("room-1"), 0);
}

#[tokio::test]
async fn join_at_capacity_returns_room_full_error() {
    let bridge = MockSfuBridge::new();
    let state = InMemoryRoomState::new();

    let secret = test_secret();

    // Fill to capacity (3)
    for (id, name) in [("alice", "Alice"), ("bob", "Bob"), ("carol", "Carol")] {
        handle_sfu_join(
            &bridge,
            &state,
            "room-cap",
            id,
            name,
            None,
            &custom_mode(&secret),
            "sfu://localhost",
            3,
            &InviteStore::default(),
            None,
        )
        .await
        .unwrap();
    }

    // 4th join should fail with RoomFull
    let result = handle_sfu_join(
        &bridge,
        &state,
        "room-cap",
        "dave",
        "Dave",
        None,
        &custom_mode(&secret),
        "sfu://localhost",
        3,
        &InviteStore::default(),
        None,
    )
    .await;

    assert!(
        matches!(result, Err(SfuError::RoomFull)),
        "join at capacity should return RoomFull, got: {result:?}"
    );

    // Room count must still be 3
    assert_eq!(state.peer_count("room-cap"), 3);
}

#[tokio::test]
async fn six_participants_join_and_leave_full_lifecycle() {
    let bridge = MockSfuBridge::new();
    let state = InMemoryRoomState::new();
    let conns = TestConnections::new();

    let secret = test_secret();
    let peers = [
        ("p1", "User1"),
        ("p2", "User2"),
        ("p3", "User3"),
        ("p4", "User4"),
        ("p5", "User5"),
        ("p6", "User6"),
    ];

    // All 6 join
    for (id, name) in &peers {
        let s = handle_sfu_join(
            &bridge,
            &state,
            "room-6",
            id,
            name,
            None,
            &custom_mode(&secret),
            "sfu://localhost",
            6,
            &InviteStore::default(),
            None,
        )
        .await
        .unwrap();
        dispatch(s, "room-6", &state, &conns);
    }

    assert_eq!(state.peer_count("room-6"), 6);

    // 7th join should fail
    let overflow = handle_sfu_join(
        &bridge,
        &state,
        "room-6",
        "p7",
        "User7",
        None,
        &custom_mode(&secret),
        "sfu://localhost",
        6,
        &InviteStore::default(),
        None,
    )
    .await;
    assert!(matches!(overflow, Err(SfuError::RoomFull)));

    // Clear inboxes
    for (id, _) in &peers {
        conns.take(id);
    }

    // All 6 leave
    for (id, _) in &peers {
        let s = handle_sfu_leave(&bridge, &state, "room-6", id)
            .await
            .unwrap();
        dispatch(s, "room-6", &state, &conns);
    }

    assert_eq!(state.peer_count("room-6"), 0);

    // destroy_room called exactly once
    let destroy_count = bridge
        .get_calls()
        .iter()
        .filter(|c| matches!(c, MockSfuCall::DestroyRoom(_)))
        .count();
    assert_eq!(
        destroy_count, 1,
        "destroy_room should be called exactly once"
    );
}

// ---------------------------------------------------------------------------
// Task 14.2 — P2P backward compatibility
// ---------------------------------------------------------------------------

#[tokio::test]
async fn p2p_and_sfu_rooms_coexist_independently() {
    let bridge = MockSfuBridge::new();
    let state = InMemoryRoomState::new();
    let conns = TestConnections::new();

    let secret = test_secret();

    // Create a P2P room by adding peers directly (as the P2P path does in ws.rs)
    state.add_peer("p2p-alice".to_string(), "p2p-room".to_string());
    state.add_peer("p2p-bob".to_string(), "p2p-room".to_string());

    // Create an SFU room via handle_sfu_join
    let s1 = handle_sfu_join(
        &bridge,
        &state,
        "sfu-room",
        "sfu-alice",
        "Alice",
        None,
        &custom_mode(&secret),
        "sfu://localhost",
        4,
        &InviteStore::default(),
        None,
    )
    .await
    .unwrap();
    dispatch(s1, "sfu-room", &state, &conns);

    let s2 = handle_sfu_join(
        &bridge,
        &state,
        "sfu-room",
        "sfu-bob",
        "Bob",
        None,
        &custom_mode(&secret),
        "sfu://localhost",
        4,
        &InviteStore::default(),
        None,
    )
    .await
    .unwrap();
    dispatch(s2, "sfu-room", &state, &conns);

    // P2P room is unaffected
    let p2p_peers = state.get_peers_in_room(&"p2p-room".to_string());
    assert_eq!(p2p_peers.len(), 2);
    assert!(p2p_peers.contains(&"p2p-alice".to_string()));
    assert!(p2p_peers.contains(&"p2p-bob".to_string()));

    // SFU room has its own participants
    assert_eq!(state.peer_count("sfu-room"), 2);

    // P2P peers received no SFU messages
    let p2p_alice_msgs = conns.take("p2p-alice");
    assert!(
        p2p_alice_msgs.is_empty(),
        "P2P peer should not receive SFU messages"
    );

    // SFU peers received their messages
    let sfu_alice_msgs = conns.all_for("sfu-alice");
    assert!(
        sfu_alice_msgs
            .iter()
            .any(|m| matches!(m, SignalingMessage::Joined(_)))
    );

    // Leaving SFU room does not affect P2P room
    handle_sfu_leave(&bridge, &state, "sfu-room", "sfu-alice")
        .await
        .unwrap();
    handle_sfu_leave(&bridge, &state, "sfu-room", "sfu-bob")
        .await
        .unwrap();

    let p2p_peers_after = state.get_peers_in_room(&"p2p-room".to_string());
    assert_eq!(
        p2p_peers_after.len(),
        2,
        "P2P room unaffected by SFU room teardown"
    );
}
