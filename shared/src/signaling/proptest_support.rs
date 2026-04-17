use super::*;
use proptest::prelude::*;

// Arbitrary implementations for property testing.
// These are exposed when the "proptest-support" feature is enabled,
// allowing other crates (like wavis-backend) to use them in their tests.

impl Arbitrary for SessionDescription {
    type Parameters = ();
    type Strategy = BoxedStrategy<Self>;

    fn arbitrary_with(_: Self::Parameters) -> Self::Strategy {
        (
            any::<String>(),
            prop::sample::select(vec!["offer".to_string(), "answer".to_string()]),
        )
            .prop_map(|(sdp, sdp_type)| SessionDescription { sdp, sdp_type })
            .boxed()
    }
}

impl Arbitrary for JoinPayload {
    type Parameters = ();
    type Strategy = BoxedStrategy<Self>;

    fn arbitrary_with(_: Self::Parameters) -> Self::Strategy {
        any::<String>()
            .prop_map(|room_id| JoinPayload {
                room_id,
                room_type: None,
                invite_code: None,
                display_name: None,
                profile_color: None,
            })
            .boxed()
    }
}

impl Arbitrary for JoinedPayload {
    type Parameters = ();
    type Strategy = BoxedStrategy<Self>;

    fn arbitrary_with(_: Self::Parameters) -> Self::Strategy {
        (
            any::<String>(),
            any::<String>(),
            1..=6u32,
            prop::option::of(any::<IceConfigPayload>()),
            prop::option::of(any::<WireSharePermission>()),
        )
            .prop_map(
                |(room_id, peer_id, peer_count, ice_config, share_permission)| JoinedPayload {
                    room_id,
                    peer_id,
                    peer_count,
                    participants: vec![],
                    ice_config,
                    share_permission,
                },
            )
            .boxed()
    }
}

impl Arbitrary for OfferPayload {
    type Parameters = ();
    type Strategy = BoxedStrategy<Self>;

    fn arbitrary_with(_: Self::Parameters) -> Self::Strategy {
        any::<SessionDescription>()
            .prop_map(|session_description| OfferPayload {
                session_description,
            })
            .boxed()
    }
}

impl Arbitrary for AnswerPayload {
    type Parameters = ();
    type Strategy = BoxedStrategy<Self>;

    fn arbitrary_with(_: Self::Parameters) -> Self::Strategy {
        any::<SessionDescription>()
            .prop_map(|session_description| AnswerPayload {
                session_description,
            })
            .boxed()
    }
}

impl Arbitrary for IceCandidate {
    type Parameters = ();
    type Strategy = BoxedStrategy<Self>;

    fn arbitrary_with(_: Self::Parameters) -> Self::Strategy {
        (any::<String>(), any::<String>(), any::<u16>())
            .prop_map(|(candidate, sdp_mid, sdp_mline_index)| IceCandidate {
                candidate,
                sdp_mid,
                sdp_mline_index,
            })
            .boxed()
    }
}

impl Arbitrary for IceCandidatePayload {
    type Parameters = ();
    type Strategy = BoxedStrategy<Self>;

    fn arbitrary_with(_: Self::Parameters) -> Self::Strategy {
        any::<IceCandidate>()
            .prop_map(|candidate| IceCandidatePayload { candidate })
            .boxed()
    }
}

impl Arbitrary for ErrorPayload {
    type Parameters = ();
    type Strategy = BoxedStrategy<Self>;

    fn arbitrary_with(_: Self::Parameters) -> Self::Strategy {
        any::<String>()
            .prop_map(|message| ErrorPayload { message })
            .boxed()
    }
}

impl Arbitrary for ParticipantInfo {
    type Parameters = ();
    type Strategy = BoxedStrategy<Self>;

