//! Closed-alpha invite code normalization, hashing, and redemption.
//!
//! Raw invite codes are never persisted. The database stores only HMAC-SHA256
//! hashes computed with a dedicated server-side pepper.

use chrono::{DateTime, Utc};
use hmac::{Hmac, Mac};
use rand::RngCore;
use sha2::Sha256;
use sqlx::{PgPool, Postgres, Transaction};
use uuid::Uuid;

use crate::redaction::Sensitive;

type HmacSha256 = Hmac<Sha256>;
type AlphaInviteRow = (
    Uuid,
    Option<chrono::DateTime<chrono::Utc>>,
    Option<chrono::DateTime<chrono::Utc>>,
    i32,
    i32,
);

const HASH_DOMAIN: &[u8] = b"alpha-invite:v1:";
const GENERATED_CODE_BYTES: usize = 16;
const MAX_LABEL_CHARS: usize = 100;

#[allow(dead_code)]
type AlphaInviteAdminRow = (
    Uuid,
    Option<String>,
    DateTime<Utc>,
    Option<DateTime<Utc>>,
    Option<DateTime<Utc>>,
    i32,
    i32,
);

#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum AlphaInviteError {
    #[error("invite code invalid")]
    Invalid,
    #[error("database error: {0}")]
    DatabaseError(String),
}

#[allow(dead_code)]
#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum AlphaInviteAdminError {
    #[error("invite label must contain 1 to 100 characters and no control characters")]
    InvalidLabel,
    #[error("invite expiry must be in the future")]
    InvalidExpiry,
    #[error("maximum redemptions must be positive")]
    InvalidMaxRedemptions,
    #[error("invite pepper must be at least 32 bytes")]
    PepperTooShort,
    #[error("generated invite code could not be hashed")]
    CodeGenerationFailed,
    #[error("invite not found")]
    NotFound,
    #[error("database operation failed: {0}")]
    DatabaseError(String),
}

#[allow(dead_code)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AlphaInviteStatus {
    Active,
    Disabled,
    Expired,
    Exhausted,
}

#[allow(dead_code)]
impl AlphaInviteStatus {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Active => "active",
            Self::Disabled => "disabled",
            Self::Expired => "expired",
            Self::Exhausted => "exhausted",
        }
    }
}

#[allow(dead_code)]
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AlphaInviteAdminRecord {
    pub invite_id: Uuid,
    pub label: Option<String>,
    pub created_at: DateTime<Utc>,
    pub expires_at: Option<DateTime<Utc>>,
    pub disabled_at: Option<DateTime<Utc>>,
    pub max_redemptions: i32,
    pub redemption_count: i32,
}

#[allow(dead_code)]
impl AlphaInviteAdminRecord {
    pub fn status_at(&self, now: DateTime<Utc>) -> AlphaInviteStatus {
        if self.disabled_at.is_some() {
            AlphaInviteStatus::Disabled
        } else if self.expires_at.is_some_and(|expires_at| now >= expires_at) {
            AlphaInviteStatus::Expired
        } else if self.redemption_count >= self.max_redemptions {
            AlphaInviteStatus::Exhausted
        } else {
            AlphaInviteStatus::Active
        }
    }

    pub fn remaining_redemptions(&self) -> i32 {
        self.max_redemptions.saturating_sub(self.redemption_count)
    }
}

#[allow(dead_code)]
#[derive(Debug)]
pub struct CreatedAlphaInvite {
    pub invite: AlphaInviteAdminRecord,
    pub raw_code: Sensitive<String>,
}

#[allow(dead_code)]
fn record_from_row(row: AlphaInviteAdminRow) -> AlphaInviteAdminRecord {
    AlphaInviteAdminRecord {
        invite_id: row.0,
        label: row.1,
        created_at: row.2,
        expires_at: row.3,
        disabled_at: row.4,
        max_redemptions: row.5,
        redemption_count: row.6,
    }
}

