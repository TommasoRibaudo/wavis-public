//! Integration tests for device management domain layer.
//!
//! Tests Properties 14, 15, 16 from the user-identity-recovery spec.
//! All tests require a running Postgres instance.
//! Run with: `cargo test --test device_integration -- --ignored`

use serial_test::serial;
use sqlx::PgPool;
use uuid::Uuid;

use wavis_backend::auth::auth::{generate_refresh_token, hash_refresh_token, register_user};
use wavis_backend::auth::device::{create_device, list_devices, logout_all, revoke_device};
use wavis_backend::auth::phrase::PhraseConfig;

const TEST_SECRET: &[u8] = b"test-secret-at-least-32-bytes!!!";
const TEST_PEPPER: &[u8] = b"test-pepper-at-least-32-bytes!!!";
const TEST_ENCRYPTION_KEY: &[u8] = &[0u8; 32];
const TEST_ACCESS_TTL: u64 = 900;
const TEST_REFRESH_TTL_DAYS: u32 = 30;

fn test_phrase_config() -> PhraseConfig {
    PhraseConfig {
        memory_cost_kib: 256,
        iterations: 1,
        parallelism: 1,
    }
}

async fn test_pool() -> PgPool {
    let url = std::env::var("DATABASE_URL")
        .unwrap_or_else(|_| "postgres://wavis:wavis@localhost:5432/wavis".to_string());
    let pool = sqlx::postgres::PgPoolOptions::new()
        .max_connections(5)
        .connect(&url)
        .await
        .expect("Failed to connect to test database");
    sqlx::migrate!()
        .run(&pool)
        .await
        .expect("Failed to run migrations");
    pool
}

async fn truncate_tables(pool: &PgPool) {
    sqlx::query(
        "TRUNCATE refresh_tokens, pairings, devices, channel_memberships, channels, users CASCADE",
    )
    .execute(pool)
    .await
    .expect("Failed to truncate tables");
}

/// Helper: create a refresh token for a device in the DB.
async fn create_refresh_token_for_device(pool: &PgPool, device_id: Uuid) {
    let raw = generate_refresh_token();
    let hash = hash_refresh_token(&raw, TEST_PEPPER);
    let expires_at = chrono::Utc::now() + chrono::Duration::days(30);
    sqlx::query(
        "INSERT INTO refresh_tokens (refresh_id, device_id, token_hash, family_id, expires_at) \
         VALUES (gen_random_uuid(), $1, $2, gen_random_uuid(), $3)",
    )
    .bind(device_id)
    .bind(&hash)
    .bind(expires_at)
    .execute(pool)
    .await
    .unwrap();
}

// ---------------------------------------------------------------------------
// Feature: user-identity-recovery, Property 14: Device revocation cascades
// Create device + tokens, revoke device, verify all tokens have revoked_at set.
// Validates: Requirements 3.5, 11.1, 11.2
// ---------------------------------------------------------------------------

#[tokio::test]
#[ignore]
#[serial]
async fn prop14_device_revocation_cascades_to_tokens() {
    let pool = test_pool().await;
    truncate_tables(&pool).await;

    // Register a user (creates user + device + 1 refresh token)
    let reg = register_user(
        &pool,
        "cascade-phrase",
        "device-cascade",
        TEST_SECRET,
        TEST_ACCESS_TTL,
        TEST_REFRESH_TTL_DAYS,
        TEST_PEPPER,
        &test_phrase_config(),
        TEST_ENCRYPTION_KEY,
    )
    .await
    .unwrap();

    // Create additional refresh tokens for this device
    create_refresh_token_for_device(&pool, reg.device_id).await;
    create_refresh_token_for_device(&pool, reg.device_id).await;

    // Verify we have 3 tokens for this device, all unrevoked
    let token_count: (i64,) = sqlx::query_as(
        "SELECT COUNT(*) FROM refresh_tokens WHERE device_id = $1 AND revoked_at IS NULL",
    )
    .bind(reg.device_id)
    .fetch_one(&pool)
    .await
    .unwrap();
    assert_eq!(
        token_count.0, 3,
        "should have 3 unrevoked tokens before revocation"
    );

    // Revoke the device
    revoke_device(&pool, reg.user_id, reg.device_id)
        .await
        .unwrap();

    // All tokens for this device should now have revoked_at set
    let unrevoked: (i64,) = sqlx::query_as(
        "SELECT COUNT(*) FROM refresh_tokens WHERE device_id = $1 AND revoked_at IS NULL",
    )
    .bind(reg.device_id)
    .fetch_one(&pool)
    .await
    .unwrap();
    assert_eq!(
        unrevoked.0, 0,
        "all tokens should be revoked after device revocation"
    );

    let revoked: (i64,) = sqlx::query_as(
        "SELECT COUNT(*) FROM refresh_tokens WHERE device_id = $1 AND revoked_at IS NOT NULL",
    )
    .bind(reg.device_id)
    .fetch_one(&pool)
    .await
    .unwrap();
    assert_eq!(revoked.0, 3, "all 3 tokens should have revoked_at set");

    // Device itself should have revoked_at set
    let device_revoked: Option<chrono::DateTime<chrono::Utc>> =
        sqlx::query_scalar("SELECT revoked_at FROM devices WHERE device_id = $1")
            .bind(reg.device_id)
            .fetch_one(&pool)
            .await
            .unwrap();
    assert!(
        device_revoked.is_some(),
        "device should have revoked_at set"
    );
}

// ---------------------------------------------------------------------------
// Feature: user-identity-recovery, Property 15: Logout-all atomicity
// Create user + devices + tokens, logout_all, verify epoch = E+1 and all
// tokens revoked.
// Validates: Requirements 10.1, 10.2
// ---------------------------------------------------------------------------

