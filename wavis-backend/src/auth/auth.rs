//! Device-authentication domain: token lifecycle, credential verification,
//! and account management operations.
//!
//! **Owns:** token issuance, revocation, rotation, refresh-token generation
//! and HMAC-peppered hashing, session-epoch enforcement, user registration,
//! device registration, account recovery, phrase rotation, claim validation,
//! and background sweep of expired/consumed tokens.
//!
//! **Does not own:** JWT signing, verification, key material, or token
//! encoding/decoding — those primitives live in `domain::jwt`. Does not own
//! HTTP request parsing or response formatting (that is
//! `handlers::auth_routes`), or direct database schema management.
//!
//! **Key invariants:**
//! - Refresh tokens are hashed with an HMAC pepper before storage — the raw
//!   token is never persisted.
//! - Reuse of a consumed refresh token triggers a session epoch bump,
//!   invalidating all access tokens for that user immediately.
//! - Access-token signing/validation delegates to `domain::jwt`, which
//!   supports zero-downtime key rotation.
//!
//! **Layering:** called by `handlers::auth_routes`. Calls into
//! `domain::jwt`, `domain::device`, `domain::phrase`, and Postgres via
//! `sqlx`.

use base64::{Engine as _, engine::general_purpose::URL_SAFE_NO_PAD};
use hmac::{Hmac, Mac};
use rand::{Rng, RngCore};
use sha2::Sha256;
use uuid::Uuid;
use zeroize::Zeroize;

use crate::auth::device;
use crate::auth::jwt::sign_access_token;
use crate::auth::phrase::{self, DummyVerifier, PhraseConfig};

type HmacSha256 = Hmac<Sha256>;

#[derive(Debug)]
pub struct DeviceRegistration {
    pub user_id: Uuid,
    pub device_id: Uuid,
    pub recovery_id: String,
    pub access_token: String,
    pub refresh_token: String,
}

#[derive(Debug)]
pub struct TokenPair {
    pub access_token: String,
    pub refresh_token: String,
    pub user_id: Uuid,
    pub device_id: Uuid,
}

#[derive(Debug, thiserror::Error)]
#[allow(dead_code)]
pub enum AuthError {
    #[error("secret must be at least 32 bytes")]
    SecretTooShort,
    #[error("token signing failed: {0}")]
    SigningFailed(String),
    #[error("token validation failed")]
    ValidationFailed,
    #[error("token expired")]
    TokenExpired,
    #[error("invalid token")]
    InvalidToken,
    #[error("refresh token invalid or expired")]
    RefreshTokenInvalid,
    #[error("token reuse detected — all tokens revoked for user")]
    TokenReuseDetected,
    #[error("session epoch mismatch — token invalidated by security event")]
    EpochMismatch,
    #[error("database error: {0}")]
    DatabaseError(String),
    #[error("phrase verification failed")]
    PhraseVerificationFailed,
    #[error("recovery ID not found")]
    RecoveryIdNotFound,
    #[error("device revoked")]
    DeviceRevoked,
}

/// Check that the token's epoch matches the current DB epoch for the user.
/// Returns Ok(()) if they match, Err(EpochMismatch) otherwise.
pub async fn check_session_epoch(
    pool: &sqlx::PgPool,
    user_id: &Uuid,
    token_epoch: i32,
) -> Result<(), AuthError> {
    let current_epoch: Option<i32> =
        sqlx::query_scalar("SELECT session_epoch FROM users WHERE user_id = $1")
            .bind(user_id)
            .fetch_optional(pool)
            .await
            .map_err(|e| AuthError::DatabaseError(e.to_string()))?;

    match current_epoch {
        Some(db_epoch) if db_epoch == token_epoch => Ok(()),
        Some(_) => Err(AuthError::EpochMismatch),
        None => Err(AuthError::InvalidToken), // user doesn't exist
    }
}

/// Generate a cryptographically random refresh token (256-bit, base64url-encoded, no padding).
pub fn generate_refresh_token() -> String {
    let mut bytes = [0u8; 32];
    rand::thread_rng().fill_bytes(&mut bytes);
    URL_SAFE_NO_PAD.encode(bytes)
}

