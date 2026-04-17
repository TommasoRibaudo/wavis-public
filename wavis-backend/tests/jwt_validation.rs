//! Integration tests for JWT uniqueness and time validation contracts.
//! Validates: Requirements 4.1, 4.2, 4.3, 4.4, 4.5, 4.6

use jsonwebtoken::{Algorithm, EncodingKey, Header, encode};
use std::time::{SystemTime, UNIX_EPOCH};
use uuid::Uuid;
use wavis_backend::auth::jwt::{
    DEFAULT_JWT_ISSUER, DEFAULT_TOKEN_TTL_SECS, MediaTokenClaims, SFU_AUDIENCE, sign_media_token,
    validate_media_token, validate_media_token_with_rotation,
};

fn test_secret() -> Vec<u8> {
    b"a-secret-that-is-at-least-32-bytes-long!!".to_vec()
}

/// Requirement 4.1: issued media token's jti is a valid UUID v4.
#[test]
fn jti_is_uuid_v4() {
    let secret = test_secret();
    let token = sign_media_token(
        "room-1",
        "peer-1",
        &secret,
        DEFAULT_JWT_ISSUER,
        DEFAULT_TOKEN_TTL_SECS,
    )
    .expect("signing should succeed");

    let claims = validate_media_token(&token, &secret, SFU_AUDIENCE, DEFAULT_JWT_ISSUER)
        .expect("validation should succeed");

    let parsed = Uuid::parse_str(&claims.jti).expect("jti must be a valid UUID");
    assert_eq!(
        parsed.get_version(),
        Some(uuid::Version::Random),
        "jti must be UUID v4 (random), got {:?}",
        parsed.get_version()
    );
}

/// Requirement 4.2: two tokens for the same participant have different jti values.
#[test]
fn two_tokens_have_different_jti() {
    let secret = test_secret();

    let token_a = sign_media_token(
        "room-1",
        "peer-1",
        &secret,
        DEFAULT_JWT_ISSUER,
        DEFAULT_TOKEN_TTL_SECS,
    )
    .expect("first sign should succeed");
    let token_b = sign_media_token(
        "room-1",
        "peer-1",
        &secret,
        DEFAULT_JWT_ISSUER,
        DEFAULT_TOKEN_TTL_SECS,
    )
    .expect("second sign should succeed");

    let claims_a = validate_media_token(&token_a, &secret, SFU_AUDIENCE, DEFAULT_JWT_ISSUER)
        .expect("first validation should succeed");
    let claims_b = validate_media_token(&token_b, &secret, SFU_AUDIENCE, DEFAULT_JWT_ISSUER)
        .expect("second validation should succeed");

    assert_ne!(
        claims_a.jti, claims_b.jti,
        "two tokens for the same participant must have different jti values"
    );
}

/// Requirement 4.3: key rotation — token signed with S1 validates when
/// current=S2 and previous=S1.
#[test]
fn key_rotation_previous_secret_validates() {
    let s1 = b"old-secret-that-is-at-least-32-bytes-long!".to_vec();
    let s2 = b"new-secret-that-is-at-least-32-bytes-long!".to_vec();

    let token = sign_media_token(
        "room-1",
        "peer-1",
        &s1,
        DEFAULT_JWT_ISSUER,
        DEFAULT_TOKEN_TTL_SECS,
    )
    .expect("signing with S1 should succeed");

    let claims = validate_media_token_with_rotation(
        &token,
        &s2,
        Some(&s1),
        SFU_AUDIENCE,
        DEFAULT_JWT_ISSUER,
    )
    .expect("rotation with previous=S1 should succeed");

    assert_eq!(claims.room_id, "room-1");
    assert_eq!(claims.participant_id, "peer-1");
}

/// Requirement 4.4: key rotation — token signed with S1 is rejected when
/// current=S2 and previous=None.
#[test]
fn key_rotation_no_previous_rejects() {
    let s1 = b"old-secret-that-is-at-least-32-bytes-long!".to_vec();
    let s2 = b"new-secret-that-is-at-least-32-bytes-long!".to_vec();

    let token = sign_media_token(
        "room-1",
        "peer-1",
        &s1,
        DEFAULT_JWT_ISSUER,
        DEFAULT_TOKEN_TTL_SECS,
    )
    .expect("signing with S1 should succeed");

    let result =
        validate_media_token_with_rotation(&token, &s2, None, SFU_AUDIENCE, DEFAULT_JWT_ISSUER);

    assert!(
        result.is_err(),
        "S1-signed token must be rejected when current=S2 and no previous secret"
    );
}

