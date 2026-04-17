//! Integration test: P2P relay through real domain functions + real InMemoryRoomState
//!
//! This is the P2P equivalent of `sfu_relay_integration.rs`. It exercises the
//! actual `handle_p2p_join` → `relay_signaling` → `handle_disconnect` chain
//! with a real `InMemoryRoomState`, verifying that:
//!   - RoomInfo is created on join (room type discoverable)
//!   - Offer/Answer/ICE relay correctly between two peers
//!   - Disconnect notifies the remaining peer and cleans up state
//!   - Third peer is rejected (capacity = 2)
//!   - Room info is cleaned up when both peers leave
//!
//! Requirements: 1.1, 1.2, 1.3, 1.4, 1.5, 2.1, 2.2, 6.3

use shared::signaling::{
    AnswerPayload, IceCandidate, IceCandidatePayload, OfferPayload, SessionDescription,
    SignalingMessage,
};
use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use wavis_backend::channel::invite::InviteStore;
use wavis_backend::state::{InMemoryRoomState, RoomType};
use wavis_backend::voice::relay::{
    P2PJoinResult, RelayResult, RoomState, handle_disconnect, handle_p2p_join, relay_signaling,
};
use wavis_backend::voice::sfu_relay::{OutboundSignal, SignalTarget};

/// Convenience wrapper: join with a no-op dummy InviteStore (pre-task-11 wiring).
fn p2p_join(state: &InMemoryRoomState, room_id: &str, peer_id: &str) -> P2PJoinResult {
    let dummy = InviteStore::default();
    handle_p2p_join(state, room_id, peer_id, &dummy, None)
}

// ---------------------------------------------------------------------------
// Helpers (mirrors sfu_relay_integration.rs pattern)
// ---------------------------------------------------------------------------

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
// Test: First peer joins — receives Joined, RoomInfo created
// ---------------------------------------------------------------------------

#[test]
fn first_peer_join_creates_room_info_and_receives_joined() {
    let state = InMemoryRoomState::new();
    let conns = TestConnections::new();

    let result = p2p_join(&state, "room-1", "alice");
    let signals = match result {
        P2PJoinResult::Joined(s) => s,
        P2PJoinResult::RoomFull => panic!("first join should not be RoomFull"),
        P2PJoinResult::InviteRejected(r) => panic!("unexpected InviteRejected: {r:?}"),
    };
    dispatch(signals, "room-1", &state, &conns);

    // Alice receives Joined with peer_count=1
    let msgs = conns.take("alice");
    assert_eq!(msgs.len(), 1);
    match &msgs[0] {
        SignalingMessage::Joined(payload) => {
            assert_eq!(payload.room_id, "room-1");
            assert_eq!(payload.peer_id, "alice");
            assert_eq!(payload.peer_count, 1);
            assert!(payload.participants.is_empty());
        }
        other => panic!("expected Joined, got {other:?}"),
    }

    // RoomInfo exists and is P2P
    let info = state
        .get_room_info("room-1")
        .expect("RoomInfo should exist after join");
    assert_eq!(info.room_type, RoomType::P2P);
    assert_eq!(info.max_participants, 2);
}

// ---------------------------------------------------------------------------
// Test: Second peer joins — both receive Joined with peer_count=2
// ---------------------------------------------------------------------------

#[test]
fn second_peer_join_notifies_both_peers() {
    let state = InMemoryRoomState::new();
    let conns = TestConnections::new();

    // Alice joins
    let s1 = match p2p_join(&state, "room-1", "alice") {
        P2PJoinResult::Joined(s) => s,
        _ => panic!("unexpected join failure"),
    };
    dispatch(s1, "room-1", &state, &conns);
    conns.take("alice"); // clear

    // Bob joins
    let s2 = match p2p_join(&state, "room-1", "bob") {
        P2PJoinResult::Joined(s) => s,
        _ => panic!("unexpected join failure"),
    };
    dispatch(s2, "room-1", &state, &conns);

    // Bob receives Joined with peer_count=2
    let bob_msgs = conns.take("bob");
    assert_eq!(bob_msgs.len(), 1);
    match &bob_msgs[0] {
        SignalingMessage::Joined(p) => {
            assert_eq!(p.peer_id, "bob");
            assert_eq!(p.peer_count, 2);
        }
        other => panic!("expected Joined for bob, got {other:?}"),
    }

    // Alice receives updated Joined with peer_count=2
    let alice_msgs = conns.take("alice");
    assert_eq!(alice_msgs.len(), 1);
    match &alice_msgs[0] {
        SignalingMessage::Joined(p) => {
            assert_eq!(p.peer_id, "alice");
            assert_eq!(p.peer_count, 2);
        }
        other => panic!("expected Joined for alice, got {other:?}"),
    }
}

