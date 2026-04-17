//! Integration test: Full call flow with loopback
//!
//! Two `CallManager` peers with mock audio backends, connected via an
//! in-process signaling relay. Verifies:
//! - Full SDP offer/answer exchange
//! - ICE candidate exchange via relay
//! - Connection state reaches `Connected` on both sides
//! - `audio.play_remote()` called on both peers
//! - One peer hangs up → other peer cleans up
//!
//! Requirements: 3.1–3.10, 5.1, 5.3, 6.1

use shared::signaling::{
    self, AnswerPayload, IceCandidatePayload, OfferPayload, SessionDescription, SignalingMessage,
};
use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use wavis_client_shared::audio::{MockAudioBackend, MockCall};
use wavis_client_shared::ice_config::IceConfig;
use wavis_client_shared::webrtc::{
    CallManager, CallState, ConnectionState, MockPcCall, MockPeerConnectionBackend,
};

// ---------------------------------------------------------------------------
// In-process signaling relay
// ---------------------------------------------------------------------------

/// Simple in-process relay that routes messages between two named peers.
struct InProcessRelay {
    /// peer_id → inbox of JSON messages
    inboxes: HashMap<String, Vec<String>>,
    /// peer_id → the other peer_id
    peer_map: HashMap<String, String>,
}

impl InProcessRelay {
    fn new(peer_a: &str, peer_b: &str) -> Self {
        let mut peer_map = HashMap::new();
        peer_map.insert(peer_a.to_string(), peer_b.to_string());
        peer_map.insert(peer_b.to_string(), peer_a.to_string());

        let mut inboxes = HashMap::new();
        inboxes.insert(peer_a.to_string(), Vec::new());
        inboxes.insert(peer_b.to_string(), Vec::new());

        Self { inboxes, peer_map }
    }

    /// Send a signaling message from `sender` — relay routes it to the other peer.
    fn send(&mut self, sender: &str, msg: &SignalingMessage) {
        let json = signaling::to_json(msg).expect("serialize");
        let target = self.peer_map.get(sender).expect("unknown sender").clone();
        self.inboxes.get_mut(&target).unwrap().push(json);
    }

    /// Drain all pending messages for `peer_id`.
    fn take_messages(&mut self, peer_id: &str) -> Vec<SignalingMessage> {
        self.inboxes
            .get_mut(peer_id)
            .map(std::mem::take)
            .unwrap_or_default()
            .into_iter()
            .map(|json| signaling::parse(&json).expect("parse"))
            .collect()
    }
}