/// Requirement 4.5: token with nbf in the future is rejected.
/// Note: jsonwebtoken has a default 60s leeway for nbf, so we set nbf
/// well beyond that (now + 120s) to ensure rejection.
#[test]
fn future_nbf_rejected() {
    let secret = test_secret();
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_secs();

    let claims = MediaTokenClaims {
        room_id: "room-1".to_string(),
        participant_id: "peer-1".to_string(),
        permissions: vec!["publish".to_string(), "subscribe".to_string()],
        exp: now + 3600,
        nbf: now + 120, // 120 seconds in the future (beyond default 60s leeway)
        iat: now,
        jti: Uuid::new_v4().to_string(),
        aud: SFU_AUDIENCE.to_string(),
        iss: DEFAULT_JWT_ISSUER.to_string(),
    };

    let key = EncodingKey::from_secret(&secret);
    let token = encode(&Header::new(Algorithm::HS256), &claims, &key)
        .expect("manual encoding should succeed");

    let result = validate_media_token(&token, &secret, SFU_AUDIENCE, DEFAULT_JWT_ISSUER);
    assert!(
        result.is_err(),
        "token with nbf = now + 120s must be rejected"
    );
}

/// Requirement 4.6: token with exp in the past is rejected.
#[test]
fn expired_token_rejected() {
    let secret = test_secret();
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_secs();

    let claims = MediaTokenClaims {
        room_id: "room-1".to_string(),
        participant_id: "peer-1".to_string(),
        permissions: vec!["publish".to_string(), "subscribe".to_string()],
        exp: now - 3600, // 1 hour in the past
        nbf: now - 7200,
        iat: now - 7200,
        jti: Uuid::new_v4().to_string(),
        aud: SFU_AUDIENCE.to_string(),
        iss: DEFAULT_JWT_ISSUER.to_string(),
    };

    let key = EncodingKey::from_secret(&secret);
    let token = encode(&Header::new(Algorithm::HS256), &claims, &key)
        .expect("manual encoding should succeed");

    let result = validate_media_token(&token, &secret, SFU_AUDIENCE, DEFAULT_JWT_ISSUER);
    assert!(
        result.is_err(),
        "token with exp in the past must be rejected"
    );
}

// Feature: test-quality-hardening, Property 2: JWT jti uniqueness
// **Validates: Requirements 4.2**

use proptest::prelude::*;

proptest! {
    #![proptest_config(ProptestConfig::with_cases(100))]

    /// Property 2: For any room_id, participant_id, and valid secret (≥ 32 bytes),
    /// issuing two MediaTokens produces tokens with different jti values.
    #[test]
    fn jti_uniqueness_property(
        room_id in "[a-zA-Z0-9]{1,32}",
        participant_id in "[a-zA-Z0-9]{1,32}",
        secret in prop::collection::vec(any::<u8>(), 32..=64),
    ) {
        let token_a = sign_media_token(
            &room_id,
            &participant_id,
            &secret,
            DEFAULT_JWT_ISSUER,
            DEFAULT_TOKEN_TTL_SECS,
        )
        .expect("first sign should succeed");

        let token_b = sign_media_token(
            &room_id,
            &participant_id,
            &secret,
            DEFAULT_JWT_ISSUER,
            DEFAULT_TOKEN_TTL_SECS,
        )
        .expect("second sign should succeed");

        let claims_a = validate_media_token(
            &token_a,
            &secret,
            SFU_AUDIENCE,
            DEFAULT_JWT_ISSUER,
        )
        .expect("first validation should succeed");

        let claims_b = validate_media_token(
            &token_b,
            &secret,
            SFU_AUDIENCE,
            DEFAULT_JWT_ISSUER,
        )
        .expect("second validation should succeed");

        prop_assert_ne!(
            claims_a.jti,
            claims_b.jti,
            "two tokens for the same inputs must have different jti values"
        );
    }
}