// ---------------------------------------------------------------------------
// Test: Third peer rejected (capacity = 2)
// ---------------------------------------------------------------------------

#[test]
fn third_peer_join_rejected_room_full() {
    let state = InMemoryRoomState::new();

    p2p_join(&state, "room-1", "alice");
    p2p_join(&state, "room-1", "bob");

    let result = p2p_join(&state, "room-1", "charlie");
    assert!(
        matches!(result, P2PJoinResult::RoomFull),
        "third peer should be rejected"
    );

    // Room still has exactly 2 peers
    assert_eq!(state.peer_count("room-1"), 2);
}

/// Full P2P flow: join → offer → answer → ICE → disconnect, using real
/// domain functions and real InMemoryRoomState.
#[test]
fn full_p2p_signaling_flow_with_real_state() {
    let state = InMemoryRoomState::new();
    let conns = TestConnections::new();

    // 1. Both peers join
    let s1 = match p2p_join(&state, "room-1", "alice") {
        P2PJoinResult::Joined(s) => s,
        _ => panic!("join failed"),
    };
    dispatch(s1, "room-1", &state, &conns);
    conns.take("alice"); // clear

    let s2 = match p2p_join(&state, "room-1", "bob") {
        P2PJoinResult::Joined(s) => s,
        _ => panic!("join failed"),
    };
    dispatch(s2, "room-1", &state, &conns);
    conns.take("alice");
    conns.take("bob");

    // Verify room info exists and is P2P
    let info = state.get_room_info("room-1").expect("RoomInfo must exist");
    assert_eq!(info.room_type, RoomType::P2P);

    // 2. Alice sends Offer → Bob receives it
    let offer = SignalingMessage::Offer(OfferPayload {
        session_description: SessionDescription {
            sdp: "v=0\r\noffer-sdp".to_string(),
            sdp_type: "offer".to_string(),
        },
    });
    match relay_signaling(&state, "alice", offer.clone()) {
        RelayResult::Relayed {
            target_peer_id,
            message,
        } => {
            assert_eq!(target_peer_id, "bob");
            assert_eq!(message, offer);
        }
        RelayResult::NoPeer { .. } => panic!("relay should succeed"),
    }

    // 3. Bob sends Answer → Alice receives it
    let answer = SignalingMessage::Answer(AnswerPayload {
        session_description: SessionDescription {
            sdp: "v=0\r\nanswer-sdp".to_string(),
            sdp_type: "answer".to_string(),
        },
    });
    match relay_signaling(&state, "bob", answer.clone()) {
        RelayResult::Relayed {
            target_peer_id,
            message,
        } => {
            assert_eq!(target_peer_id, "alice");
            assert_eq!(message, answer);
        }
        RelayResult::NoPeer { .. } => panic!("relay should succeed"),
    }

    // 4. Both exchange ICE candidates
    let ice = SignalingMessage::IceCandidate(IceCandidatePayload {
        candidate: IceCandidate {
            candidate: "candidate:1 1 udp 2130706431 192.168.1.1 5000 typ host".to_string(),
            sdp_mid: "0".to_string(),
            sdp_mline_index: 0,
        },
    });
    match relay_signaling(&state, "alice", ice.clone()) {
        RelayResult::Relayed {
            target_peer_id,
            message,
        } => {
            assert_eq!(target_peer_id, "bob");
            assert_eq!(message, ice);
        }
        RelayResult::NoPeer { .. } => panic!("ICE relay should succeed"),
    }

    // 5. Alice disconnects → Bob gets PeerLeft
    let disconnect_result = handle_disconnect(&state, "alice");
    match disconnect_result {
        Some((target, msg)) => {
            assert_eq!(target, "bob");
            assert_eq!(msg, SignalingMessage::PeerLeft);
        }
        None => panic!("disconnect should notify bob"),
    }

    // 6. Clean up state (as ws.rs does)
    state.remove_peer("alice");

    // Bob is still in the room
    assert_eq!(state.peer_count("room-1"), 1);

    // Bob leaves
    state.remove_peer("bob");

    // Room is fully cleaned up (room_info removed when last peer leaves)
    assert_eq!(state.peer_count("room-1"), 0);
    assert!(
        state.get_room_info("room-1").is_none(),
        "RoomInfo should be cleaned up"
    );
}

