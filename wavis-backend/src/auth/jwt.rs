//! JWT signing and verification primitives for all Wavis token types.
//!
//! **Owns:** token encoding/decoding, signing, verification, key material
//! handling, and claims definitions for both device-auth access tokens
//! (`AuthTokenClaims`) and SFU media tokens (`MediaTokenClaims`). Also
//! wraps LiveKit access-token generation.
//!
//! **Does not own:** token lifecycle decisions (issuance, revocation,
//! rotation, refresh-token management, epoch enforcement) — that is
//! `domain::auth`. Does not own token policy (TTL choices, permission
//! sets) or room lifecycle.
//!
//! **Key invariants:**
//! - All JWT signing uses HMAC-SHA256 with a minimum 32-byte secret.
//! - Media tokens carry `room_id`, `participant_id`, and a permissions list.
//! - Auth tokens carry `sub` (user_id), `did` (device_id), and `epoch`.
//! - Validation supports zero-downtime key rotation: current secret first,
//!   then previous secret if present.
//! - LiveKit tokens use the LiveKit SDK's own signing path — this module
//!   only wraps the call to keep token creation centralized.
//!
//! **Layering:** pure domain utility. Called by `domain::auth`,
//! `domain::sfu_relay`, `domain::voice_orchestrator`, and
//! `domain::pairing`. No handler or state dependencies.

use crate::auth::auth::AuthError;
use crate::voice::sfu_bridge::SfuError;
use jsonwebtoken::{Algorithm, DecodingKey, EncodingKey, Header, Validation, decode, encode};
use livekit_api::access_token::{AccessToken, VideoGrants};
use serde::{Deserialize, Serialize};
use std::time::{SystemTime, UNIX_EPOCH};
use uuid::Uuid;

/// Default token TTL in seconds (6 hours).
/// Long-lived because the backend controls room membership independently.
pub const DEFAULT_TOKEN_TTL_SECS: u64 = 21600;

/// Legacy alias for backward compatibility.
pub const TOKEN_TTL_SECS: u64 = DEFAULT_TOKEN_TTL_SECS;

/// Default TTL for LiveKit tokens: 6 hours.
/// Long-lived because the backend controls room membership independently —
/// the token only grants media access, not signaling authority.
/// Keeping this short caused mid-call disconnects when the token expired.
pub const LIVEKIT_TOKEN_TTL_SECS: u64 = 21600;

/// Audience claim value for SFU tokens.
pub const SFU_AUDIENCE: &str = "wavis-sfu";

/// Default JWT issuer for MediaTokens.
#[allow(dead_code)]
pub const DEFAULT_JWT_ISSUER: &str = "wavis-backend";

/// JWT claims for a MediaToken.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct MediaTokenClaims {
    pub room_id: String,
    pub participant_id: String,
    pub permissions: Vec<String>,
    pub exp: u64,
    pub nbf: u64,
    pub iat: u64,
    pub jti: String,
    pub aud: String,
    pub iss: String,
}

/// Sign a MediaToken JWT for the given room and participant.
///
/// # Arguments
/// - `room_id` — room the participant is joining
/// - `participant_id` — peer ID assigned by the backend
/// - `secret` — shared secret (must be ≥ 32 bytes)
/// - `issuer` — JWT issuer claim (use `DEFAULT_JWT_ISSUER` for default)
/// - `ttl_secs` — token lifetime in seconds (use `DEFAULT_TOKEN_TTL_SECS` for default)
pub fn sign_media_token(
    room_id: &str,
    participant_id: &str,
    secret: &[u8],
    issuer: &str,
    ttl_secs: u64,
) -> Result<String, SfuError> {
    if secret.len() < 32 {
        return Err(SfuError::TokenError(
            "SFU_JWT_SECRET must be at least 32 bytes".to_string(),
        ));
    }

    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_err(|e| SfuError::TokenError(format!("system time error: {e}")))?
        .as_secs();

    let claims = MediaTokenClaims {
        room_id: room_id.to_string(),
        participant_id: participant_id.to_string(),
        permissions: vec!["publish".to_string(), "subscribe".to_string()],
        exp: now + ttl_secs,
        nbf: now,
        iat: now,
        jti: Uuid::new_v4().to_string(),
        aud: SFU_AUDIENCE.to_string(),
        iss: issuer.to_string(),
    };

    let key = EncodingKey::from_secret(secret);
    encode(&Header::new(Algorithm::HS256), &claims, &key)
        .map_err(|e| SfuError::TokenError(format!("JWT signing failed: {e}")))
}

/// Sign a LiveKit AccessToken JWT for the given room and participant.
///
/// Uses the `livekit-api` crate's `AccessToken` builder with `VideoGrants`
/// (room_join, can_publish, can_subscribe). The resulting JWT is opaque to
/// the backend — it is validated by the LiveKit server using the shared API secret.
///
/// # Arguments
/// - `room_id` — LiveKit room name (must be non-empty)
/// - `participant_id` — participant identity (must be non-empty)
/// - `api_key` — LiveKit API key (must be non-empty)
/// - `api_secret` — LiveKit API secret (must be non-empty)
/// - `ttl_secs` — token lifetime in seconds (use `LIVEKIT_TOKEN_TTL_SECS` for default)
pub fn sign_livekit_token(
    room_id: &str,
    participant_id: &str,
    display_name: &str,
    api_key: &str,
    api_secret: &str,
    ttl_secs: u64,
) -> Result<String, SfuError> {
    if api_key.is_empty() || api_secret.is_empty() {
        return Err(SfuError::TokenError(
            "LiveKit api_key and api_secret must be non-empty".to_string(),
        ));
    }
    if room_id.is_empty() || participant_id.is_empty() {
        return Err(SfuError::TokenError(
            "room_id and participant_id must be non-empty".to_string(),
        ));
    }

    let grants = VideoGrants {
        room_join: true,
        room: room_id.to_string(),
        can_publish: true,
        can_subscribe: true,
        ..Default::default()
    };

    AccessToken::with_api_key(api_key, api_secret)
        .with_identity(participant_id)
        .with_name(display_name)
        .with_grants(grants)
        .with_ttl(std::time::Duration::from_secs(ttl_secs))
        .to_jwt()
        .map_err(|e| SfuError::TokenError(format!("LiveKit token signing failed: {e}")))
}

#[allow(dead_code)] // will be used when backend validates incoming client tokens
/// Validate a MediaToken JWT and return its claims.
///
/// Rejects tokens with: expired `exp`, wrong `aud`, wrong `iss`, invalid signature.
pub fn validate_media_token(
    token: &str,
    secret: &[u8],
    expected_audience: &str,
    expected_issuer: &str,
) -> Result<MediaTokenClaims, SfuError> {
    if secret.len() < 32 {
        return Err(SfuError::TokenError(
            "SFU_JWT_SECRET must be at least 32 bytes".to_string(),
        ));
    }

    let key = DecodingKey::from_secret(secret);
    let mut validation = Validation::new(Algorithm::HS256);
    validation.set_audience(&[expected_audience]);
    validation.set_issuer(&[expected_issuer]);
    validation.validate_nbf = true;
    // Validate expiry by default (jsonwebtoken does this automatically)

    decode::<MediaTokenClaims>(token, &key, &validation)
        .map(|data| data.claims)
        .map_err(|e| SfuError::TokenError(format!("JWT validation failed: {e}")))
}

