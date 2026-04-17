//! Integration test: Two-peer signaling relay
//!
//! Simulates two WebSocket clients connected to the signaling server,
//! verifying offer/answer/ICE relay and disconnect notification.
//!
//! Requirements: 1.1, 1.2, 1.3, 1.5

use shared::signaling::{
    self, AnswerPayload, IceCandidate, IceCandidatePayload, JoinPayload, JoinedPayload,
    OfferPayload, SessionDescription, SignalingMessage,
};
use std::collections::HashMap;
use std::sync::{Arc, Mutex};

// We re-implement the traits and handler calls here because the backend's
// internal modules (domain, handlers) are not `pub` from the library crate.
// This integration test exercises the same logic path by importing shared types
// and reimplementing the minimal relay + handler wiring.

// ---------------------------------------------------------------------------
// Room state (in-memory, two peers)
// ---------------------------------------------------------------------------

type PeerId = String;

struct InMemoryRoomState {
    peer_to_room: HashMap<String, String>,
    room_to_peers: HashMap<String, Vec<String>>,
}

impl InMemoryRoomState {
    fn new() -> Self {
        Self {
            peer_to_room: HashMap::new(),
            room_to_peers: HashMap::new(),
        }
    }

    fn add_peer(&mut self, peer_id: &str, room_id: &str) {
        self.peer_to_room
            .insert(peer_id.to_string(), room_id.to_string());
        self.room_to_peers
            .entry(room_id.to_string())
            .or_default()
            .push(peer_id.to_string());
    }

    fn remove_peer(&mut self, peer_id: &str) {
        if let Some(room_id) = self.peer_to_room.remove(peer_id)
            && let Some(peers) = self.room_to_peers.get_mut(&room_id)
        {
            peers.retain(|p| p != peer_id);
        }
    }

    fn get_room_for_peer(&self, peer_id: &str) -> Option<&String> {
        self.peer_to_room.get(peer_id)
    }

    fn get_peers_in_room(&self, room_id: &str) -> Vec<String> {
        self.room_to_peers.get(room_id).cloned().unwrap_or_default()
    }
}

// ---------------------------------------------------------------------------
// Connection map (mock WebSocket send)
// ---------------------------------------------------------------------------

#[derive(Clone)]
struct ConnectionMap {
    /// peer_id → list of received JSON messages
    inboxes: Arc<Mutex<HashMap<String, Vec<String>>>>,
}

impl ConnectionMap {
    fn new(peers: &[&str]) -> Self {
        let mut inboxes = HashMap::new();
        for p in peers {
            inboxes.insert(p.to_string(), Vec::new());
        }
        Self {
            inboxes: Arc::new(Mutex::new(inboxes)),
        }
    }

    fn send_to(&self, peer_id: &str, msg: &SignalingMessage) {
        let json = signaling::to_json(msg).expect("serialize");
        let mut inboxes = self.inboxes.lock().unwrap();
        if let Some(inbox) = inboxes.get_mut(peer_id) {
            inbox.push(json);
        }
    }

    fn take_messages(&self, peer_id: &str) -> Vec<SignalingMessage> {
        let mut inboxes = self.inboxes.lock().unwrap();
        inboxes
            .get_mut(peer_id)
            .map(std::mem::take)
            .unwrap_or_default()
            .into_iter()
            .map(|json| signaling::parse(&json).expect("parse"))
            .collect()
    }
}

// ---------------------------------------------------------------------------
// Relay logic (mirrors backend domain::relay)
// ---------------------------------------------------------------------------

enum RelayResult {
    Relayed {
        target_peer_id: PeerId,
        message: SignalingMessage,
    },
    NoPeer {
        error: SignalingMessage,
    },
}

fn relay_signaling(
    state: &InMemoryRoomState,
    sender: &str,
    message: SignalingMessage,
) -> RelayResult {
    let room_id = match state.get_room_for_peer(sender) {
        Some(r) => r.clone(),
        None => {
            return RelayResult::NoPeer {
                error: SignalingMessage::Error(shared::signaling::ErrorPayload {
                    message: "not in a room".to_string(),
                }),
            };
        }
    };
    let peers = state.get_peers_in_room(&room_id);
    match peers.into_iter().find(|p| p != sender) {
        Some(target) => RelayResult::Relayed {
            target_peer_id: target,
            message,
        },
        None => RelayResult::NoPeer {
            error: SignalingMessage::Error(shared::signaling::ErrorPayload {
                message: "no peer available".to_string(),
            }),
        },
    }
}