/// Disconnect when alone in room returns None (no one to notify).
#[test]
fn disconnect_alone_returns_none() {
    let state = InMemoryRoomState::new();
    p2p_join(&state, "room-1", "alice");

    let result = handle_disconnect(&state, "alice");
    assert!(result.is_none());
}

/// Relay when not in a room returns NoPeer error.
#[test]
fn relay_without_room_returns_error() {
    let state = InMemoryRoomState::new();

    let offer = SignalingMessage::Offer(OfferPayload {
        session_description: SessionDescription {
            sdp: "test".to_string(),
            sdp_type: "offer".to_string(),
        },
    });
    match relay_signaling(&state, "ghost", offer) {
        RelayResult::NoPeer { error } => {
            assert!(matches!(error, SignalingMessage::Error(_)));
        }
        RelayResult::Relayed { .. } => panic!("should not relay"),
    }
}

/// Relay when alone in room returns NoPeer error.
#[test]
fn relay_alone_returns_no_peer() {
    let state = InMemoryRoomState::new();
    p2p_join(&state, "room-1", "alice");

    let offer = SignalingMessage::Offer(OfferPayload {
        session_description: SessionDescription {
            sdp: "test".to_string(),
            sdp_type: "offer".to_string(),
        },
    });
    match relay_signaling(&state, "alice", offer) {
        RelayResult::NoPeer { error } => match error {
            SignalingMessage::Error(p) => assert_eq!(p.message, "no peer available"),
            _ => panic!("expected Error variant"),
        },
        RelayResult::Relayed { .. } => panic!("should not relay when alone"),
    }
}

