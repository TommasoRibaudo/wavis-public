use super::*;
use crate::audio::{MockAudioBackend, MockCall};
use crate::webrtc::{MockPcCall, MockPeerConnectionBackend};
use proptest::prelude::*;

// -----------------------------------------------------------------------
// Property 7: Publish connection carries exactly one audio track
// **Validates: Requirements 3.3**
//
// For any RoomSession::join_room() call, exactly one create_peer_connection
// and one add_audio_track call is made on the PeerConnectionBackend.
// -----------------------------------------------------------------------

proptest! {
    #![proptest_config(ProptestConfig { cases: 50, .. ProptestConfig::default() })]

    /// Feature: sfu-multi-party-voice, Property 7: Publish connection carries exactly one audio track
    #[test]
    fn publish_connection_carries_exactly_one_audio_track(
        room_id in "[a-z]{4,8}",
    ) {
        let session = make_session();
        session.join_room(&room_id, None).unwrap();

        let calls = session.pc_backend.calls();
        let create_count = calls.iter().filter(|c| matches!(c, MockPcCall::CreatePeerConnection)).count();
        let track_count = calls.iter().filter(|c| matches!(c, MockPcCall::AddAudioTrack(_))).count();

        prop_assert_eq!(create_count, 1, "Expected exactly 1 create_peer_connection call");
        prop_assert_eq!(track_count, 1, "Expected exactly 1 add_audio_track call");
    }
}

// -----------------------------------------------------------------------
// Property 1: Join message construction
// Feature: interactive-cli-client
// **Validates: Requirements 2.1, 5.1**
//
// For any room ID string and optional invite code string, when
// RoomSession::join_room is called, the resulting SignalingMessage::Join
// payload SHALL have room_id equal to the input, room_type equal to
// Some("sfu"), and invite_code matching the input (None for create,
// Some for join).
// -----------------------------------------------------------------------

proptest! {
    #![proptest_config(ProptestConfig { cases: 100, .. ProptestConfig::default() })]

    /// Feature: interactive-cli-client, Property 1: Join message construction
    #[test]
    fn join_message_construction(
        room_id in "[a-zA-Z0-9_-]{1,32}",
        invite_code in proptest::option::of("[a-zA-Z0-9]{6,20}"),
    ) {
        let audio = MockAudioBackend::new();
        let pc = MockPeerConnectionBackend::new();
        let ice = IceConfig {
            stun_urls: vec!["stun:stun.example.com:19302".to_string()],
            turn_urls: vec!["turn:turn.example.com:3478".to_string()],
            turn_username: "user".to_string(),
            turn_credential: "pass".to_string(),
        };
        let ws = MockWs::new();
        let sent = Arc::clone(&ws.sent);

        let session = RoomSession::new(audio, pc, ice, ws);
        session.join_room(&room_id, invite_code.as_deref()).unwrap();

        let messages = sent.lock().unwrap();
        prop_assert_eq!(messages.len(), 1, "Expected exactly one sent message");

        let parsed: SignalingMessage = shared::signaling::parse(&messages[0])
            .expect("Failed to parse sent message");

        match parsed {
            SignalingMessage::Join(payload) => {
                prop_assert_eq!(&payload.room_id, &room_id);
                prop_assert_eq!(payload.room_type, Some("sfu".to_string()));
                prop_assert_eq!(payload.invite_code, invite_code);
            }
            other => {
                prop_assert!(false, "Expected Join message, got {:?}", other);
            }
        }
    }
}

// -----------------------------------------------------------------------
// Unit tests
// -----------------------------------------------------------------------

#[test]
fn join_room_sends_join_message_and_sets_up_pc() {
    let session = make_session();
    session.join_room("myroom", None).unwrap();

    let calls = session.pc_backend.calls();
    assert!(calls
        .iter()
        .any(|c| matches!(c, MockPcCall::CreatePeerConnection)));
    assert!(calls
        .iter()
        .any(|c| matches!(c, MockPcCall::AddAudioTrack(_))));
}

#[test]
fn join_room_twice_returns_already_in_room() {
    let session = make_session();
    session.join_room("room1", None).unwrap();
    let err = session.join_room("room1", None).unwrap_err();
    assert!(matches!(err, RoomError::AlreadyInRoom));
}

#[test]
fn leave_room_when_not_in_room_returns_error() {
    let session = make_session();
    let err = session.leave_room().unwrap_err();
    assert!(matches!(err, RoomError::NotInRoom));
}

#[test]
fn leave_room_clears_subscribe_tracks() {
    let session = make_session();
    session.join_room("room1", None).unwrap();
    session
        .handle_incoming(&participant_joined_json("peer-1", "Alice"))
        .unwrap();
    assert_eq!(session.subscribe_track_count(), 1);

    session.leave_room().unwrap();
    assert_eq!(session.subscribe_track_count(), 0);
}

// -----------------------------------------------------------------------
// Property 5: RoomSession connects to LiveKit when token received in LiveKit mode
// **Validates: Requirements 6.1, 6.2**
//
// For any valid sfu_url + token, when a MediaToken message is received and
// the session has a MockLiveKitConnection, exactly one Connect call is made
// with the correct url and token, followed by a PublishAudio call.
// -----------------------------------------------------------------------

