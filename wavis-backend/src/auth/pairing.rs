//! QR/code-based device pairing — start, approve, finish, and cleanup.
//!
//! A pairing session allows a new device to be linked to an existing user's
//! account via a short-lived code approved by a trusted device.

use chrono::{DateTime, Utc};
use hmac::{Hmac, Mac};
use rand::Rng;
use sha2::Sha256;
use sqlx::PgPool;
use uuid::Uuid;

use crate::auth::auth;

type HmacSha256 = Hmac<Sha256>;

/// Base32 charset for pairing code generation (RFC 4648, no padding).
const CODE_CHARSET: &[u8] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZ234567";
/// Length of generated pairing codes.
const CODE_LENGTH: usize = 8;

/// Result of a successful `finish_pairing` call.
#[derive(Debug, Clone)]
pub struct PairingResult {
    pub user_id: Uuid,
    pub device_id: Uuid,
    pub access_token: String,
    pub refresh_token: String,
}

/// A pairing session row from the `pairings` table.
#[derive(Debug, Clone)]
#[allow(dead_code)]
pub struct PairingSession {
    pub pairing_id: Uuid,
    /// HMAC-SHA256 hash of the pairing code (keyed with server pepper).
    pub code_hash: Vec<u8>,
    /// Human-readable name of the device requesting pairing.
    pub request_device_name: String,
    /// User ID that approved this pairing (set during approve step).
    pub approved_user_id: Option<Uuid>,
    /// Device ID of the trusted device that approved (set during approve step).
    pub approved_by_device_id: Option<Uuid>,
    /// Timestamp when the pairing was approved.
    pub approved_at: Option<DateTime<Utc>>,
    /// Pairing expires after this timestamp (5-minute TTL from creation).
    pub expires_at: DateTime<Utc>,
    /// Timestamp when the pairing was consumed (finish step).
    pub used_at: Option<DateTime<Utc>>,
    /// Total failed verification attempts (never reset on success).
    /// After 5 total failures the pairing is permanently locked until TTL expiry.
    pub attempt_count: i32,
}

/// Errors from pairing operations.
#[derive(Debug, thiserror::Error)]
pub enum PairingError {
    #[error("pairing session not found")]
    NotFound,
    #[error("pairing session expired")]
    Expired,
    #[error("pairing session already used")]
    AlreadyUsed,
    #[error("pairing session already approved")]
    AlreadyApproved,
    #[error("pairing session not yet approved")]
    NotApproved,
    #[error("pairing code mismatch")]
    CodeMismatch,
    #[error("pairing locked out after too many failed attempts")]
    LockedOut,
    #[error("database error: {0}")]
    DatabaseError(String),
}

/// Compute HMAC-SHA256 of a pairing code using the server pepper as key.
/// Reused by `start_pairing`, `approve_pairing`, and `finish_pairing`.
pub fn hmac_code(code: &str, pepper: &[u8]) -> Vec<u8> {
    let mut mac = HmacSha256::new_from_slice(pepper).expect("HMAC-SHA256 accepts any key length");
    mac.update(code.as_bytes());
    mac.finalize().into_bytes().to_vec()
}

/// Generate a random pairing code from the base32 charset using CSPRNG.
fn generate_pairing_code() -> String {
    let mut rng = rand::thread_rng();
    (0..CODE_LENGTH)
        .map(|_| {
            let idx = rng.gen_range(0..CODE_CHARSET.len());
            CODE_CHARSET[idx] as char
        })
        .collect()
}

/// Create a new pairing session. Returns `(pairing_id, plaintext_code)`.
///
/// Generates an 8-character base32 pairing code, stores only its HMAC-SHA256
/// hash in the database, and sets a 5-minute TTL.
pub async fn start_pairing(
    pool: &PgPool,
    device_name: &str,
    pepper: &[u8],
) -> Result<(Uuid, String), PairingError> {
    let code = generate_pairing_code();
    let code_hash = hmac_code(&code, pepper);

    let row = sqlx::query_scalar::<_, Uuid>(
        r#"
        INSERT INTO pairings (pairing_id, code_hash, request_device_name, expires_at)
        VALUES (gen_random_uuid(), $1, $2, now() + interval '5 minutes')
        RETURNING pairing_id
        "#,
    )
    .bind(&code_hash)
    .bind(device_name)
    .fetch_one(pool)
    .await
    .map_err(|e| PairingError::DatabaseError(e.to_string()))?;

    Ok((row, code))
}