/// Full P2P lifecycle: join → offer → answer → ICE → disconnect → cleanup.
///
/// This is the critical integration test that mirrors what happens in a real
/// P2P call through the backend. It exercises the complete chain:
/// handle_p2p_join → relay_signaling (offer/answer/ICE) → handle_disconnect → remove_peer.
#[test]
fn full_p2p_lifecycle_join_relay_disconnect_cleanup() {
    let state = InMemoryRoomState::new();
    let conns = TestConnections::new();

    // --- Join phase ---
    match p2p_join(&state, "room-1", "peer-1") {
        P2PJoinResult::Joined(s) => dispatch(s, "room-1", &state, &conns),
        _ => panic!("join 1 failed"),
    }
    match p2p_join(&state, "room-1", "peer-2") {
        P2PJoinResult::Joined(s) => dispatch(s, "room-1", &state, &conns),
        _ => panic!("join 2 failed"),
    }
    assert_eq!(state.peer_count("room-1"), 2);
    conns.take("peer-1");
    conns.take("peer-2");

    // --- Offer: peer-2 → peer-1 ---
    let offer = SignalingMessage::Offer(OfferPayload {
        session_description: SessionDescription {
            sdp: "v=0\r\noffer-sdp".to_string(),
            sdp_type: "offer".to_string(),
        },
    });
    match relay_signaling(&state, "peer-2", offer.clone()) {
        RelayResult::Relayed {
            target_peer_id,
            message,
        } => {
            assert_eq!(target_peer_id, "peer-1");
            assert_eq!(message, offer);
        }
        RelayResult::NoPeer { .. } => panic!("offer relay failed"),
    }

    // --- Answer: peer-1 → peer-2 ---
    let answer = SignalingMessage::Answer(AnswerPayload {
        session_description: SessionDescription {
            sdp: "v=0\r\nanswer-sdp".to_string(),
            sdp_type: "answer".to_string(),
        },
    });
    match relay_signaling(&state, "peer-1", answer.clone()) {
        RelayResult::Relayed {
            target_peer_id,
            message,
        } => {
            assert_eq!(target_peer_id, "peer-2");
            assert_eq!(message, answer);
        }
        RelayResult::NoPeer { .. } => panic!("answer relay failed"),
    }

    // --- ICE candidates: bidirectional ---
    let ice = SignalingMessage::IceCandidate(IceCandidatePayload {
        candidate: IceCandidate {
            candidate: "candidate:1 1 UDP 2130706431 192.168.1.1 5000 typ host".to_string(),
            sdp_mid: "0".to_string(),
            sdp_mline_index: 0,
        },
    });
    match relay_signaling(&state, "peer-1", ice.clone()) {
        RelayResult::Relayed { target_peer_id, .. } => assert_eq!(target_peer_id, "peer-2"),
        _ => panic!("ICE relay 1→2 failed"),
    }
    match relay_signaling(&state, "peer-2", ice.clone()) {
        RelayResult::Relayed { target_peer_id, .. } => assert_eq!(target_peer_id, "peer-1"),
        _ => panic!("ICE relay 2→1 failed"),
    }

    // --- Disconnect: peer-2 leaves ---
    let dc = handle_disconnect(&state, "peer-2");
    assert!(dc.is_some(), "disconnect should notify peer-1");
    let (target, msg) = dc.unwrap();
    assert_eq!(target, "peer-1");
    assert!(matches!(msg, SignalingMessage::PeerLeft));

    // Simulate handler cleanup (ws.rs does this after the loop)
    state.remove_peer("peer-2");
    assert_eq!(state.peer_count("room-1"), 1);

    // --- peer-1 also leaves ---
    state.remove_peer("peer-1");
    assert_eq!(state.peer_count("room-1"), 0);
    assert_eq!(state.active_room_count(), 0);
    // RoomInfo should be cleaned up when last peer leaves
    assert!(
        state.get_room_info("room-1").is_none(),
        "RoomInfo should be cleaned up when room is empty"
    );
}

/// Relay rejects messages when peer is alone (no one to relay to).
#[test]
fn relay_rejects_when_peer_is_alone() {
    let state = InMemoryRoomState::new();

    match p2p_join(&state, "room-1", "lonely") {
        P2PJoinResult::Joined(_) => {}
        _ => panic!("join failed"),
    }

    let offer = SignalingMessage::Offer(OfferPayload {
        session_description: SessionDescription {
            sdp: "v=0\r\n".to_string(),
            sdp_type: "offer".to_string(),
        },
    });
    match relay_signaling(&state, "lonely", offer) {
        RelayResult::NoPeer { error } => {
            assert!(matches!(error, SignalingMessage::Error(_)));
        }
        RelayResult::Relayed { .. } => panic!("should not relay when alone"),
    }
}

/// Two independent P2P rooms don't interfere with each other.
#[test]
fn independent_p2p_rooms_are_isolated() {
    let state = InMemoryRoomState::new();

    // Room A
    p2p_join(&state, "room-a", "alice");
    p2p_join(&state, "room-a", "bob");

    // Room B
    p2p_join(&state, "room-b", "carol");
    p2p_join(&state, "room-b", "dave");

    assert_eq!(state.active_room_count(), 2);
    assert_eq!(state.total_participant_count(), 4);

    // Relay in room A goes to the right peer
    let offer = SignalingMessage::Offer(OfferPayload {
        session_description: SessionDescription {
            sdp: "v=0\r\n".to_string(),
            sdp_type: "offer".to_string(),
        },
    });
    match relay_signaling(&state, "alice", offer.clone()) {
        RelayResult::Relayed { target_peer_id, .. } => assert_eq!(target_peer_id, "bob"),
        _ => panic!("relay in room-a failed"),
    }
    match relay_signaling(&state, "carol", offer) {
        RelayResult::Relayed { target_peer_id, .. } => assert_eq!(target_peer_id, "dave"),
        _ => panic!("relay in room-b failed"),
    }

    // Disconnect in room A doesn't affect room B
    state.remove_peer("alice");
    state.remove_peer("bob");
    assert_eq!(state.active_room_count(), 1);
    assert_eq!(state.peer_count("room-b"), 2);
}