pub fn validate_alpha_invite_label(label: &str) -> Result<String, AlphaInviteAdminError> {
    let label = label.trim();
    let length = label.chars().count();
    if length == 0 || length > MAX_LABEL_CHARS || label.chars().any(char::is_control) {
        return Err(AlphaInviteAdminError::InvalidLabel);
    }
    Ok(label.to_string())
}

/// Generate a normalization-safe invite code containing 128 bits of entropy.
fn generate_raw_alpha_invite_code() -> String {
    let mut bytes = [0u8; GENERATED_CODE_BYTES];
    rand::thread_rng().fill_bytes(&mut bytes);
    let encoded = hex::encode_upper(bytes);
    let mut grouped = String::with_capacity(encoded.len() + encoded.len() / 4 - 1);
    for (index, character) in encoded.chars().enumerate() {
        if index > 0 && index % 4 == 0 {
            grouped.push('-');
        }
        grouped.push(character);
    }
    grouped
}

fn validate_create_options(
    label: &str,
    expires_at: Option<DateTime<Utc>>,
    max_redemptions: i32,
    pepper_length: usize,
    now: DateTime<Utc>,
) -> Result<String, AlphaInviteAdminError> {
    let label = validate_alpha_invite_label(label)?;
    if max_redemptions <= 0 {
        return Err(AlphaInviteAdminError::InvalidMaxRedemptions);
    }
    if expires_at.is_some_and(|expiry| expiry <= now) {
        return Err(AlphaInviteAdminError::InvalidExpiry);
    }
    if pepper_length < 32 {
        return Err(AlphaInviteAdminError::PepperTooShort);
    }
    Ok(label)
}

#[allow(dead_code)]
pub async fn create_alpha_invite(
    pool: &PgPool,
    label: &str,
    expires_at: Option<DateTime<Utc>>,
    max_redemptions: i32,
    pepper: &Sensitive<Vec<u8>>,
) -> Result<CreatedAlphaInvite, AlphaInviteAdminError> {
    let label = validate_create_options(
        label,
        expires_at,
        max_redemptions,
        pepper.inner().len(),
        Utc::now(),
    )?;

    let raw_code = generate_raw_alpha_invite_code();
    let code_hash = hash_invite_code(&raw_code, pepper.inner())
        .map_err(|_| AlphaInviteAdminError::CodeGenerationFailed)?;
    let row: AlphaInviteAdminRow = sqlx::query_as(
        "INSERT INTO alpha_invites (code_hash, label, expires_at, max_redemptions) \
         VALUES ($1, $2, $3, $4) \
         RETURNING invite_id, label, created_at, expires_at, disabled_at, \
                   max_redemptions, redemption_count",
    )
    .bind(code_hash)
    .bind(label)
    .bind(expires_at)
    .bind(max_redemptions)
    .fetch_one(pool)
    .await
    .map_err(|error| AlphaInviteAdminError::DatabaseError(error.to_string()))?;

    Ok(CreatedAlphaInvite {
        invite: record_from_row(row),
        raw_code: Sensitive(raw_code),
    })
}

#[allow(dead_code)]
pub async fn list_alpha_invites(
    pool: &PgPool,
    label: Option<&str>,
) -> Result<Vec<AlphaInviteAdminRecord>, AlphaInviteAdminError> {
    let rows: Vec<AlphaInviteAdminRow> = if let Some(label) = label {
        let label = validate_alpha_invite_label(label)?;
        sqlx::query_as(
            "SELECT invite_id, label, created_at, expires_at, disabled_at, \
                    max_redemptions, redemption_count \
             FROM alpha_invites WHERE label = $1 \
             ORDER BY created_at DESC, invite_id DESC",
        )
        .bind(label)
        .fetch_all(pool)
        .await
    } else {
        sqlx::query_as(
            "SELECT invite_id, label, created_at, expires_at, disabled_at, \
                    max_redemptions, redemption_count \
             FROM alpha_invites ORDER BY created_at DESC, invite_id DESC",
        )
        .fetch_all(pool)
        .await
    }
    .map_err(|error| AlphaInviteAdminError::DatabaseError(error.to_string()))?;

    Ok(rows.into_iter().map(record_from_row).collect())
}

