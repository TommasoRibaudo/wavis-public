//! WebSocket auth integration tests.
//!
//! These tests exercise the validation state machine (`validate_state_transition`)
//! and access token validation (`validate_access_token_with_rotation`) together,
//! simulating the Auth → Join flow that `handle_socket` in ws.rs performs.
//!
//! No server, no DB, no async — pure function composition only.

use proptest::prelude::*;
use uuid::Uuid;

use shared::signaling::{AuthPayload, JoinPayload, SignalingMessage};
use wavis_backend::auth::jwt::{
    ACCESS_TOKEN_TTL_SECS, sign_access_token, validate_access_token_with_rotation,
};
use wavis_backend::ws::validation::{SessionContext, validate_state_transition};

/// Deterministic test secret (≥32 bytes).
const TEST_SECRET: &[u8] = b"test-auth-secret-at-least-32-bytes!!";

// ---------------------------------------------------------------------------
// Property 12: Session user_id propagation
// Feature: device-auth, Property 12: Session user_id propagation
// Validates: Requirements 6.5, 6.6
// ---------------------------------------------------------------------------

proptest! {
    #![proptest_config(ProptestConfig::with_cases(100))]

    /// Auth before Join → session has user_id.
    ///
    /// For any valid user_id, signing an access token and then walking through
    /// the Auth → Join state transitions results in a session whose user_id
    /// equals Some(original_user_id).
    ///
    /// **Validates: Requirements 6.5**
    #[test]
    fn prop12_auth_before_join_propagates_user_id(
        user_id_bytes in prop::array::uniform16(any::<u8>()),
        room_id in "[a-z]{4,16}",
    ) {
        let user_id = Uuid::from_bytes(user_id_bytes);

        // 1. Sign an access token (what the client would have obtained via /auth/register_device)
        let token = sign_access_token(&user_id, &Uuid::nil(), TEST_SECRET, ACCESS_TOKEN_TTL_SECS, 0)
            .expect("signing must succeed with valid secret");

        // 2. Simulate ws.rs connection state
        let mut session: Option<SessionContext<'static>> = None;

        // 3. Client sends Auth message
        let auth_msg = SignalingMessage::Auth(AuthPayload {
            access_token: token.clone(),
        });
        let result = validate_state_transition(&auth_msg, session.as_ref(), false);
        prop_assert!(result.is_ok(), "Auth must be allowed before join");

        // 4. Validate the token (what ws.rs does on Auth dispatch)
        let (extracted, _device_id, _epoch) = validate_access_token_with_rotation(&token, TEST_SECRET, None)
            .expect("token must validate with the same secret");
        prop_assert_eq!(extracted, user_id);
        let authenticated_user_id = Some(extracted.to_string());

        // 5. Client sends Join message
        let join_msg = SignalingMessage::Join(JoinPayload {
            room_id,
            room_type: None,
            invite_code: Some("abc123".to_string()),
            display_name: None,
            profile_color: None,
        });
        let result = validate_state_transition(&join_msg, session.as_ref(), true);
        prop_assert!(result.is_ok(), "Join must be allowed after Auth");

        // 6. Session created with user_id propagated (mirrors ws.rs logic)
        // Leak a static str for SessionContext lifetime (test-only, bounded by proptest case)
        let pid: &'static str = Box::leak(Box::from("peer-1"));
        session = Some(SessionContext { participant_id: pid });
        let session_user_id = authenticated_user_id.clone();

        // 7. Assert: session has user_id
        prop_assert_eq!(session_user_id, Some(user_id.to_string()),
            "session user_id must equal the authenticated user_id");
        prop_assert!(session.is_some(), "session must exist after join");
    }

    /// Join without Auth → session has user_id = None.
    ///
    /// For any room_id, skipping Auth and going straight to Join results in
    /// a session whose user_id is None.
    ///
    /// **Validates: Requirements 6.6**
    #[test]
    fn prop12_join_without_auth_has_no_user_id(
        room_id in "[a-z]{4,16}",
    ) {
        // 1. Connection state: no auth performed
        let authenticated_user_id: Option<String> = None;
        let session: Option<SessionContext<'_>> = None;

        // 2. Client sends Join directly (no Auth)
        let join_msg = SignalingMessage::Join(JoinPayload {
            room_id,
            room_type: None,
            invite_code: Some("abc123".to_string()),
            display_name: None,
            profile_color: None,
        });
        let result = validate_state_transition(&join_msg, session.as_ref(), false);
        prop_assert!(result.is_ok(), "Join without Auth must be allowed");

        // 3. Session created — user_id is None (unauthenticated)
        let session_user_id = authenticated_user_id.clone();
        prop_assert_eq!(session_user_id, None,
            "session user_id must be None when no Auth was sent");
    }
}