// ---------------------------------------------------------------------------
// Test: Offer relays from alice to bob
// ---------------------------------------------------------------------------

#[test]
fn offer_relays_to_other_peer() {
    let state = InMemoryRoomState::new();

    p2p_join(&state, "room-1", "alice");
    p2p_join(&state, "room-1", "bob");

    let offer = SignalingMessage::Offer(OfferPayload {
        session_description: SessionDescription {
            sdp: "v=0\r\noffer-sdp".to_string(),
            sdp_type: "offer".to_string(),
        },
    });

    let result = relay_signaling(&state, "alice", offer.clone());
    match result {
        RelayResult::Relayed {
            target_peer_id,
            message,
        } => {
            assert_eq!(target_peer_id, "bob");
            assert_eq!(message, offer);
        }
        RelayResult::NoPeer { .. } => panic!("expected Relayed, got NoPeer"),
    }
}

// ---------------------------------------------------------------------------
// Test: Answer relays from bob to alice
// ---------------------------------------------------------------------------

#[test]
fn answer_relays_to_other_peer() {
    let state = InMemoryRoomState::new();

    p2p_join(&state, "room-1", "alice");
    p2p_join(&state, "room-1", "bob");

    let answer = SignalingMessage::Answer(AnswerPayload {
        session_description: SessionDescription {
            sdp: "v=0\r\nanswer-sdp".to_string(),
            sdp_type: "answer".to_string(),
        },
    });

    let result = relay_signaling(&state, "bob", answer.clone());
    match result {
        RelayResult::Relayed {
            target_peer_id,
            message,
        } => {
            assert_eq!(target_peer_id, "alice");
            assert_eq!(message, answer);
        }
        RelayResult::NoPeer { .. } => panic!("expected Relayed, got NoPeer"),
    }
}

// ---------------------------------------------------------------------------
// Test: ICE candidates relay bidirectionally
// ---------------------------------------------------------------------------

#[test]
fn ice_candidates_relay_bidirectionally() {
    let state = InMemoryRoomState::new();

    p2p_join(&state, "room-1", "alice");
    p2p_join(&state, "room-1", "bob");

    let ice_a = SignalingMessage::IceCandidate(IceCandidatePayload {
        candidate: IceCandidate {
            candidate: "candidate:1 1 udp 2130706431 192.168.1.1 5000 typ host".to_string(),
            sdp_mid: "0".to_string(),
            sdp_mline_index: 0,
        },
    });

    // Alice → Bob
    match relay_signaling(&state, "alice", ice_a.clone()) {
        RelayResult::Relayed {
            target_peer_id,
            message,
        } => {
            assert_eq!(target_peer_id, "bob");
            assert_eq!(message, ice_a);
        }
        RelayResult::NoPeer { .. } => panic!("expected Relayed"),
    }

    let ice_b = SignalingMessage::IceCandidate(IceCandidatePayload {
        candidate: IceCandidate {
            candidate: "candidate:2 1 udp 2130706431 10.0.0.1 6000 typ host".to_string(),
            sdp_mid: "0".to_string(),
            sdp_mline_index: 0,
        },
    });

    // Bob → Alice
    match relay_signaling(&state, "bob", ice_b.clone()) {
        RelayResult::Relayed {
            target_peer_id,
            message,
        } => {
            assert_eq!(target_peer_id, "alice");
            assert_eq!(message, ice_b);
        }
        RelayResult::NoPeer { .. } => panic!("expected Relayed"),
    }
}

// ---------------------------------------------------------------------------
// Test: Relay before second peer joins returns NoPeer
// ---------------------------------------------------------------------------

#[test]
fn relay_before_second_peer_returns_no_peer() {
    let state = InMemoryRoomState::new();

    p2p_join(&state, "room-1", "alice");

    let offer = SignalingMessage::Offer(OfferPayload {
        session_description: SessionDescription {
            sdp: "offer".to_string(),
            sdp_type: "offer".to_string(),
        },
    });

    let result = relay_signaling(&state, "alice", offer);
    match result {
        RelayResult::NoPeer { error } => {
            assert!(matches!(error, SignalingMessage::Error(_)));
        }
        RelayResult::Relayed { .. } => panic!("expected NoPeer when alone in room"),
    }
}