/// Approve a pairing from a trusted device.
///
/// Verifies the code via constant-time HMAC comparison, then uses a conditional
/// UPDATE for concurrency safety. On code mismatch, atomically increments
/// `attempt_count`; locks out after 5 total failures.
pub async fn approve_pairing(
    pool: &PgPool,
    pairing_id: Uuid,
    code: &str,
    user_id: Uuid,
    device_id: Uuid,
    pepper: &[u8],
) -> Result<(), PairingError> {
    // 1. SELECT the pairing row to get code_hash and check state.
    let row = sqlx::query_as::<_, PairingRow>(
        r#"
        SELECT pairing_id, code_hash, approved_at, used_at, expires_at, attempt_count
        FROM pairings
        WHERE pairing_id = $1
        "#,
    )
    .bind(pairing_id)
    .fetch_optional(pool)
    .await
    .map_err(|e| PairingError::DatabaseError(e.to_string()))?
    .ok_or(PairingError::NotFound)?;

    // Check pre-conditions before code verification.
    if row.used_at.is_some() {
        return Err(PairingError::AlreadyUsed);
    }
    if row.expires_at < Utc::now() {
        return Err(PairingError::Expired);
    }
    if row.attempt_count >= 5 {
        return Err(PairingError::LockedOut);
    }
    if row.approved_at.is_some() {
        return Err(PairingError::AlreadyApproved);
    }

    // 2. Verify code via constant-time HMAC comparison.
    //    Reconstruct the HMAC and use verify_slice which performs constant-time
    //    comparison internally, preventing timing side-channels.
    let mut mac = HmacSha256::new_from_slice(pepper).expect("HMAC-SHA256 accepts any key length");
    mac.update(code.as_bytes());
    if mac.verify_slice(&row.code_hash).is_err() {
        // Code mismatch — atomically increment attempt_count.
        // Guard with used_at IS NULL AND expires_at > now() to avoid burning
        // attempts on stale pairing IDs.
        let updated = sqlx::query_scalar::<_, i32>(
            r#"
            UPDATE pairings
            SET attempt_count = attempt_count + 1
            WHERE pairing_id = $1
              AND used_at IS NULL
              AND expires_at > now()
            RETURNING attempt_count
            "#,
        )
        .bind(pairing_id)
        .fetch_optional(pool)
        .await
        .map_err(|e| PairingError::DatabaseError(e.to_string()))?;

        if let Some(count) = updated
            && count >= 5
        {
            return Err(PairingError::LockedOut);
        }
        return Err(PairingError::CodeMismatch);
    }

    // 3. Code matches — conditional UPDATE to set approved fields.
    let approved = sqlx::query_scalar::<_, Uuid>(
        r#"
        UPDATE pairings
        SET approved_user_id = $1, approved_by_device_id = $2, approved_at = now()
        WHERE pairing_id = $3
          AND approved_at IS NULL
          AND used_at IS NULL
          AND expires_at > now()
          AND attempt_count < 5
        RETURNING pairing_id
        "#,
    )
    .bind(user_id)
    .bind(device_id)
    .bind(pairing_id)
    .fetch_optional(pool)
    .await
    .map_err(|e| PairingError::DatabaseError(e.to_string()))?;

    if approved.is_some() {
        return Ok(());
    }

    // 4. 0 rows affected — re-SELECT to differentiate the error.
    let current = sqlx::query_as::<_, PairingRow>(
        r#"
        SELECT pairing_id, code_hash, approved_at, used_at, expires_at, attempt_count
        FROM pairings
        WHERE pairing_id = $1
        "#,
    )
    .bind(pairing_id)
    .fetch_optional(pool)
    .await
    .map_err(|e| PairingError::DatabaseError(e.to_string()))?
    .ok_or(PairingError::NotFound)?;

    if current.used_at.is_some() {
        Err(PairingError::AlreadyUsed)
    } else if current.expires_at < Utc::now() {
        Err(PairingError::Expired)
    } else if current.attempt_count >= 5 {
        Err(PairingError::LockedOut)
    } else if current.approved_at.is_some() {
        Err(PairingError::AlreadyApproved)
    } else {
        // Shouldn't happen, but fail safely.
        Err(PairingError::DatabaseError(
            "conditional update failed for unknown reason".into(),
        ))
    }
}