proptest! {
    #![proptest_config(ProptestConfig { cases: 50, .. ProptestConfig::default() })]

    /// Feature: livekit-integration, Property 5: RoomSession connects to LiveKit when token received
    #[test]
    fn room_session_connects_to_livekit_when_token_received(
        sfu_url in "wss://[a-z]{4,8}\\.livekit\\.cloud",
        token in "[a-zA-Z0-9]{20,40}",
        room_id in "[a-z]{4,8}",
    ) {
        let audio = MockAudioBackend::new();
        let pc = MockPeerConnectionBackend::new();
        let ice = IceConfig {
            stun_urls: vec!["stun:stun.example.com:19302".to_string()],
            turn_urls: vec![],
            turn_username: String::new(),
            turn_credential: String::new(),
        };
        let ws = MockWs::new();
        let livekit = MockLiveKitConnection::new();
        let livekit_calls = Arc::clone(&livekit.calls);

        let session = RoomSession::with_livekit(audio, pc, ice, ws, livekit);
        session.join_room(&room_id, None).unwrap();

        // Simulate MediaToken arriving from backend
        let media_token_json = format!(
            r#"{{"type":"media_token","sfuUrl":"{}","token":"{}"}}"#,
            sfu_url, token
        );
        session.handle_incoming(&media_token_json).unwrap();

        let calls = livekit_calls.lock().unwrap();
        // Must have a Connect call with the correct url and token
        let connect_call = calls.iter().find(|c| matches!(c, MockLiveKitCall::Connect { .. }));
        prop_assert!(connect_call.is_some(), "Expected a Connect call");
        if let Some(MockLiveKitCall::Connect { url, token: tok }) = connect_call {
            prop_assert_eq!(url, &sfu_url);
            prop_assert_eq!(tok, &token);
        }
        // Must have a PublishAudio call after successful connect
        let publish_call = calls.iter().find(|c| matches!(c, MockLiveKitCall::PublishAudio));
        prop_assert!(publish_call.is_some(), "Expected a PublishAudio call after connect");

        // SFU mode must be LiveKit
        let mode = session.sfu_mode();
        prop_assert!(
            matches!(mode, SfuConnectionMode::LiveKit { .. }),
            "Expected LiveKit mode after MediaToken"
        );
    }
}

// -----------------------------------------------------------------------
// Property 6: RoomSession suppresses SDP/ICE in LiveKit mode
// **Validates: Requirements 6.3**
//
// After a MediaToken is received and LiveKit mode is active, incoming
// Answer and IceCandidate messages must NOT call set_remote_answer or
// add_ice_candidate on the PeerConnectionBackend.
// -----------------------------------------------------------------------

proptest! {
    #![proptest_config(ProptestConfig { cases: 50, .. ProptestConfig::default() })]

    /// Feature: livekit-integration, Property 6: RoomSession suppresses SDP/ICE in LiveKit mode
    #[test]
    fn room_session_suppresses_sdp_ice_in_livekit_mode(
        sfu_url in "wss://[a-z]{4,8}\\.livekit\\.cloud",
        token in "[a-zA-Z0-9]{20,40}",
        sdp in "[a-zA-Z0-9 =]{10,50}",
        candidate in "candidate:[a-z0-9]{8,16}",
    ) {
        let audio = MockAudioBackend::new();
        let pc = MockPeerConnectionBackend::new();
        let ice = IceConfig {
            stun_urls: vec!["stun:stun.example.com:19302".to_string()],
            turn_urls: vec![],
            turn_username: String::new(),
            turn_credential: String::new(),
        };
        let ws = MockWs::new();
        let livekit = MockLiveKitConnection::new();

        let session = RoomSession::with_livekit(audio, pc, ice, ws, livekit);
        session.join_room("testroom", None).unwrap();

        // Switch to LiveKit mode via MediaToken
        let media_token_json = format!(
            r#"{{"type":"media_token","sfuUrl":"{}","token":"{}"}}"#,
            sfu_url, token
        );
        session.handle_incoming(&media_token_json).unwrap();

        // Confirm we're in LiveKit mode
        prop_assume!(matches!(session.sfu_mode(), SfuConnectionMode::LiveKit { .. }));

        // Snapshot call count after join + media_token (create_peer_connection, add_audio_track)
        let calls_before = session.pc_backend.calls().len();

        // Send Answer — should be suppressed in LiveKit mode
        let answer_json = format!(
            r#"{{"type":"answer","sessionDescription":{{"sdp":"{}","type":"answer"}}}}"#,
            sdp.replace('"', "'")
        );
        let _ = session.handle_incoming(&answer_json);

        // Send IceCandidate — should be suppressed in LiveKit mode
        let ice_json = format!(
            r#"{{"type":"ice_candidate","candidate":{{"candidate":"{}","sdpMid":"0","sdpMLineIndex":0}}}}"#,
            candidate
        );
        let _ = session.handle_incoming(&ice_json);

        let calls_after = session.pc_backend.calls().len();
        prop_assert_eq!(
            calls_after, calls_before,
            "No new PC calls expected in LiveKit mode (SDP/ICE suppressed)"
        );
    }
}