#[allow(dead_code)]
pub async fn disable_alpha_invite(
    pool: &PgPool,
    invite_id: Uuid,
) -> Result<AlphaInviteAdminRecord, AlphaInviteAdminError> {
    let row: Option<AlphaInviteAdminRow> = sqlx::query_as(
        "UPDATE alpha_invites SET disabled_at = COALESCE(disabled_at, now()) \
         WHERE invite_id = $1 \
         RETURNING invite_id, label, created_at, expires_at, disabled_at, \
                   max_redemptions, redemption_count",
    )
    .bind(invite_id)
    .fetch_optional(pool)
    .await
    .map_err(|error| AlphaInviteAdminError::DatabaseError(error.to_string()))?;

    row.map(record_from_row)
        .ok_or(AlphaInviteAdminError::NotFound)
}

pub fn normalize_invite_code(code: &str) -> Result<String, AlphaInviteError> {
    let normalized: String = code
        .trim()
        .chars()
        .filter(|c| !c.is_ascii_whitespace() && *c != '-')
        .map(|c| c.to_ascii_uppercase())
        .collect();

    if normalized.is_empty() {
        return Err(AlphaInviteError::Invalid);
    }

    Ok(normalized)
}

pub fn hash_invite_code(code: &str, pepper: &[u8]) -> Result<Vec<u8>, AlphaInviteError> {
    let normalized = normalize_invite_code(code)?;
    let mut mac = HmacSha256::new_from_slice(pepper).expect("HMAC-SHA256 accepts any key length");
    mac.update(HASH_DOMAIN);
    mac.update(normalized.as_bytes());
    Ok(mac.finalize().into_bytes().to_vec())
}

pub async fn redeem_alpha_invite(
    tx: &mut Transaction<'_, Postgres>,
    code: &str,
    pepper: &[u8],
) -> Result<Uuid, AlphaInviteError> {
    let code_hash = hash_invite_code(code, pepper)?;

    let row: Option<AlphaInviteRow> = sqlx::query_as(
        "SELECT invite_id, expires_at, disabled_at, max_redemptions, redemption_count \
             FROM alpha_invites \
             WHERE code_hash = $1 \
             FOR UPDATE",
    )
    .bind(&code_hash)
    .fetch_optional(&mut **tx)
    .await
    .map_err(|e| AlphaInviteError::DatabaseError(e.to_string()))?;

    let Some((invite_id, expires_at, disabled_at, max_redemptions, redemption_count)) = row else {
        return Err(AlphaInviteError::Invalid);
    };

    let now = chrono::Utc::now();
    if disabled_at.is_some()
        || expires_at.is_some_and(|expires_at| now >= expires_at)
        || redemption_count >= max_redemptions
    {
        return Err(AlphaInviteError::Invalid);
    }

    sqlx::query(
        "UPDATE alpha_invites \
         SET redemption_count = redemption_count + 1 \
         WHERE invite_id = $1",
    )
    .bind(invite_id)
    .execute(&mut **tx)
    .await
    .map_err(|e| AlphaInviteError::DatabaseError(e.to_string()))?;

    Ok(invite_id)
}