#[tokio::test]
#[ignore]
#[serial]
async fn prop15_logout_all_atomicity() {
    let pool = test_pool().await;
    truncate_tables(&pool).await;

    // Register a user
    let reg = register_user(
        &pool,
        "logout-all-phrase",
        "device-1",
        TEST_SECRET,
        TEST_ACCESS_TTL,
        TEST_REFRESH_TTL_DAYS,
        TEST_PEPPER,
        &test_phrase_config(),
        TEST_ENCRYPTION_KEY,
    )
    .await
    .unwrap();

    // Create additional devices with tokens
    let device2 = create_device(&pool, reg.user_id, "device-2").await.unwrap();
    create_refresh_token_for_device(&pool, device2).await;

    let device3 = create_device(&pool, reg.user_id, "device-3").await.unwrap();
    create_refresh_token_for_device(&pool, device3).await;

    // Get initial epoch
    let epoch_before: i32 =
        sqlx::query_scalar("SELECT session_epoch FROM users WHERE user_id = $1")
            .bind(reg.user_id)
            .fetch_one(&pool)
            .await
            .unwrap();
    assert_eq!(epoch_before, 0);

    // Count total unrevoked tokens across all devices
    let tokens_before: (i64,) = sqlx::query_as(
        "SELECT COUNT(*) FROM refresh_tokens rt \
         JOIN devices d ON rt.device_id = d.device_id \
         WHERE d.user_id = $1 AND rt.revoked_at IS NULL",
    )
    .bind(reg.user_id)
    .fetch_one(&pool)
    .await
    .unwrap();
    assert_eq!(
        tokens_before.0, 3,
        "should have 3 unrevoked tokens (1 per device)"
    );

    // Logout all
    let new_epoch = logout_all(&pool, reg.user_id).await.unwrap();

    // Epoch should be E+1
    assert_eq!(new_epoch, epoch_before + 1, "epoch should be bumped by 1");

    // Verify epoch in DB
    let db_epoch: i32 = sqlx::query_scalar("SELECT session_epoch FROM users WHERE user_id = $1")
        .bind(reg.user_id)
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(db_epoch, epoch_before + 1);

    // All tokens should be revoked
    let unrevoked_after: (i64,) = sqlx::query_as(
        "SELECT COUNT(*) FROM refresh_tokens rt \
         JOIN devices d ON rt.device_id = d.device_id \
         WHERE d.user_id = $1 AND rt.revoked_at IS NULL",
    )
    .bind(reg.user_id)
    .fetch_one(&pool)
    .await
    .unwrap();
    assert_eq!(
        unrevoked_after.0, 0,
        "all tokens should be revoked after logout_all"
    );
}

// ---------------------------------------------------------------------------
// Feature: user-identity-recovery, Property 16: Device listing completeness
// Create N devices (some revoked), list, verify count and field correctness.
// Validates: Requirements 12.1
// ---------------------------------------------------------------------------

#[tokio::test]
#[ignore]
#[serial]
async fn prop16_device_listing_completeness() {
    let pool = test_pool().await;
    truncate_tables(&pool).await;

    // Register a user (creates 1 device)
    let reg = register_user(
        &pool,
        "listing-phrase",
        "device-primary",
        TEST_SECRET,
        TEST_ACCESS_TTL,
        TEST_REFRESH_TTL_DAYS,
        TEST_PEPPER,
        &test_phrase_config(),
        TEST_ENCRYPTION_KEY,
    )
    .await
    .unwrap();

    // Create additional devices
    let dev2 = create_device(&pool, reg.user_id, "device-laptop")
        .await
        .unwrap();
    let dev3 = create_device(&pool, reg.user_id, "device-tablet")
        .await
        .unwrap();
    let dev4 = create_device(&pool, reg.user_id, "device-old-phone")
        .await
        .unwrap();

    // Revoke some devices
    revoke_device(&pool, reg.user_id, dev3).await.unwrap();
    revoke_device(&pool, reg.user_id, dev4).await.unwrap();

    // List all devices
    let devices = list_devices(&pool, reg.user_id).await.unwrap();

    // Should return all 4 devices (including revoked ones)
    assert_eq!(
        devices.len(),
        4,
        "should list all 4 devices including revoked"
    );

    // Verify device IDs are all present
    let device_ids: Vec<Uuid> = devices.iter().map(|d| d.device_id).collect();
    assert!(device_ids.contains(&reg.device_id));
    assert!(device_ids.contains(&dev2));
    assert!(device_ids.contains(&dev3));
    assert!(device_ids.contains(&dev4));

    // Verify revocation status
    let primary = devices
        .iter()
        .find(|d| d.device_id == reg.device_id)
        .unwrap();
    assert!(
        primary.revoked_at.is_none(),
        "primary device should not be revoked"
    );
    assert_eq!(primary.device_name, "device-primary");

    let laptop = devices.iter().find(|d| d.device_id == dev2).unwrap();
    assert!(laptop.revoked_at.is_none(), "laptop should not be revoked");
    assert_eq!(laptop.device_name, "device-laptop");

    let tablet = devices.iter().find(|d| d.device_id == dev3).unwrap();
    assert!(tablet.revoked_at.is_some(), "tablet should be revoked");
    assert_eq!(tablet.device_name, "device-tablet");

    let old_phone = devices.iter().find(|d| d.device_id == dev4).unwrap();
    assert!(
        old_phone.revoked_at.is_some(),
        "old phone should be revoked"
    );
    assert_eq!(old_phone.device_name, "device-old-phone");

    // Verify created_at is set for all devices
    for d in &devices {
        assert!(
            d.created_at <= chrono::Utc::now(),
            "created_at should be in the past"
        );
    }
}