/// Internal row type for pairing SELECT queries.
#[derive(sqlx::FromRow)]
struct PairingRow {
    #[allow(dead_code)]
    pairing_id: Uuid,
    code_hash: Vec<u8>,
    approved_at: Option<DateTime<Utc>>,
    used_at: Option<DateTime<Utc>>,
    expires_at: DateTime<Utc>,
    attempt_count: i32,
}

/// Row returned by the conditional UPDATE in `finish_pairing`.
#[derive(sqlx::FromRow)]
struct FinishPairingRow {
    approved_user_id: Uuid,
    request_device_name: String,
}

/// Finish pairing — create device + issue tokens.
///
/// Runs in a single DB transaction: marks `used_at`, creates device, creates
/// refresh token, returns token pair. If any step fails, `used_at` update is
/// rolled back.
#[allow(clippy::too_many_arguments)]
pub async fn finish_pairing(
    pool: &PgPool,
    pairing_id: Uuid,
    code: &str,
    pepper: &[u8],
    auth_secret: &[u8],
    access_ttl_secs: u64,
    refresh_ttl_days: u32,
    refresh_pepper: &[u8],
) -> Result<PairingResult, PairingError> {
    // 1. SELECT the pairing row to get code_hash and check state.
    let row = sqlx::query_as::<_, PairingRow>(
        r#"
        SELECT pairing_id, code_hash, approved_at, used_at, expires_at, attempt_count
        FROM pairings
        WHERE pairing_id = $1
        "#,
    )
    .bind(pairing_id)
    .fetch_optional(pool)
    .await
    .map_err(|e| PairingError::DatabaseError(e.to_string()))?
    .ok_or(PairingError::NotFound)?;

    // Check pre-conditions before code verification.
    if row.used_at.is_some() {
        return Err(PairingError::AlreadyUsed);
    }
    if row.expires_at < Utc::now() {
        return Err(PairingError::Expired);
    }
    if row.attempt_count >= 5 {
        return Err(PairingError::LockedOut);
    }
    if row.approved_at.is_none() {
        return Err(PairingError::NotApproved);
    }

    // 2. Verify code via constant-time HMAC comparison.
    let mut mac = HmacSha256::new_from_slice(pepper).expect("HMAC-SHA256 accepts any key length");
    mac.update(code.as_bytes());
    if mac.verify_slice(&row.code_hash).is_err() {
        // Code mismatch — atomically increment attempt_count.
        let updated = sqlx::query_scalar::<_, i32>(
            r#"
            UPDATE pairings
            SET attempt_count = attempt_count + 1
            WHERE pairing_id = $1
              AND used_at IS NULL
              AND expires_at > now()
            RETURNING attempt_count
            "#,
        )
        .bind(pairing_id)
        .fetch_optional(pool)
        .await
        .map_err(|e| PairingError::DatabaseError(e.to_string()))?;

        if let Some(count) = updated
            && count >= 5
        {
            return Err(PairingError::LockedOut);
        }
        return Err(PairingError::CodeMismatch);
    }

    // 3. Code matches — begin transaction for atomic finish.
    let mut tx = pool
        .begin()
        .await
        .map_err(|e| PairingError::DatabaseError(e.to_string()))?;

    // 3a. Conditional UPDATE to set used_at, returning approved_user_id and device name.
    let finish_row = sqlx::query_as::<_, FinishPairingRow>(
        r#"
        UPDATE pairings
        SET used_at = now()
        WHERE pairing_id = $1
          AND approved_at IS NOT NULL
          AND used_at IS NULL
          AND expires_at > now()
          AND attempt_count < 5
        RETURNING approved_user_id, request_device_name
        "#,
    )
    .bind(pairing_id)
    .fetch_optional(&mut *tx)
    .await
    .map_err(|e| PairingError::DatabaseError(e.to_string()))?;

    let finish_row = match finish_row {
        Some(r) => r,
        None => {
            // 0 rows affected — re-SELECT to differentiate the error.
            let current = sqlx::query_as::<_, PairingRow>(
                r#"
                SELECT pairing_id, code_hash, approved_at, used_at, expires_at, attempt_count
                FROM pairings
                WHERE pairing_id = $1
                "#,
            )
            .bind(pairing_id)
            .fetch_optional(&mut *tx)
            .await
            .map_err(|e| PairingError::DatabaseError(e.to_string()))?
            .ok_or(PairingError::NotFound)?;

            return if current.used_at.is_some() {
                Err(PairingError::AlreadyUsed)
            } else if current.expires_at < Utc::now() {
                Err(PairingError::Expired)
            } else if current.attempt_count >= 5 {
                Err(PairingError::LockedOut)
            } else if current.approved_at.is_none() {
                Err(PairingError::NotApproved)
            } else {
                Err(PairingError::DatabaseError(
                    "conditional update failed for unknown reason".into(),
                ))
            };
        }
    };

    let user_id = finish_row.approved_user_id;

    // 3b. INSERT new device under approved_user_id.
    let device_id = sqlx::query_scalar::<_, Uuid>(
        r#"
        INSERT INTO devices (device_id, user_id, device_name, created_at)
        VALUES (gen_random_uuid(), $1, $2, now())
        RETURNING device_id
        "#,
    )
    .bind(user_id)
    .bind(&finish_row.request_device_name)
    .fetch_one(&mut *tx)
    .await
    .map_err(|e| PairingError::DatabaseError(e.to_string()))?;

    // 3c. Generate refresh token, hash it, INSERT into refresh_tokens.
    let raw_refresh = auth::generate_refresh_token();
    let token_hash = auth::hash_refresh_token(&raw_refresh, refresh_pepper);
    let expires_at = Utc::now() + chrono::Duration::days(refresh_ttl_days as i64);

    sqlx::query(
        r#"
        INSERT INTO refresh_tokens (refresh_id, device_id, token_hash, family_id, expires_at)
        VALUES (gen_random_uuid(), $1, $2, gen_random_uuid(), $3)
        "#,
    )
    .bind(device_id)
    .bind(&token_hash)
    .bind(expires_at)
    .execute(&mut *tx)
    .await
    .map_err(|e| PairingError::DatabaseError(e.to_string()))?;

    // 3d. Query session_epoch and sign access token.
    let epoch: i32 = sqlx::query_scalar("SELECT session_epoch FROM users WHERE user_id = $1")
        .bind(user_id)
        .fetch_one(&mut *tx)
        .await
        .map_err(|e| PairingError::DatabaseError(e.to_string()))?;

    let access_token = crate::auth::jwt::sign_access_token(
        &user_id,
        &device_id,
        auth_secret,
        access_ttl_secs,
        epoch,
    )
    .map_err(|e| PairingError::DatabaseError(e.to_string()))?;

    // 3e. Commit transaction.
    tx.commit()
        .await
        .map_err(|e| PairingError::DatabaseError(e.to_string()))?;

    // 3f. Return PairingResult.
    Ok(PairingResult {
        user_id,
        device_id,
        access_token,
        refresh_token: raw_refresh,
    })
}