fn test_ice_config() -> IceConfig {
    IceConfig {
        stun_urls: vec!["stun:stun.example.com:3478".to_string()],
        turn_urls: vec!["turn:turn.example.com:3478".to_string()],
        turn_username: "user".to_string(),
        turn_credential: "pass".to_string(),
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[test]
fn full_call_flow_two_peers_with_loopback() {
    // --- Setup two peers with mock backends ---
    let audio_a = MockAudioBackend::new();
    let pc_a = MockPeerConnectionBackend::new();
    pc_a.set_offer_sdp("alice-offer-sdp");

    let audio_b = MockAudioBackend::new();
    let pc_b = MockPeerConnectionBackend::new();
    pc_b.set_answer_sdp("bob-answer-sdp");

    let manager_a = CallManager::new(audio_a, pc_a, test_ice_config());
    let manager_b = CallManager::new(audio_b, pc_b, test_ice_config());

    let mut relay = InProcessRelay::new("alice", "bob");

    // Collect ICE candidates from both peers
    let ice_from_a: Arc<Mutex<Vec<shared::signaling::IceCandidate>>> =
        Arc::new(Mutex::new(Vec::new()));
    let ice_from_b: Arc<Mutex<Vec<shared::signaling::IceCandidate>>> =
        Arc::new(Mutex::new(Vec::new()));

    let ice_a_clone = Arc::clone(&ice_from_a);
    manager_a.on_ice_candidate(move |c| {
        ice_a_clone.lock().unwrap().push(c);
    });

    let ice_b_clone = Arc::clone(&ice_from_b);
    manager_b.on_ice_candidate(move |c| {
        ice_b_clone.lock().unwrap().push(c);
    });

    // --- Step 1: Alice starts call → creates offer ---
    // Req 3.1, 3.2, 3.3, 5.1
    let offer_sdp = manager_a.start_call().unwrap();
    assert_eq!(offer_sdp, "alice-offer-sdp");
    assert_eq!(manager_a.state(), CallState::Negotiating);

    // Verify mic was captured
    assert!(manager_a
        .audio
        .calls()
        .iter()
        .any(|c| matches!(c, MockCall::CaptureMic)));

    // Send offer through relay
    let offer_msg = SignalingMessage::Offer(OfferPayload {
        session_description: SessionDescription {
            sdp: offer_sdp,
            sdp_type: "offer".to_string(),
        },
    });
    relay.send("alice", &offer_msg);

    // --- Step 2: Bob receives offer → creates answer ---
    // Req 3.4, 3.2, 5.1
    let bob_msgs = relay.take_messages("bob");
    assert_eq!(bob_msgs.len(), 1);
    match &bob_msgs[0] {
        SignalingMessage::Offer(payload) => {
            let answer_sdp = manager_b
                .accept_call(&payload.session_description.sdp)
                .unwrap();
            assert_eq!(answer_sdp, "bob-answer-sdp");
            assert_eq!(manager_b.state(), CallState::Negotiating);

            // Verify Bob captured mic
            assert!(manager_b
                .audio
                .calls()
                .iter()
                .any(|c| matches!(c, MockCall::CaptureMic)));

            // Send answer through relay
            let answer_msg = SignalingMessage::Answer(AnswerPayload {
                session_description: SessionDescription {
                    sdp: answer_sdp,
                    sdp_type: "answer".to_string(),
                },
            });
            relay.send("bob", &answer_msg);
        }
        _ => panic!("Expected Offer message for Bob"),
    }

    // --- Step 3: Alice receives answer → sets remote description ---
    // Req 3.5
    let alice_msgs = relay.take_messages("alice");
    assert_eq!(alice_msgs.len(), 1);
    match &alice_msgs[0] {
        SignalingMessage::Answer(payload) => {
            manager_a
                .set_answer(&payload.session_description.sdp)
                .unwrap();
            assert_eq!(manager_a.state(), CallState::Connecting);
        }
        _ => panic!("Expected Answer message for Alice"),
    }

    // --- Step 4: ICE candidate exchange ---
    // Req 3.6, 3.7

    // Simulate Alice gathering ICE candidates
    let alice_candidates = vec![
        shared::signaling::IceCandidate {
            candidate: "candidate:1 1 udp 2130706431 192.168.1.1 5000 typ host".to_string(),
            sdp_mid: "0".to_string(),
            sdp_mline_index: 0,
        },
        shared::signaling::IceCandidate {
            candidate: "candidate:2 1 udp 1694498815 203.0.113.1 5001 typ srflx".to_string(),
            sdp_mid: "0".to_string(),
            sdp_mline_index: 0,
        },
    ];

    for c in &alice_candidates {
        manager_a.pc_backend.simulate_ice_candidate(c.clone());
    }

    // Forward Alice's ICE candidates through relay to Bob
    let gathered_a = ice_from_a.lock().unwrap().clone();
    assert_eq!(gathered_a.len(), 2);
    for c in &gathered_a {
        let msg = SignalingMessage::IceCandidate(IceCandidatePayload {
            candidate: c.clone(),
        });
        relay.send("alice", &msg);
    }

    // Bob receives and adds Alice's ICE candidates
    let bob_ice_msgs = relay.take_messages("bob");
    assert_eq!(bob_ice_msgs.len(), 2);
    for msg in &bob_ice_msgs {
        if let SignalingMessage::IceCandidate(payload) = msg {
            manager_b.add_ice_candidate(&payload.candidate).unwrap();
        }
    }

    // Simulate Bob gathering ICE candidates
    let bob_candidates = vec![shared::signaling::IceCandidate {
        candidate: "candidate:3 1 udp 2130706431 10.0.0.1 6000 typ host".to_string(),
        sdp_mid: "0".to_string(),
        sdp_mline_index: 0,
    }];

    for c in &bob_candidates {
        manager_b.pc_backend.simulate_ice_candidate(c.clone());
    }

    let gathered_b = ice_from_b.lock().unwrap().clone();
    assert_eq!(gathered_b.len(), 1);
    for c in &gathered_b {
        let msg = SignalingMessage::IceCandidate(IceCandidatePayload {
            candidate: c.clone(),
        });
        relay.send("bob", &msg);
    }

    // Alice receives and adds Bob's ICE candidates
    let alice_ice_msgs = relay.take_messages("alice");
    assert_eq!(alice_ice_msgs.len(), 1);
    for msg in &alice_ice_msgs {
        if let SignalingMessage::IceCandidate(payload) = msg {
            manager_a.add_ice_candidate(&payload.candidate).unwrap();
        }
    }

    // Verify ICE candidates were added to both PeerConnections
    let pc_a_calls = manager_a.pc_backend.calls();
    let added_a: Vec<_> = pc_a_calls
        .iter()
        .filter(|c| matches!(c, MockPcCall::AddIceCandidate(_)))
        .collect();
    assert_eq!(added_a.len(), 1); // Bob's 1 candidate

    let pc_b_calls = manager_b.pc_backend.calls();
    let added_b: Vec<_> = pc_b_calls
        .iter()
        .filter(|c| matches!(c, MockPcCall::AddIceCandidate(_)))
        .collect();
    assert_eq!(added_b.len(), 2); // Alice's 2 candidates

    // --- Step 5: ICE reaches connected state on both sides ---
    // Req 3.8, 5.3
    manager_a
        .pc_backend
        .simulate_connection_state(ConnectionState::Connected);
    manager_b
        .pc_backend
        .simulate_connection_state(ConnectionState::Connected);

    assert_eq!(manager_a.state(), CallState::Connected);
    assert_eq!(manager_b.state(), CallState::Connected);

    // Verify play_remote() called on both peers
    assert!(manager_a
        .audio
        .calls()
        .iter()
        .any(|c| matches!(c, MockCall::PlayRemote(_))));
    assert!(manager_b
        .audio
        .calls()
        .iter()
        .any(|c| matches!(c, MockCall::PlayRemote(_))));

    // --- Step 6: Alice hangs up → Bob cleans up ---
    // Req 3.10, 5.4, 6.1
    manager_a.hangup().unwrap();
    assert_eq!(manager_a.state(), CallState::Closed);

    // Verify Alice's resources released
    assert!(manager_a
        .pc_backend
        .calls()
        .iter()
        .any(|c| matches!(c, MockPcCall::Close)));
    assert!(manager_a
        .audio
        .calls()
        .iter()
        .any(|c| matches!(c, MockCall::Stop)));

    // Simulate server sending peer_left to Bob
    relay.send("alice", &SignalingMessage::PeerLeft);
    let bob_final = relay.take_messages("bob");
    assert_eq!(bob_final.len(), 1);
    assert_eq!(bob_final[0], SignalingMessage::PeerLeft);

    // Bob receives peer_left → hangup
    manager_b.hangup().unwrap();
    assert_eq!(manager_b.state(), CallState::Closed);

    // Verify Bob's resources released
    assert!(manager_b
        .pc_backend
        .calls()
        .iter()
        .any(|c| matches!(c, MockPcCall::Close)));
    assert!(manager_b
        .audio
        .calls()
        .iter()
        .any(|c| matches!(c, MockCall::Stop)));
}

#[test]
fn ice_failed_triggers_cleanup_on_both_peers() {
    let audio_a = MockAudioBackend::new();
    let pc_a = MockPeerConnectionBackend::new();
    let audio_b = MockAudioBackend::new();
    let pc_b = MockPeerConnectionBackend::new();

    let manager_a = CallManager::new(audio_a, pc_a, test_ice_config());
    let manager_b = CallManager::new(audio_b, pc_b, test_ice_config());

    // Start calls on both sides
    let _ = manager_a.start_call().unwrap();
    let _ = manager_b.accept_call("remote-offer").unwrap();

    // ICE fails on both sides (Req 3.9)
    manager_a
        .pc_backend
        .simulate_connection_state(ConnectionState::Failed);
    manager_b
        .pc_backend
        .simulate_connection_state(ConnectionState::Failed);

    assert_eq!(manager_a.state(), CallState::Failed);
    assert_eq!(manager_b.state(), CallState::Failed);

    // Both should have cleaned up
    assert!(manager_a
        .pc_backend
        .calls()
        .iter()
        .any(|c| matches!(c, MockPcCall::Close)));
    assert!(manager_a
        .audio
        .calls()
        .iter()
        .any(|c| matches!(c, MockCall::Stop)));

    assert!(manager_b
        .pc_backend
        .calls()
        .iter()
        .any(|c| matches!(c, MockPcCall::Close)));
    assert!(manager_b
        .audio
        .calls()
        .iter()
        .any(|c| matches!(c, MockCall::Stop)));
}