/// Compute HMAC-SHA256 of a raw refresh token using a server-side pepper.
/// The pepper provides defense-in-depth: even if the DB is leaked, an attacker
/// cannot brute-force token hashes without also compromising the pepper.
pub fn hash_refresh_token(raw_token: &str, pepper: &[u8]) -> Vec<u8> {
    let mut mac = HmacSha256::new_from_slice(pepper).expect("HMAC-SHA256 accepts any key length");
    mac.update(raw_token.as_bytes());
    mac.finalize().into_bytes().to_vec()
}

/// Recovery ID charset: A-Z0-9 (36 chars).
const RECOVERY_ID_CHARSET: &[u8] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZ0123456789";

/// Generate a recovery ID in format `wvs-XXXX-XXXX` where X ∈ [A-Z0-9].
pub fn generate_recovery_id() -> String {
    let mut rng = rand::thread_rng();
    let chars: String = (0..8)
        .map(|_| {
            let idx = rng.gen_range(0..RECOVERY_ID_CHARSET.len());
            RECOVERY_ID_CHARSET[idx] as char
        })
        .collect();
    format!("wvs-{}-{}", &chars[..4], &chars[4..])
}

/// Register a new user with a secret phrase(password in frontend): create user → hash phrase →
/// encrypt salt+verifier → generate recovery_id → create device → issue tokens.
///
/// Returns `DeviceRegistration` with `user_id`, `device_id`, `recovery_id`,
/// `access_token`, `refresh_token`.
#[allow(clippy::too_many_arguments)]
pub async fn register_user(
    pool: &sqlx::PgPool,
    phrase: &str,
    device_name: &str,
    auth_secret: &[u8],
    access_ttl_secs: u64,
    refresh_ttl_days: u32,
    pepper: &[u8],
    phrase_config: &PhraseConfig,
    encryption_key: &[u8],
) -> Result<DeviceRegistration, AuthError> {
    // 1. Create user (session_epoch defaults to 0)
    let user_id: Uuid = sqlx::query_scalar("INSERT INTO users DEFAULT VALUES RETURNING user_id")
        .fetch_one(pool)
        .await
        .map_err(|e| AuthError::DatabaseError(e.to_string()))?;

    // 2. Hash phrase with user_id binding
    let mut phrase_copy = phrase.to_string();
    let hash = phrase::hash_phrase(&phrase_copy, &user_id, phrase_config)
        .map_err(|e| AuthError::DatabaseError(e.to_string()))?;
    // Best-effort zeroize the phrase copy
    phrase_copy.zeroize();

    // 3. Encrypt salt + verifier before DB write
    let (enc_salt, enc_verifier) =
        phrase::encrypt_phrase_data(&hash.salt, &hash.verifier, encryption_key)
            .map_err(|e| AuthError::DatabaseError(e.to_string()))?;

    // 4. Generate recovery_id with retry on unique constraint collision
    let mut recovery_id = generate_recovery_id();
    let max_retries = 5;
    for attempt in 0..=max_retries {
        let result = sqlx::query(
            "UPDATE users SET phrase_salt = $1, phrase_verifier = $2, recovery_id = $3 WHERE user_id = $4",
        )
        .bind(&enc_salt)
        .bind(&enc_verifier)
        .bind(&recovery_id)
        .bind(user_id)
        .execute(pool)
        .await;

        match result {
            Ok(_) => break,
            Err(e) => {
                // Check for unique constraint violation on recovery_id
                let err_str = e.to_string();
                if (err_str.contains("idx_users_recovery_id") || err_str.contains("23505"))
                    && attempt < max_retries
                {
                    recovery_id = generate_recovery_id();
                    continue;
                }
                return Err(AuthError::DatabaseError(e.to_string()));
            }
        }
    }

    // 5. Create device under this user
    let device_id = device::create_device(pool, user_id, device_name)
        .await
        .map_err(|e| AuthError::DatabaseError(e.to_string()))?;

    // 6. Generate refresh token, hash it, store it
    let raw_refresh = generate_refresh_token();
    let token_hash = hash_refresh_token(&raw_refresh, pepper);
    let expires_at = chrono::Utc::now() + chrono::Duration::days(refresh_ttl_days as i64);

    sqlx::query(
        "INSERT INTO refresh_tokens (refresh_id, device_id, token_hash, family_id, expires_at) \
         VALUES (gen_random_uuid(), $1, $2, gen_random_uuid(), $3)",
    )
    .bind(device_id)
    .bind(&token_hash)
    .bind(expires_at)
    .execute(pool)
    .await
    .map_err(|e| AuthError::DatabaseError(e.to_string()))?;

    // 7. Sign access token (new user starts at epoch 0)
    let access_token = sign_access_token(&user_id, &device_id, auth_secret, access_ttl_secs, 0)?;

    Ok(DeviceRegistration {
        user_id,
        device_id,
        recovery_id,
        access_token,
        refresh_token: raw_refresh,
    })
}