    fn arbitrary_with(_: Self::Parameters) -> Self::Strategy {
        (
            any::<String>(),
            any::<String>(),
            proptest::option::of(any::<String>()),
            proptest::option::of(any::<String>()),
        )
            .prop_map(
                |(participant_id, display_name, user_id, profile_color)| ParticipantInfo {
                    participant_id,
                    display_name,
                    user_id,
                    profile_color,
                },
            )
            .boxed()
    }
}

impl Arbitrary for ParticipantJoinedPayload {
    type Parameters = ();
    type Strategy = BoxedStrategy<Self>;

    fn arbitrary_with(_: Self::Parameters) -> Self::Strategy {
        (
            any::<String>(),
            any::<String>(),
            proptest::option::of(any::<String>()),
            proptest::option::of(any::<String>()),
        )
            .prop_map(|(participant_id, display_name, user_id, profile_color)| {
                ParticipantJoinedPayload {
                    participant_id,
                    display_name,
                    user_id,
                    profile_color,
                }
            })
            .boxed()
    }
}

impl Arbitrary for ParticipantLeftPayload {
    type Parameters = ();
    type Strategy = BoxedStrategy<Self>;

    fn arbitrary_with(_: Self::Parameters) -> Self::Strategy {
        any::<String>()
            .prop_map(|participant_id| ParticipantLeftPayload { participant_id })
            .boxed()
    }
}

impl Arbitrary for RoomStatePayload {
    type Parameters = ();
    type Strategy = BoxedStrategy<Self>;

    fn arbitrary_with(_: Self::Parameters) -> Self::Strategy {
        prop::collection::vec(any::<ParticipantInfo>(), 0..=6)
            .prop_map(|participants| RoomStatePayload { participants })
            .boxed()
    }
}

impl Arbitrary for MediaTokenPayload {
    type Parameters = ();
    type Strategy = BoxedStrategy<Self>;

    fn arbitrary_with(_: Self::Parameters) -> Self::Strategy {
        (any::<String>(), any::<String>())
            .prop_map(|(token, sfu_url)| MediaTokenPayload { token, sfu_url })
            .boxed()
    }
}

impl Arbitrary for KickParticipantPayload {
    type Parameters = ();
    type Strategy = BoxedStrategy<Self>;

    fn arbitrary_with(_: Self::Parameters) -> Self::Strategy {
        any::<String>()
            .prop_map(|target_participant_id| KickParticipantPayload {
                target_participant_id,
            })
            .boxed()
    }
}

impl Arbitrary for MuteParticipantPayload {
    type Parameters = ();
    type Strategy = BoxedStrategy<Self>;

    fn arbitrary_with(_: Self::Parameters) -> Self::Strategy {
        any::<String>()
            .prop_map(|target_participant_id| MuteParticipantPayload {
                target_participant_id,
            })
            .boxed()
    }
}

impl Arbitrary for UnmuteParticipantPayload {
    type Parameters = ();
    type Strategy = BoxedStrategy<Self>;

    fn arbitrary_with(_: Self::Parameters) -> Self::Strategy {
        any::<String>()
            .prop_map(|target_participant_id| UnmuteParticipantPayload {
                target_participant_id,
            })
            .boxed()
    }
}

impl Arbitrary for ParticipantKickedPayload {
    type Parameters = ();
    type Strategy = BoxedStrategy<Self>;

    fn arbitrary_with(_: Self::Parameters) -> Self::Strategy {
        (any::<String>(), any::<String>())
            .prop_map(|(participant_id, reason)| ParticipantKickedPayload {
                participant_id,
                reason,
            })
            .boxed()
    }
}

impl Arbitrary for ParticipantMutedPayload {
    type Parameters = ();
    type Strategy = BoxedStrategy<Self>;

    fn arbitrary_with(_: Self::Parameters) -> Self::Strategy {
        any::<String>()
            .prop_map(|participant_id| ParticipantMutedPayload { participant_id })
            .boxed()
    }
}

