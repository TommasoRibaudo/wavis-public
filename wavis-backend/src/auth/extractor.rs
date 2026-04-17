//! Bearer-token authentication extractor for Axum handlers.
//!
//! **Owns:** extracting and validating the `Authorization: Bearer <token>`
//! header. Produces an [`AuthenticatedUser`] (user_id + device_id) that
//! downstream handlers can depend on for identity.
//!
//! **Does not own:** token creation, refresh, revocation, or any other auth
//! business logic. Validation is delegated to `domain::auth` functions
//! (`validate_access_token_with_rotation`, `check_session_epoch`).
//!
//! **Key invariants:**
//! - Rejection is opaque: all auth failures return the same 401 response
//!   to avoid leaking whether a token was expired, revoked, or malformed.
//! - Validation checks JWT signature, claims, device revocation status,
//!   *and* session epoch against the database — a revoked device is rejected
//!   even if the JWT is cryptographically valid.
//!
//! **Layering:** used by `handlers::auth_routes`, `handlers::channel_routes`,
//! and any future authenticated endpoint. Delegates to `domain::auth` for
//! all verification logic.

use axum::Json;
use axum::extract::FromRequestParts;
use axum::http::StatusCode;
use axum::http::request::Parts;
use uuid::Uuid;

use crate::app_state::AppState;
use crate::auth::auth::check_session_epoch;
use crate::auth::jwt::validate_access_token_with_rotation;
use crate::error::ErrorResponse;

/// Extracted from a valid `Authorization: Bearer <token>` header.
/// Available in handler signatures for authenticated endpoints.
/// Validates JWT signature/claims, device revocation status, AND session epoch against DB.
pub struct AuthenticatedUser {
    pub user_id: Uuid,
    pub device_id: Uuid,
}

fn reject() -> (StatusCode, Json<ErrorResponse>) {
    (
        StatusCode::UNAUTHORIZED,
        Json(ErrorResponse {
            error: "authentication failed".to_string(),
        }),
    )
}

impl FromRequestParts<AppState> for AuthenticatedUser {
    type Rejection = (StatusCode, Json<ErrorResponse>);

    async fn from_request_parts(
        parts: &mut Parts,
        state: &AppState,
    ) -> Result<Self, Self::Rejection> {
        let header_value = parts
            .headers
            .get("authorization")
            .and_then(|v| v.to_str().ok())
            .ok_or_else(reject)?;

        let token = header_value.strip_prefix("Bearer ").ok_or_else(reject)?;

        let (user_id, device_id, token_epoch) = validate_access_token_with_rotation(
            token,
            &state.auth_jwt_secret,
            state
                .auth_jwt_secret_previous
                .as_deref()
                .map(|v| v.as_slice()),
        )
        .map_err(|_| reject())?;

        // Verify the device has not been revoked.
        let revoked_at: Option<Option<chrono::DateTime<chrono::Utc>>> =
            sqlx::query_scalar("SELECT revoked_at FROM devices WHERE device_id = $1")
                .bind(device_id)
                .fetch_optional(&state.db_pool)
                .await
                .map_err(|_| reject())?;

        match revoked_at {
            Some(None) => {}           // device exists and is not revoked — OK
            _ => return Err(reject()), // not found OR revoked_at IS NOT NULL
        }

        // Verify the token's epoch matches the current DB epoch.
        // This rejects tokens issued before a security event (reuse detection).
        check_session_epoch(&state.db_pool, &user_id, token_epoch)
            .await
            .map_err(|_| reject())?;

        Ok(AuthenticatedUser { user_id, device_id })
    }
}

#[cfg(test)]
mod tests {
    use proptest::prelude::*;
    use uuid::Uuid;

    use crate::auth::jwt::{sign_access_token, validate_access_token_with_rotation};

    fn arb_uuid() -> impl Strategy<Value = Uuid> {
        any::<[u8; 16]>().prop_map(Uuid::from_bytes)
    }

    fn arb_secret() -> impl Strategy<Value = Vec<u8>> {
        prop::collection::vec(any::<u8>(), 32..=64)
    }

    // Feature: channel-membership, Property 1: Auth extractor sign/extract round-trip
    // Validates: Requirements 13.1
    proptest! {
        #![proptest_config(ProptestConfig::with_cases(512))]

        #[test]
        fn prop_sign_extract_roundtrip(
            user_id in arb_uuid(),
            device_id in arb_uuid(),
            secret in arb_secret(),
            ttl in 60u64..=3600,
            epoch in 0i32..=100,
        ) {
            let token = sign_access_token(&user_id, &device_id, &secret, ttl, epoch)
                .expect("signing should succeed for valid secret");

            let (extracted, _extracted_did, extracted_epoch) = validate_access_token_with_rotation(&token, &secret, None)
                .expect("validation should succeed for freshly signed token");

            prop_assert_eq!(extracted, user_id);
            prop_assert_eq!(extracted_epoch, epoch);
        }
    }

    // Feature: channel-membership, Property 2: Auth extractor rejection
    // Validates: Requirements 13.2
    proptest! {
        #![proptest_config(ProptestConfig::with_cases(512))]

        #[test]
        fn prop_wrong_secret_rejected(
            user_id in arb_uuid(),
            device_id in arb_uuid(),
            sign_secret in arb_secret(),
            wrong_secret in arb_secret(),
        ) {
            // Only test when secrets actually differ
            prop_assume!(sign_secret != wrong_secret);

            let token = sign_access_token(&user_id, &device_id, &sign_secret, 3600, 0)
                .expect("signing should succeed");

            let result = validate_access_token_with_rotation(&token, &wrong_secret, None);
            prop_assert!(result.is_err(), "wrong secret must be rejected");
        }

        #[test]
        fn prop_empty_token_rejected(
            secret in arb_secret(),
        ) {
            let result = validate_access_token_with_rotation("", &secret, None);
            prop_assert!(result.is_err(), "empty token must be rejected");
        }

        #[test]
        fn prop_garbage_token_rejected(
            garbage in "[a-zA-Z0-9._\\-]{1,200}",
            secret in arb_secret(),
        ) {
            let result = validate_access_token_with_rotation(&garbage, &secret, None);
            prop_assert!(result.is_err(), "garbage token must be rejected");
        }
    }
}