/// Register a new device: create user + initial refresh token.
/// Returns DeviceRegistration with user_id, access_token, refresh_token.
///
/// DEPRECATED: Use `register_user` instead. This function is kept for backward
/// compatibility with existing tests and the old `/auth/register_device` endpoint.
pub async fn register_device(
    pool: &sqlx::PgPool,
    auth_secret: &[u8],
    access_ttl_secs: u64,
    refresh_ttl_days: u32,
    pepper: &[u8],
) -> Result<DeviceRegistration, AuthError> {
    // Create user (session_epoch defaults to 0)
    let user_id: Uuid = sqlx::query_scalar("INSERT INTO users DEFAULT VALUES RETURNING user_id")
        .fetch_one(pool)
        .await
        .map_err(|e| AuthError::DatabaseError(e.to_string()))?;

    // Create a transitional device with device_id = user_id (migration artifact)
    let device_id = device::create_device(pool, user_id, "")
        .await
        .map_err(|e| AuthError::DatabaseError(e.to_string()))?;

    // Generate refresh token
    let raw_refresh = generate_refresh_token();
    let token_hash = hash_refresh_token(&raw_refresh, pepper);
    let expires_at = chrono::Utc::now() + chrono::Duration::days(refresh_ttl_days as i64);

    // Store refresh token hash (new schema: device_id FK)
    sqlx::query(
        "INSERT INTO refresh_tokens (refresh_id, device_id, token_hash, family_id, expires_at) \
         VALUES (gen_random_uuid(), $1, $2, gen_random_uuid(), $3)",
    )
    .bind(device_id)
    .bind(&token_hash)
    .bind(expires_at)
    .execute(pool)
    .await
    .map_err(|e| AuthError::DatabaseError(e.to_string()))?;

    // New user always starts at epoch 0
    let access_token = sign_access_token(&user_id, &device_id, auth_secret, access_ttl_secs, 0)?;

    Ok(DeviceRegistration {
        user_id,
        device_id,
        recovery_id: String::new(),
        access_token,
        refresh_token: raw_refresh,
    })
}