impl Arbitrary for ParticipantUnmutedPayload {
    type Parameters = ();
    type Strategy = BoxedStrategy<Self>;

    fn arbitrary_with(_: Self::Parameters) -> Self::Strategy {
        any::<String>()
            .prop_map(|participant_id| ParticipantUnmutedPayload { participant_id })
            .boxed()
    }
}

impl Arbitrary for ParticipantDeafenedPayload {
    type Parameters = ();
    type Strategy = BoxedStrategy<Self>;

    fn arbitrary_with(_: Self::Parameters) -> Self::Strategy {
        any::<String>()
            .prop_map(|participant_id| ParticipantDeafenedPayload { participant_id })
            .boxed()
    }
}

impl Arbitrary for ParticipantUndeafenedPayload {
    type Parameters = ();
    type Strategy = BoxedStrategy<Self>;

    fn arbitrary_with(_: Self::Parameters) -> Self::Strategy {
        any::<String>()
            .prop_map(|participant_id| ParticipantUndeafenedPayload { participant_id })
            .boxed()
    }
}

impl Arbitrary for WireSharePermission {
    type Parameters = ();
    type Strategy = BoxedStrategy<Self>;

    fn arbitrary_with(_: Self::Parameters) -> Self::Strategy {
        prop_oneof![
            Just(WireSharePermission::Anyone),
            Just(WireSharePermission::HostOnly),
        ]
        .boxed()
    }
}

impl Arbitrary for JoinRejectionReason {
    type Parameters = ();
    type Strategy = BoxedStrategy<Self>;

    fn arbitrary_with(_: Self::Parameters) -> Self::Strategy {
        prop_oneof![
            Just(JoinRejectionReason::InviteExpired),
            Just(JoinRejectionReason::InviteRevoked),
            Just(JoinRejectionReason::InviteInvalid),
            Just(JoinRejectionReason::InviteRequired),
            Just(JoinRejectionReason::InviteExhausted),
            Just(JoinRejectionReason::RoomFull),
            Just(JoinRejectionReason::RateLimited),
            Just(JoinRejectionReason::NotAuthorized),
        ]
        .boxed()
    }
}

impl Arbitrary for JoinRejectedPayload {
    type Parameters = ();
    type Strategy = BoxedStrategy<Self>;

    fn arbitrary_with(_: Self::Parameters) -> Self::Strategy {
        any::<JoinRejectionReason>()
            .prop_map(|reason| JoinRejectedPayload { reason })
            .boxed()
    }
}

impl Arbitrary for SfuColdStartingPayload {
    type Parameters = ();
    type Strategy = BoxedStrategy<Self>;

    fn arbitrary_with(_: Self::Parameters) -> Self::Strategy {
        (0u32..=300u32)
            .prop_map(|estimated_wait_secs| SfuColdStartingPayload {
                estimated_wait_secs,
            })
            .boxed()
    }
}

impl Arbitrary for InviteCreatePayload {
    type Parameters = ();
    type Strategy = BoxedStrategy<Self>;

    fn arbitrary_with(_: Self::Parameters) -> Self::Strategy {
        prop::option::of(1u32..=100u32)
            .prop_map(|max_uses| InviteCreatePayload { max_uses })
            .boxed()
    }
}

impl Arbitrary for InviteCreatedPayload {
    type Parameters = ();
    type Strategy = BoxedStrategy<Self>;

    fn arbitrary_with(_: Self::Parameters) -> Self::Strategy {
        (any::<String>(), 1u64..=3600u64, 1u32..=100u32)
            .prop_map(
                |(invite_code, expires_in_secs, max_uses)| InviteCreatedPayload {
                    invite_code,
                    expires_in_secs,
                    max_uses,
                },
            )
            .boxed()
    }
}

impl Arbitrary for InviteRevokePayload {
    type Parameters = ();
    type Strategy = BoxedStrategy<Self>;

