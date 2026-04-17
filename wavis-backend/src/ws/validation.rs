//! Signaling state-machine validation.
//!
//! **Owns:** the rules that decide whether a given `SignalingMessage` is legal
//! for the current session state (unauthenticated, authenticated-but-not-joined,
//! or in-session). This is a pure guard — it performs no I/O and mutates no
//! state.
//!
//! **Does not own:** message dispatch, session lifecycle, or any transport
//! concerns. The WebSocket handler calls [`validate_state_transition`] and
//! acts on the result; this module only returns `Ok` or `Err`.
//!
//! **Key invariants:**
//! - `Ping` is always allowed regardless of state.
//! - `Auth` is rejected once already authenticated.
//! - `Join` / `CreateRoom` are rejected once a session exists.
//! - All other actions require an active session.
//!
//! **Layering:** called by `handlers::ws`, depends only on shared signaling
//! types. No domain or state dependencies.

use shared::signaling::SignalingMessage;

/// Minimal session context needed for state machine validation.
/// Mirrors the fields of `SignalingSession` in `ws.rs` that are relevant here.
pub struct SessionContext<'a> {
    #[allow(dead_code)]
    pub participant_id: &'a str,
}

/// Validate that a signaling message is valid for the current session state.
///
/// Rules (Req 12.1, 12.2, 6.4, 6.6, 6.7):
/// - No session, not authenticated → Auth and Join/CreateRoom allowed; all others rejected.
/// - No session, authenticated → Auth rejected ("already authenticated"); Join/CreateRoom allowed; all others rejected.
/// - Has session → Auth rejected ("auth not permitted after join"); Join/CreateRoom rejected ("already joined"); all others allowed.
///
/// Returns `Ok(())` if the transition is valid, or `Err` with a human-readable
/// reason string. The handler sends this as a structured error without closing
/// the connection (Req 12.5).
pub fn validate_state_transition(
    msg: &SignalingMessage,
    session: Option<&SessionContext<'_>>,
    authenticated: bool,
) -> Result<(), &'static str> {
    // Keepalive ping — allowed in every state, no auth required.
    if matches!(msg, SignalingMessage::Ping) {
        return Ok(());
    }

    match session {
        None => {
            // Pre-join states
            if matches!(msg, SignalingMessage::Auth(_)) {
                if authenticated {
                    Err("already authenticated")
                } else {
                    Ok(())
                }
            } else if matches!(
                msg,
                SignalingMessage::Join(_) | SignalingMessage::CreateRoom(_)
            ) {
                Ok(())
            } else if matches!(msg, SignalingMessage::JoinVoice(_)) {
                // JoinVoice requires authentication (unlike Join/CreateRoom)
                if authenticated {
                    Ok(())
                } else {
                    Err("not authenticated")
                }
            } else if matches!(msg, SignalingMessage::ChatHistoryRequest(_)) {
                // ChatHistoryRequest requires an active session (post-join only)
                Err("not in a room")
            } else {
                Err("not authenticated")
            }
        }
        Some(_) => {
            // Post-join: Auth is never allowed after join
            if matches!(msg, SignalingMessage::Auth(_)) {
                Err("auth not permitted after join")
            } else if matches!(
                msg,
                SignalingMessage::Join(_)
                    | SignalingMessage::CreateRoom(_)
                    | SignalingMessage::JoinVoice(_)
            ) {
                Err("already joined")
            } else {
                Ok(())
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use proptest::prelude::*;
    use shared::signaling::{
        AuthPayload, ChatHistoryRequestPayload, ChatSendPayload, CreateRoomPayload, JoinPayload,
        JoinVoicePayload, SignalingMessage,
    };

    fn join_msg() -> SignalingMessage {
        SignalingMessage::Join(JoinPayload {
            room_id: "room-1".to_string(),
            room_type: None,
            invite_code: None,
            display_name: None,
            profile_color: None,
        })
    }

    fn create_room_msg() -> SignalingMessage {
        SignalingMessage::CreateRoom(CreateRoomPayload {
            room_id: "room-1".to_string(),
            room_type: None,
            display_name: None,
            profile_color: None,
        })
    }

    fn join_voice_msg() -> SignalingMessage {
        SignalingMessage::JoinVoice(JoinVoicePayload {
            channel_id: "00000000-0000-0000-0000-000000000001".to_string(),
            display_name: None,
            profile_color: None,
            supports_sub_rooms: None,
        })
    }

    fn auth_msg() -> SignalingMessage {
        SignalingMessage::Auth(AuthPayload {
            access_token: "test-token".to_string(),
        })
    }

    fn session() -> SessionContext<'static> {
        SessionContext {
            participant_id: "peer-1",
        }
    }

    /// Returns true for messages that have special-case handling in the state machine
    /// (Join, CreateRoom, JoinVoice, Auth, Ping, ChatHistoryRequest).
    fn is_pre_join_or_auth(m: &SignalingMessage) -> bool {
        matches!(
            m,
            SignalingMessage::Join(_)
                | SignalingMessage::CreateRoom(_)
                | SignalingMessage::JoinVoice(_)
                | SignalingMessage::Auth(_)
                | SignalingMessage::Ping
                | SignalingMessage::ChatHistoryRequest(_)
        )
    }

    // --- Unit tests (existing, updated with authenticated param) ---

    #[test]
    fn join_allowed_without_session() {
        assert!(validate_state_transition(&join_msg(), None, false).is_ok());
    }

    #[test]
    fn create_room_allowed_without_session() {
        assert!(validate_state_transition(&create_room_msg(), None, false).is_ok());
    }

    #[test]
    fn non_join_rejected_without_session() {
        let msg = SignalingMessage::Leave;
        assert_eq!(
            validate_state_transition(&msg, None, false),
            Err("not authenticated")
        );
    }

    #[test]
    fn join_rejected_with_session() {
        let s = session();
        assert_eq!(
            validate_state_transition(&join_msg(), Some(&s), false),
            Err("already joined")
        );
    }

    #[test]
    fn create_room_rejected_with_session() {
        let s = session();
        assert_eq!(
            validate_state_transition(&create_room_msg(), Some(&s), false),
            Err("already joined")
        );
    }

    #[test]
    fn non_join_allowed_with_session() {
        let s = session();
        let msg = SignalingMessage::Leave;
        assert!(validate_state_transition(&msg, Some(&s), false).is_ok());
    }

    // --- Auth-specific unit tests (Req 6.4, 6.6, 6.7) ---

    #[test]
    fn auth_allowed_no_session_not_authenticated() {
        assert!(validate_state_transition(&auth_msg(), None, false).is_ok());
    }

    #[test]
    fn auth_rejected_no_session_already_authenticated() {
        assert_eq!(
            validate_state_transition(&auth_msg(), None, true),
            Err("already authenticated")
        );
    }

    #[test]
    fn auth_rejected_with_session() {
        let s = session();
        assert_eq!(
            validate_state_transition(&auth_msg(), Some(&s), false),
            Err("auth not permitted after join")
        );
    }

    #[test]
    fn auth_rejected_with_session_and_authenticated() {
        let s = session();
        assert_eq!(
            validate_state_transition(&auth_msg(), Some(&s), true),
            Err("auth not permitted after join")
        );
    }

    #[test]
    fn join_allowed_when_authenticated_no_session() {
        assert!(validate_state_transition(&join_msg(), None, true).is_ok());
    }

    #[test]
    fn create_room_allowed_when_authenticated_no_session() {
        assert!(validate_state_transition(&create_room_msg(), None, true).is_ok());
    }

    // --- JoinVoice-specific unit tests (Req 3.5, 5.6) ---

    #[test]
    fn join_voice_rejected_without_session_not_authenticated() {
        assert_eq!(
            validate_state_transition(&join_voice_msg(), None, false),
            Err("not authenticated")
        );
    }

    #[test]
    fn join_voice_allowed_when_authenticated_no_session() {
        assert!(validate_state_transition(&join_voice_msg(), None, true).is_ok());
    }

    #[test]
    fn join_voice_rejected_with_session() {
        let s = session();
        assert_eq!(
            validate_state_transition(&join_voice_msg(), Some(&s), false),
            Err("already joined")
        );
    }

    #[test]
    fn join_voice_rejected_with_session_and_authenticated() {
        let s = session();
        assert_eq!(
            validate_state_transition(&join_voice_msg(), Some(&s), true),
            Err("already joined")
        );
    }

    // Feature: phase3-security-hardening, Property 13: State machine rejects invalid transitions
    // Validates: Requirements 12.1, 12.2
    proptest! {
        #![proptest_config(ProptestConfig::with_cases(100))]

        #[test]
        fn prop13_non_join_rejected_without_session(
            msg in any::<SignalingMessage>()
                .prop_filter("not a pre-join or auth message", |m| !is_pre_join_or_auth(m))
        ) {
            let result = validate_state_transition(&msg, None, false);
            prop_assert_eq!(result, Err("not authenticated"),
                "non-Join/CreateRoom/Auth message without session must be rejected");
        }

        #[test]
        fn prop13_join_rejected_with_session(
            room_id in "[a-z]{4,16}",
        ) {
            let msg = SignalingMessage::Join(JoinPayload {
                room_id,
                room_type: None,
                invite_code: None,
                display_name: None,
                profile_color: None,
            });
            let s = session();
            let result = validate_state_transition(&msg, Some(&s), false);
            prop_assert_eq!(result, Err("already joined"),
                "Join with active session must be rejected");
        }

        #[test]
        fn prop13_create_room_rejected_with_session(
            room_id in "[a-z]{4,16}",
        ) {
            let msg = SignalingMessage::CreateRoom(CreateRoomPayload {
                room_id,
                room_type: None,
                display_name: None,
                profile_color: None,
            });
            let s = session();
            let result = validate_state_transition(&msg, Some(&s), false);
            prop_assert_eq!(result, Err("already joined"),
                "CreateRoom with active session must be rejected");
        }

        #[test]
        fn prop13_join_always_allowed_without_session(
            room_id in "[a-z]{4,16}",
            invite_code in prop::option::of("[a-z0-9]{8,16}"),
            authenticated in any::<bool>(),
        ) {
            let msg = SignalingMessage::Join(JoinPayload {
                room_id,
                room_type: None,
                invite_code,
                display_name: None,
                profile_color: None,
            });
            let result = validate_state_transition(&msg, None, authenticated);
            prop_assert!(result.is_ok(), "Join without session must always be allowed regardless of auth state");
        }

        #[test]
        fn prop13_create_room_always_allowed_without_session(
            room_id in "[a-z]{4,16}",
            authenticated in any::<bool>(),
        ) {
            let msg = SignalingMessage::CreateRoom(CreateRoomPayload {
                room_id,
                room_type: None,
                display_name: None,
                profile_color: None,
            });
            let result = validate_state_transition(&msg, None, authenticated);
            prop_assert!(result.is_ok(), "CreateRoom without session must always be allowed regardless of auth state");
        }

        #[test]
        fn prop13_non_join_allowed_with_session(
            msg in any::<SignalingMessage>()
                .prop_filter("not a pre-join or auth message", |m| !is_pre_join_or_auth(m))
        ) {
            let s = session();
            let result = validate_state_transition(&msg, Some(&s), false);
            prop_assert!(result.is_ok(),
                "non-Join/CreateRoom/Auth message with active session must be allowed");
        }
    }

    // Feature: device-auth, Property 11: State machine Auth gating
    // Validates: Requirements 6.4, 6.7
    proptest! {
        #![proptest_config(ProptestConfig::with_cases(100))]

        /// Auth allowed ONLY when no session AND not authenticated.
        #[test]
        fn prop11_auth_allowed_no_session_not_authenticated(
            access_token in "[a-zA-Z0-9_\\-\\.]{10,100}",
        ) {
            let msg = SignalingMessage::Auth(AuthPayload { access_token });
            let result = validate_state_transition(&msg, None, false);
            prop_assert!(result.is_ok(),
                "Auth must be allowed when no session and not authenticated");
        }

        /// Auth rejected with "already authenticated" when no session but authenticated=true.
        #[test]
        fn prop11_auth_rejected_already_authenticated(
            access_token in "[a-zA-Z0-9_\\-\\.]{10,100}",
        ) {
            let msg = SignalingMessage::Auth(AuthPayload { access_token });
            let result = validate_state_transition(&msg, None, true);
            prop_assert_eq!(result, Err("already authenticated"),
                "Auth must be rejected when already authenticated (pre-join)");
        }

        /// Auth rejected with "auth not permitted after join" when session exists,
        /// regardless of authenticated flag.
        #[test]
        fn prop11_auth_rejected_with_session(
            access_token in "[a-zA-Z0-9_\\-\\.]{10,100}",
            authenticated in any::<bool>(),
        ) {
            let msg = SignalingMessage::Auth(AuthPayload { access_token });
            let s = session();
            let result = validate_state_transition(&msg, Some(&s), authenticated);
            prop_assert_eq!(result, Err("auth not permitted after join"),
                "Auth must be rejected when session exists, regardless of auth state");
        }

        /// Join/CreateRoom always allowed when no session, regardless of authenticated bool.
        #[test]
        fn prop11_join_create_room_always_allowed_pre_session(
            room_id in "[a-z]{4,16}",
            authenticated in any::<bool>(),
            use_join in any::<bool>(),
        ) {
            let msg = if use_join {
                SignalingMessage::Join(JoinPayload {
                    room_id,
                    room_type: None,
                    invite_code: None,
                    display_name: None,
                    profile_color: None,
                })
            } else {
                SignalingMessage::CreateRoom(CreateRoomPayload {
                    room_id,
                    room_type: None,
                    display_name: None,
                    profile_color: None,
                })
            };
            let result = validate_state_transition(&msg, None, authenticated);
            prop_assert!(result.is_ok(),
                "Join/CreateRoom must always be allowed pre-session regardless of auth state");
        }
    }

    // Feature: channel-voice-orchestration, Property 2: JoinVoice requires authentication
    // Validates: Requirements 3.5, 5.6
    proptest! {
        #![proptest_config(ProptestConfig::with_cases(256))]

        #[test]
        fn prop2_join_voice_rejected_when_not_authenticated(
            payload in any::<JoinVoicePayload>(),
        ) {
            let msg = SignalingMessage::JoinVoice(payload);
            let result = validate_state_transition(&msg, None, false);
            prop_assert_eq!(result, Err("not authenticated"),
                "JoinVoice without authentication must be rejected with 'not authenticated'");
        }

        #[test]
        fn prop2_join_voice_allowed_when_authenticated_no_session(
            payload in any::<JoinVoicePayload>(),
        ) {
            let msg = SignalingMessage::JoinVoice(payload);
            let result = validate_state_transition(&msg, None, true);
            prop_assert!(result.is_ok(),
                "JoinVoice with authentication and no session must be allowed");
        }
    }

    // Feature: channel-voice-orchestration, Property 3: JoinVoice state machine consistency
    // Validates: Requirements 5.6, 8.3
    proptest! {
        #![proptest_config(ProptestConfig::with_cases(256))]

        #[test]
        fn prop3_join_voice_rejected_with_existing_session(
            payload in any::<JoinVoicePayload>(),
            authenticated in any::<bool>(),
        ) {
            let msg = SignalingMessage::JoinVoice(payload);
            let s = session();
            let result = validate_state_transition(&msg, Some(&s), authenticated);
            prop_assert_eq!(result, Err("already joined"),
                "JoinVoice with existing session must be rejected with 'already joined'");
        }
    }

    // --- ChatSend state machine error message format tests ---

    /// ChatSend without session (unauthenticated) produces "not authenticated" error.
    #[test]
    fn chat_send_no_session_error_message() {
        let msg = SignalingMessage::ChatSend(ChatSendPayload {
            text: "hello".to_string(),
        });
        let result = validate_state_transition(&msg, None, false);
        assert_eq!(result, Err("not authenticated"));
    }

    /// ChatSend without session (authenticated but not joined) also produces "not authenticated".
    /// This is correct: the state machine requires an active session (post-join), not just auth.
    #[test]
    fn chat_send_authenticated_no_session_error_message() {
        let msg = SignalingMessage::ChatSend(ChatSendPayload {
            text: "hello".to_string(),
        });
        let result = validate_state_transition(&msg, None, true);
        assert_eq!(result, Err("not authenticated"));
    }

    // Feature: ephemeral-room-chat, Property 5: ChatSend requires active session
    // Validates: Requirements 2.4, 7.1, 7.2
    proptest! {
        #![proptest_config(ProptestConfig::with_cases(100))]

        /// ChatSend must be allowed when session is Some (participant has joined a room).
        #[test]
        fn prop5_chat_send_allowed_with_session(
            payload in any::<ChatSendPayload>(),
            authenticated in any::<bool>(),
        ) {
            let msg = SignalingMessage::ChatSend(payload);
            let s = session();
            let result = validate_state_transition(&msg, Some(&s), authenticated);
            prop_assert!(result.is_ok(),
                "ChatSend with active session must be allowed regardless of auth state");
        }

        /// ChatSend must be rejected when session is None (not joined a room),
        /// regardless of authentication status.
        #[test]
        fn prop5_chat_send_rejected_without_session(
            payload in any::<ChatSendPayload>(),
            authenticated in any::<bool>(),
        ) {
            let msg = SignalingMessage::ChatSend(payload);
            let result = validate_state_transition(&msg, None, authenticated);
            prop_assert!(result.is_err(),
                "ChatSend without active session must be rejected regardless of auth state");
        }
    }

    // --- ChatHistoryRequest state machine tests (Req 8.1, 8.2) ---

    fn chat_history_request_msg() -> SignalingMessage {
        SignalingMessage::ChatHistoryRequest(ChatHistoryRequestPayload { since: None })
    }

    /// ChatHistoryRequest without session (unauthenticated) produces "not in a room" error.
    #[test]
    fn chat_history_request_no_session_not_authenticated() {
        let result = validate_state_transition(&chat_history_request_msg(), None, false);
        assert_eq!(result, Err("not in a room"));
    }

    /// ChatHistoryRequest without session (authenticated but not joined) produces "not in a room".
    #[test]
    fn chat_history_request_no_session_authenticated() {
        let result = validate_state_transition(&chat_history_request_msg(), None, true);
        assert_eq!(result, Err("not in a room"));
    }

    /// ChatHistoryRequest with active session is allowed.
    #[test]
    fn chat_history_request_allowed_with_session() {
        let s = session();
        let result = validate_state_transition(&chat_history_request_msg(), Some(&s), false);
        assert!(result.is_ok());
    }

    /// ChatHistoryRequest with active session + authenticated is allowed.
    #[test]
    fn chat_history_request_allowed_with_session_authenticated() {
        let s = session();
        let result = validate_state_transition(&chat_history_request_msg(), Some(&s), true);
        assert!(result.is_ok());
    }

    // Feature: chat-history-persistence, Property 11: State machine — ChatHistoryRequest requires active session
    // Validates: Requirements 8.1, 8.2
    proptest! {
        #![proptest_config(ProptestConfig::with_cases(100))]

        /// ChatHistoryRequest must be allowed when session is Some (participant has joined a room),
        /// regardless of authentication status.
        #[test]
        fn property_11_chat_history_request_allowed_with_session(
            payload in any::<ChatHistoryRequestPayload>(),
            authenticated in any::<bool>(),
        ) {
            let msg = SignalingMessage::ChatHistoryRequest(payload);
            let s = session();
            let result = validate_state_transition(&msg, Some(&s), authenticated);
            prop_assert!(result.is_ok(),
                "ChatHistoryRequest with active session must be allowed regardless of auth state");
        }

        /// ChatHistoryRequest must be rejected with "not in a room" when session is None,
        /// regardless of authentication status.
        #[test]
        fn property_11_chat_history_request_rejected_without_session(
            payload in any::<ChatHistoryRequestPayload>(),
            authenticated in any::<bool>(),
        ) {
            let msg = SignalingMessage::ChatHistoryRequest(payload);
            let result = validate_state_transition(&msg, None, authenticated);
            prop_assert_eq!(result, Err("not in a room"),
                "ChatHistoryRequest without active session must be rejected with 'not in a room'");
        }
    }
}