/// Recover an account using recovery_id + secret phrase(password in frontend).
///
/// Lookup user by recovery_id; if not found, verify against dummy verifier
/// then return error (timing-equalized). If found, decrypt salt+verifier,
/// verify phrase with user_id binding. On success: create new device, issue
/// tokens. On failure: return opaque 401 (indistinguishable from missing
/// recovery_id).
#[allow(clippy::too_many_arguments)]
pub async fn recover_account(
    pool: &sqlx::PgPool,
    recovery_id: &str,
    phrase: &str,
    device_name: &str,
    auth_secret: &[u8],
    access_ttl_secs: u64,
    refresh_ttl_days: u32,
    pepper: &[u8],
    phrase_config: &PhraseConfig,
    encryption_key: &[u8],
    dummy_verifier: &DummyVerifier,
) -> Result<DeviceRegistration, AuthError> {
    // 1. Lookup user by recovery_id
    #[allow(clippy::type_complexity)]
    let row: Option<(Uuid, Option<Vec<u8>>, Option<Vec<u8>>)> = sqlx::query_as(
        "SELECT user_id, phrase_salt, phrase_verifier FROM users WHERE recovery_id = $1",
    )
    .bind(recovery_id)
    .fetch_optional(pool)
    .await
    .map_err(|e| AuthError::DatabaseError(e.to_string()))?;

    let mut phrase_copy = phrase.to_string();

    let (user_id, enc_salt, enc_verifier) = match row {
        Some((uid, Some(s), Some(v))) => (uid, s, v),
        _ => {
            // Recovery ID not found (or phrase data missing) — verify against
            // dummy verifier for timing equalization, then return opaque error.
            let _ = phrase::verify_dummy(&phrase_copy, dummy_verifier, phrase_config);
            phrase_copy.zeroize();
            return Err(AuthError::RecoveryIdNotFound);
        }
    };

    // 2. Decrypt salt + verifier
    let (salt, verifier) = phrase::decrypt_phrase_data(&enc_salt, &enc_verifier, encryption_key)
        .map_err(|e| AuthError::DatabaseError(e.to_string()))?;

    // 3. Verify phrase with user_id binding
    let valid = phrase::verify_phrase(&phrase_copy, &user_id, &salt, &verifier, phrase_config)
        .map_err(|e| AuthError::DatabaseError(e.to_string()))?;

    // Best-effort zeroize the phrase copy
    phrase_copy.zeroize();

    if !valid {
        return Err(AuthError::PhraseVerificationFailed);
    }

    // 4. Create new device under this user
    let device_id = device::create_device(pool, user_id, device_name)
        .await
        .map_err(|e| AuthError::DatabaseError(e.to_string()))?;

    // 5. Generate refresh token, hash it, store it
    let raw_refresh = generate_refresh_token();
    let token_hash = hash_refresh_token(&raw_refresh, pepper);
    let expires_at = chrono::Utc::now() + chrono::Duration::days(refresh_ttl_days as i64);

    sqlx::query(
        "INSERT INTO refresh_tokens (refresh_id, device_id, token_hash, family_id, expires_at) \
         VALUES (gen_random_uuid(), $1, $2, gen_random_uuid(), $3)",
    )
    .bind(device_id)
    .bind(&token_hash)
    .bind(expires_at)
    .execute(pool)
    .await
    .map_err(|e| AuthError::DatabaseError(e.to_string()))?;

    // 6. Fetch current epoch and sign access token
    let epoch: i32 = sqlx::query_scalar("SELECT session_epoch FROM users WHERE user_id = $1")
        .bind(user_id)
        .fetch_one(pool)
        .await
        .map_err(|e| AuthError::DatabaseError(e.to_string()))?;

    let access_token =
        sign_access_token(&user_id, &device_id, auth_secret, access_ttl_secs, epoch)?;

    Ok(DeviceRegistration {
        user_id,
        device_id,
        recovery_id: recovery_id.to_string(),
        access_token,
        refresh_token: raw_refresh,
    })
}

/// Rotate the user's secret phrase(password in frontend).
///
/// Verifies `current_phrase` against stored verifier. On success: generate new
/// salt, hash `new_phrase`, encrypt, increment `phrase_version`, store.
/// Discards both phrases from memory after processing (zeroize).
pub async fn rotate_phrase(
    pool: &sqlx::PgPool,
    user_id: Uuid,
    current_phrase: &str,
    new_phrase: &str,
    phrase_config: &PhraseConfig,
    encryption_key: &[u8],
) -> Result<(), AuthError> {
    // 1. Fetch encrypted phrase data
    #[allow(clippy::type_complexity)]
    let row: Option<(Option<Vec<u8>>, Option<Vec<u8>>)> =
        sqlx::query_as("SELECT phrase_salt, phrase_verifier FROM users WHERE user_id = $1")
            .bind(user_id)
            .fetch_optional(pool)
            .await
            .map_err(|e| AuthError::DatabaseError(e.to_string()))?;

    let (enc_salt, enc_verifier) = match row {
        Some((Some(s), Some(v))) => (s, v),
        _ => return Err(AuthError::PhraseVerificationFailed),
    };

    // 2. Decrypt salt + verifier
    let (salt, verifier) = phrase::decrypt_phrase_data(&enc_salt, &enc_verifier, encryption_key)
        .map_err(|e| AuthError::DatabaseError(e.to_string()))?;

    // 3. Verify current phrase
    let mut current_copy = current_phrase.to_string();
    let valid = phrase::verify_phrase(&current_copy, &user_id, &salt, &verifier, phrase_config)
        .map_err(|e| AuthError::DatabaseError(e.to_string()))?;
    current_copy.zeroize();

    if !valid {
        return Err(AuthError::PhraseVerificationFailed);
    }

    // 4. Hash new phrase
    let mut new_copy = new_phrase.to_string();
    let new_hash = phrase::hash_phrase(&new_copy, &user_id, phrase_config)
        .map_err(|e| AuthError::DatabaseError(e.to_string()))?;
    new_copy.zeroize();

    // 5. Encrypt new salt + verifier
    let (new_enc_salt, new_enc_verifier) =
        phrase::encrypt_phrase_data(&new_hash.salt, &new_hash.verifier, encryption_key)
            .map_err(|e| AuthError::DatabaseError(e.to_string()))?;

    // 6. Store updated values, increment phrase_version
    sqlx::query(
        "UPDATE users SET phrase_salt = $1, phrase_verifier = $2, phrase_version = phrase_version + 1 \
         WHERE user_id = $3",
    )
    .bind(&new_enc_salt)
    .bind(&new_enc_verifier)
    .bind(user_id)
    .execute(pool)
    .await
    .map_err(|e| AuthError::DatabaseError(e.to_string()))?;

    Ok(())
}