/// Validate a MediaToken JWT with key rotation support.
///
/// Tries the current secret first. If validation fails and a previous secret
/// is provided, retries with the previous secret (zero-downtime rotation).
/// When `previous_secret` is `None`, behaves identically to `validate_media_token`.
#[allow(dead_code)]
pub fn validate_media_token_with_rotation(
    token: &str,
    current_secret: &[u8],
    previous_secret: Option<&[u8]>,
    expected_audience: &str,
    expected_issuer: &str,
) -> Result<MediaTokenClaims, SfuError> {
    match validate_media_token(token, current_secret, expected_audience, expected_issuer) {
        Ok(claims) => Ok(claims),
        Err(_) if previous_secret.is_some() => validate_media_token(
            token,
            previous_secret.unwrap(),
            expected_audience,
            expected_issuer,
        ),
        Err(e) => Err(e),
    }
}

// ---------------------------------------------------------------------------
// Device-auth access-token primitives
// ---------------------------------------------------------------------------

/// Claims for device-auth access tokens.
/// Separate from MediaTokenClaims to maintain domain separation.
#[derive(Debug, Serialize, Deserialize)]
pub struct AuthTokenClaims {
    pub sub: String, // user_id (UUID string)
    /// Device ID (UUID string). Added for per-device identity in tokens.
    #[serde(default)]
    pub did: String,
    pub exp: u64,    // Unix timestamp
    pub aud: String, // "wavis"
    pub iss: String, // "wavis-backend"
    pub iat: u64,    // Issued at
    /// Session epoch — incremented on security events (reuse detection).
    /// Access tokens are rejected if their epoch does not match the current
    /// DB value, enabling immediate invalidation without key rotation.
    #[serde(default)]
    pub epoch: i32,
}

pub const AUTH_AUDIENCE: &str = "wavis";
pub const AUTH_ISSUER: &str = "wavis-backend";
pub const ACCESS_TOKEN_TTL_SECS: u64 = 900; // 15 minutes

/// Sign an access token for a given user_id and device_id.
/// Returns Err(AuthError::SecretTooShort) if secret < 32 bytes.
pub fn sign_access_token(
    user_id: &Uuid,
    device_id: &Uuid,
    secret: &[u8],
    ttl_secs: u64,
    epoch: i32,
) -> Result<String, AuthError> {
    if secret.len() < 32 {
        return Err(AuthError::SecretTooShort);
    }

    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("system clock before UNIX epoch")
        .as_secs();

    let claims = AuthTokenClaims {
        sub: user_id.to_string(),
        did: device_id.to_string(),
        exp: now + ttl_secs,
        aud: AUTH_AUDIENCE.to_string(),
        iss: AUTH_ISSUER.to_string(),
        iat: now,
        epoch,
    };

    encode(
        &Header::new(Algorithm::HS256),
        &claims,
        &EncodingKey::from_secret(secret),
    )
    .map_err(|e| AuthError::SigningFailed(e.to_string()))
}

/// Validate an access token. Returns the (user_id, device_id, epoch) triple on success.
/// Checks: signature, exp, aud="wavis", iss="wavis-backend".
pub fn validate_access_token(token: &str, secret: &[u8]) -> Result<(Uuid, Uuid, i32), AuthError> {
    let mut validation = Validation::new(Algorithm::HS256);
    validation.set_audience(&[AUTH_AUDIENCE]);
    validation.set_issuer(&[AUTH_ISSUER]);
    validation.set_required_spec_claims(&["sub", "exp", "aud", "iss", "iat"]);

    let token_data =
        decode::<AuthTokenClaims>(token, &DecodingKey::from_secret(secret), &validation).map_err(
            |e| match e.kind() {
                jsonwebtoken::errors::ErrorKind::ExpiredSignature => AuthError::TokenExpired,
                _ => AuthError::ValidationFailed,
            },
        )?;

    let user_id = Uuid::parse_str(&token_data.claims.sub).map_err(|_| AuthError::InvalidToken)?;
    // Parse device_id from the `did` claim. For backward compatibility with tokens
    // signed before the device_id migration, fall back to a nil UUID if empty.
    let device_id = if token_data.claims.did.is_empty() {
        Uuid::nil()
    } else {
        Uuid::parse_str(&token_data.claims.did).map_err(|_| AuthError::InvalidToken)?
    };
    Ok((user_id, device_id, token_data.claims.epoch))
}