    fn arbitrary_with(_: Self::Parameters) -> Self::Strategy {
        any::<String>()
            .prop_map(|invite_code| InviteRevokePayload { invite_code })
            .boxed()
    }
}

impl Arbitrary for InviteRevokedPayload {
    type Parameters = ();
    type Strategy = BoxedStrategy<Self>;

    fn arbitrary_with(_: Self::Parameters) -> Self::Strategy {
        any::<String>()
            .prop_map(|invite_code| InviteRevokedPayload { invite_code })
            .boxed()
    }
}

impl Arbitrary for IceConfigPayload {
    type Parameters = ();
    type Strategy = BoxedStrategy<Self>;

    fn arbitrary_with(_: Self::Parameters) -> Self::Strategy {
        (
            prop::collection::vec("[a-z0-9:.]+", 0..=2),
            prop::collection::vec("[a-z0-9:.]+", 0..=2),
            any::<String>(),
            any::<String>(),
        )
            .prop_map(
                |(stun_urls, turn_urls, turn_username, turn_credential)| IceConfigPayload {
                    stun_urls,
                    turn_urls,
                    turn_username,
                    turn_credential,
                },
            )
            .boxed()
    }
}

impl Arbitrary for ShareStartedPayload {
    type Parameters = ();
    type Strategy = BoxedStrategy<Self>;

    fn arbitrary_with(_: Self::Parameters) -> Self::Strategy {
        (any::<String>(), any::<String>())
            .prop_map(|(participant_id, display_name)| ShareStartedPayload {
                participant_id,
                display_name,
            })
            .boxed()
    }
}

impl Arbitrary for StopSharePayload {
    type Parameters = ();
    type Strategy = BoxedStrategy<Self>;

    fn arbitrary_with(_: Self::Parameters) -> Self::Strategy {
        prop::option::of(any::<String>())
            .prop_map(|target_participant_id| StopSharePayload {
                target_participant_id,
            })
            .boxed()
    }
}

impl Arbitrary for ShareStoppedPayload {
    type Parameters = ();
    type Strategy = BoxedStrategy<Self>;

    fn arbitrary_with(_: Self::Parameters) -> Self::Strategy {
        (any::<String>(), any::<String>())
            .prop_map(|(participant_id, display_name)| ShareStoppedPayload {
                participant_id,
                display_name,
            })
            .boxed()
    }
}
impl Arbitrary for ShareStatePayload {
    type Parameters = ();
    type Strategy = BoxedStrategy<Self>;

    fn arbitrary_with(_: Self::Parameters) -> Self::Strategy {
        prop::collection::vec(any::<String>(), 0..=6)
            .prop_map(|participant_ids| ShareStatePayload { participant_ids })
            .boxed()
    }
}

impl Arbitrary for SetSharePermissionPayload {
    type Parameters = ();
    type Strategy = BoxedStrategy<Self>;

    fn arbitrary_with(_: Self::Parameters) -> Self::Strategy {
        any::<WireSharePermission>()
            .prop_map(|permission| SetSharePermissionPayload { permission })
            .boxed()
    }
}

impl Arbitrary for SharePermissionChangedPayload {
    type Parameters = ();
    type Strategy = BoxedStrategy<Self>;

    fn arbitrary_with(_: Self::Parameters) -> Self::Strategy {
        any::<WireSharePermission>()
            .prop_map(|permission| SharePermissionChangedPayload { permission })
            .boxed()
    }
}

impl Arbitrary for CreateRoomPayload {
    type Parameters = ();
    type Strategy = BoxedStrategy<Self>;

    fn arbitrary_with(_: Self::Parameters) -> Self::Strategy {
        (any::<String>(), prop::option::of(any::<String>()))
            .prop_map(|(room_id, room_type)| CreateRoomPayload {
                room_id,
                room_type,
                display_name: None,
                profile_color: None,
            })
            .boxed()
    }
}

impl Arbitrary for RoomCreatedPayload {
    type Parameters = ();
    type Strategy = BoxedStrategy<Self>;

