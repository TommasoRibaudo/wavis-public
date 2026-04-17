//! Device management — list, revoke, logout-all, and create.
//!
//! Provides per-device lifecycle operations: creating devices under a user,
//! listing all devices, revoking individual devices (cascading to refresh
//! tokens), and logout-all (atomic epoch bump + token revocation).

use chrono::{DateTime, Utc};
use sqlx::PgPool;
use uuid::Uuid;

/// Device info returned by `list_devices`.
#[derive(Debug, Clone, sqlx::FromRow)]
pub struct DeviceInfo {
    pub device_id: Uuid,
    pub device_name: String,
    pub created_at: DateTime<Utc>,
    pub revoked_at: Option<DateTime<Utc>>,
}

/// Errors from device management operations.
#[derive(Debug, thiserror::Error)]
pub enum DeviceError {
    #[error("device not found")]
    NotFound,
    #[error("device does not belong to authenticated user")]
    NotOwned,
    #[error("database error: {0}")]
    DatabaseError(String),
}

/// Create a new device under a user. Returns the new `device_id`.
///
/// The device gets a fresh UUID via `gen_random_uuid()` — never reuses
/// `user_id` as `device_id` (that equality was a one-time migration artifact).
pub async fn create_device(
    pool: &PgPool,
    user_id: Uuid,
    device_name: &str,
) -> Result<Uuid, DeviceError> {
    let device_id: Uuid = sqlx::query_scalar(
        "INSERT INTO devices (device_id, user_id, device_name, created_at) \
         VALUES (gen_random_uuid(), $1, $2, now()) \
         RETURNING device_id",
    )
    .bind(user_id)
    .bind(device_name)
    .fetch_one(pool)
    .await
    .map_err(|e| DeviceError::DatabaseError(e.to_string()))?;

    Ok(device_id)
}

/// List all devices for a user, ordered by creation time ascending.
pub async fn list_devices(pool: &PgPool, user_id: Uuid) -> Result<Vec<DeviceInfo>, DeviceError> {
    let rows = sqlx::query_as::<_, DeviceInfo>(
        r#"SELECT device_id, device_name, created_at, revoked_at
           FROM devices
           WHERE user_id = $1
           ORDER BY created_at ASC"#,
    )
    .bind(user_id)
    .fetch_all(pool)
    .await
    .map_err(|e| DeviceError::DatabaseError(e.to_string()))?;

    Ok(rows)
}

/// Revoke a device — sets `revoked_at` and revokes all its refresh tokens.
///
/// Verifies the target device belongs to the authenticated user (returns
/// `NotOwned` if not). Self-revocation is allowed.
pub async fn revoke_device(
    pool: &PgPool,
    user_id: Uuid,
    target_device_id: Uuid,
) -> Result<(), DeviceError> {
    // Verify the device exists and belongs to this user.
    let owner: Option<Uuid> =
        sqlx::query_scalar("SELECT user_id FROM devices WHERE device_id = $1")
            .bind(target_device_id)
            .fetch_optional(pool)
            .await
            .map_err(|e| DeviceError::DatabaseError(e.to_string()))?;

    match owner {
        None => return Err(DeviceError::NotFound),
        Some(owner_id) if owner_id != user_id => return Err(DeviceError::NotOwned),
        _ => {}
    }

    // Set revoked_at on the device (idempotent — only if not already revoked).
    sqlx::query(
        "UPDATE devices SET revoked_at = now() \
         WHERE device_id = $1 AND revoked_at IS NULL",
    )
    .bind(target_device_id)
    .execute(pool)
    .await
    .map_err(|e| DeviceError::DatabaseError(e.to_string()))?;

    // Revoke all refresh tokens associated with this device.
    sqlx::query(
        "UPDATE refresh_tokens SET revoked_at = now() \
         WHERE device_id = $1 AND revoked_at IS NULL",
    )
    .bind(target_device_id)
    .execute(pool)
    .await
    .map_err(|e| DeviceError::DatabaseError(e.to_string()))?;

    Ok(())
}

/// Logout all — atomically bump `session_epoch` and revoke all refresh tokens
/// for the user. Returns the new epoch value.
///
/// The revoke-all query joins through the `devices` table since refresh tokens
/// reference `device_id`, not `user_id` directly.
pub async fn logout_all(pool: &PgPool, user_id: Uuid) -> Result<i32, DeviceError> {
    let mut tx = pool
        .begin()
        .await
        .map_err(|e| DeviceError::DatabaseError(e.to_string()))?;

    // Bump session_epoch by 1, returning the new value.
    let new_epoch: i32 = sqlx::query_scalar(
        "UPDATE users SET session_epoch = session_epoch + 1 \
         WHERE user_id = $1 \
         RETURNING session_epoch",
    )
    .bind(user_id)
    .fetch_one(&mut *tx)
    .await
    .map_err(|e| DeviceError::DatabaseError(e.to_string()))?;

    // Revoke all refresh tokens for this user (join through devices table).
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
    .map_err(|e| DeviceError::DatabaseError(e.to_string()))?;

    tx.commit()
        .await
        .map_err(|e| DeviceError::DatabaseError(e.to_string()))?;

    Ok(new_epoch)
}