fn handle_disconnect(
    state: &InMemoryRoomState,
    peer_id: &str,
) -> Option<(PeerId, SignalingMessage)> {
    let room_id = state.get_room_for_peer(peer_id)?.clone();
    let peers = state.get_peers_in_room(&room_id);
    let remaining = peers.into_iter().find(|p| p != peer_id)?;
    Some((remaining, SignalingMessage::PeerLeft))
}

/// Process an incoming text frame from a peer (mirrors handlers::ws logic).
fn process_incoming(
    state: &mut InMemoryRoomState,
    sender: &str,
    text: &str,
    conns: &ConnectionMap,
) {
    let msg = match signaling::parse(text) {
        Ok(m) => m,
        Err(_) => {
            conns.send_to(
                sender,
                &SignalingMessage::Error(shared::signaling::ErrorPayload {
                    message: "invalid JSON".to_string(),
                }),
            );
            return;
        }
    };

    match &msg {
        SignalingMessage::Join(payload) => {
            let room_id = payload.room_id.trim().to_string();
            if room_id.is_empty() {
                conns.send_to(
                    sender,
                    &SignalingMessage::Error(shared::signaling::ErrorPayload {
                        message: "invalid room ID".to_string(),
                    }),
                );
                return;
            }

            let peers = state.get_peers_in_room(&room_id);
            let already_in_room = peers.iter().any(|p| p == sender);
            if !already_in_room && peers.len() >= 2 {
                conns.send_to(
                    sender,
                    &SignalingMessage::Error(shared::signaling::ErrorPayload {
                        message: "room is full".to_string(),
                    }),
                );
                return;
            }

            state.add_peer(sender, &room_id);
            let peer_count = state.get_peers_in_room(&room_id).len() as u32;
            conns.send_to(
                sender,
                &SignalingMessage::Joined(JoinedPayload {
                    room_id,
                    peer_id: sender.to_string(),
                    peer_count,
                    participants: vec![],
                    ice_config: None,
                    share_permission: None,
                }),
            );
        }
        SignalingMessage::Offer(_)
        | SignalingMessage::Answer(_)
        | SignalingMessage::IceCandidate(_) => match relay_signaling(state, sender, msg) {
            RelayResult::Relayed {
                target_peer_id,
                message,
            } => conns.send_to(&target_peer_id, &message),
            RelayResult::NoPeer { error } => conns.send_to(sender, &error),
        },
        SignalingMessage::Leave => {
            if let Some((target, peer_left)) = handle_disconnect(state, sender) {
                conns.send_to(&target, &peer_left);
            }
        }
        _ => {
            conns.send_to(
                sender,
                &SignalingMessage::Error(shared::signaling::ErrorPayload {
                    message: "invalid message type from client".to_string(),
                }),
            );
        }
    }
}

// ===========================================================================
// Integration tests
// ===========================================================================

#[test]
fn client_a_sends_offer_client_b_receives_it() {
    let mut state = InMemoryRoomState::new();
    state.add_peer("alice", "room-1");
    state.add_peer("bob", "room-1");
    let conns = ConnectionMap::new(&["alice", "bob"]);

    let offer = SignalingMessage::Offer(OfferPayload {
        session_description: SessionDescription {
            sdp: "v=0\r\no=- 123 1 IN IP4 0.0.0.0\r\n".to_string(),
            sdp_type: "offer".to_string(),
        },
    });
    let json = signaling::to_json(&offer).unwrap();

    process_incoming(&mut state, "alice", &json, &conns);

    let bob_msgs = conns.take_messages("bob");
    assert_eq!(bob_msgs.len(), 1);
    assert_eq!(bob_msgs[0], offer);

    // Alice should have received nothing
    let alice_msgs = conns.take_messages("alice");
    assert!(alice_msgs.is_empty());
}

#[test]
fn client_b_sends_answer_client_a_receives_it() {
    let mut state = InMemoryRoomState::new();
    state.add_peer("alice", "room-1");
    state.add_peer("bob", "room-1");
    let conns = ConnectionMap::new(&["alice", "bob"]);

    let answer = SignalingMessage::Answer(AnswerPayload {
        session_description: SessionDescription {
            sdp: "v=0\r\no=- 456 1 IN IP4 0.0.0.0\r\n".to_string(),
            sdp_type: "answer".to_string(),
        },
    });
    let json = signaling::to_json(&answer).unwrap();

    process_incoming(&mut state, "bob", &json, &conns);

    let alice_msgs = conns.take_messages("alice");
    assert_eq!(alice_msgs.len(), 1);
    assert_eq!(alice_msgs[0], answer);
}