/// Rotate a refresh token: consume old, issue new pair.
/// Implements reuse detection via consumed_refresh_tokens table.
/// Retries up to 3 times on serialization failure (SQLSTATE 40001).
pub async fn rotate_refresh_token(
    pool: &sqlx::PgPool,
    raw_refresh_token: &str,
    auth_secret: &[u8],
    access_ttl_secs: u64,
    refresh_ttl_days: u32,
    pepper: &[u8],
) -> Result<TokenPair, AuthError> {
    let token_hash = hash_refresh_token(raw_refresh_token, pepper);
    let max_retries = 3;

    for attempt in 0..max_retries {
        match try_rotate(
            pool,
            &token_hash,
            auth_secret,
            access_ttl_secs,
            refresh_ttl_days,
            pepper,
        )
        .await
        {
            Ok(pair) => return Ok(pair),
            Err(AuthError::DatabaseError(ref msg))
                if msg.contains("40001") && attempt < max_retries - 1 =>
            {
                // Serialization failure — retry with brief backoff
                tokio::time::sleep(std::time::Duration::from_millis(10 * (attempt as u64 + 1)))
                    .await;
                continue;
            }
            Err(e) => return Err(e),
        }
    }

    Err(AuthError::DatabaseError(
        "serialization retry exhausted".to_string(),
    ))
}