    fn arbitrary_with(_: Self::Parameters) -> Self::Strategy {
        (
            any::<String>(),
            any::<String>(),
            any::<String>(),
            any::<u64>(),
            any::<u32>(),
            prop::option::of(any::<IceConfigPayload>()),
        )
            .prop_map(
                |(room_id, peer_id, invite_code, expires_in_secs, max_uses, ice_config)| {
                    RoomCreatedPayload {
                        room_id,
                        peer_id,
                        invite_code,
                        expires_in_secs,
                        max_uses,
                        ice_config,
                    }
                },
            )
            .boxed()
    }
}

impl Arbitrary for AuthPayload {
    type Parameters = ();
    type Strategy = BoxedStrategy<Self>;

    fn arbitrary_with(_: Self::Parameters) -> Self::Strategy {
        any::<String>()
            .prop_map(|access_token| AuthPayload { access_token })
            .boxed()
    }
}

impl Arbitrary for AuthSuccessPayload {
    type Parameters = ();
    type Strategy = BoxedStrategy<Self>;

    fn arbitrary_with(_: Self::Parameters) -> Self::Strategy {
        any::<String>()
            .prop_map(|user_id| AuthSuccessPayload { user_id })
            .boxed()
    }
}

impl Arbitrary for JoinVoicePayload {
    type Parameters = ();
    type Strategy = BoxedStrategy<Self>;

    fn arbitrary_with(_: Self::Parameters) -> Self::Strategy {
        (
            any::<String>(),
            prop::option::of(any::<String>()),
            prop::option::of(any::<String>()),
        )
            .prop_map(
                |(channel_id, display_name, profile_color)| JoinVoicePayload {
                    channel_id,
                    display_name,
                    profile_color,
                    supports_sub_rooms: None,
                },
            )
            .boxed()
    }
}

impl Arbitrary for WireSubRoomMembershipSource {
    type Parameters = ();
    type Strategy = BoxedStrategy<Self>;

    fn arbitrary_with(_: Self::Parameters) -> Self::Strategy {
        prop_oneof![
            Just(WireSubRoomMembershipSource::Explicit),
            Just(WireSubRoomMembershipSource::LegacyRoomOne),
        ]
        .boxed()
    }
}

impl Arbitrary for SubRoomInfoPayload {
    type Parameters = ();
    type Strategy = BoxedStrategy<Self>;

    fn arbitrary_with(_: Self::Parameters) -> Self::Strategy {
        (
            any::<String>(),
            1u32..=6u32,
            any::<bool>(),
            prop::collection::vec(any::<String>(), 0..=6),
            prop::option::of(any::<u64>()),
        )
            .prop_map(
                |(sub_room_id, room_number, is_default, participant_ids, delete_at_ms)| {
                    SubRoomInfoPayload {
                        sub_room_id,
                        room_number,
                        is_default,
                        participant_ids,
                        delete_at_ms,
                    }
                },
            )
            .boxed()
    }
}

impl Arbitrary for CreateSubRoomPayload {
    type Parameters = ();
    type Strategy = BoxedStrategy<Self>;

    fn arbitrary_with(_: Self::Parameters) -> Self::Strategy {
        Just(CreateSubRoomPayload {}).boxed()
    }
}

impl Arbitrary for JoinSubRoomPayload {
    type Parameters = ();
    type Strategy = BoxedStrategy<Self>;

    fn arbitrary_with(_: Self::Parameters) -> Self::Strategy {
        any::<String>()
            .prop_map(|sub_room_id| JoinSubRoomPayload { sub_room_id })
            .boxed()
    }
}

impl Arbitrary for LeaveSubRoomPayload {
    type Parameters = ();
    type Strategy = BoxedStrategy<Self>;

    fn arbitrary_with(_: Self::Parameters) -> Self::Strategy {
        Just(LeaveSubRoomPayload {}).boxed()
    }
}

