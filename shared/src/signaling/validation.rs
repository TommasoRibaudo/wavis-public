use super::*;

/// Field length limits for signaling messages.
pub const MAX_ROOM_ID_LEN: usize = 128;
pub const MAX_PEER_ID_LEN: usize = 128;
pub const MAX_DISPLAY_NAME_LEN: usize = 64;
pub const MAX_INVITE_CODE_LEN: usize = 64;
pub const MAX_SDP_LEN: usize = 65_536; // 64 KB
pub const MAX_CANDIDATE_LEN: usize = 2_048; // 2 KB
pub const MAX_CHANNEL_ID_LEN: usize = 64;
pub const MAX_CHAT_TEXT_LEN: usize = 2_000;
pub const MAX_PROFILE_COLOR_LEN: usize = 16;
pub const MAX_CHAT_HISTORY_SINCE_LEN: usize = 64;
pub const MAX_SUB_ROOM_ID_LEN: usize = 128;

/// Error returned when a field exceeds its maximum allowed length.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ValidationError {
    pub field: String,
    pub actual_len: usize,
    pub max_len: usize,
}

impl std::fmt::Display for ValidationError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "field '{}' exceeds max length: {} > {}",
            self.field, self.actual_len, self.max_len
        )
    }
}

impl std::error::Error for ValidationError {}

/// Check a single string field against a max length.
/// Returns `Err(ValidationError)` if the field exceeds the limit.
#[inline]
fn check(field: &str, value: &str, max_len: usize) -> Result<(), ValidationError> {
    let actual_len = value.len();
    if actual_len > max_len {
        Err(ValidationError {
            field: field.to_string(),
            actual_len,
            max_len,
        })
    } else {
        Ok(())
    }
}