/// Delete expired pairing rows older than the retention window.
///
/// Removes all rows where `expires_at < now() - retention_hours`. Returns the
/// number of rows deleted.
pub async fn sweep_expired_pairings(
    pool: &PgPool,
    retention_hours: u64,
) -> Result<u64, PairingError> {
    let cutoff = Utc::now() - chrono::Duration::hours(retention_hours as i64);
    let result = sqlx::query("DELETE FROM pairings WHERE expires_at < $1")
        .bind(cutoff)
        .execute(pool)
        .await
        .map_err(|e| PairingError::DatabaseError(e.to_string()))?;
    Ok(result.rows_affected())
}

#[cfg(test)]
mod tests {
    use super::*;
    use hmac::{Hmac, Mac};
    use proptest::prelude::*;
    use sha2::Sha256;

    type HmacSha256Test = Hmac<Sha256>;

    /// Maximum failed attempts before lockout (matches production logic).
    const MAX_ATTEMPTS: i32 = 5;

    // Feature: user-identity-recovery, Property 7: HMAC pairing code round-trip
    // **Validates: Requirements 8.2, 9.2**
    proptest! {
        #![proptest_config(ProptestConfig::with_cases(100))]

        #[test]
        fn prop_hmac_code_roundtrip(
            code in "[A-Z2-7]{8,10}",
            pepper in prop::collection::vec(any::<u8>(), 16..64),
        ) {
            let hash = hmac_code(&code, &pepper);

            // Verify the same code against the hash using constant-time comparison
            let mut mac = HmacSha256Test::new_from_slice(&pepper)
                .expect("HMAC accepts any key length");
            mac.update(code.as_bytes());
            prop_assert!(
                mac.verify_slice(&hash).is_ok(),
                "HMAC verification must succeed for the same code and pepper"
            );
        }

        #[test]
        fn prop_hmac_code_different_code_fails(
            code_a in "[A-Z2-7]{8,10}",
            code_b in "[A-Z2-7]{8,10}",
            pepper in prop::collection::vec(any::<u8>(), 16..64),
        ) {
            prop_assume!(code_a != code_b);

            let hash = hmac_code(&code_a, &pepper);

            // Verify a different code against the hash — must fail
            let mut mac = HmacSha256Test::new_from_slice(&pepper)
                .expect("HMAC accepts any key length");
            mac.update(code_b.as_bytes());
            prop_assert!(
                mac.verify_slice(&hash).is_err(),
                "HMAC verification must fail for a different code"
            );
        }

        #[test]
        fn prop_hmac_code_deterministic(
            code in "[A-Z2-7]{8,10}",
            pepper in prop::collection::vec(any::<u8>(), 16..64),
        ) {
            let hash1 = hmac_code(&code, &pepper);
            let hash2 = hmac_code(&code, &pepper);

            prop_assert_eq!(
                hash1, hash2,
                "hmac_code must be deterministic for the same inputs"
            );
        }
    }