pub async fn record_redemption(
    tx: &mut Transaction<'_, Postgres>,
    invite_id: Uuid,
    user_id: Uuid,
    device_id: Uuid,
) -> Result<(), AlphaInviteError> {
    sqlx::query(
        "INSERT INTO alpha_invite_redemptions (invite_id, user_id, device_id) \
         VALUES ($1, $2, $3)",
    )
    .bind(invite_id)
    .bind(user_id)
    .bind(device_id)
    .execute(&mut **tx)
    .await
    .map_err(|e| AlphaInviteError::DatabaseError(e.to_string()))?;

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    const PEPPER: &[u8] = b"test-alpha-invite-pepper-32b!!!!";

    #[test]
    fn normalize_invite_code_removes_spacing_hyphens_and_uppercases() {
        assert_eq!(
            normalize_invite_code(" abcd-1234 ef ").unwrap(),
            "ABCD1234EF"
        );
    }

    #[test]
    fn normalize_invite_code_rejects_empty_values() {
        assert_eq!(
            normalize_invite_code(" - \t\n ").unwrap_err(),
            AlphaInviteError::Invalid
        );
    }

    #[test]
    fn hash_invite_code_is_stable_for_equivalent_codes() {
        let a = hash_invite_code("alpha-code-1", PEPPER).unwrap();
        let b = hash_invite_code(" ALPHA CODE 1 ", PEPPER).unwrap();
        assert_eq!(a, b);
    }

    #[test]
    fn hash_invite_code_differs_for_different_codes() {
        let a = hash_invite_code("alpha-code-1", PEPPER).unwrap();
        let b = hash_invite_code("alpha-code-2", PEPPER).unwrap();
        assert_ne!(a, b);
    }

    #[test]
    fn generated_codes_preserve_128_bits_through_normalization() {
        let code = Sensitive(generate_raw_alpha_invite_code());
        let normalized = normalize_invite_code(code.inner()).unwrap();

        assert_eq!(normalized.len(), GENERATED_CODE_BYTES * 2);
        assert!(
            normalized
                .chars()
                .all(|character| character.is_ascii_hexdigit())
        );
        assert!(
            normalized
                .chars()
                .all(|character| !character.is_ascii_lowercase())
        );
        assert_eq!(code.inner().matches('-').count(), 7);
    }

    #[test]
    fn invite_labels_are_trimmed_and_bounded() {
        assert_eq!(
            validate_alpha_invite_label("  person@example.com  ").unwrap(),
            "person@example.com"
        );
        assert_eq!(
            validate_alpha_invite_label(" \t\n ").unwrap_err(),
            AlphaInviteAdminError::InvalidLabel
        );
        assert_eq!(
            validate_alpha_invite_label(&"a".repeat(MAX_LABEL_CHARS + 1)).unwrap_err(),
            AlphaInviteAdminError::InvalidLabel
        );
        assert_eq!(
            validate_alpha_invite_label("line\nbreak").unwrap_err(),
            AlphaInviteAdminError::InvalidLabel
        );
    }

    #[test]
    fn creation_options_reject_unsafe_values() {
        let now = Utc::now();
        assert_eq!(
            validate_create_options("person", None, 0, 32, now).unwrap_err(),
            AlphaInviteAdminError::InvalidMaxRedemptions
        );
        assert_eq!(
            validate_create_options("person", Some(now), 1, 32, now).unwrap_err(),
            AlphaInviteAdminError::InvalidExpiry
        );
        assert_eq!(
            validate_create_options("person", None, 1, 31, now).unwrap_err(),
            AlphaInviteAdminError::PepperTooShort
        );
    }

    #[test]
    fn invite_status_has_safe_precedence_and_remaining_count() {
        let now = Utc::now();
        let mut invite = AlphaInviteAdminRecord {
            invite_id: Uuid::nil(),
            label: Some("person".to_string()),
            created_at: now,
            expires_at: None,
            disabled_at: None,
            max_redemptions: 2,
            redemption_count: 1,
        };
        assert_eq!(invite.status_at(now), AlphaInviteStatus::Active);
        assert_eq!(invite.remaining_redemptions(), 1);

        invite.redemption_count = 2;
        assert_eq!(invite.status_at(now), AlphaInviteStatus::Exhausted);
        invite.expires_at = Some(now);
        assert_eq!(invite.status_at(now), AlphaInviteStatus::Expired);
        invite.disabled_at = Some(now);
        assert_eq!(invite.status_at(now), AlphaInviteStatus::Disabled);
    }
}
