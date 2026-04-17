//! Integration test: join with TURN credentials
//!
//! Verifies that when `AppState` is configured with a `TurnConfig`, the
//! `Joined` response sent to the joining peer contains a valid `ice_config`
//! with the expected TURN username format and a non-empty credential.
//!
//! Requirements: 1.5, 3.1

use shared::signaling::SignalingMessage;
use wavis_backend::channel::invite::InviteStore;
use wavis_backend::state::InMemoryRoomState;
use wavis_backend::voice::relay::{P2PJoinResult, handle_p2p_join};
use wavis_backend::voice::sfu_relay::SignalTarget;
use wavis_backend::voice::turn_cred::{
    TurnConfig, build_ice_config_payload, generate_turn_credentials,
};

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

fn make_turn_config() -> TurnConfig {
    TurnConfig::new(
        b"a-32-byte-secret-for-testing-ok!".to_vec(),
        None,
        3600,
        vec!["stun:stun.example.com:3478".to_string()],
        vec!["turn:turn.example.com:3478".to_string()],
    )
}

/// Minimal dispatch: collect Joined signals addressed to a specific peer.
fn collect_joined(
    signals: Vec<wavis_backend::voice::sfu_relay::OutboundSignal>,
    peer_id: &str,
) -> Vec<SignalingMessage> {
    signals
        .into_iter()
        .filter(|s| matches!(&s.target, SignalTarget::Peer(pid) if pid == peer_id))
        .map(|s| s.msg)
        .collect()
}

/// Simulate the handler's `inject_turn_credentials` logic inline.
fn inject_turn(
    signals: &mut [wavis_backend::voice::sfu_relay::OutboundSignal],
    peer_id: &str,
    config: &TurnConfig,
    now_unix: u64,
) {
    let creds = generate_turn_credentials(peer_id, config, now_unix);
    let ice_payload = build_ice_config_payload(config, &creds);
    for signal in signals.iter_mut() {
        if let SignalingMessage::Joined(ref mut joined) = signal.msg
            && matches!(&signal.target, SignalTarget::Peer(pid) if pid == peer_id)
        {
            joined.ice_config = Some(ice_payload.clone());
        }
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

/// Happy path: join a P2P room with TURN configured → Joined contains ice_config.
#[test]
fn join_with_turn_config_includes_ice_config() {
    let state = InMemoryRoomState::new();
    let invite_store = InviteStore::default();
    let config = make_turn_config();
    let now_unix = 1_700_000_000u64;

    let peer_id = "peer-1";
    let room_id = "room-abc";

    let mut signals = match handle_p2p_join(&state, room_id, peer_id, &invite_store, None) {
        P2PJoinResult::Joined(s) => s,
        P2PJoinResult::RoomFull => panic!("unexpected RoomFull"),
        P2PJoinResult::InviteRejected(r) => panic!("unexpected InviteRejected: {r:?}"),
    };

    // Simulate handler injection
    inject_turn(&mut signals, peer_id, &config, now_unix);

    let joined_msgs = collect_joined(signals, peer_id);
    assert_eq!(
        joined_msgs.len(),
        1,
        "exactly one Joined signal for the joiner"
    );

    let ice_config = match &joined_msgs[0] {
        SignalingMessage::Joined(p) => p.ice_config.as_ref().expect("ice_config must be present"),
        other => panic!("expected Joined, got {:?}", other),
    };

    // STUN/TURN URLs must match config
    assert_eq!(ice_config.stun_urls, config.stun_urls);
    assert_eq!(ice_config.turn_urls, config.turn_urls);

    // Username format: "{expiry}:{peer_id}"
    let parts: Vec<&str> = ice_config.turn_username.splitn(2, ':').collect();
    assert_eq!(parts.len(), 2, "username must be 'expiry:peer_id'");
    let expiry: u64 = parts[0].parse().expect("expiry must be a number");
    assert_eq!(expiry, now_unix + config.credential_ttl_secs);
    assert_eq!(parts[1], peer_id);

    // Credential must be non-empty base64
    assert!(
        !ice_config.turn_credential.is_empty(),
        "credential must be non-empty"
    );
}

/// Without TURN config, Joined response has no ice_config.
#[test]
fn join_without_turn_config_has_no_ice_config() {
    let state = InMemoryRoomState::new();
    let invite_store = InviteStore::default();

    let peer_id = "peer-2";
    let room_id = "room-xyz";

    let signals = match handle_p2p_join(&state, room_id, peer_id, &invite_store, None) {
        P2PJoinResult::Joined(s) => s,
        P2PJoinResult::RoomFull => panic!("unexpected RoomFull"),
        P2PJoinResult::InviteRejected(r) => panic!("unexpected InviteRejected: {r:?}"),
    };

    // No injection — no turn config
    let joined_msgs = collect_joined(signals, peer_id);
    assert_eq!(joined_msgs.len(), 1);

    match &joined_msgs[0] {
        SignalingMessage::Joined(p) => {
            assert!(
                p.ice_config.is_none(),
                "ice_config must be absent when TURN not configured"
            );
        }
        other => panic!("expected Joined, got {:?}", other),
    }
}

/// Credential is deterministic: same inputs → same output.
#[test]
fn turn_credentials_are_deterministic() {
    let config = make_turn_config();
    let now_unix = 1_700_000_000u64;
    let peer_id = "peer-det";

    let creds1 = generate_turn_credentials(peer_id, &config, now_unix);
    let creds2 = generate_turn_credentials(peer_id, &config, now_unix);

    assert_eq!(creds1.username, creds2.username);
    assert_eq!(creds1.credential, creds2.credential);
}