/// Validate with key rotation support (try current, fall back to previous).
/// Returns (user_id, device_id, epoch) on success.
pub fn validate_access_token_with_rotation(
    token: &str,
    current_secret: &[u8],
    previous_secret: Option<&[u8]>,
) -> Result<(Uuid, Uuid, i32), AuthError> {
    match validate_access_token(token, current_secret) {
        Ok(result) => Ok(result),
        Err(_) => {
            if let Some(prev) = previous_secret {
                validate_access_token(token, prev)
            } else {
                Err(AuthError::ValidationFailed)
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use jsonwebtoken::{Algorithm, EncodingKey, Header, encode};
    use proptest::prelude::*;
    use std::time::{SystemTime, UNIX_EPOCH};
    use uuid::Uuid;

    fn valid_secret() -> Vec<u8> {
        b"a-secret-that-is-at-least-32-bytes-long!!".to_vec()
    }

    fn test_auth_secret() -> Vec<u8> {
        b"test-auth-secret-that-is-32-bytes!!".to_vec()
    }

    fn arb_uuid() -> impl Strategy<Value = Uuid> {
        prop::array::uniform16(any::<u8>()).prop_map(Uuid::from_bytes)
    }

    fn arb_secret() -> impl Strategy<Value = Vec<u8>> {
        prop::collection::vec(any::<u8>(), 32..=64)
    }

    #[test]
    fn livekit_token_builder_inputs_correctness() {
        // Unit test for LiveKit path: verify AccessToken builder receives correct inputs
        // We decode only minimal stable fields (sub/identity, video.room, exp) without
        // asserting token authenticity, as the livekit-api crate's internal claim layout
        // is not guaranteed stable across versions.
        // Validates: Requirements 1.1, 1.2, 1.3, 1.4, 1.5, 1.6

        let room_id = "test-room-123";
        let participant_id = "peer-42";
        let api_key = "test-api-key";
        let api_secret = "test-api-secret-long-enough";
        let ttl_secs = 600;

        let token = sign_livekit_token(
            room_id,
            participant_id,
            participant_id,
            api_key,
            api_secret,
            ttl_secs,
        )
        .expect("sign_livekit_token should succeed");

        assert!(!token.is_empty(), "token must be non-empty");

        // Decode without signature verification to inspect claims
        let claims = livekit_api::access_token::Claims::from_unverified(&token)
            .expect("token must be a valid JWT");

        // Requirement 1.4: participant_id matches input (sub claim)
        assert_eq!(
            &claims.sub, participant_id,
            "sub (identity) must equal participant_id"
        );

        // Requirement 1.3: room_id matches input
        assert_eq!(&claims.video.room, room_id, "video.room must equal room_id");

        // Requirement 1.5: permissions contain expected grants
        assert!(claims.video.room_join, "video.room_join must be true");
        assert!(claims.video.can_publish, "video.can_publish must be true");
        assert!(
            claims.video.can_subscribe,
            "video.can_subscribe must be true"
        );

        // Requirement 1.1: exp is in the future and within TTL range
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_secs() as usize;
        assert!(claims.exp > now, "exp must be in the future");
        assert!(
            claims.exp <= now + ttl_secs as usize + 2,
            "exp must be within ttl_secs of now"
        );

        // Note: We do NOT assert token authenticity here — only claim construction correctness.
        // The LiveKit server validates the token signature using the shared API secret.
    }

    #[test]
    fn sign_and_verify_round_trip() {
        let secret = valid_secret();
        let token = sign_media_token(
            "room-1",
            "peer-1",
            &secret,
            DEFAULT_JWT_ISSUER,
            TOKEN_TTL_SECS,
        )
        .unwrap();
        let claims =
            validate_media_token(&token, &secret, SFU_AUDIENCE, DEFAULT_JWT_ISSUER).unwrap();
        assert_eq!(claims.room_id, "room-1");
        assert_eq!(claims.participant_id, "peer-1");
        assert_eq!(claims.permissions, vec!["publish", "subscribe"]);
        assert_eq!(claims.aud, SFU_AUDIENCE);
        assert_eq!(claims.iss, DEFAULT_JWT_ISSUER);
    }

    #[test]
    fn short_secret_is_rejected_on_sign() {
        let short_secret = b"too-short";
        let result = sign_media_token(
            "room-1",
            "peer-1",
            short_secret,
            DEFAULT_JWT_ISSUER,
            TOKEN_TTL_SECS,
        );
        assert!(result.is_err());
    }

    #[test]
    fn short_secret_is_rejected_on_verify() {
        let secret = valid_secret();
        let token = sign_media_token(
            "room-1",
            "peer-1",
            &secret,
            DEFAULT_JWT_ISSUER,
            TOKEN_TTL_SECS,
        )
        .unwrap();
        let short_secret = b"too-short";
        let result = validate_media_token(&token, short_secret, SFU_AUDIENCE, DEFAULT_JWT_ISSUER);
        assert!(result.is_err());
    }

    #[test]
    fn expired_token_is_rejected() {
        let secret = valid_secret();
        // Build a token with exp already in the past by constructing claims directly
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
            jti: uuid::Uuid::new_v4().to_string(),
            aud: SFU_AUDIENCE.to_string(),
            iss: DEFAULT_JWT_ISSUER.to_string(),
        };
        let key = jsonwebtoken::EncodingKey::from_secret(&secret);
        let token = jsonwebtoken::encode(
            &jsonwebtoken::Header::new(jsonwebtoken::Algorithm::HS256),
            &claims,
            &key,
        )
        .unwrap();
        let result = validate_media_token(&token, &secret, SFU_AUDIENCE, DEFAULT_JWT_ISSUER);
        assert!(result.is_err(), "expired token should be rejected");
    }

    #[test]
    fn wrong_audience_is_rejected() {
        let secret = valid_secret();
        let token = sign_media_token(
            "room-1",
            "peer-1",
            &secret,
            DEFAULT_JWT_ISSUER,
            TOKEN_TTL_SECS,
        )
        .unwrap();
        let result = validate_media_token(&token, &secret, "wrong-audience", DEFAULT_JWT_ISSUER);
        assert!(result.is_err(), "wrong audience should be rejected");
    }

    #[test]
    fn corrupted_signature_is_rejected() {
        let secret = valid_secret();
        let token = sign_media_token(
            "room-1",
            "peer-1",
            &secret,
            DEFAULT_JWT_ISSUER,
            TOKEN_TTL_SECS,
        )
        .unwrap();
        // Corrupt the signature by appending garbage
        let corrupted = format!("{token}CORRUPTED");
        let result = validate_media_token(&corrupted, &secret, SFU_AUDIENCE, DEFAULT_JWT_ISSUER);
        assert!(result.is_err(), "corrupted token should be rejected");
    }

    // --- Unit tests for validate_media_token_with_rotation ---

    #[test]
    fn rotation_current_secret_succeeds() {
        let secret = valid_secret();
        let token = sign_media_token(
            "room-1",
            "peer-1",
            &secret,
            DEFAULT_JWT_ISSUER,
            TOKEN_TTL_SECS,
        )
        .unwrap();
        let claims = validate_media_token_with_rotation(
            &token,
            &secret,
            None,
            SFU_AUDIENCE,
            DEFAULT_JWT_ISSUER,
        )
        .unwrap();
        assert_eq!(claims.room_id, "room-1");
        assert_eq!(claims.participant_id, "peer-1");
    }

    #[test]
    fn rotation_falls_back_to_previous_secret() {
        let old_secret = valid_secret();
        let new_secret = b"different-secret-at-least-32-bytes-long!!".to_vec();
        // Sign with old secret
        let token = sign_media_token(
            "room-1",
            "peer-1",
            &old_secret,
            DEFAULT_JWT_ISSUER,
            TOKEN_TTL_SECS,
        )
        .unwrap();
        // Validate with new current + old previous — should succeed via fallback
        let claims = validate_media_token_with_rotation(
            &token,
            &new_secret,
            Some(&old_secret),
            SFU_AUDIENCE,
            DEFAULT_JWT_ISSUER,
        )
        .unwrap();
        assert_eq!(claims.room_id, "room-1");
    }

    #[test]
    fn rotation_no_previous_secret_fails_on_wrong_current() {
        let signing_secret = valid_secret();
        let wrong_secret = b"wrong-secret-at-least-32-bytes-long!!!".to_vec();
        let token = sign_media_token(
            "room-1",
            "peer-1",
            &signing_secret,
            DEFAULT_JWT_ISSUER,
            TOKEN_TTL_SECS,
        )
        .unwrap();
        let result = validate_media_token_with_rotation(
            &token,
            &wrong_secret,
            None,
            SFU_AUDIENCE,
            DEFAULT_JWT_ISSUER,
        );
        assert!(
            result.is_err(),
            "should fail when current secret is wrong and no previous is set"
        );
    }

    #[test]
    fn rotation_both_secrets_wrong_fails() {
        let signing_secret = valid_secret();
        let wrong_current = b"wrong-current-at-least-32-bytes-long!!".to_vec();
        let wrong_previous = b"wrong-previous-at-least-32-bytes-long!".to_vec();
        let token = sign_media_token(
            "room-1",
            "peer-1",
            &signing_secret,
            DEFAULT_JWT_ISSUER,
            TOKEN_TTL_SECS,
        )
        .unwrap();
        let result = validate_media_token_with_rotation(
            &token,
            &wrong_current,
            Some(&wrong_previous),
            SFU_AUDIENCE,
            DEFAULT_JWT_ISSUER,
        );
        assert!(result.is_err(), "should fail when both secrets are wrong");
    }

    // --- Property 1 (livekit-integration): LiveKit token generation produces valid JWT ---
    // Validates: Requirements 4.3, 4.4, 4.5, 4.7, 4.8

    proptest! {
        #![proptest_config(ProptestConfig::with_cases(100))]

        #[test]
        fn prop_livekit_token_produces_valid_jwt(
            room_id in "[a-zA-Z0-9_-]{1,32}",
            participant_id in "[a-zA-Z0-9_-]{1,32}",
            api_key in "[a-zA-Z0-9]{4,16}",
            api_secret in "[a-zA-Z0-9]{8,32}",
            ttl_secs in 1u64..=3600u64,
        ) {
            let token = sign_livekit_token(&room_id, &participant_id, &participant_id, &api_key, &api_secret, ttl_secs)
                .expect("sign_livekit_token should succeed for valid inputs");

            prop_assert!(!token.is_empty(), "token must be non-empty");

            // Decode without signature verification to inspect claims
            let claims = livekit_api::access_token::Claims::from_unverified(&token)
                .expect("token must be a valid JWT");

            prop_assert_eq!(&claims.sub, &participant_id, "sub (identity) must equal participant_id");
            prop_assert_eq!(&claims.video.room, &room_id, "video.room must equal room_id");
            prop_assert!(claims.video.room_join, "video.room_join must be true");
            prop_assert!(claims.video.can_publish, "video.can_publish must be true");
            prop_assert!(claims.video.can_subscribe, "video.can_subscribe must be true");

            let now = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_secs() as usize;
            prop_assert!(
                claims.exp > now,
                "exp must be in the future (exp={}, now={})", claims.exp, now
            );
            prop_assert!(
                claims.exp <= now + ttl_secs as usize + 2,
                "exp must be within ttl_secs of now"
            );
        }

        #[test]
        fn prop_livekit_token_empty_inputs_rejected(
            room_id in "[a-zA-Z0-9_-]{1,32}",
            participant_id in "[a-zA-Z0-9_-]{1,32}",
            api_key in "[a-zA-Z0-9]{4,16}",
            api_secret in "[a-zA-Z0-9]{8,32}",
            empty_field in 0usize..4usize,
        ) {
            let (r, p, k, s) = match empty_field {
                0 => ("", participant_id.as_str(), api_key.as_str(), api_secret.as_str()),
                1 => (room_id.as_str(), "", api_key.as_str(), api_secret.as_str()),
                2 => (room_id.as_str(), participant_id.as_str(), "", api_secret.as_str()),
                _ => (room_id.as_str(), participant_id.as_str(), api_key.as_str(), ""),
            };
            let result = sign_livekit_token(r, p, p, k, s, 60);
            prop_assert!(result.is_err(), "empty input (field={empty_field}) should be rejected");
        }
    }

    // --- Property 1: MediaToken claims correctness ---
    // Feature: token-and-signaling-auth, Property 1: MediaToken claims correctness
    // For any valid room_id, participant_id, issuer, audience, and TTL configuration,
    // the issued MediaToken's decoded claims SHALL have: exp within [now+300, now+900],
    // iss matching the configured issuer, aud matching the configured audience,
    // room_id matching the input room, participant_id matching the input identity,
    // and permissions containing only the expected grants.
    // Validates: Requirements 1.1, 1.2, 1.3, 1.4, 1.5, 1.6

    proptest! {
        #![proptest_config(ProptestConfig::with_cases(100))]

        #[test]
        fn prop_media_token_claims_correctness(
            room_id in "[a-z0-9-]{1,32}",
            participant_id in "[a-z0-9-]{1,32}",
            issuer in "[a-zA-Z0-9_-]{1,32}",
            ttl_secs in 300u64..=900u64,  // 5-15 minutes as per Requirement 1.1
            secret_suffix in "[a-zA-Z0-9]{8,32}",
        ) {
            let secret = format!("base-secret-32-bytes-minimum!!!{secret_suffix}");
            let secret_bytes = secret.as_bytes();
            // Use SFU_AUDIENCE as the audience for both signing and validation
            let audience = SFU_AUDIENCE;

            let now_before = SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap()
                .as_secs();

            // Sign the token with the given parameters
            let token = sign_media_token(&room_id, &participant_id, secret_bytes, &issuer, ttl_secs)
                .expect("signing should succeed");

            let now_after = SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap()
                .as_secs();

            // Validate and decode the token
            let claims = validate_media_token(&token, secret_bytes, audience, &issuer)
                .expect("validation should succeed");

            // Requirement 1.3: room_id matches input
            prop_assert_eq!(&claims.room_id, &room_id, "room_id must match input");

            // Requirement 1.4: participant_id matches input
            prop_assert_eq!(&claims.participant_id, &participant_id, "participant_id must match input");

            // Requirement 1.5: permissions contain only expected grants
            prop_assert_eq!(
                &claims.permissions,
                &vec!["publish".to_string(), "subscribe".to_string()],
                "permissions must be [publish, subscribe]"
            );

            // Requirement 1.2: aud matches configured audience
            prop_assert_eq!(&claims.aud, audience, "aud must match configured audience");

            // Requirement 1.2: iss matches configured issuer
            prop_assert_eq!(&claims.iss, &issuer, "iss must match configured issuer");

            // Requirement 1.1: exp is within the TTL range [now+ttl_secs-2, now+ttl_secs+2]
            // Allow 2 second clock skew for test execution time
            prop_assert!(
                claims.exp >= now_before + ttl_secs - 2,
                "exp must be at least now + ttl_secs (exp={}, now_before={}, ttl={})",
                claims.exp, now_before, ttl_secs
            );
            prop_assert!(
                claims.exp <= now_after + ttl_secs + 2,
                "exp must be at most now + ttl_secs + 2 (exp={}, now_after={}, ttl={})",
                claims.exp, now_after, ttl_secs
            );

            // Requirement 1.1: exp is in the future
            prop_assert!(claims.exp > now_before, "exp must be in the future");
        }

        // --- Property 2: MediaToken validation rejects mismatched iss/aud ---
        // Feature: token-and-signaling-auth, Property 2: MediaToken validation rejects mismatched iss/aud
        // For any valid MediaToken and any modified iss or aud value that differs from the expected
        // configuration, validate_media_token SHALL return an error.
        // Validates: Requirements 1.7

        #[test]
        fn prop_media_token_rejects_mismatched_iss_aud(
            room_id in "[a-z0-9-]{1,32}",
            participant_id in "[a-z0-9-]{1,32}",
            issuer in "[a-zA-Z0-9_-]{1,32}",
            wrong_issuer in "[a-zA-Z0-9_-]{1,32}",
            wrong_audience in "[a-zA-Z0-9_-]{1,32}",
            ttl_secs in 300u64..=900u64,
            secret_suffix in "[a-zA-Z0-9]{8,32}",
            mutation_type in 0usize..2usize,
        ) {
            let secret = format!("base-secret-32-bytes-minimum!!!{secret_suffix}");
            let secret_bytes = secret.as_bytes();
            let audience = SFU_AUDIENCE;

            // Sign a valid token with the original issuer and audience
            let token = sign_media_token(&room_id, &participant_id, secret_bytes, &issuer, ttl_secs)
                .expect("signing should succeed");

            // Verify the token is valid with correct iss/aud
            let valid_result = validate_media_token(&token, secret_bytes, audience, &issuer);
            prop_assert!(valid_result.is_ok(), "token should be valid with correct iss/aud");

            // Now test rejection with mismatched iss or aud
            let result = match mutation_type {
                0 => {
                    // Wrong issuer (must be different from original)
                    let modified_issuer = if wrong_issuer == issuer {
                        format!("{issuer}-modified")
                    } else {
                        wrong_issuer
                    };
                    validate_media_token(&token, secret_bytes, audience, &modified_issuer)
                }
                _ => {
                    // Wrong audience (must be different from SFU_AUDIENCE)
                    let modified_audience = if wrong_audience == audience {
                        format!("{audience}-modified")
                    } else {
                        wrong_audience
                    };
                    validate_media_token(&token, secret_bytes, &modified_audience, &issuer)
                }
            };

            prop_assert!(
                result.is_err(),
                "token should be rejected with mismatched iss/aud (mutation_type={mutation_type})"
            );
        }

        // --- Property 2 (legacy): Invalid token rejection ---
        // Feature: sfu-multi-party-voice, Property 2: Invalid token rejection
        // Validates: Requirements 1.8, 9.2

        #[test]
        fn prop_invalid_token_rejected(
            room_id in "[a-z0-9-]{1,32}",
            participant_id in "[a-z0-9-]{1,32}",
            wrong_suffix in "[a-zA-Z0-9]{8,32}",
            mutation in 0usize..3usize,
        ) {
            let secret = b"base-secret-32-bytes-minimum!!!X";
            let token = sign_media_token(&room_id, &participant_id, secret, DEFAULT_JWT_ISSUER, TOKEN_TTL_SECS)
                .expect("signing should succeed");

            let result = match mutation {
                0 => {
                    // Corrupt signature
                    let corrupted = format!("{token}X");
                    validate_media_token(&corrupted, secret, SFU_AUDIENCE, DEFAULT_JWT_ISSUER)
                }
                1 => {
                    // Wrong audience
                    let wrong_aud = format!("wrong-{wrong_suffix}");
                    validate_media_token(&token, secret, &wrong_aud, DEFAULT_JWT_ISSUER)
                }
                _ => {
                    // Wrong secret
                    let wrong_secret = format!("wrong-secret-32-bytes-minimum!!{wrong_suffix}");
                    validate_media_token(&token, wrong_secret.as_bytes(), SFU_AUDIENCE, DEFAULT_JWT_ISSUER)
                }
            };

            prop_assert!(result.is_err(), "mutated token should be rejected (mutation={mutation})");
        }
    }

    // --- Feature: security-hardening, Property 4: Short JWT secrets are rejected ---
    // For any byte string with length in [0, 31], sign_media_token SHALL return an error.
    // Validates: Requirements 2.3

    // --- Feature: security-hardening, Property 5: Key rotation dual-secret validation ---
    // For any valid MediaToken signed with secret S1, and given current_secret = S2 and
    // previous_secret = Some(S1) where S1 ≠ S2, validate_media_token_with_rotation SHALL
    // succeed. When previous_secret is None, validation with a non-signing secret SHALL fail.
    // Validates: Requirements 2.7, 2.8, 2.9

    proptest! {
        #![proptest_config(ProptestConfig::with_cases(100))]

        #[test]
        fn prop_short_jwt_secret_rejected(
            secret_len in 0usize..32usize,
            seed in proptest::collection::vec(any::<u8>(), 0..64),
        ) {
            // Generate a byte string of exactly secret_len bytes
            let short_secret: Vec<u8> = seed.into_iter().cycle().take(secret_len).collect();
            prop_assert!(short_secret.len() < 32, "secret must be < 32 bytes");

            let result = sign_media_token(
                "room-1",
                "peer-1",
                &short_secret,
                DEFAULT_JWT_ISSUER,
                DEFAULT_TOKEN_TTL_SECS,
            );
            prop_assert!(
                result.is_err(),
                "sign_media_token must reject secret of length {} (< 32 bytes)",
                short_secret.len()
            );
        }

        #[test]
        fn prop_key_rotation_dual_secret_validation(
            room_id in "[a-z0-9-]{1,32}",
            participant_id in "[a-z0-9-]{1,32}",
            s1_suffix in "[a-zA-Z0-9]{8,32}",
            s2_suffix in "[a-zA-Z0-9]{8,32}",
        ) {
            let s1 = format!("secret-one-at-least-32-bytes!!!{s1_suffix}");
            let s2 = format!("secret-two-at-least-32-bytes!!!{s2_suffix}");

            // Ensure S1 ≠ S2
            let s2 = if s1 == s2 {
                format!("{s2}-different")
            } else {
                s2
            };

            let s1_bytes = s1.as_bytes();
            let s2_bytes = s2.as_bytes();

            // Sign a token with S1
            let token = sign_media_token(
                &room_id,
                &participant_id,
                s1_bytes,
                DEFAULT_JWT_ISSUER,
                DEFAULT_TOKEN_TTL_SECS,
            )
            .expect("signing with S1 should succeed");

            // Case 1: current=S2, previous=Some(S1) → should succeed via fallback
            let result = validate_media_token_with_rotation(
                &token,
                s2_bytes,
                Some(s1_bytes),
                SFU_AUDIENCE,
                DEFAULT_JWT_ISSUER,
            );
            prop_assert!(
                result.is_ok(),
                "rotation with previous=S1 should succeed, got: {:?}",
                result.err()
            );

            let claims = result.unwrap();
            prop_assert_eq!(&claims.room_id, &room_id);
            prop_assert_eq!(&claims.participant_id, &participant_id);

            // Case 2: current=S2, previous=None → should fail
            let result_no_prev = validate_media_token_with_rotation(
                &token,
                s2_bytes,
                None,
                SFU_AUDIENCE,
                DEFAULT_JWT_ISSUER,
            );
            prop_assert!(
                result_no_prev.is_err(),
                "rotation without previous secret should fail when current != signing secret"
            );
        }
    }

    // --- Feature: security-hardening, Property 7: MediaToken includes jti, nbf, iat ---
    // For any valid room_id, participant_id, and secret, a signed MediaToken's decoded claims
    // SHALL contain: a jti that is a valid UUID v4, an nbf equal to iat, and both nbf and iat
    // within ±5 seconds of the current unix timestamp.
    // Validates: Requirements 5.1, 5.2, 5.3

    // --- Feature: security-hardening, Property 8: MediaToken with future nbf is rejected ---
    // For any MediaToken where nbf is set to a time in the future (current time + offset where
    // offset > 0), validate_media_token SHALL return an error.
    // Validates: Requirements 5.4

    // --- Feature: security-hardening, Property 9: MediaToken sign-then-validate round trip ---
    // For any valid room_id, participant_id, issuer, and secret (≥ 32 bytes), signing a
    // MediaToken and then validating it SHALL produce claims where room_id, participant_id,
    // permissions, aud, iss, jti, nbf, and iat all match the original values.
    // Validates: Requirements 5.6

    proptest! {
        #![proptest_config(ProptestConfig::with_cases(100))]

        #[test]
        fn prop_media_token_includes_jti_nbf_iat(
            room_id in "[a-z0-9-]{1,32}",
            participant_id in "[a-z0-9-]{1,32}",
            secret_suffix in "[a-zA-Z0-9]{8,32}",
        ) {
            let secret = format!("base-secret-32-bytes-minimum!!!{secret_suffix}");
            let secret_bytes = secret.as_bytes();

            let now_before = SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap()
                .as_secs();

            let token = sign_media_token(
                &room_id,
                &participant_id,
                secret_bytes,
                DEFAULT_JWT_ISSUER,
                DEFAULT_TOKEN_TTL_SECS,
            )
            .expect("signing should succeed");

            let now_after = SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap()
                .as_secs();

            // Decode the token to inspect claims
            let claims = validate_media_token(
                &token,
                secret_bytes,
                SFU_AUDIENCE,
                DEFAULT_JWT_ISSUER,
            )
            .expect("validation should succeed");

            // Requirement 5.1: jti is a valid UUID v4
            let parsed_uuid = uuid::Uuid::parse_str(&claims.jti);
            prop_assert!(
                parsed_uuid.is_ok(),
                "jti must be a valid UUID, got: {}",
                claims.jti
            );
            let parsed_uuid = parsed_uuid.unwrap();
            prop_assert_eq!(
                parsed_uuid.get_version(),
                Some(uuid::Version::Random),
                "jti must be UUID v4 (random), got version: {:?}",
                parsed_uuid.get_version()
            );

            // Requirement 5.2 + 5.3: nbf == iat
            prop_assert_eq!(
                claims.nbf,
                claims.iat,
                "nbf must equal iat (nbf={}, iat={})",
                claims.nbf,
                claims.iat
            );

            // Both nbf and iat within ±5 seconds of current unix timestamp
            prop_assert!(
                claims.iat >= now_before.saturating_sub(5),
                "iat must be within 5s of now (iat={}, now_before={})",
                claims.iat,
                now_before
            );
            prop_assert!(
                claims.iat <= now_after + 5,
                "iat must be within 5s of now (iat={}, now_after={})",
                claims.iat,
                now_after
            );
            prop_assert!(
                claims.nbf >= now_before.saturating_sub(5),
                "nbf must be within 5s of now (nbf={}, now_before={})",
                claims.nbf,
                now_before
            );
            prop_assert!(
                claims.nbf <= now_after + 5,
                "nbf must be within 5s of now (nbf={}, now_after={})",
                claims.nbf,
                now_after
            );
        }

        #[test]
        fn prop_media_token_future_nbf_rejected(
            room_id in "[a-z0-9-]{1,32}",
            participant_id in "[a-z0-9-]{1,32}",
            secret_suffix in "[a-zA-Z0-9]{8,32}",
            future_offset in 120u64..3600u64,
        ) {
            let secret = format!("base-secret-32-bytes-minimum!!!{secret_suffix}");
            let secret_bytes = secret.as_bytes();

            let now = SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap()
                .as_secs();

            // Manually construct claims with nbf set to the future
            let claims = MediaTokenClaims {
                room_id: room_id.clone(),
                participant_id: participant_id.clone(),
                permissions: vec!["publish".to_string(), "subscribe".to_string()],
                exp: now + future_offset + DEFAULT_TOKEN_TTL_SECS,
                nbf: now + future_offset,
                iat: now,
                jti: Uuid::new_v4().to_string(),
                aud: SFU_AUDIENCE.to_string(),
                iss: DEFAULT_JWT_ISSUER.to_string(),
            };

            // Encode directly with jsonwebtoken
            let key = EncodingKey::from_secret(secret_bytes);
            let token = encode(&Header::new(Algorithm::HS256), &claims, &key)
                .expect("encoding should succeed");

            // Validate — should reject because nbf is in the future
            let result = validate_media_token(
                &token,
                secret_bytes,
                SFU_AUDIENCE,
                DEFAULT_JWT_ISSUER,
            );
            prop_assert!(
                result.is_err(),
                "token with future nbf (now + {}s) must be rejected",
                future_offset
            );
        }

        #[test]
        fn prop_media_token_sign_then_validate_round_trip(
            room_id in "[a-z0-9-]{1,32}",
            participant_id in "[a-z0-9-]{1,32}",
            issuer in "[a-zA-Z0-9_-]{1,32}",
            secret_suffix in "[a-zA-Z0-9]{8,32}",
            ttl_secs in 300u64..=900u64,
        ) {
            let secret = format!("base-secret-32-bytes-minimum!!!{secret_suffix}");
            let secret_bytes = secret.as_bytes();

            let now_before = SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap()
                .as_secs();

            // Sign the token
            let token = sign_media_token(
                &room_id,
                &participant_id,
                secret_bytes,
                &issuer,
                ttl_secs,
            )
            .expect("signing should succeed");

            let now_after = SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap()
                .as_secs();

            // Validate and get claims back
            let claims = validate_media_token(
                &token,
                secret_bytes,
                SFU_AUDIENCE,
                &issuer,
            )
            .expect("validation should succeed");

            // Round-trip: all fields must match original values
            prop_assert_eq!(&claims.room_id, &room_id, "room_id round-trip mismatch");
            prop_assert_eq!(&claims.participant_id, &participant_id, "participant_id round-trip mismatch");
            prop_assert_eq!(
                &claims.permissions,
                &vec!["publish".to_string(), "subscribe".to_string()],
                "permissions round-trip mismatch"
            );
            prop_assert_eq!(&claims.aud, SFU_AUDIENCE, "aud round-trip mismatch");
            prop_assert_eq!(&claims.iss, &issuer, "iss round-trip mismatch");

            // jti must be a valid UUID v4
            let parsed_uuid = uuid::Uuid::parse_str(&claims.jti);
            prop_assert!(parsed_uuid.is_ok(), "jti must be a valid UUID after round-trip");
            prop_assert_eq!(
                parsed_uuid.unwrap().get_version(),
                Some(uuid::Version::Random),
                "jti must be UUID v4 after round-trip"
            );

            // nbf and iat must match (both set to now at sign time)
            prop_assert_eq!(claims.nbf, claims.iat, "nbf must equal iat after round-trip");

            // nbf/iat within expected time range
            prop_assert!(
                claims.iat >= now_before.saturating_sub(1) && claims.iat <= now_after + 1,
                "iat must be within signing time window (iat={}, before={}, after={})",
                claims.iat, now_before, now_after
            );
        }
    }

    // -----------------------------------------------------------------------
    // Access-token (device-auth) primitive tests
    // -----------------------------------------------------------------------

    // Feature: device-auth, Example 1: Access token sign/validate round-trip
    // Validates: Requirements 3.6, 1.3
    #[test]
    fn access_token_round_trip() {
        let secret = test_auth_secret();
        let user_id = Uuid::parse_str("aaaaaaaa-aaaa-4aaa-8aaa-aaaaaaaaaaaa").unwrap();
        let device_id = Uuid::parse_str("bbbbbbbb-bbbb-4bbb-8bbb-bbbbbbbbbbbb").unwrap();
        let epoch = 7;

        let token = sign_access_token(&user_id, &device_id, &secret, ACCESS_TOKEN_TTL_SECS, epoch)
            .expect("signing should succeed");

        let result = validate_access_token(&token, &secret).expect("validation should succeed");
        assert_eq!(result, (user_id, device_id, epoch));
    }

    // Feature: device-auth, Example 2: Expired access token rejection
    // Validates: Requirements 3.3
    #[test]
    fn access_token_expired_is_rejected() {
        let secret = test_auth_secret();
        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_secs();
        let claims = AuthTokenClaims {
            sub: "aaaaaaaa-aaaa-4aaa-8aaa-aaaaaaaaaaaa".to_string(),
            did: "bbbbbbbb-bbbb-4bbb-8bbb-bbbbbbbbbbbb".to_string(),
            exp: now.saturating_sub(3600),
            aud: AUTH_AUDIENCE.to_string(),
            iss: AUTH_ISSUER.to_string(),
            iat: now.saturating_sub(7200),
            epoch: 7,
        };
        let token = encode(
            &Header::new(Algorithm::HS256),
            &claims,
            &EncodingKey::from_secret(&secret),
        )
        .expect("encoding should succeed");

        assert!(validate_access_token(&token, &secret).is_err());
    }

    // Feature: device-auth, Example 3: Tampered access token rejection
    // Validates: Requirements 3.3
    #[test]
    fn access_token_tampered_token_is_rejected() {
        let secret = test_auth_secret();
        let user_id = Uuid::parse_str("aaaaaaaa-aaaa-4aaa-8aaa-aaaaaaaaaaaa").unwrap();
        let device_id = Uuid::parse_str("bbbbbbbb-bbbb-4bbb-8bbb-bbbbbbbbbbbb").unwrap();
        let token = sign_access_token(&user_id, &device_id, &secret, ACCESS_TOKEN_TTL_SECS, 7)
            .expect("signing should succeed");
        let parts: Vec<&str> = token.split('.').collect();
        assert_eq!(
            parts.len(),
            3,
            "JWT must contain header, payload, signature"
        );
        let mut signature_bytes = parts[2].as_bytes().to_vec();
        let last_idx = signature_bytes
            .len()
            .checked_sub(1)
            .expect("JWT signature segment must be non-empty");
        signature_bytes[last_idx] ^= 0x01;
        let tampered = format!(
            "{}.{}.{}",
            parts[0],
            parts[1],
            String::from_utf8_lossy(&signature_bytes)
        );

        assert!(validate_access_token(&tampered, &secret).is_err());
    }

    // Feature: device-auth, Example 4: Wrong-key access token rejection
    // Validates: Requirements 3.3
    #[test]
    fn access_token_wrong_key_is_rejected() {
        let secret = test_auth_secret();
        let wrong_secret = b"different-auth-secret-32-bytes-long".to_vec();
        let user_id = Uuid::parse_str("aaaaaaaa-aaaa-4aaa-8aaa-aaaaaaaaaaaa").unwrap();
        let device_id = Uuid::parse_str("bbbbbbbb-bbbb-4bbb-8bbb-bbbbbbbbbbbb").unwrap();
        let token = sign_access_token(&user_id, &device_id, &secret, ACCESS_TOKEN_TTL_SECS, 7)
            .expect("signing should succeed");

        assert!(validate_access_token(&token, &wrong_secret).is_err());
    }

    // Feature: device-auth, Example 5: Malformed access token rejection
    // Validates: Requirements 3.3
    #[test]
    fn access_token_malformed_input_is_rejected() {
        let secret = test_auth_secret();

        for malformed in ["", "not-a-jwt", "a.b", "a.b.c.d.e"] {
            assert!(
                validate_access_token(malformed, &secret).is_err(),
                "malformed token {malformed:?} should be rejected"
            );
        }
    }

    // Feature: device-auth, Example 6: Rotation-based access token revocation
    // Validates: Requirements 3.5
    #[test]
    fn access_token_revoked_by_key_rotation_is_rejected() {
        let previous_secret = test_auth_secret();
        let current_secret = b"rotated-auth-secret-32-bytes-long!!".to_vec();
        let user_id = Uuid::parse_str("aaaaaaaa-aaaa-4aaa-8aaa-aaaaaaaaaaaa").unwrap();
        let device_id = Uuid::parse_str("bbbbbbbb-bbbb-4bbb-8bbb-bbbbbbbbbbbb").unwrap();
        let token = sign_access_token(
            &user_id,
            &device_id,
            &previous_secret,
            ACCESS_TOKEN_TTL_SECS,
            7,
        )
        .expect("signing should succeed");

        // Epoch-based revocation requires DB state and is covered in auth_integration.
        assert!(validate_access_token_with_rotation(&token, &current_secret, None).is_err());
    }

    // Feature: device-auth, Property 1: Access token sign/validate round-trip
    // Validates: Requirements 3.6, 1.3
    proptest! {
        #![proptest_config(ProptestConfig::with_cases(100))]
        #[test]
        fn prop_access_sign_validate_round_trip(
            user_id in arb_uuid(),
            device_id in arb_uuid(),
            secret in arb_secret(),
            ttl_secs in 60u64..=3600,
            epoch in 0i32..=100,
        ) {
            let token = sign_access_token(&user_id, &device_id, &secret, ttl_secs, epoch)
                .expect("signing should succeed with valid secret");
            let (recovered_uid, recovered_did, recovered_epoch) = validate_access_token(&token, &secret)
                .expect("validation should succeed for freshly signed token");
            prop_assert_eq!(recovered_uid, user_id);
            prop_assert_eq!(recovered_did, device_id);
            prop_assert_eq!(recovered_epoch, epoch);
        }
    }

    // Feature: device-auth, Property 2: Access token claims invariant
    // Validates: Requirements 3.2
    proptest! {
        #![proptest_config(ProptestConfig::with_cases(100))]
        #[test]
        fn prop_access_token_claims_invariant(
            user_id in arb_uuid(),
            device_id in arb_uuid(),
            secret in arb_secret(),
            ttl_secs in 60u64..=3600,
            epoch in 0i32..=100,
        ) {
            let before = SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap()
                .as_secs();

            let token = sign_access_token(&user_id, &device_id, &secret, ttl_secs, epoch)
                .expect("signing should succeed");

            let after = SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap()
                .as_secs();

            // Decode with permissive validation to inspect raw claims
            let mut permissive = Validation::new(Algorithm::HS256);
            permissive.insecure_disable_signature_validation();
            permissive.validate_exp = false;
            permissive.validate_aud = false;
            permissive.set_required_spec_claims::<&str>(&[]);

            let token_data = decode::<AuthTokenClaims>(
                &token,
                &DecodingKey::from_secret(&[]), // ignored with insecure validation
                &permissive,
            )
            .expect("permissive decode should succeed");

            let claims = token_data.claims;
            prop_assert_eq!(&claims.sub, &user_id.to_string());
            prop_assert_eq!(&claims.aud, AUTH_AUDIENCE);
            prop_assert_eq!(&claims.iss, AUTH_ISSUER);
            prop_assert_eq!(claims.epoch, epoch);
            // exp should be between (before + ttl_secs) and (after + ttl_secs)
            prop_assert!(claims.exp >= before + ttl_secs);
            prop_assert!(claims.exp <= after + ttl_secs);
        }
    }

    // Feature: device-auth, Property 3: Invalid token rejection
    // Validates: Requirements 3.3
    proptest! {
        #![proptest_config(ProptestConfig::with_cases(100))]
        #[test]
        fn prop_access_invalid_token_rejection(
            user_id in arb_uuid(),
            device_id in arb_uuid(),
            secret in arb_secret(),
            wrong_secret in arb_secret(),
            flip_pos in 0usize..10,
        ) {
            let token = sign_access_token(&user_id, &device_id, &secret, 900, 0)
                .expect("signing should succeed");

            // Sub-case 1: Corrupted signature (flip a byte in the last segment)
            {
                let parts: Vec<&str> = token.split('.').collect();
                prop_assert_eq!(parts.len(), 3);
                let mut sig_bytes = parts[2].as_bytes().to_vec();
                if !sig_bytes.is_empty() {
                    let idx = flip_pos % sig_bytes.len();
                    sig_bytes[idx] ^= 0xFF;
                }
                let corrupted = format!(
                    "{}.{}.{}",
                    parts[0],
                    parts[1],
                    String::from_utf8_lossy(&sig_bytes)
                );
                prop_assert!(validate_access_token(&corrupted, &secret).is_err());
            }

            // Sub-case 2: Token signed with wrong secret
            {
                if wrong_secret != secret {
                    let wrong_token = sign_access_token(&user_id, &device_id, &wrong_secret, 900, 0)
                        .expect("signing should succeed");
                    prop_assert!(validate_access_token(&wrong_token, &secret).is_err());
                }
            }

            // Sub-case 3: Expired token (sign with exp in the past)
            {
                let now = SystemTime::now()
                    .duration_since(UNIX_EPOCH)
                    .unwrap()
                    .as_secs();
                let expired_claims = AuthTokenClaims {
                    sub: user_id.to_string(),
                    did: device_id.to_string(),
                    exp: now.saturating_sub(3600), // 1 hour in the past
                    aud: AUTH_AUDIENCE.to_string(),
                    iss: AUTH_ISSUER.to_string(),
                    iat: now.saturating_sub(7200),
                    epoch: 0,
                };
                let expired_token = encode(
                    &Header::new(Algorithm::HS256),
                    &expired_claims,
                    &EncodingKey::from_secret(&secret),
                )
                .expect("encoding should succeed");
                prop_assert!(validate_access_token(&expired_token, &secret).is_err());
            }
        }
    }

    // Feature: device-auth, Property 4: Key rotation fallback
    // Validates: Requirements 3.5
    proptest! {
        #![proptest_config(ProptestConfig::with_cases(100))]
        #[test]
        fn prop_access_key_rotation_fallback(
            user_id in arb_uuid(),
            device_id in arb_uuid(),
            current_secret in arb_secret(),
            previous_secret in arb_secret(),
            unrelated_secret in arb_secret(),
        ) {
            // Token signed with previous secret should be accepted via rotation
            let token = sign_access_token(&user_id, &device_id, &previous_secret, 900, 0)
                .expect("signing should succeed");

            let result = validate_access_token_with_rotation(
                &token,
                &current_secret,
                Some(&previous_secret),
            );
            prop_assert!(result.is_ok(), "rotation should accept token signed with previous secret");
            let (recovered, _, _epoch) = result.unwrap();
            prop_assert_eq!(recovered, user_id);

            // Token signed with unrelated secret should be rejected even with rotation
            if unrelated_secret != current_secret && unrelated_secret != previous_secret {
                let bad_token = sign_access_token(&user_id, &device_id, &unrelated_secret, 900, 0)
                    .expect("signing should succeed");
                let bad_result = validate_access_token_with_rotation(
                    &bad_token,
                    &current_secret,
                    Some(&previous_secret),
                );
                prop_assert!(bad_result.is_err(), "unrelated secret should be rejected");
            }
        }
    }

    // Feature: device-auth, Property 5: Secret length enforcement
    // Validates: Requirements 3.1, 8.4
    proptest! {
        #![proptest_config(ProptestConfig::with_cases(100))]
        #[test]
        fn prop_access_secret_too_short_rejected(
            user_id in arb_uuid(),
            device_id in arb_uuid(),
            short_secret in prop::collection::vec(any::<u8>(), 0..32),
        ) {
            let result = sign_access_token(&user_id, &device_id, &short_secret, 900, 0);
            prop_assert!(matches!(result, Err(AuthError::SecretTooShort)));
        }
    }

    proptest! {
        #![proptest_config(ProptestConfig::with_cases(100))]
        #[test]
        fn prop_access_secret_sufficient_succeeds(
            user_id in arb_uuid(),
            device_id in arb_uuid(),
            valid_secret in prop::collection::vec(any::<u8>(), 32..=64),
        ) {
            let result = sign_access_token(&user_id, &device_id, &valid_secret, 900, 0);
            prop_assert!(result.is_ok());
        }
    }

    // Feature: security-hardening, Property: Session epoch round-trip
    // Validates: epoch claim survives sign → validate cycle
    proptest! {
        #![proptest_config(ProptestConfig::with_cases(100))]
        #[test]
        fn prop_access_epoch_round_trip(
            user_id in arb_uuid(),
            device_id in arb_uuid(),
            secret in arb_secret(),
            epoch in -100i32..=100,
        ) {
            let token = sign_access_token(&user_id, &device_id, &secret, 900, epoch)
                .expect("signing should succeed");
            let (_, _, recovered_epoch) = validate_access_token(&token, &secret)
                .expect("validation should succeed");
            prop_assert_eq!(recovered_epoch, epoch);
        }
    }

    // Feature: user-identity-recovery, Property 1: Access token sign/validate round-trip
    // **Validates: Requirements 2.1**
    proptest! {
        #![proptest_config(ProptestConfig::with_cases(100))]
        #[test]
        fn prop_access_identity_recovery_roundtrip(
            user_id in arb_uuid(),
            device_id in arb_uuid(),
            secret in arb_secret(),
            ttl_secs in 60u64..=3600,
            epoch in 0i32..=100,
        ) {
            let token = sign_access_token(&user_id, &device_id, &secret, ttl_secs, epoch)
                .expect("signing should succeed");

            let (recovered_uid, recovered_did, recovered_epoch) =
                validate_access_token(&token, &secret)
                    .expect("validation should succeed for freshly signed token");

            prop_assert_eq!(recovered_uid, user_id, "user_id must round-trip");
            prop_assert_eq!(recovered_did, device_id, "device_id must round-trip");
            prop_assert_eq!(recovered_epoch, epoch, "epoch must round-trip");

            // Validation with a different secret must fail
            let wrong_secret: Vec<u8> = secret.iter().map(|b| b ^ 0xFF).collect();
            if wrong_secret != secret {
                prop_assert!(
                    validate_access_token(&token, &wrong_secret).is_err(),
                    "wrong secret must be rejected"
                );
            }
        }
    }
}