#[test]
fn answer_message_calls_set_remote_answer() {
    let session = make_session();
    session.join_room("room1", None).unwrap();

    let answer_json = r#"{"type":"answer","sessionDescription":{"sdp":"v=0...","type":"answer"}}"#;
    session.handle_incoming(answer_json).unwrap();

    let calls = session.pc_backend.calls();
    assert!(calls
        .iter()
        .any(|c| matches!(c, MockPcCall::SetRemoteAnswer(_))));
}

#[test]
fn ice_candidate_message_calls_add_ice_candidate() {
    let session = make_session();
    session.join_room("room1", None).unwrap();

    let ice_json = r#"{"type":"ice_candidate","candidate":{"candidate":"candidate:...","sdpMid":"0","sdpMLineIndex":0}}"#;
    session.handle_incoming(ice_json).unwrap();

    let calls = session.pc_backend.calls();
    assert!(calls
        .iter()
        .any(|c| matches!(c, MockPcCall::AddIceCandidate(_))));
}

// -----------------------------------------------------------------------
// Unit test: leave_room calls LiveKitConnection::disconnect in LiveKit mode
// Requirements: 7.1, 7.4
// -----------------------------------------------------------------------

// -----------------------------------------------------------------------
// Property 1: Mic opened exactly once per lifecycle
// **Validates: Requirements 2.1, 2.2, 2.3**
//
// For any RoomSession that joins an SFU room and subsequently receives a
// MediaToken (triggering LiveKit mode), `capture_mic` SHALL have been
// called exactly once during that RoomSession_Lifecycle.
// -----------------------------------------------------------------------

proptest! {
    #![proptest_config(ProptestConfig::with_cases(100))]

    /// Feature: livekit-audio-fix, Property 1: Mic opened exactly once per lifecycle
    #[test]
    fn prop_mic_opened_exactly_once(
        room_id in "[a-z]{4,8}",
    ) {
        let audio = MockAudioBackend::new();
        let pc = MockPeerConnectionBackend::new();
        let ice = IceConfig {
            stun_urls: vec!["stun:stun.example.com:19302".to_string()],
            turn_urls: vec![],
            turn_username: String::new(),
            turn_credential: String::new(),
        };
        let ws = MockWs::new();
        let livekit = MockLiveKitConnection::new();

        let session = RoomSession::with_livekit(audio, pc, ice, ws, livekit);
        session.join_room(&room_id, None).unwrap();

        // Simulate MediaToken arriving — triggers LiveKit mode switch
        let media_token_json =
            r#"{"type":"media_token","sfuUrl":"ws://test","token":"tok"}"#;
        session.handle_incoming(media_token_json).unwrap();

        // Count how many times capture_mic was called (access via private field, same module)
        let capture_count = session
            .audio
            .calls()
            .iter()
            .filter(|c| matches!(c, MockCall::CaptureMic))
            .count();

        prop_assert_eq!(
            capture_count,
            1,
            "Expected capture_mic to be called exactly once, got {}",
            capture_count
        );
    }
}

#[test]
fn leave_room_clears_turn_credentials() {
    let session = make_session();
    session.join_room("room1", None).unwrap();

    // Verify credentials are set before leave
    {
        let cfg = session.ice_config.lock().unwrap();
        assert!(
            !cfg.turn_credential.is_empty(),
            "credential should be set before leave"
        );
    }

    session.leave_room().unwrap();

    // After leave, TURN credentials must be zeroed (Requirements: 7.5)
    let cfg = session.ice_config.lock().unwrap();
    assert!(
        cfg.turn_username.is_empty(),
        "turn_username should be cleared after leave"
    );
    assert!(
        cfg.turn_credential.is_empty(),
        "turn_credential should be cleared after leave"
    );
}

#[test]
fn leave_room_calls_livekit_disconnect_in_livekit_mode() {
    let audio = MockAudioBackend::new();
    let pc = MockPeerConnectionBackend::new();
    let ice = IceConfig {
        stun_urls: vec!["stun:stun.example.com:19302".to_string()],
        turn_urls: vec![],
        turn_username: String::new(),
        turn_credential: String::new(),
    };
    let ws = MockWs::new();
    let mock_lk = MockLiveKitConnection::new();
    let calls = Arc::clone(&mock_lk.calls);

    let session = RoomSession::with_livekit(audio, pc, ice, ws, mock_lk);

    // Put the session in a room
    session.join_room("testroom", None).unwrap();

    // Simulate LiveKit mode (as if MediaToken was received and connect succeeded)
    *session.sfu_mode.lock().unwrap() = SfuConnectionMode::LiveKit {
        livekit_url: "wss://test.livekit.io".to_string(),
        token: "test-token".to_string(),
    };

    session.leave_room().unwrap();

    let recorded = calls.lock().unwrap();
    assert!(
        recorded.contains(&MockLiveKitCall::Disconnect),
        "Expected MockLiveKitCall::Disconnect to be recorded, got: {:?}",
        *recorded
    );
}