// ---------------------------------------------------------------------------
// Test: Disconnect notifies remaining peer and state cleanup
// ---------------------------------------------------------------------------

#[test]
fn disconnect_notifies_remaining_peer() {
    let state = InMemoryRoomState::new();

    p2p_join(&state, "room-1", "alice");
    p2p_join(&state, "room-1", "bob");

    // Alice disconnects — should notify bob
    let result = handle_disconnect(&state, "alice");
    match result {
        Some((target, msg)) => {
            assert_eq!(target, "bob");
            assert_eq!(msg, SignalingMessage::PeerLeft);
        }
        None => panic!("expected disconnect notification"),
    }

    // Clean up state (handler does this after handle_disconnect)
    state.remove_peer("alice");

    // Bob is still in the room
    assert_eq!(state.peer_count("room-1"), 1);
    assert_eq!(state.get_room_for_peer("bob"), Some("room-1".to_string()));

    // RoomInfo still exists (one peer remains)
    assert!(state.get_room_info("room-1").is_some());
}

// ---------------------------------------------------------------------------
// Test: Both peers leave — room info cleaned up
// ---------------------------------------------------------------------------

#[test]
fn both_peers_leave_cleans_up_room_info() {
    let state = InMemoryRoomState::new();

    p2p_join(&state, "room-1", "alice");
    p2p_join(&state, "room-1", "bob");

    // Verify room info exists
    assert!(state.get_room_info("room-1").is_some());

    // Alice disconnects
    handle_disconnect(&state, "alice");
    state.remove_peer("alice");

    // Bob disconnects
    handle_disconnect(&state, "bob");
    state.remove_peer("bob");

    // Room is fully cleaned up
    assert_eq!(state.peer_count("room-1"), 0);
    assert!(
        state.get_room_info("room-1").is_none(),
        "RoomInfo should be cleaned up when last peer leaves"
    );
    assert!(state.get_peers_in_room(&"room-1".to_string()).is_empty());
}

// ---------------------------------------------------------------------------
// Test: Full lifecycle — join → offer/answer/ICE → disconnect
// ---------------------------------------------------------------------------

#[test]
fn full_p2p_lifecycle_join_relay_disconnect() {
    let state = InMemoryRoomState::new();
    let conns = TestConnections::new();

    // 1. Alice joins
    let s1 = match p2p_join(&state, "room-1", "alice") {
        P2PJoinResult::Joined(s) => s,
        _ => panic!("unexpected join failure"),
    };
    dispatch(s1, "room-1", &state, &conns);
    conns.take("alice");

    // 2. Bob joins — both get Joined
    let s2 = match p2p_join(&state, "room-1", "bob") {
        P2PJoinResult::Joined(s) => s,
        _ => panic!("unexpected join failure"),
    };
    dispatch(s2, "room-1", &state, &conns);
    conns.take("alice");
    conns.take("bob");

    // 3. Alice sends Offer → Bob receives it
    let offer = SignalingMessage::Offer(OfferPayload {
        session_description: SessionDescription {
            sdp: "offer-sdp".to_string(),
            sdp_type: "offer".to_string(),
        },
    });
    match relay_signaling(&state, "alice", offer.clone()) {
        RelayResult::Relayed {
            target_peer_id,
            message,
        } => {
            assert_eq!(target_peer_id, "bob");
            assert_eq!(message, offer);
        }
        RelayResult::NoPeer { .. } => panic!("expected relay"),
    }

    // 4. Bob sends Answer → Alice receives it
    let answer = SignalingMessage::Answer(AnswerPayload {
        session_description: SessionDescription {
            sdp: "answer-sdp".to_string(),
            sdp_type: "answer".to_string(),
        },
    });
    match relay_signaling(&state, "bob", answer.clone()) {
        RelayResult::Relayed {
            target_peer_id,
            message,
        } => {
            assert_eq!(target_peer_id, "alice");
            assert_eq!(message, answer);
        }
        RelayResult::NoPeer { .. } => panic!("expected relay"),
    }

    // 5. Both exchange ICE candidates
    for i in 0..3 {
        let ice = SignalingMessage::IceCandidate(IceCandidatePayload {
            candidate: IceCandidate {
                candidate: format!("candidate:{i}"),
                sdp_mid: "0".to_string(),
                sdp_mline_index: 0,
            },
        });
        assert!(matches!(
            relay_signaling(&state, "alice", ice),
            RelayResult::Relayed { .. }
        ));
    }

    // 6. Alice disconnects → Bob gets PeerLeft
    let (target, msg) = handle_disconnect(&state, "alice").expect("should notify bob");
    assert_eq!(target, "bob");
    assert_eq!(msg, SignalingMessage::PeerLeft);
    state.remove_peer("alice");

    // 7. Bob disconnects — room fully cleaned up
    assert!(
        handle_disconnect(&state, "bob").is_none(),
        "bob is alone, no one to notify"
    );
    state.remove_peer("bob");

    assert_eq!(state.peer_count("room-1"), 0);
    assert!(state.get_room_info("room-1").is_none());
}

