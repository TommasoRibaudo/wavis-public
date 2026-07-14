//! Closed-alpha invite code normalization, hashing, and redemption.
//!
//! Raw invite codes are never persisted. The database stores only HMAC-SHA256
//! hashes computed with a dedicated server-side pepper.

use hmac::{Hmac, Mac};
use sha2::Sha256;
use sqlx::{Postgres, Transaction};
use uuid::Uuid;

type HmacSha256 = Hmac<Sha256>;

const HASH_DOMAIN: &[u8] = b"alpha-invite:v1:";

#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum AlphaInviteError {
    #[error("invite code invalid")]
    Invalid,
    #[error("database error: {0}")]
    DatabaseError(String),
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

    let row: Option<(
        Uuid,
        Option<chrono::DateTime<chrono::Utc>>,
        Option<chrono::DateTime<chrono::Utc>>,
        i32,
        i32,
    )> = sqlx::query_as(
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
    use super::{AlphaInviteError, hash_invite_code, normalize_invite_code};

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
}