/// Validate all string field lengths in a `SignalingMessage`.
///
/// Returns `Ok(())` if all fields are within limits, or `Err` with the first
/// offending field's name and actual length.
///
/// Called after serde deserialization, before domain processing (Req 14.2).
pub fn validate_field_lengths(msg: &SignalingMessage) -> Result<(), ValidationError> {
    match msg {
        SignalingMessage::Join(p) => {
            check("room_id", &p.room_id, MAX_ROOM_ID_LEN)?;
            if let Some(code) = &p.invite_code {
                check("invite_code", code, MAX_INVITE_CODE_LEN)?;
            }
            if let Some(name) = &p.display_name {
                check("display_name", name, MAX_DISPLAY_NAME_LEN)?;
            }
            if let Some(color) = &p.profile_color {
                check("profileColor", color, MAX_PROFILE_COLOR_LEN)?;
            }
        }
        SignalingMessage::Joined(p) => {
            check("room_id", &p.room_id, MAX_ROOM_ID_LEN)?;
            check("peer_id", &p.peer_id, MAX_PEER_ID_LEN)?;
            for participant in &p.participants {
                check(
                    "participant_id",
                    &participant.participant_id,
                    MAX_PEER_ID_LEN,
                )?;
                check(
                    "display_name",
                    &participant.display_name,
                    MAX_DISPLAY_NAME_LEN,
                )?;
                if let Some(color) = &participant.profile_color {
                    check("profileColor", color, MAX_PROFILE_COLOR_LEN)?;
                }
            }
        }
        SignalingMessage::Offer(p) => {
            check("sdp", &p.session_description.sdp, MAX_SDP_LEN)?;
        }
        SignalingMessage::Answer(p) => {
            check("sdp", &p.session_description.sdp, MAX_SDP_LEN)?;
        }
        SignalingMessage::IceCandidate(p) => {
            check("candidate", &p.candidate.candidate, MAX_CANDIDATE_LEN)?;
        }
        SignalingMessage::PeerLeft => {}
        SignalingMessage::Leave => {}
        SignalingMessage::Error(p) => {
            // No strict limit on error messages, but cap at SDP length to prevent abuse
            check("message", &p.message, MAX_SDP_LEN)?;
        }
        SignalingMessage::JoinRejected(_) => {}
        SignalingMessage::InviteCreate(_) => {}
        SignalingMessage::InviteCreated(p) => {
            check("invite_code", &p.invite_code, MAX_INVITE_CODE_LEN)?;
        }
        SignalingMessage::InviteRevoke(p) => {
            check("invite_code", &p.invite_code, MAX_INVITE_CODE_LEN)?;
        }
        SignalingMessage::InviteRevoked(p) => {
            check("invite_code", &p.invite_code, MAX_INVITE_CODE_LEN)?;
        }
        SignalingMessage::ParticipantJoined(p) => {
            check("participant_id", &p.participant_id, MAX_PEER_ID_LEN)?;
            check("display_name", &p.display_name, MAX_DISPLAY_NAME_LEN)?;
            if let Some(color) = &p.profile_color {
                check("profileColor", color, MAX_PROFILE_COLOR_LEN)?;
            }
        }
        SignalingMessage::ParticipantLeft(p) => {
            check("participant_id", &p.participant_id, MAX_PEER_ID_LEN)?;
        }
        SignalingMessage::RoomState(p) => {
            for participant in &p.participants {
                check(
                    "participant_id",
                    &participant.participant_id,
                    MAX_PEER_ID_LEN,
                )?;
                check(
                    "display_name",
                    &participant.display_name,
                    MAX_DISPLAY_NAME_LEN,
                )?;
                if let Some(color) = &participant.profile_color {
                    check("profileColor", color, MAX_PROFILE_COLOR_LEN)?;
                }
            }
        }
        SignalingMessage::MediaToken(_) => {}
        SignalingMessage::KickParticipant(p) => {
            check(
                "target_participant_id",
                &p.target_participant_id,
                MAX_PEER_ID_LEN,
            )?;
        }
        SignalingMessage::MuteParticipant(p) => {
            check(
                "target_participant_id",
                &p.target_participant_id,
                MAX_PEER_ID_LEN,
            )?;
        }
        SignalingMessage::UnmuteParticipant(p) => {
            check(
                "target_participant_id",
                &p.target_participant_id,
                MAX_PEER_ID_LEN,
            )?;
        }
        SignalingMessage::ParticipantKicked(p) => {
            check("participant_id", &p.participant_id, MAX_PEER_ID_LEN)?;
        }
        SignalingMessage::ParticipantMuted(p) => {
            check("participant_id", &p.participant_id, MAX_PEER_ID_LEN)?;
        }
        SignalingMessage::ParticipantUnmuted(p) => {
            check("participant_id", &p.participant_id, MAX_PEER_ID_LEN)?;
        }
        SignalingMessage::SelfDeafen => {}
        SignalingMessage::SelfUndeafen => {}
        SignalingMessage::ParticipantDeafened(p) => {
            check("participant_id", &p.participant_id, MAX_PEER_ID_LEN)?;
        }
        SignalingMessage::ParticipantUndeafened(p) => {
            check("participant_id", &p.participant_id, MAX_PEER_ID_LEN)?;
        }
        SignalingMessage::StartShare => {}
        SignalingMessage::ShareStarted(p) => {
            check("participant_id", &p.participant_id, MAX_PEER_ID_LEN)?;
            check("display_name", &p.display_name, MAX_DISPLAY_NAME_LEN)?;
        }
        SignalingMessage::StopShare(p) => {
            if let Some(target) = &p.target_participant_id {
                check("target_participant_id", target, MAX_PEER_ID_LEN)?;
            }
        }
        SignalingMessage::ShareStopped(p) => {
            check("participant_id", &p.participant_id, MAX_PEER_ID_LEN)?;
            check("display_name", &p.display_name, MAX_DISPLAY_NAME_LEN)?;
        }
        SignalingMessage::StopAllShares => {}
        SignalingMessage::ShareState(p) => {
            for id in &p.participant_ids {
                if id.is_empty() {
                    return Err(ValidationError {
                        field: "participant_id".to_string(),
                        actual_len: 0,
                        max_len: MAX_PEER_ID_LEN,
                    });
                }
                check("participant_id", id, MAX_PEER_ID_LEN)?;
            }
        }
        SignalingMessage::SetSharePermission(_) => {
            // permission is a WireSharePermission enum — validated at deserialization
        }
        SignalingMessage::SharePermissionChanged(_) => {
            // permission is a WireSharePermission enum — validated at deserialization
        }
        SignalingMessage::CreateRoom(p) => {
            check("room_id", &p.room_id, MAX_ROOM_ID_LEN)?;
            if let Some(name) = &p.display_name {
                check("display_name", name, MAX_DISPLAY_NAME_LEN)?;
            }
            if let Some(color) = &p.profile_color {
                check("profileColor", color, MAX_PROFILE_COLOR_LEN)?;
            }
        }
        SignalingMessage::RoomCreated(p) => {
            check("room_id", &p.room_id, MAX_ROOM_ID_LEN)?;
            check("peer_id", &p.peer_id, MAX_PEER_ID_LEN)?;
            check("invite_code", &p.invite_code, MAX_INVITE_CODE_LEN)?;
        }
        SignalingMessage::Auth(p) => {
            check("accessToken", &p.access_token, 2048)?;
        }
        SignalingMessage::AuthSuccess(p) => {
            check("userId", &p.user_id, 64)?;
        }
        SignalingMessage::AuthFailed(p) => {
            check("reason", &p.reason, 256)?;
        }
        SignalingMessage::JoinVoice(p) => {
            check("channelId", &p.channel_id, MAX_CHANNEL_ID_LEN)?;
            if let Some(ref name) = p.display_name {
                check("displayName", name, MAX_DISPLAY_NAME_LEN)?;
            }
            if let Some(ref color) = p.profile_color {
                check("profileColor", color, MAX_PROFILE_COLOR_LEN)?;
            }
        }
        SignalingMessage::CreateSubRoom(_) => {}
        SignalingMessage::JoinSubRoom(p) => {
            check("subRoomId", &p.sub_room_id, MAX_SUB_ROOM_ID_LEN)?;
        }
        SignalingMessage::LeaveSubRoom(_) => {}
        SignalingMessage::SubRoomState(p) => {
            for room in &p.rooms {
                check("subRoomId", &room.sub_room_id, MAX_SUB_ROOM_ID_LEN)?;
                for participant_id in &room.participant_ids {
                    check("participantId", participant_id, MAX_PEER_ID_LEN)?;
                }
            }
        }
        SignalingMessage::SubRoomCreated(p) => {
            check("subRoomId", &p.room.sub_room_id, MAX_SUB_ROOM_ID_LEN)?;
            for participant_id in &p.room.participant_ids {
                check("participantId", participant_id, MAX_PEER_ID_LEN)?;
            }
        }
        SignalingMessage::SubRoomJoined(p) => {
            check("participantId", &p.participant_id, MAX_PEER_ID_LEN)?;
            check("subRoomId", &p.sub_room_id, MAX_SUB_ROOM_ID_LEN)?;
        }
        SignalingMessage::SubRoomLeft(p) => {
            check("participantId", &p.participant_id, MAX_PEER_ID_LEN)?;
            check("subRoomId", &p.sub_room_id, MAX_SUB_ROOM_ID_LEN)?;
        }
        SignalingMessage::SubRoomDeleted(p) => {
            check("subRoomId", &p.sub_room_id, MAX_SUB_ROOM_ID_LEN)?;
        }
        SignalingMessage::SfuColdStarting(_) => {}
        SignalingMessage::ChatSend(p) => {
            check("text", &p.text, MAX_CHAT_TEXT_LEN)?;
        }
        // Defensive guard: ChatMessage is server-constructed and broadcast — clients should
        // never send it. This arm exists to catch future bugs if a code path accidentally
        // routes a client-supplied ChatMessage through validation.
        SignalingMessage::ChatMessage(p) => {
            check("text", &p.text, MAX_CHAT_TEXT_LEN)?;
            check("participantId", &p.participant_id, MAX_PEER_ID_LEN)?;
            check("displayName", &p.display_name, MAX_DISPLAY_NAME_LEN)?;
        }
        SignalingMessage::ChatHistoryRequest(p) => {
            if let Some(ref since) = p.since {
                check("since", since, MAX_CHAT_HISTORY_SINCE_LEN)?;
            }
        }
        // ChatHistoryResponse is server-generated — no validation needed
        SignalingMessage::ChatHistoryResponse(_) => {}
        SignalingMessage::Ping => {}
        // SessionDisplaced is server-generated — no validation needed
        SignalingMessage::SessionDisplaced(_) => {}
        SignalingMessage::ViewerSubscribed(p) => {
            check("targetId", &p.target_id, MAX_PEER_ID_LEN)?;
        }
        // ViewerJoined is server-generated — no validation needed
        SignalingMessage::ViewerJoined(_) => {}
        SignalingMessage::UpdateProfileColor(p) => {
            check("profileColor", &p.profile_color, MAX_PROFILE_COLOR_LEN)?;
        }
        // ParticipantColorUpdated is server-generated — validate defensively
        SignalingMessage::ParticipantColorUpdated(p) => {
            check("participant_id", &p.participant_id, MAX_PEER_ID_LEN)?;
            check("profileColor", &p.profile_color, MAX_PROFILE_COLOR_LEN)?;
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use proptest::prelude::*;

    fn long_str(n: usize) -> String {
        "x".repeat(n)
    }

    // --- Unit tests for boundary conditions ---

    #[test]
    fn valid_join_passes() {
        let msg = SignalingMessage::Join(JoinPayload {
            room_id: "room-1".to_string(),
            room_type: None,
            invite_code: Some("code-abc".to_string()),
            display_name: None,
            profile_color: None,
        });
        assert!(validate_field_lengths(&msg).is_ok());
    }

    #[test]
    fn oversized_room_id_rejected() {
        let msg = SignalingMessage::Join(JoinPayload {
            room_id: long_str(MAX_ROOM_ID_LEN + 1),
            room_type: None,
            invite_code: None,
            display_name: None,
            profile_color: None,
        });
        let err = validate_field_lengths(&msg).unwrap_err();
        assert_eq!(err.field, "room_id");
        assert_eq!(err.max_len, MAX_ROOM_ID_LEN);
    }

    #[test]
    fn valid_join_sub_room_passes() {
        let msg = SignalingMessage::JoinSubRoom(JoinSubRoomPayload {
            sub_room_id: "sub-room-2".to_string(),
        });
        assert!(validate_field_lengths(&msg).is_ok());
    }

    #[test]
    fn oversized_sub_room_id_rejected() {
        let msg = SignalingMessage::JoinSubRoom(JoinSubRoomPayload {
            sub_room_id: long_str(MAX_SUB_ROOM_ID_LEN + 1),
        });
        let err = validate_field_lengths(&msg).unwrap_err();
        assert_eq!(err.field, "subRoomId");
        assert_eq!(err.max_len, MAX_SUB_ROOM_ID_LEN);
    }

    #[test]
    fn exact_max_room_id_passes() {
        let msg = SignalingMessage::Join(JoinPayload {
            room_id: long_str(MAX_ROOM_ID_LEN),
            room_type: None,
            invite_code: None,
            display_name: None,
            profile_color: None,
        });
        assert!(validate_field_lengths(&msg).is_ok());
    }

    #[test]
    fn oversized_sdp_rejected() {
        let msg = SignalingMessage::Offer(OfferPayload {
            session_description: SessionDescription {
                sdp: long_str(MAX_SDP_LEN + 1),
                sdp_type: "offer".to_string(),
            },
        });
        let err = validate_field_lengths(&msg).unwrap_err();
        assert_eq!(err.field, "sdp");
        assert_eq!(err.max_len, MAX_SDP_LEN);
    }

    #[test]
    fn oversized_candidate_rejected() {
        let msg = SignalingMessage::IceCandidate(IceCandidatePayload {
            candidate: IceCandidate {
                candidate: long_str(MAX_CANDIDATE_LEN + 1),
                sdp_mid: "0".to_string(),
                sdp_mline_index: 0,
            },
        });
        let err = validate_field_lengths(&msg).unwrap_err();
        assert_eq!(err.field, "candidate");
        assert_eq!(err.max_len, MAX_CANDIDATE_LEN);
    }

    #[test]
    fn oversized_invite_code_rejected() {
        let msg = SignalingMessage::InviteRevoke(InviteRevokePayload {
            invite_code: long_str(MAX_INVITE_CODE_LEN + 1),
        });
        let err = validate_field_lengths(&msg).unwrap_err();
        assert_eq!(err.field, "invite_code");
    }

    #[test]
    fn oversized_display_name_rejected() {
        let msg = SignalingMessage::ParticipantJoined(ParticipantJoinedPayload {
            participant_id: "peer-1".to_string(),
            display_name: long_str(MAX_DISPLAY_NAME_LEN + 1),
            user_id: None,
            profile_color: None,
        });
        let err = validate_field_lengths(&msg).unwrap_err();
        assert_eq!(err.field, "display_name");
    }

    #[test]
    fn screen_share_variants_validate_participant_id() {
        let msg = SignalingMessage::ShareStarted(ShareStartedPayload {
            participant_id: long_str(MAX_PEER_ID_LEN + 1),
            display_name: "test".to_string(),
        });
        let err = validate_field_lengths(&msg).unwrap_err();
        assert_eq!(err.field, "participant_id");

        let msg2 = SignalingMessage::StopShare(StopSharePayload {
            target_participant_id: Some(long_str(MAX_PEER_ID_LEN + 1)),
        });
        let err2 = validate_field_lengths(&msg2).unwrap_err();
        assert_eq!(err2.field, "target_participant_id");
    }

    #[test]
    fn share_state_empty_participant_id_rejected() {
        let msg = SignalingMessage::ShareState(ShareStatePayload {
            participant_ids: vec!["peer-1".to_string(), "".to_string()],
        });
        let err = validate_field_lengths(&msg).unwrap_err();
        assert_eq!(err.field, "participant_id");
        assert_eq!(err.actual_len, 0);
    }

    #[test]
    fn share_state_oversized_participant_id_rejected() {
        let msg = SignalingMessage::ShareState(ShareStatePayload {
            participant_ids: vec![long_str(MAX_PEER_ID_LEN + 1)],
        });
        let err = validate_field_lengths(&msg).unwrap_err();
        assert_eq!(err.field, "participant_id");
    }

    #[test]
    fn share_state_valid_passes() {
        let msg = SignalingMessage::ShareState(ShareStatePayload {
            participant_ids: vec!["peer-1".to_string(), "peer-2".to_string()],
        });
        assert!(validate_field_lengths(&msg).is_ok());
    }

    #[test]
    fn share_state_empty_list_passes() {
        let msg = SignalingMessage::ShareState(ShareStatePayload {
            participant_ids: vec![],
        });
        assert!(validate_field_lengths(&msg).is_ok());
    }

    #[test]
    fn stop_all_shares_passes() {
        let msg = SignalingMessage::StopAllShares;
        assert!(validate_field_lengths(&msg).is_ok());
    }

    // Feature: phase3-security-hardening, Property 11: Field length validation rejects oversized fields
    // Validates: Requirements 11.2, 11.3, 14.3
    proptest! {
        #![proptest_config(ProptestConfig::with_cases(200))]

        #[test]
        fn prop11_oversized_room_id_rejected(
            excess in 1usize..=1000usize,
        ) {
            let msg = SignalingMessage::Join(JoinPayload {
                room_id: long_str(MAX_ROOM_ID_LEN + excess),
                room_type: None,
                invite_code: None,
                display_name: None,
                profile_color: None,
            });
            let result = validate_field_lengths(&msg);
            prop_assert!(result.is_err(), "oversized room_id must be rejected");
            let err = result.unwrap_err();
            prop_assert_eq!(&err.field, "room_id");
            prop_assert!(err.actual_len > MAX_ROOM_ID_LEN);
        }

        #[test]
        fn prop11_valid_room_id_passes(
            len in 0usize..=MAX_ROOM_ID_LEN,
        ) {
            let msg = SignalingMessage::Join(JoinPayload {
                room_id: long_str(len),
                room_type: None,
                invite_code: None,
                display_name: None,
                profile_color: None,
            });
            prop_assert!(validate_field_lengths(&msg).is_ok());
        }

        #[test]
        fn prop11_oversized_sdp_rejected(
            excess in 1usize..=1000usize,
        ) {
            let msg = SignalingMessage::Offer(OfferPayload {
                session_description: SessionDescription {
                    sdp: long_str(MAX_SDP_LEN + excess),
                    sdp_type: "offer".to_string(),
                },
            });
            let result = validate_field_lengths(&msg);
            prop_assert!(result.is_err());
            prop_assert_eq!(&result.unwrap_err().field, "sdp");
        }

        #[test]
        fn prop11_oversized_candidate_rejected(
            excess in 1usize..=1000usize,
        ) {
            let msg = SignalingMessage::IceCandidate(IceCandidatePayload {
                candidate: IceCandidate {
                    candidate: long_str(MAX_CANDIDATE_LEN + excess),
                    sdp_mid: "0".to_string(),
                    sdp_mline_index: 0,
                },
            });
            let result = validate_field_lengths(&msg);
            prop_assert!(result.is_err());
            prop_assert_eq!(&result.unwrap_err().field, "candidate");
        }

        #[test]
        fn prop11_oversized_peer_id_rejected(
            excess in 1usize..=1000usize,
        ) {
            let msg = SignalingMessage::KickParticipant(KickParticipantPayload {
                target_participant_id: long_str(MAX_PEER_ID_LEN + excess),
            });
            let result = validate_field_lengths(&msg);
            prop_assert!(result.is_err());
            prop_assert_eq!(&result.unwrap_err().field, "target_participant_id");
        }

        #[test]
        fn prop11_oversized_display_name_rejected(
            excess in 1usize..=1000usize,
        ) {
            let msg = SignalingMessage::ParticipantJoined(ParticipantJoinedPayload {
                participant_id: "peer-1".to_string(),
                display_name: long_str(MAX_DISPLAY_NAME_LEN + excess),
                user_id: None,
                profile_color: None,
            });
            let result = validate_field_lengths(&msg);
            prop_assert!(result.is_err());
            prop_assert_eq!(&result.unwrap_err().field, "display_name");
        }

        #[test]
        fn prop11_oversized_invite_code_rejected(
            excess in 1usize..=1000usize,
        ) {
            let msg = SignalingMessage::InviteRevoke(InviteRevokePayload {
                invite_code: long_str(MAX_INVITE_CODE_LEN + excess),
            });
            let result = validate_field_lengths(&msg);
            prop_assert!(result.is_err());
            prop_assert_eq!(&result.unwrap_err().field, "invite_code");
        }
    }

    // Feature: channel-voice-orchestration, Property 16: JoinVoice field-length validation
    // Validates: Requirements 5.5, 10.3
    proptest! {
        #![proptest_config(ProptestConfig::with_cases(200))]

        #[test]
        fn prop16_oversized_channel_id_rejected(
            excess in 1usize..=1000usize,
        ) {
            let msg = SignalingMessage::JoinVoice(JoinVoicePayload {
                channel_id: long_str(MAX_CHANNEL_ID_LEN + excess),
                display_name: None,
                profile_color: None,
                supports_sub_rooms: None,
            });
            let result = validate_field_lengths(&msg);
            prop_assert!(result.is_err(), "oversized channelId must be rejected");
            let err = result.unwrap_err();
            prop_assert_eq!(&err.field, "channelId");
            prop_assert!(err.actual_len > MAX_CHANNEL_ID_LEN);
        }

        #[test]
        fn prop16_oversized_display_name_rejected(
            excess in 1usize..=1000usize,
        ) {
            let msg = SignalingMessage::JoinVoice(JoinVoicePayload {
                channel_id: "valid-channel-id".to_string(),
                display_name: Some(long_str(MAX_DISPLAY_NAME_LEN + excess)),
                profile_color: None,
                supports_sub_rooms: None,
            });
            let result = validate_field_lengths(&msg);
            prop_assert!(result.is_err(), "oversized displayName must be rejected");
            let err = result.unwrap_err();
            prop_assert_eq!(&err.field, "displayName");
            prop_assert!(err.actual_len > MAX_DISPLAY_NAME_LEN);
        }

        #[test]
        fn prop16_valid_join_voice_passes(
            channel_id_len in 0usize..=MAX_CHANNEL_ID_LEN,
            display_name_len in 0usize..=MAX_DISPLAY_NAME_LEN,
            has_display_name in proptest::bool::ANY,
        ) {
            let display_name = if has_display_name {
                Some(long_str(display_name_len))
            } else {
                None
            };
            let msg = SignalingMessage::JoinVoice(JoinVoicePayload {
                channel_id: long_str(channel_id_len),
                display_name,
                profile_color: None,
                supports_sub_rooms: None,
            });
            prop_assert!(validate_field_lengths(&msg).is_ok(),
                "valid-length JoinVoice must pass validation");
        }
    }

    // --- ChatSend error message format tests ---

    /// Verify exact error format for oversized ChatSend text field.
    #[test]
    fn chat_send_oversized_text_error_format() {
        let msg = SignalingMessage::ChatSend(ChatSendPayload {
            text: long_str(2001),
        });
        let err = validate_field_lengths(&msg).unwrap_err();
        assert_eq!(err.field, "text");
        assert_eq!(err.actual_len, 2001);
        assert_eq!(err.max_len, MAX_CHAT_TEXT_LEN);
        // Verify the Display format matches what the handler sends as the Error payload.
        assert_eq!(
            err.to_string(),
            "field 'text' exceeds max length: 2001 > 2000"
        );
    }

    /// ChatSend at exactly 2000 chars passes validation.
    #[test]
    fn chat_send_exact_max_text_passes() {
        let msg = SignalingMessage::ChatSend(ChatSendPayload {
            text: long_str(MAX_CHAT_TEXT_LEN),
        });
        assert!(validate_field_lengths(&msg).is_ok());
    }

    // Feature: ephemeral-room-chat, Property 11: Field length validation rejects oversized chat text
    // Validates: Requirements 6.1, 6.2, 6.4
    proptest! {
        #![proptest_config(ProptestConfig::with_cases(100))]

        #[test]
        fn prop11_chat_oversized_text_rejected(
            excess in 1usize..=2000usize,
        ) {
            let msg = SignalingMessage::ChatSend(ChatSendPayload {
                text: long_str(MAX_CHAT_TEXT_LEN + excess),
            });
            let result = validate_field_lengths(&msg);
            prop_assert!(result.is_err(), "oversized chat text must be rejected");
            let err = result.unwrap_err();
            prop_assert_eq!(&err.field, "text");
            prop_assert!(err.actual_len > MAX_CHAT_TEXT_LEN);
        }

        #[test]
        fn prop11_chat_valid_text_passes(
            len in 1usize..=MAX_CHAT_TEXT_LEN,
        ) {
            let msg = SignalingMessage::ChatSend(ChatSendPayload {
                text: long_str(len),
            });
            prop_assert!(validate_field_lengths(&msg).is_ok(),
                "chat text of 1-2000 chars must pass validation");
        }
    }

    // Feature: phase3-security-hardening, Property 12: Unknown signaling variants are rejected
    // Validates: Requirements 11.1
    proptest! {
        #![proptest_config(ProptestConfig::with_cases(200))]

        #[test]
        fn prop12_unknown_type_rejected(
            type_val in "[a-z_]{1,32}",
        ) {
            let known = [
                "join", "joined", "join_rejected", "invite_create", "invite_created",
                "invite_revoke", "invite_revoked", "offer", "answer", "ice_candidate",
                "peer_left", "leave", "error", "participant_joined", "participant_left",
                "room_state", "media_token", "kick_participant", "mute_participant",
                "participant_kicked", "participant_muted",
                "start_share", "share_started", "stop_share", "share_stopped",
                "stop_all_shares", "share_state",
                "create_room", "room_created",
                "auth", "auth_success", "auth_failed",
                "join_voice",
                "sfu_cold_starting",
                "chat_send", "chat_message",
                "chat_history_request", "chat_history_response",
                "unmute_participant", "participant_unmuted",
                "set_share_permission", "share_permission_changed",
                "ping",
                "update_profile_color", "participant_color_updated",
            ];
            prop_assume!(!known.contains(&type_val.as_str()));

            let json = format!(r#"{{"type":"{}"}}"#, type_val);
            let result = parse(&json);
            prop_assert!(result.is_err(),
                "unknown type '{}' should produce a ParseError", type_val);
        }
    }

    // Feature: chat-history-persistence, Property 10: Field-length validation
    // Validates: Requirements 7.1, 7.2
    proptest! {
        #![proptest_config(ProptestConfig::with_cases(100))]

        #[test]
        fn property_10_chat_history_request_since_within_limit_passes(
            len in 0usize..=MAX_CHAT_HISTORY_SINCE_LEN,
        ) {
            let msg = SignalingMessage::ChatHistoryRequest(ChatHistoryRequestPayload {
                since: Some(long_str(len)),
            });
            prop_assert!(validate_field_lengths(&msg).is_ok(),
                "since field of length {} must pass validation", len);
        }

        #[test]
        fn property_10_chat_history_request_since_over_limit_rejected(
            excess in 1usize..=200usize,
        ) {
            let msg = SignalingMessage::ChatHistoryRequest(ChatHistoryRequestPayload {
                since: Some(long_str(MAX_CHAT_HISTORY_SINCE_LEN + excess)),
            });
            let result = validate_field_lengths(&msg);
            prop_assert!(result.is_err(), "since field of length {} must be rejected",
                MAX_CHAT_HISTORY_SINCE_LEN + excess);
            let err = result.unwrap_err();
            prop_assert_eq!(&err.field, "since");
            prop_assert!(err.actual_len > MAX_CHAT_HISTORY_SINCE_LEN);
        }

        #[test]
        fn property_10_chat_history_request_since_none_passes(
            _dummy in 0u8..1u8,
        ) {
            let msg = SignalingMessage::ChatHistoryRequest(ChatHistoryRequestPayload {
                since: None,
            });
            prop_assert!(validate_field_lengths(&msg).is_ok(),
                "ChatHistoryRequest with since: None must pass validation");
        }

        #[test]
        fn property_10_chat_history_response_always_passes(
            msg_count in 0usize..=10usize,
            text_len in 0usize..=200usize,
        ) {
            let messages: Vec<ChatHistoryMessagePayload> = (0..msg_count)
                .map(|i| ChatHistoryMessagePayload {
                    message_id: format!("msg-{}", i),
                    participant_id: format!("peer-{}", i),
                    display_name: format!("user-{}", i),
                    text: long_str(text_len),
                    timestamp: "2025-01-15T10:00:00Z".to_string(),
                })
                .collect();
            let msg = SignalingMessage::ChatHistoryResponse(ChatHistoryResponsePayload {
                messages,
            });
            prop_assert!(validate_field_lengths(&msg).is_ok(),
                "ChatHistoryResponse must always pass validation regardless of content");
        }
    }
}