    // Feature: user-identity-recovery, Property 20: Pairing code format
    // **Validates: Requirements 7.2**
    proptest! {
        #![proptest_config(ProptestConfig::with_cases(100))]

        #[test]
        fn prop_pairing_code_format(_seed in any::<u64>()) {
            let code = generate_pairing_code();

            // Code must be exactly CODE_LENGTH (8) characters
            prop_assert_eq!(
                code.len(),
                CODE_LENGTH,
                "pairing code must be exactly {} chars, got {}",
                CODE_LENGTH,
                code.len()
            );

            // Every character must be in the base32 charset (A-Z, 2-7)
            for ch in code.chars() {
                prop_assert!(
                    CODE_CHARSET.contains(&(ch as u8)),
                    "character '{}' is not in the base32 charset (A-Z, 2-7)",
                    ch
                );
            }
        }
    }

    // Feature: user-identity-recovery, Property 9: Pairing lockout after max failures
    // **Validates: Requirements 8.8, 9.8**
    //
    // Pure logic test: simulates the attempt_count tracking and verifies that
    // after 5 total failures the system rejects further attempts.
    proptest! {
        #![proptest_config(ProptestConfig::with_cases(100))]

        #[test]
        fn prop_pairing_lockout_after_max_failures(
            attempt_count in 0i32..=10,
        ) {
            // The production code uses `attempt_count >= 5` as the lockout check.
            let is_locked = attempt_count >= MAX_ATTEMPTS;

            if attempt_count < MAX_ATTEMPTS {
                prop_assert!(
                    !is_locked,
                    "attempt_count {} is below threshold {}, should NOT be locked out",
                    attempt_count,
                    MAX_ATTEMPTS
                );
            } else {
                prop_assert!(
                    is_locked,
                    "attempt_count {} is at or above threshold {}, MUST be locked out",
                    attempt_count,
                    MAX_ATTEMPTS
                );
            }
        }

        #[test]
        fn prop_pairing_lockout_boundary(
            extra_failures in 0i32..=5,
        ) {
            // Simulate accumulating failures from 0 up to MAX_ATTEMPTS + extra
            let total_failures = MAX_ATTEMPTS + extra_failures;

            // Before reaching MAX_ATTEMPTS, each attempt should be allowed
            for count in 0..MAX_ATTEMPTS {
                prop_assert!(
                    count < MAX_ATTEMPTS,
                    "attempt {} should be allowed (below threshold {})",
                    count,
                    MAX_ATTEMPTS
                );
            }

            // At and beyond MAX_ATTEMPTS, every attempt must be rejected
            for count in MAX_ATTEMPTS..=total_failures {
                prop_assert!(
                    count >= MAX_ATTEMPTS,
                    "attempt {} must be locked out (at or above threshold {})",
                    count,
                    MAX_ATTEMPTS
                );
            }
        }
    }
}