// ---------------------------------------------------------------------------
// Test: RoomInfo.room_type is discoverable after P2P join (regression)
// ---------------------------------------------------------------------------

#[test]
fn room_type_discoverable_after_p2p_join() {
    let state = InMemoryRoomState::new();

    p2p_join(&state, "room-1", "alice");

    // This is the critical check: the handler uses get_room_info() to determine
    // room type for subsequent Offer/Answer/ICE messages. If handle_p2p_join
    // doesn't call create_room(), this returns None and the handler can't route.
    let info = state
        .get_room_info("room-1")
        .expect("RoomInfo must exist after handle_p2p_join — handler depends on this for routing");
    assert_eq!(info.room_type, RoomType::P2P);
}

// ---------------------------------------------------------------------------
// Test: Independent rooms don't interfere
// ---------------------------------------------------------------------------

#[test]
fn independent_rooms_do_not_interfere() {
    let state = InMemoryRoomState::new();

    // Room A
    p2p_join(&state, "room-a", "alice");
    p2p_join(&state, "room-a", "bob");

    // Room B
    p2p_join(&state, "room-b", "carol");
    p2p_join(&state, "room-b", "dave");

    // Relay in room A doesn't leak to room B
    let offer = SignalingMessage::Offer(OfferPayload {
        session_description: SessionDescription {
            sdp: "room-a-offer".to_string(),
            sdp_type: "offer".to_string(),
        },
    });
    match relay_signaling(&state, "alice", offer) {
        RelayResult::Relayed { target_peer_id, .. } => {
            assert_eq!(target_peer_id, "bob", "should relay to bob, not carol/dave");
        }
        RelayResult::NoPeer { .. } => panic!("expected relay"),
    }

    // Relay in room B
    let offer_b = SignalingMessage::Offer(OfferPayload {
        session_description: SessionDescription {
            sdp: "room-b-offer".to_string(),
            sdp_type: "offer".to_string(),
        },
    });
    match relay_signaling(&state, "carol", offer_b) {
        RelayResult::Relayed { target_peer_id, .. } => {
            assert_eq!(target_peer_id, "dave");
        }
        RelayResult::NoPeer { .. } => panic!("expected relay"),
    }

    // Disconnect in room A doesn't affect room B
    handle_disconnect(&state, "alice");
    state.remove_peer("alice");

    assert_eq!(state.peer_count("room-b"), 2);
    assert!(state.get_room_info("room-b").is_some());
}

// ---------------------------------------------------------------------------
// Test: Rejoin after disconnect works
// ---------------------------------------------------------------------------

#[test]
fn rejoin_after_disconnect_works() {
    let state = InMemoryRoomState::new();

    // First session
    p2p_join(&state, "room-1", "alice");
    p2p_join(&state, "room-1", "bob");

    // Both leave
    state.remove_peer("alice");
    state.remove_peer("bob");
    assert!(state.get_room_info("room-1").is_none());

    // New session — same room
    let result = p2p_join(&state, "room-1", "carol");
    assert!(matches!(result, P2PJoinResult::Joined(_)));

    let info = state
        .get_room_info("room-1")
        .expect("RoomInfo should be recreated");
    assert_eq!(info.room_type, RoomType::P2P);
    assert_eq!(state.peer_count("room-1"), 1);
}
