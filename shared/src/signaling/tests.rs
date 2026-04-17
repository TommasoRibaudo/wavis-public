use super::*;
use proptest::prelude::*;

// --- Unit tests for invite lifecycle messages ---

#[test]
fn test_invite_create_serialization() {
    let msg = SignalingMessage::InviteCreate(InviteCreatePayload { max_uses: Some(5) });
    let json = to_json(&msg).unwrap();
    assert!(json.contains(r#""type":"invite_create"#));
    assert!(json.contains(r#""maxUses":5"#));
}

#[test]
fn test_invite_create_default_max_uses() {
    let msg = SignalingMessage::InviteCreate(InviteCreatePayload { max_uses: None });
    let json = to_json(&msg).unwrap();
    assert!(json.contains(r#""type":"invite_create"#));
    let parsed = parse(&json).unwrap();
    match parsed {
        SignalingMessage::InviteCreate(payload) => {
            assert_eq!(payload.max_uses, None);
        }
        _ => panic!("Expected InviteCreate variant"),
    }
}

#[test]
fn test_invite_created_serialization() {
    let msg = SignalingMessage::InviteCreated(InviteCreatedPayload {
        invite_code: "test_code_123".to_string(),
        expires_in_secs: 3600,
        max_uses: 6,
    });
    let json = to_json(&msg).unwrap();
    assert!(json.contains(r#""type":"invite_created"#));
    assert!(json.contains(r#""inviteCode":"test_code_123"#));
    assert!(json.contains(r#""expiresInSecs":3600"#));
    assert!(json.contains(r#""maxUses":6"#));
}

#[test]
fn test_invite_revoke_serialization() {
    let msg = SignalingMessage::InviteRevoke(InviteRevokePayload {
        invite_code: "code_to_revoke".to_string(),
    });
    let json = to_json(&msg).unwrap();
    assert!(json.contains(r#""type":"invite_revoke"#));
    assert!(json.contains(r#""inviteCode":"code_to_revoke"#));
}

#[test]
fn test_invite_revoked_serialization() {
    let msg = SignalingMessage::InviteRevoked(InviteRevokedPayload {
        invite_code: "revoked_code".to_string(),
    });
    let json = to_json(&msg).unwrap();
    assert!(json.contains(r#""type":"invite_revoked"#));
    assert!(json.contains(r#""inviteCode":"revoked_code"#));
}

// --- Unit tests for JoinRejected ---

#[test]
fn test_join_rejected_invite_expired() {
    let msg = SignalingMessage::JoinRejected(JoinRejectedPayload {
        reason: JoinRejectionReason::InviteExpired,
    });
    let json = to_json(&msg).unwrap();
    assert_eq!(
        json,
        r#"{"type":"join_rejected","reason":"invite_expired"}"#
    );
}

#[test]
fn test_join_rejected_room_full() {
    let msg = SignalingMessage::JoinRejected(JoinRejectedPayload {
        reason: JoinRejectionReason::RoomFull,
    });
    let json = to_json(&msg).unwrap();
    assert_eq!(json, r#"{"type":"join_rejected","reason":"room_full"}"#);
}

#[test]
fn test_sfu_cold_starting_serialization() {
    let msg = SignalingMessage::SfuColdStarting(SfuColdStartingPayload {
        estimated_wait_secs: 120,
    });
    let json = to_json(&msg).unwrap();
    assert_eq!(
        json,
        r#"{"type":"sfu_cold_starting","estimatedWaitSecs":120}"#
    );
    assert_eq!(parse(&json).unwrap(), msg);
}

#[test]
fn test_join_voice_supports_sub_rooms_serialization() {
    let msg = SignalingMessage::JoinVoice(JoinVoicePayload {
        channel_id: "00000000-0000-0000-0000-000000000001".to_string(),
        display_name: Some("alice".to_string()),
        profile_color: Some("#E06C75".to_string()),
        supports_sub_rooms: Some(true),
    });
    let json = to_json(&msg).unwrap();
    assert!(json.contains(r#""type":"join_voice""#));
    assert!(json.contains(r#""supportsSubRooms":true"#));
    assert_eq!(parse(&json).unwrap(), msg);
}

#[test]
fn test_sub_room_state_round_trip() {
    let msg = SignalingMessage::SubRoomState(SubRoomStatePayload {
        rooms: vec![
            SubRoomInfoPayload {
                sub_room_id: "sub-room-1".to_string(),
                room_number: 1,
                is_default: true,
                participant_ids: vec!["peer-a".to_string()],
                delete_at_ms: None,
            },
            SubRoomInfoPayload {
                sub_room_id: "sub-room-2".to_string(),
                room_number: 2,
                is_default: false,
                participant_ids: vec![],
                delete_at_ms: Some(1_746_000_000_000),
            },
        ],
    });
    let json = to_json(&msg).unwrap();
    assert!(json.contains(r#""type":"sub_room_state""#));
    assert!(json.contains(r#""subRoomId":"sub-room-1""#));
    assert!(json.contains(r#""roomNumber":1"#));
    assert_eq!(parse(&json).unwrap(), msg);
}

// --- Unit tests for ChatSend serialization ---

#[test]
fn test_chat_send_serialization() {
    let msg = SignalingMessage::ChatSend(ChatSendPayload {
        text: "hello".to_string(),
    });
    let json = to_json(&msg).unwrap();
    assert_eq!(json, r#"{"type":"chat_send","text":"hello"}"#);
}

// --- Property tests ---

proptest! {
    #![proptest_config(ProptestConfig::with_cases(200))]

    /// Feature: invite-code-hardening, Property 18: JoinRejected serialization format
    /// Validates: Requirements 10.1, 10.2
    #[test]
    fn prop_join_rejected_serialization_format(reason in any::<JoinRejectionReason>()) {
        let msg = SignalingMessage::JoinRejected(JoinRejectedPayload { reason });
        let json = to_json(&msg).expect("serialization should succeed");

        let parsed: serde_json::Value = serde_json::from_str(&json)
            .expect("should be valid JSON");

        let obj = parsed.as_object().expect("should be an object");
        prop_assert_eq!(obj.len(), 2, "should have exactly 2 fields");

        prop_assert_eq!(
            obj.get("type").and_then(|v| v.as_str()),
            Some("join_rejected"),
            "type field should be 'join_rejected'"
        );

        let reason_str = obj.get("reason")
            .and_then(|v| v.as_str())
            .expect("reason field should be a string");

        let valid_reasons = [
            "invite_expired",
            "invite_revoked",
            "invite_invalid",
            "invite_required",
            "invite_exhausted",
            "room_full",
            "rate_limited",
            "not_authorized",
        ];
        prop_assert!(
            valid_reasons.contains(&reason_str),
            "reason '{}' should be one of the valid snake_case variants",
            reason_str
        );
    }

    /// Feature: invite-code-hardening, Property 19: JoinRejected round-trip
    /// Validates: Requirements 10.4
    #[test]
    fn prop_join_rejected_round_trip(reason in any::<JoinRejectionReason>()) {
        let original = SignalingMessage::JoinRejected(JoinRejectedPayload { reason });
        let json = to_json(&original).expect("serialization should succeed");
        let deserialized = parse(&json).expect("deserialization should succeed");
        prop_assert_eq!(original, deserialized);
    }

    // Feature: ephemeral-room-chat, Property 9: ChatMessage serialization round-trip
    /// **Validates: Requirements 5.5**
    ///
    /// For any valid ChatMessagePayload (with non-empty participant_id, display_name,
    /// text of 1–2000 chars, and valid ISO 8601 timestamp), serializing a
    /// SignalingMessage::ChatMessage via to_json then deserializing via parse should
    /// produce an equal SignalingMessage.
    #[test]
    fn prop_chat_message_round_trip(
        participant_id in ".{1,50}",
        display_name in ".{1,50}",
        text in ".{1,2000}",
        ts_year in 2020u16..2030u16,
        ts_month in 1u8..=12u8,
        ts_day in 1u8..=28u8,
        ts_hour in 0u8..=23u8,
        ts_min in 0u8..=59u8,
        ts_sec in 0u8..=59u8,
    ) {
        let timestamp = format!(
            "{:04}-{:02}-{:02}T{:02}:{:02}:{:02}Z",
            ts_year, ts_month, ts_day, ts_hour, ts_min, ts_sec
        );
        let original = SignalingMessage::ChatMessage(ChatMessagePayload {
            participant_id,
            display_name,
            text,
            timestamp,
            message_id: None,
        });
        let json = to_json(&original).expect("serialization should succeed");
        let deserialized = parse(&json).expect("deserialization should succeed");
        prop_assert_eq!(original, deserialized);
    }

    /// Feature: chat-history-persistence, Property 9: Signaling serialization round-trip
    /// **Validates: Requirements 6.1, 6.2, 6.3, 6.4, 6.5, 6.6**
    ///
    /// For any valid ChatHistoryRequest, ChatHistoryResponse, or ChatMessage (with
    /// optional messageId) signaling message, serializing via to_json and then
    /// deserializing via parse should produce an equivalent SignalingMessage.
    #[test]
    fn prop_chat_history_signaling_round_trip(
        msg in prop_oneof![
            any::<ChatHistoryRequestPayload>().prop_map(SignalingMessage::ChatHistoryRequest),
            any::<ChatHistoryResponsePayload>().prop_map(SignalingMessage::ChatHistoryResponse),
            any::<ChatMessagePayload>().prop_map(SignalingMessage::ChatMessage),
        ]
    ) {
        let json = to_json(&msg).expect("serialization should succeed");
        let deserialized = parse(&json).expect("deserialization should succeed");
        prop_assert_eq!(msg, deserialized);
    }

    /// Feature: sfu-multi-party-voice, Property 6: Signaling message round-trip serialization
    /// Validates: Requirements 4.6, 4.7, 10.5
    ///
    /// For any SignalingMessage value (including new SFU variants ParticipantJoined,
    /// ParticipantLeft, RoomState, MediaToken), serializing to JSON and deserializing
    /// back shall produce an object equal to the original.
    #[test]
    fn prop_signaling_message_round_trip(msg in any::<SignalingMessage>()) {
        // Serialize to JSON
        let json = to_json(&msg).expect("serialization should succeed");

        // Deserialize back
        let deserialized = parse(&json).expect("deserialization should succeed");

        // Assert equality
        prop_assert_eq!(msg, deserialized);
    }

    /// Property 5: Malformed or unrecognized messages produce errors
    /// For any byte string that is either not valid UTF-8 JSON or valid JSON with
    /// an unrecognized `type` field, the parse function shall return an error.
    #[test]
    fn prop_malformed_messages_produce_errors(
        input in prop_oneof![
            // Strategy 1: Random invalid JSON strings (garbage bytes, incomplete JSON)
            any::<String>().prop_filter("not valid JSON", |s| serde_json::from_str::<serde_json::Value>(s).is_err()),

            // Strategy 2: Valid JSON but missing the "type" field
            any::<String>().prop_map(|s| format!(r#"{{"data": "{}"}}"#, s)),

            // Strategy 3: Valid JSON with unrecognized "type" values
            any::<String>()
                .prop_filter(
                    "not a known type",
                    |s| !["join", "joined", "join_rejected", "invite_create", "invite_created",
                           "invite_revoke", "invite_revoked", "offer", "answer", "ice_candidate",
                           "peer_left", "leave", "error", "participant_joined", "participant_left",
                           "room_state", "media_token", "kick_participant", "mute_participant",
                           "unmute_participant",
                           "participant_kicked", "participant_muted", "participant_unmuted",
                           "start_share", "share_started", "stop_share", "share_stopped",
                           "create_room", "room_created",
                           "auth", "auth_success", "auth_failed",
                           "join_voice", "create_sub_room", "join_sub_room", "leave_sub_room",
                           "sub_room_state", "sub_room_created", "sub_room_joined",
                           "sub_room_left", "sub_room_deleted",
                           "sfu_cold_starting",
                           "chat_send", "chat_message",
                           "chat_history_request", "chat_history_response"]
                        .contains(&s.as_str()),
                )
                .prop_map(|type_val| format!(r#"{{"type": "{}"}}"#, type_val)),

            // Strategy 4: Valid JSON with "type" field but wrong structure
            prop_oneof![
                Just(r#"{"type": "offer"}"#.to_string()), // Missing sessionDescription
                Just(r#"{"type": "answer"}"#.to_string()), // Missing sessionDescription
                Just(r#"{"type": "ice_candidate"}"#.to_string()), // Missing candidate
            ],
        ]
    ) {
        // All invalid inputs should produce a ParseError
        let result = parse(&input);
        prop_assert!(result.is_err(), "Expected parse error for input: {}", input);
    }
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(100))]

    // Feature: ephemeral-room-chat, Property 10: Wire format uses correct casing conventions
    /// **Validates: Requirements 5.6**
    ///
    /// For any valid ChatMessagePayload, the JSON string produced by to_json should
    /// contain the keys "participantId", "displayName", "text", "timestamp" (camelCase)
    /// and "type": "chat_message" (snake_case). For any valid ChatSendPayload, the JSON
    /// string produced by to_json should contain "type": "chat_send" and "text".
    #[test]
    fn prop_wire_format_casing_conventions(
        payload in any::<ChatMessagePayload>(),
        send_payload in any::<ChatSendPayload>(),
    ) {
        // --- ChatMessage wire format ---
        let chat_msg = SignalingMessage::ChatMessage(payload);
        let json = to_json(&chat_msg).expect("ChatMessage serialization should succeed");

        // snake_case type discriminator
        prop_assert!(
            json.contains(r#""type":"chat_message""#),
            "ChatMessage JSON should contain type:chat_message, got: {}", json
        );

        // camelCase payload field names
        prop_assert!(
            json.contains(r#""participantId":"#),
            "ChatMessage JSON should contain camelCase 'participantId', got: {}", json
        );
        prop_assert!(
            json.contains(r#""displayName":"#),
            "ChatMessage JSON should contain camelCase 'displayName', got: {}", json
        );
        prop_assert!(
            json.contains(r#""text":"#),
            "ChatMessage JSON should contain 'text' key, got: {}", json
        );
        prop_assert!(
            json.contains(r#""timestamp":"#),
            "ChatMessage JSON should contain 'timestamp' key, got: {}", json
        );

        // Ensure snake_case variants of camelCase fields are NOT present
        prop_assert!(
            !json.contains(r#""participant_id":"#),
            "ChatMessage JSON should NOT contain snake_case 'participant_id', got: {}", json
        );
        prop_assert!(
            !json.contains(r#""display_name":"#),
            "ChatMessage JSON should NOT contain snake_case 'display_name', got: {}", json
        );

        // --- ChatSend wire format ---
        let chat_send = SignalingMessage::ChatSend(send_payload);
        let send_json = to_json(&chat_send).expect("ChatSend serialization should succeed");

        // snake_case type discriminator
        prop_assert!(
            send_json.contains(r#""type":"chat_send""#),
            "ChatSend JSON should contain type:chat_send, got: {}", send_json
        );

        // text field present
        prop_assert!(
            send_json.contains(r#""text":"#),
            "ChatSend JSON should contain 'text' key, got: {}", send_json
        );
    }
}