async fn try_rotate(
    pool: &sqlx::PgPool,
    token_hash: &[u8],
    auth_secret: &[u8],
    access_ttl_secs: u64,
    refresh_ttl_days: u32,
    pepper: &[u8],
) -> Result<TokenPair, AuthError> {
    let mut tx = pool
        .begin()
        .await
        .map_err(|e| AuthError::DatabaseError(e.to_string()))?;

    // Consume the token via conditional UPDATE (prevents double-consume races).
    // Returns device_id and family_id if the token was valid and unconsumed.
    let consumed: Option<(Uuid, Uuid)> = sqlx::query_as(
        "UPDATE refresh_tokens \
         SET consumed_at = now() \
         WHERE token_hash = $1 \
           AND expires_at > now() \
           AND consumed_at IS NULL \
           AND revoked_at IS NULL \
         RETURNING device_id, family_id",
    )
    .bind(token_hash)
    .fetch_optional(&mut *tx)
    .await
    .map_err(|e| AuthError::DatabaseError(e.to_string()))?;

    if let Some((device_id, family_id)) = consumed {
        // Legitimate rotation — fetch user_id and session_epoch via device
        let row: Option<(Uuid, i32)> = sqlx::query_as(
            "SELECT u.user_id, u.session_epoch \
             FROM users u \
             JOIN devices d ON d.user_id = u.user_id \
             WHERE d.device_id = $1",
        )
        .bind(device_id)
        .fetch_optional(&mut *tx)
        .await
        .map_err(|e| AuthError::DatabaseError(e.to_string()))?;

        let (user_id, epoch) = row.ok_or(AuthError::InvalidToken)?;

        // Create new refresh token in the same family
        let new_raw = generate_refresh_token();
        let new_hash = hash_refresh_token(&new_raw, pepper);
        let expires_at = chrono::Utc::now() + chrono::Duration::days(refresh_ttl_days as i64);

        sqlx::query(
            "INSERT INTO refresh_tokens (refresh_id, device_id, token_hash, family_id, expires_at) \
             VALUES (gen_random_uuid(), $1, $2, $3, $4)",
        )
        .bind(device_id)
        .bind(&new_hash)
        .bind(family_id)
        .bind(expires_at)
        .execute(&mut *tx)
        .await
        .map_err(|e| AuthError::DatabaseError(e.to_string()))?;

        tx.commit()
            .await
            .map_err(|e| AuthError::DatabaseError(e.to_string()))?;

        let access_token =
            sign_access_token(&user_id, &device_id, auth_secret, access_ttl_secs, epoch)?;

        Ok(TokenPair {
            access_token,
            refresh_token: new_raw,
            user_id,
            device_id,
        })
    } else {
        // Token not consumable — check if it was already consumed (reuse detection)
        let reuse_row: Option<(Uuid,)> = sqlx::query_as(
            "SELECT rt.device_id \
             FROM refresh_tokens rt \
             WHERE rt.token_hash = $1 \
               AND rt.consumed_at IS NOT NULL",
        )
        .bind(token_hash)
        .fetch_optional(&mut *tx)
        .await
        .map_err(|e| AuthError::DatabaseError(e.to_string()))?;

        if let Some((reuse_device_id,)) = reuse_row {
            // Reuse detected — find user_id via device, revoke all tokens, bump epoch
            let reuse_user: Option<Uuid> =
                sqlx::query_scalar("SELECT user_id FROM devices WHERE device_id = $1")
                    .bind(reuse_device_id)
                    .fetch_optional(&mut *tx)
                    .await
                    .map_err(|e| AuthError::DatabaseError(e.to_string()))?;

            if let Some(user_id) = reuse_user {
                // Revoke all refresh tokens for this user (join through devices table)
                sqlx::query(
                    "UPDATE refresh_tokens SET revoked_at = now() \
                     FROM devices \
                     WHERE refresh_tokens.device_id = devices.device_id \
                       AND devices.user_id = $1 \
                       AND refresh_tokens.revoked_at IS NULL",
                )
                .bind(user_id)
                .execute(&mut *tx)
                .await
                .map_err(|e| AuthError::DatabaseError(e.to_string()))?;

                // Bump session epoch
                sqlx::query(
                    "UPDATE users SET session_epoch = session_epoch + 1 WHERE user_id = $1",
                )
                .bind(user_id)
                .execute(&mut *tx)
                .await
                .map_err(|e| AuthError::DatabaseError(e.to_string()))?;
            }

            tx.commit()
                .await
                .map_err(|e| AuthError::DatabaseError(e.to_string()))?;

            Err(AuthError::TokenReuseDetected)
        } else {
            // Token never existed or expired beyond retention — simply invalid
            tx.commit()
                .await
                .map_err(|e| AuthError::DatabaseError(e.to_string()))?;

            Err(AuthError::RefreshTokenInvalid)
        }
    }
}

/// Delete all expired refresh tokens. Called by background sweep.
pub async fn sweep_expired_tokens(pool: &sqlx::PgPool) -> Result<u64, AuthError> {
    let result = sqlx::query("DELETE FROM refresh_tokens WHERE expires_at < now()")
        .execute(pool)
        .await
        .map_err(|e| AuthError::DatabaseError(e.to_string()))?;
    Ok(result.rows_affected())
}

/// Delete consumed or revoked refresh_tokens rows older than the configured retention period.
/// Called by background sweep.
pub async fn sweep_consumed_tokens(
    pool: &sqlx::PgPool,
    retention_hours: u64,
) -> Result<u64, AuthError> {
    let result = sqlx::query(
        "DELETE FROM refresh_tokens \
         WHERE (consumed_at IS NOT NULL AND consumed_at < now() - $1::interval) \
            OR (revoked_at IS NOT NULL AND revoked_at < now() - $1::interval)",
    )
    .bind(format!("{} hours", retention_hours))
    .execute(pool)
    .await
    .map_err(|e| AuthError::DatabaseError(e.to_string()))?;
    Ok(result.rows_affected())
}