impl Arbitrary for SubRoomStatePayload {
    type Parameters = ();
    type Strategy = BoxedStrategy<Self>;

    fn arbitrary_with(_: Self::Parameters) -> Self::Strategy {
        prop::collection::vec(any::<SubRoomInfoPayload>(), 1..=6)
            .prop_map(|rooms| SubRoomStatePayload { rooms })
            .boxed()
    }
}

impl Arbitrary for SubRoomCreatedPayload {
    type Parameters = ();
    type Strategy = BoxedStrategy<Self>;

    fn arbitrary_with(_: Self::Parameters) -> Self::Strategy {
        any::<SubRoomInfoPayload>()
            .prop_map(|room| SubRoomCreatedPayload { room })
            .boxed()
    }
}

impl Arbitrary for SubRoomJoinedPayload {
    type Parameters = ();
    type Strategy = BoxedStrategy<Self>;

    fn arbitrary_with(_: Self::Parameters) -> Self::Strategy {
        (any::<String>(), any::<String>(), any::<WireSubRoomMembershipSource>())
            .prop_map(|(participant_id, sub_room_id, source)| SubRoomJoinedPayload {
                participant_id,
                sub_room_id,
                source,
            })
            .boxed()
    }
}

impl Arbitrary for SubRoomLeftPayload {
    type Parameters = ();
    type Strategy = BoxedStrategy<Self>;

    fn arbitrary_with(_: Self::Parameters) -> Self::Strategy {
        (any::<String>(), any::<String>())
            .prop_map(|(participant_id, sub_room_id)| SubRoomLeftPayload {
                participant_id,
                sub_room_id,
            })
            .boxed()
    }
}

impl Arbitrary for SubRoomDeletedPayload {
    type Parameters = ();
    type Strategy = BoxedStrategy<Self>;

    fn arbitrary_with(_: Self::Parameters) -> Self::Strategy {
        any::<String>()
            .prop_map(|sub_room_id| SubRoomDeletedPayload { sub_room_id })
            .boxed()
    }
}

impl Arbitrary for AuthFailedPayload {
    type Parameters = ();
    type Strategy = BoxedStrategy<Self>;

    fn arbitrary_with(_: Self::Parameters) -> Self::Strategy {
        any::<String>()
            .prop_map(|reason| AuthFailedPayload { reason })
            .boxed()
    }
}

impl Arbitrary for ChatSendPayload {
    type Parameters = ();
    type Strategy = BoxedStrategy<Self>;

    fn arbitrary_with(_: Self::Parameters) -> Self::Strategy {
        any::<String>()
            .prop_map(|text| ChatSendPayload { text })
            .boxed()
    }
}

impl Arbitrary for ChatMessagePayload {
    type Parameters = ();
    type Strategy = BoxedStrategy<Self>;

    fn arbitrary_with(_: Self::Parameters) -> Self::Strategy {
        (
            any::<String>(),
            any::<String>(),
            any::<String>(),
            any::<String>(),
            prop::option::of(any::<String>()),
        )
            .prop_map(
                |(participant_id, display_name, text, timestamp, message_id)| ChatMessagePayload {
                    participant_id,
                    display_name,
                    text,
                    timestamp,
                    message_id,
                },
            )
            .boxed()
    }
}
impl Arbitrary for ChatHistoryRequestPayload {
    type Parameters = ();
    type Strategy = BoxedStrategy<Self>;

    fn arbitrary_with(_: Self::Parameters) -> Self::Strategy {
        prop::option::of(any::<String>())
            .prop_map(|since| ChatHistoryRequestPayload { since })
            .boxed()
    }
}

impl Arbitrary for ChatHistoryMessagePayload {
    type Parameters = ();
    type Strategy = BoxedStrategy<Self>;