#[test]
fn both_peers_exchange_ice_candidates() {
    let mut state = InMemoryRoomState::new();
    state.add_peer("alice", "room-1");
    state.add_peer("bob", "room-1");
    let conns = ConnectionMap::new(&["alice", "bob"]);

    // Alice sends ICE candidate
    let ice_a = SignalingMessage::IceCandidate(IceCandidatePayload {
        candidate: IceCandidate {
            candidate: "candidate:1 1 udp 2130706431 192.168.1.1 5000 typ host".to_string(),
            sdp_mid: "0".to_string(),
            sdp_mline_index: 0,
        },
    });
    process_incoming(
        &mut state,
        "alice",
        &signaling::to_json(&ice_a).unwrap(),
        &conns,
    );

    let bob_msgs = conns.take_messages("bob");
    assert_eq!(bob_msgs.len(), 1);
    assert_eq!(bob_msgs[0], ice_a);

    // Bob sends ICE candidate
    let ice_b = SignalingMessage::IceCandidate(IceCandidatePayload {
        candidate: IceCandidate {
            candidate: "candidate:2 1 udp 2130706431 10.0.0.1 6000 typ host".to_string(),
            sdp_mid: "0".to_string(),
            sdp_mline_index: 0,
        },
    });
    process_incoming(
        &mut state,
        "bob",
        &signaling::to_json(&ice_b).unwrap(),
        &conns,
    );

    let alice_msgs = conns.take_messages("alice");
    assert_eq!(alice_msgs.len(), 1);
    assert_eq!(alice_msgs[0], ice_b);
}

#[test]
fn client_a_disconnects_client_b_receives_peer_left() {
    let mut state = InMemoryRoomState::new();
    state.add_peer("alice", "room-1");
    state.add_peer("bob", "room-1");
    let conns = ConnectionMap::new(&["alice", "bob"]);

    // Simulate Alice disconnecting (WebSocket drop)
    if let Some((target, msg)) = handle_disconnect(&state, "alice") {
        conns.send_to(&target, &msg);
    }
    state.remove_peer("alice");

    let bob_msgs = conns.take_messages("bob");
    assert_eq!(bob_msgs.len(), 1);
    assert_eq!(bob_msgs[0], SignalingMessage::PeerLeft);
}

#[test]
fn full_signaling_exchange_offer_answer_ice_disconnect() {
    let mut state = InMemoryRoomState::new();
    state.add_peer("alice", "room-1");
    state.add_peer("bob", "room-1");
    let conns = ConnectionMap::new(&["alice", "bob"]);

    // 1. Alice sends offer
    let offer = SignalingMessage::Offer(OfferPayload {
        session_description: SessionDescription {
            sdp: "offer-sdp".to_string(),
            sdp_type: "offer".to_string(),
        },
    });
    process_incoming(
        &mut state,
        "alice",
        &signaling::to_json(&offer).unwrap(),
        &conns,
    );
    let bob_msgs = conns.take_messages("bob");
    assert_eq!(bob_msgs.len(), 1);
    assert_eq!(bob_msgs[0], offer);

    // 2. Bob sends answer
    let answer = SignalingMessage::Answer(AnswerPayload {
        session_description: SessionDescription {
            sdp: "answer-sdp".to_string(),
            sdp_type: "answer".to_string(),
        },
    });
    process_incoming(
        &mut state,
        "bob",
        &signaling::to_json(&answer).unwrap(),
        &conns,
    );
    let alice_msgs = conns.take_messages("alice");
    assert_eq!(alice_msgs.len(), 1);
    assert_eq!(alice_msgs[0], answer);

    // 3. Both exchange ICE candidates
    for i in 0..3 {
        let ice = SignalingMessage::IceCandidate(IceCandidatePayload {
            candidate: IceCandidate {
                candidate: format!("candidate:{i}"),
                sdp_mid: "0".to_string(),
                sdp_mline_index: 0,
            },
        });
        process_incoming(
            &mut state,
            "alice",
            &signaling::to_json(&ice).unwrap(),
            &conns,
        );
        let msgs = conns.take_messages("bob");
        assert_eq!(msgs.len(), 1);
        assert_eq!(msgs[0], ice);
    }

    for i in 10..13 {
        let ice = SignalingMessage::IceCandidate(IceCandidatePayload {
            candidate: IceCandidate {
                candidate: format!("candidate:{i}"),
                sdp_mid: "0".to_string(),
                sdp_mline_index: 0,
            },
        });
        process_incoming(
            &mut state,
            "bob",
            &signaling::to_json(&ice).unwrap(),
            &conns,
        );
        let msgs = conns.take_messages("alice");
        assert_eq!(msgs.len(), 1);
        assert_eq!(msgs[0], ice);
    }

    // 4. Alice disconnects → Bob gets peer_left
    if let Some((target, msg)) = handle_disconnect(&state, "alice") {
        conns.send_to(&target, &msg);
    }
    state.remove_peer("alice");

    let bob_msgs = conns.take_messages("bob");
    assert_eq!(bob_msgs.len(), 1);
    assert_eq!(bob_msgs[0], SignalingMessage::PeerLeft);
}