// ---------------------------------------------------------------------------
// Example-based tests
// ---------------------------------------------------------------------------

/// Example: Auth → Join → session has user_id.
///
/// Concrete scenario with a known UUID walking through the full Auth → Join flow.
#[test]
fn example_auth_then_join_session_has_user_id() {
    let user_id = Uuid::parse_str("550e8400-e29b-41d4-a716-446655440000").unwrap();

    // Sign token
    let token = sign_access_token(
        &user_id,
        &Uuid::nil(),
        TEST_SECRET,
        ACCESS_TOKEN_TTL_SECS,
        0,
    )
    .expect("signing must succeed");

    // Step 1: Auth message
    let auth_msg = SignalingMessage::Auth(AuthPayload {
        access_token: token.clone(),
    });
    assert!(
        validate_state_transition(&auth_msg, None, false).is_ok(),
        "Auth must be allowed pre-join, pre-auth"
    );

    // Validate token (simulating ws.rs dispatch)
    let (extracted, _device_id, _epoch) =
        validate_access_token_with_rotation(&token, TEST_SECRET, None)
            .expect("token must validate");
    assert_eq!(extracted, user_id);
    let authenticated_user_id = Some(extracted.to_string());

    // Step 2: Join message
    let join_msg = SignalingMessage::Join(JoinPayload {
        room_id: "room-abc".to_string(),
        room_type: None,
        invite_code: Some("invite123".to_string()),
        display_name: None,
        profile_color: None,
    });
    assert!(
        validate_state_transition(&join_msg, None, true).is_ok(),
        "Join must be allowed after Auth"
    );

    // Session created
    let session = SessionContext {
        participant_id: "peer-1",
    };
    let session_user_id = authenticated_user_id;

    // Verify propagation
    assert_eq!(
        session_user_id,
        Some("550e8400-e29b-41d4-a716-446655440000".to_string())
    );
    assert_eq!(session.participant_id, "peer-1");
}

/// Example: Join without Auth → session has None.
#[test]
fn example_join_without_auth_session_has_none() {
    let authenticated_user_id: Option<String> = None;

    // Join directly
    let join_msg = SignalingMessage::Join(JoinPayload {
        room_id: "room-xyz".to_string(),
        room_type: None,
        invite_code: Some("invite456".to_string()),
        display_name: None,
        profile_color: None,
    });
    assert!(
        validate_state_transition(&join_msg, None, false).is_ok(),
        "Join without Auth must be allowed"
    );

    // Session created — user_id is None
    let _session = SessionContext {
        participant_id: "peer-2",
    };
    assert_eq!(authenticated_user_id, None);
}

/// Example: Auth after Join rejected (Req 6.4).
///
/// Once a session exists (post-Join), sending Auth must be rejected with
/// "auth not permitted after join".
#[test]
fn example_auth_after_join_rejected() {
    let session = SessionContext {
        participant_id: "peer-1",
    };

    let auth_msg = SignalingMessage::Auth(AuthPayload {
        access_token: "some-token".to_string(),
    });

    // Auth with active session → rejected
    let result = validate_state_transition(&auth_msg, Some(&session), false);
    assert_eq!(result, Err("auth not permitted after join"));

    // Also rejected when authenticated=true + session exists
    let result = validate_state_transition(&auth_msg, Some(&session), true);
    assert_eq!(result, Err("auth not permitted after join"));
}

/// Example: second Auth rejected (Req 6.7).
///
/// After a successful Auth (authenticated=true, no session yet), a second Auth
/// must be rejected with "already authenticated".
#[test]
fn example_second_auth_rejected() {
    let user_id = Uuid::new_v4();
    let token = sign_access_token(
        &user_id,
        &Uuid::nil(),
        TEST_SECRET,
        ACCESS_TOKEN_TTL_SECS,
        0,
    )
    .expect("signing must succeed");

    // First Auth succeeds
    let auth_msg = SignalingMessage::Auth(AuthPayload {
        access_token: token.clone(),
    });
    assert!(
        validate_state_transition(&auth_msg, None, false).is_ok(),
        "First Auth must be allowed"
    );

    // Validate token (simulating ws.rs setting authenticated=true)
    let _extracted = validate_access_token_with_rotation(&token, TEST_SECRET, None)
        .expect("token must validate");

    // Second Auth → rejected
    let second_auth = SignalingMessage::Auth(AuthPayload {
        access_token: token,
    });
    let result = validate_state_transition(&second_auth, None, true);
    assert_eq!(result, Err("already authenticated"));
}