    fn arbitrary_with(_: Self::Parameters) -> Self::Strategy {
        (
            any::<String>(),
            any::<String>(),
            any::<String>(),
            any::<String>(),
            any::<String>(),
        )
            .prop_map(
                |(message_id, participant_id, display_name, text, timestamp)| {
                    ChatHistoryMessagePayload {
                        message_id,
                        participant_id,
                        display_name,
                        text,
                        timestamp,
                    }
                },
            )
            .boxed()
    }
}

impl Arbitrary for ChatHistoryResponsePayload {
    type Parameters = ();
    type Strategy = BoxedStrategy<Self>;

    fn arbitrary_with(_: Self::Parameters) -> Self::Strategy {
        prop::collection::vec(any::<ChatHistoryMessagePayload>(), 0..=5)
            .prop_map(|messages| ChatHistoryResponsePayload { messages })
            .boxed()
    }
}

impl Arbitrary for ViewerSubscribedPayload {
    type Parameters = ();
    type Strategy = BoxedStrategy<Self>;

    fn arbitrary_with(_: Self::Parameters) -> Self::Strategy {
        any::<String>()
            .prop_map(|target_id| ViewerSubscribedPayload { target_id })
            .boxed()
    }
}

impl Arbitrary for ViewerJoinedPayload {
    type Parameters = ();
    type Strategy = BoxedStrategy<Self>;

    fn arbitrary_with(_: Self::Parameters) -> Self::Strategy {
        (any::<String>(), any::<String>())
            .prop_map(|(viewer_id, display_name)| ViewerJoinedPayload {
                viewer_id,
                display_name,
            })
            .boxed()
    }
}

impl Arbitrary for SessionDisplacedPayload {
    type Parameters = ();
    type Strategy = BoxedStrategy<Self>;

    fn arbitrary_with(_: Self::Parameters) -> Self::Strategy {
        any::<String>()
            .prop_map(|reason| SessionDisplacedPayload { reason })
            .boxed()
    }
}

impl Arbitrary for SignalingMessage {
    type Parameters = ();
    type Strategy = BoxedStrategy<Self>;