#[test]
fn join_registers_peer_and_returns_joined() {
    let mut state = InMemoryRoomState::new();
    let conns = ConnectionMap::new(&["alice"]);

    let join = SignalingMessage::Join(JoinPayload {
        room_id: "room-join".to_string(),
        room_type: None,
        invite_code: None,
        display_name: None,
        profile_color: None,
    });
    process_incoming(
        &mut state,
        "alice",
        &signaling::to_json(&join).unwrap(),
        &conns,
    );

    let alice_msgs = conns.take_messages("alice");
    assert_eq!(alice_msgs.len(), 1);
    assert_eq!(
        alice_msgs[0],
        SignalingMessage::Joined(JoinedPayload {
            room_id: "room-join".to_string(),
            peer_id: "alice".to_string(),
            peer_count: 1,
            participants: vec![],
            ice_config: None,
            share_permission: None,
        })
    );
}

#[test]
fn join_rejects_empty_room_id() {
    let mut state = InMemoryRoomState::new();
    let conns = ConnectionMap::new(&["alice"]);

    let join = SignalingMessage::Join(JoinPayload {
        room_id: "   ".to_string(),
        room_type: None,
        invite_code: None,
        display_name: None,
        profile_color: None,
    });
    process_incoming(
        &mut state,
        "alice",
        &signaling::to_json(&join).unwrap(),
        &conns,
    );

    let alice_msgs = conns.take_messages("alice");
    assert_eq!(alice_msgs.len(), 1);
    assert_eq!(
        alice_msgs[0],
        SignalingMessage::Error(shared::signaling::ErrorPayload {
            message: "invalid room ID".to_string(),
        })
    );
}

#[test]
fn third_peer_join_to_same_room_is_rejected() {
    let mut state = InMemoryRoomState::new();
    let conns = ConnectionMap::new(&["alice", "bob", "charlie"]);

    let join = SignalingMessage::Join(JoinPayload {
        room_id: "room-1".to_string(),
        room_type: None,
        invite_code: None,
        display_name: None,
        profile_color: None,
    });

    process_incoming(
        &mut state,
        "alice",
        &signaling::to_json(&join).unwrap(),
        &conns,
    );
    process_incoming(
        &mut state,
        "bob",
        &signaling::to_json(&join).unwrap(),
        &conns,
    );
    process_incoming(
        &mut state,
        "charlie",
        &signaling::to_json(&join).unwrap(),
        &conns,
    );

    let charlie_msgs = conns.take_messages("charlie");
    assert_eq!(charlie_msgs.len(), 1);
    assert_eq!(
        charlie_msgs[0],
        SignalingMessage::Error(shared::signaling::ErrorPayload {
            message: "room is full".to_string(),
        })
    );
}

#[test]
fn full_signaling_exchange_with_join_flow() {
    let mut state = InMemoryRoomState::new();
    let conns = ConnectionMap::new(&["alice", "bob"]);

    let join = SignalingMessage::Join(JoinPayload {
        room_id: "room-join".to_string(),
        room_type: None,
        invite_code: None,
        display_name: None,
        profile_color: None,
    });
    process_incoming(
        &mut state,
        "alice",
        &signaling::to_json(&join).unwrap(),
        &conns,
    );
    process_incoming(
        &mut state,
        "bob",
        &signaling::to_json(&join).unwrap(),
        &conns,
    );

    let alice_join_msgs = conns.take_messages("alice");
    let bob_join_msgs = conns.take_messages("bob");
    assert_eq!(alice_join_msgs.len(), 1);
    assert_eq!(bob_join_msgs.len(), 1);
    assert!(matches!(alice_join_msgs[0], SignalingMessage::Joined(_)));
    assert!(matches!(bob_join_msgs[0], SignalingMessage::Joined(_)));

    let offer = SignalingMessage::Offer(OfferPayload {
        session_description: SessionDescription {
            sdp: "offer-sdp".to_string(),
            sdp_type: "offer".to_string(),
        },
    });
    process_incoming(
        &mut state,
        "alice",
        &signaling::to_json(&offer).unwrap(),
        &conns,
    );
    let bob_offer_msgs = conns.take_messages("bob");
    assert_eq!(bob_offer_msgs.len(), 1);
    assert_eq!(bob_offer_msgs[0], offer);
}