/// Validate REFRESH_TOKEN_TTL_DAYS value. Must be 1..=365.
/// Returns Ok(()) if valid, Err with message if not.
pub fn validate_refresh_ttl(days: u32) -> Result<(), String> {
    if days == 0 {
        return Err("REFRESH_TOKEN_TTL_DAYS must be at least 1".to_string());
    }
    if days > 365 {
        return Err("REFRESH_TOKEN_TTL_DAYS must not exceed 365".to_string());
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use proptest::prelude::*;

    fn arb_pepper() -> impl Strategy<Value = Vec<u8>> {
        prop::collection::vec(any::<u8>(), 32..=64)
    }

    // Feature: device-auth, Property 6: Refresh token hash round-trip (peppered HMAC)
    // Validates: Requirements 1.2, 2.5, 4.1
    proptest! {
        #![proptest_config(ProptestConfig::with_cases(100))]
        #[test]
        fn prop_refresh_token_hash_round_trip(
            pepper in arb_pepper(),
        ) {
            let token = generate_refresh_token();
            let hash1 = hash_refresh_token(&token, &pepper);
            let hash2 = hash_refresh_token(&token, &pepper);

            // Hash is 32 bytes (HMAC-SHA256)
            prop_assert_eq!(hash1.len(), 32);
            // Deterministic
            prop_assert_eq!(&hash1, &hash2);

            // Distinct tokens produce distinct hashes
            let token2 = generate_refresh_token();
            if token != token2 {
                let hash3 = hash_refresh_token(&token2, &pepper);
                prop_assert_ne!(&hash1, &hash3);
            }

            // Different pepper produces different hash for same token
            let other_pepper = {
                let mut p = pepper.clone();
                if let Some(b) = p.first_mut() { *b ^= 0xFF; }
                p
            };
            let hash_other = hash_refresh_token(&token, &other_pepper);
            prop_assert_ne!(&hash1, &hash_other, "different pepper must produce different hash");
        }
    }

    // Feature: device-auth, Property 7: Refresh token entropy
    // Validates: Requirements 4.4
    #[test]
    fn prop_refresh_token_entropy() {
        use std::collections::HashSet;
        let mut tokens = HashSet::new();
        for _ in 0..1000 {
            let token = generate_refresh_token();
            // Decoded bytes should be at least 32 (256 bits)
            let decoded = URL_SAFE_NO_PAD.decode(&token).expect("valid base64url");
            assert!(
                decoded.len() >= 32,
                "decoded token must be at least 32 bytes"
            );
            // No duplicates
            assert!(
                tokens.insert(token),
                "duplicate token generated in batch of 1000"
            );
        }
    }

    // Feature: device-auth, Property 14: Refresh token TTL validation
    // Validates: Requirements 4.5
    proptest! {
        #![proptest_config(ProptestConfig::with_cases(100))]
        #[test]
        fn prop_refresh_ttl_validation(days in 0u32..=500) {
            let result = validate_refresh_ttl(days);
            if (1..=365).contains(&days) {
                prop_assert!(result.is_ok(), "TTL {} should be valid", days);
            } else {
                prop_assert!(result.is_err(), "TTL {} should be invalid", days);
            }
        }
    }

    // Feature: user-identity-recovery, Property 18: Recovery ID format
    // Generate many recovery_ids; verify regex `^wvs-[A-Z0-9]{4}-[A-Z0-9]{4}$`.
    // **Validates: Requirements 4.5**
    proptest! {
        #![proptest_config(ProptestConfig::with_cases(200))]
        #[test]
        fn prop_recovery_id_format(_seed in any::<u64>()) {
            let rid = generate_recovery_id();

            // Must match the format wvs-XXXX-XXXX where X ∈ [A-Z0-9]
            prop_assert_eq!(rid.len(), 13, "recovery_id must be exactly 13 chars: got '{}'", rid);
            prop_assert!(rid.starts_with("wvs-"), "recovery_id must start with 'wvs-': got '{}'", rid);

            let parts: Vec<&str> = rid.split('-').collect();
            prop_assert_eq!(parts.len(), 3, "recovery_id must have 3 dash-separated parts: got '{}'", rid);
            prop_assert_eq!(parts[0], "wvs");
            prop_assert_eq!(parts[1].len(), 4, "first segment must be 4 chars");
            prop_assert_eq!(parts[2].len(), 4, "second segment must be 4 chars");

            for ch in parts[1].chars().chain(parts[2].chars()) {
                prop_assert!(
                    ch.is_ascii_uppercase() || ch.is_ascii_digit(),
                    "character '{}' is not in [A-Z0-9]",
                    ch
                );
            }
        }
    }
}