    fn arbitrary_with(_: Self::Parameters) -> Self::Strategy {
        prop_oneof![
            any::<JoinPayload>().prop_map(SignalingMessage::Join),
            any::<JoinedPayload>().prop_map(SignalingMessage::Joined),
            any::<OfferPayload>().prop_map(SignalingMessage::Offer),
            any::<AnswerPayload>().prop_map(SignalingMessage::Answer),
            any::<IceCandidatePayload>().prop_map(SignalingMessage::IceCandidate),
            Just(SignalingMessage::PeerLeft),
            Just(SignalingMessage::Leave),
            any::<ErrorPayload>().prop_map(SignalingMessage::Error),
            any::<JoinRejectedPayload>().prop_map(SignalingMessage::JoinRejected),
            any::<InviteCreatePayload>().prop_map(SignalingMessage::InviteCreate),
            any::<InviteCreatedPayload>().prop_map(SignalingMessage::InviteCreated),
            any::<InviteRevokePayload>().prop_map(SignalingMessage::InviteRevoke),
            any::<InviteRevokedPayload>().prop_map(SignalingMessage::InviteRevoked),
            // Phase 3: SFU multi-party variants
            any::<ParticipantJoinedPayload>().prop_map(SignalingMessage::ParticipantJoined),
            any::<ParticipantLeftPayload>().prop_map(SignalingMessage::ParticipantLeft),
            any::<RoomStatePayload>().prop_map(SignalingMessage::RoomState),
            any::<MediaTokenPayload>().prop_map(SignalingMessage::MediaToken),
            // Action messages
            any::<KickParticipantPayload>().prop_map(SignalingMessage::KickParticipant),
            any::<MuteParticipantPayload>().prop_map(SignalingMessage::MuteParticipant),
            any::<UnmuteParticipantPayload>().prop_map(SignalingMessage::UnmuteParticipant),
            any::<ParticipantKickedPayload>().prop_map(SignalingMessage::ParticipantKicked),
            any::<ParticipantMutedPayload>().prop_map(SignalingMessage::ParticipantMuted),
            any::<ParticipantUnmutedPayload>().prop_map(SignalingMessage::ParticipantUnmuted),
            // Self-deafen
            Just(SignalingMessage::SelfDeafen),
            Just(SignalingMessage::SelfUndeafen),
            any::<ParticipantDeafenedPayload>().prop_map(SignalingMessage::ParticipantDeafened),
            any::<ParticipantUndeafenedPayload>().prop_map(SignalingMessage::ParticipantUndeafened),
            // Phase 3: Screen share lifecycle
            Just(SignalingMessage::StartShare),
            any::<ShareStartedPayload>().prop_map(SignalingMessage::ShareStarted),
            any::<StopSharePayload>().prop_map(SignalingMessage::StopShare),
            any::<ShareStoppedPayload>().prop_map(SignalingMessage::ShareStopped),
            Just(SignalingMessage::StopAllShares),
            any::<ShareStatePayload>().prop_map(SignalingMessage::ShareState),
            // Share permission
            any::<SetSharePermissionPayload>().prop_map(SignalingMessage::SetSharePermission),
            any::<SharePermissionChangedPayload>()
                .prop_map(SignalingMessage::SharePermissionChanged),
            // Viewer subscription
            any::<ViewerSubscribedPayload>().prop_map(SignalingMessage::ViewerSubscribed),
            any::<ViewerJoinedPayload>().prop_map(SignalingMessage::ViewerJoined),
            // Room creation
            any::<CreateRoomPayload>().prop_map(SignalingMessage::CreateRoom),
            any::<RoomCreatedPayload>().prop_map(SignalingMessage::RoomCreated),
            // Device Auth
            any::<AuthPayload>().prop_map(SignalingMessage::Auth),
            any::<AuthSuccessPayload>().prop_map(SignalingMessage::AuthSuccess),
            any::<AuthFailedPayload>().prop_map(SignalingMessage::AuthFailed),
            // Channel voice orchestration
            any::<JoinVoicePayload>().prop_map(SignalingMessage::JoinVoice),
            any::<CreateSubRoomPayload>().prop_map(SignalingMessage::CreateSubRoom),
            any::<JoinSubRoomPayload>().prop_map(SignalingMessage::JoinSubRoom),
            any::<LeaveSubRoomPayload>().prop_map(SignalingMessage::LeaveSubRoom),
            any::<SubRoomStatePayload>().prop_map(SignalingMessage::SubRoomState),
            any::<SubRoomCreatedPayload>().prop_map(SignalingMessage::SubRoomCreated),
            any::<SubRoomJoinedPayload>().prop_map(SignalingMessage::SubRoomJoined),
            any::<SubRoomLeftPayload>().prop_map(SignalingMessage::SubRoomLeft),
            any::<SubRoomDeletedPayload>().prop_map(SignalingMessage::SubRoomDeleted),
            (0u32..=300u32).prop_map(|secs| {
                SignalingMessage::SfuColdStarting(SfuColdStartingPayload {
                    estimated_wait_secs: secs,
                })
            }),
            // Ephemeral chat
            any::<ChatSendPayload>().prop_map(SignalingMessage::ChatSend),
            any::<ChatMessagePayload>().prop_map(SignalingMessage::ChatMessage),
            // Chat history
            any::<ChatHistoryRequestPayload>().prop_map(SignalingMessage::ChatHistoryRequest),
            any::<ChatHistoryResponsePayload>().prop_map(SignalingMessage::ChatHistoryResponse),
            // Keepalive
            Just(SignalingMessage::Ping),
            // Session displacement
            any::<SessionDisplacedPayload>().prop_map(SignalingMessage::SessionDisplaced),
        ]
        .boxed()
    }
}
