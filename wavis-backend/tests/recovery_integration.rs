//! Integration tests for account recovery domain layer.
//!
//! Tests Properties 5, 6, 17 from the user-identity-recovery spec.
//! All tests require a running Postgres instance.
//! Run with: `cargo test --test recovery_integration -- --ignored`

use serial_test::serial;
use sqlx::PgPool;
use uuid::Uuid;

use wavis_backend::auth::auth::{
    AuthError, DeviceRegistration, recover_account, register_user, rotate_phrase,
};
use wavis_backend::auth::jwt::validate_access_token;
use wavis_backend::auth::phrase::{self, PhraseConfig};
use wavis_backend::channel::channel;

const TEST_SECRET: &[u8] = b"test-secret-at-least-32-bytes!!!";
const TEST_PEPPER: &[u8] = b"test-pepper-at-least-32-bytes!!!";
const TEST_ENCRYPTION_KEY: &[u8] = &[0u8; 32];
const TEST_ACCESS_TTL: u64 = 900;
const TEST_REFRESH_TTL_DAYS: u32 = 30;

/// Low-cost Argon2id config for fast integration tests.
fn test_phrase_config() -> PhraseConfig {
    PhraseConfig {
        memory_cost_kib: 256,
        iterations: 1,
        parallelism: 1,
    }
}

/// Get a test database pool with migrations applied.
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

/// Truncate all auth-related tables for test isolation.
async fn truncate_tables(pool: &PgPool) {
    sqlx::query(
        "TRUNCATE refresh_tokens, pairings, devices, channel_memberships, channels, users CASCADE",
    )
    .execute(pool)
    .await
    .expect("Failed to truncate tables");
}

/// Helper: register a user with a phrase and return the registration result.
async fn register_test_user(pool: &PgPool, phrase: &str, device_name: &str) -> DeviceRegistration {
    register_user(
        pool,
        phrase,
        device_name,
        TEST_SECRET,
        TEST_ACCESS_TTL,
        TEST_REFRESH_TTL_DAYS,
        TEST_PEPPER,
        &test_phrase_config(),
        TEST_ENCRYPTION_KEY,
    )
    .await
    .unwrap()
}

// ---------------------------------------------------------------------------
// Feature: user-identity-recovery, Property 5: Recovery round-trip
// Register with phrase, recover with correct recovery_id + phrase,
// verify same user_id, different device_id.
// Validates: Requirements 5.1, 5.3, 5.4
// ---------------------------------------------------------------------------

#[tokio::test]
#[ignore]
#[serial]
async fn prop5_recovery_round_trip() {
    let pool = test_pool().await;
    truncate_tables(&pool).await;

    let phrase = "my-secret-recovery-phrase";
    let dummy = phrase::generate_dummy_verifier(&test_phrase_config());

    // Register a user with a phrase
    let reg = register_test_user(&pool, phrase, "original-device").await;
    assert!(
        !reg.recovery_id.is_empty(),
        "recovery_id should be non-empty"
    );

    // Recover with the correct recovery_id + phrase
    let recovered = recover_account(
        &pool,
        &reg.recovery_id,
        phrase,
        "recovered-device",
        TEST_SECRET,
        TEST_ACCESS_TTL,
        TEST_REFRESH_TTL_DAYS,
        TEST_PEPPER,
        &test_phrase_config(),
        TEST_ENCRYPTION_KEY,
        &dummy,
    )
    .await
    .unwrap();

    // Same user_id
    assert_eq!(
        recovered.user_id, reg.user_id,
        "recovered user_id must match original"
    );

    // Different device_id
    assert_ne!(
        recovered.device_id, reg.device_id,
        "recovered device_id must differ from original"
    );

    // Valid access token
    let (uid, did, _epoch) = validate_access_token(&recovered.access_token, TEST_SECRET).unwrap();
    assert_eq!(uid, reg.user_id);
    assert_eq!(did, recovered.device_id);

    // Non-empty refresh token
    assert!(!recovered.refresh_token.is_empty());
}

// ---------------------------------------------------------------------------
// Feature: user-identity-recovery, Property 5 (negative): Wrong phrase fails
// ---------------------------------------------------------------------------

#[tokio::test]
#[ignore]
#[serial]
async fn prop5_recovery_wrong_phrase_fails() {
    let pool = test_pool().await;
    truncate_tables(&pool).await;

    let dummy = phrase::generate_dummy_verifier(&test_phrase_config());

    let reg = register_test_user(&pool, "correct-phrase", "device-1").await;

    let result = recover_account(
        &pool,
        &reg.recovery_id,
        "wrong-phrase",
        "device-2",
        TEST_SECRET,
        TEST_ACCESS_TTL,
        TEST_REFRESH_TTL_DAYS,
        TEST_PEPPER,
        &test_phrase_config(),
        TEST_ENCRYPTION_KEY,
        &dummy,
    )
    .await;

    assert!(
        matches!(result, Err(AuthError::PhraseVerificationFailed)),
        "wrong phrase should fail, got: {:?}",
        result
    );
}

// ---------------------------------------------------------------------------
// Feature: user-identity-recovery, Property 6: Phrase rotation round-trip
// Register, rotate phrase, recover with new phrase succeeds, old phrase fails.
// Validates: Requirements 16.1, 16.2
// ---------------------------------------------------------------------------

#[tokio::test]
#[ignore]
#[serial]
async fn prop6_phrase_rotation_round_trip() {
    let pool = test_pool().await;
    truncate_tables(&pool).await;

    let old_phrase = "original-secret-phrase";
    let new_phrase = "rotated-secret-phrase";
    let dummy = phrase::generate_dummy_verifier(&test_phrase_config());

    // Register with old phrase
    let reg = register_test_user(&pool, old_phrase, "device-rotate").await;

    // Rotate phrase
    rotate_phrase(
        &pool,
        reg.user_id,
        old_phrase,
        new_phrase,
        &test_phrase_config(),
        TEST_ENCRYPTION_KEY,
    )
    .await
    .unwrap();

    // Recover with new phrase should succeed
    let recovered = recover_account(
        &pool,
        &reg.recovery_id,
        new_phrase,
        "device-after-rotate",
        TEST_SECRET,
        TEST_ACCESS_TTL,
        TEST_REFRESH_TTL_DAYS,
        TEST_PEPPER,
        &test_phrase_config(),
        TEST_ENCRYPTION_KEY,
        &dummy,
    )
    .await
    .unwrap();

    assert_eq!(recovered.user_id, reg.user_id);

    // Recover with old phrase should fail
    let old_result = recover_account(
        &pool,
        &reg.recovery_id,
        old_phrase,
        "device-old-phrase",
        TEST_SECRET,
        TEST_ACCESS_TTL,
        TEST_REFRESH_TTL_DAYS,
        TEST_PEPPER,
        &test_phrase_config(),
        TEST_ENCRYPTION_KEY,
        &dummy,
    )
    .await;

    assert!(
        matches!(old_result, Err(AuthError::PhraseVerificationFailed)),
        "old phrase should fail after rotation, got: {:?}",
        old_result
    );
}

// ---------------------------------------------------------------------------
// Feature: user-identity-recovery, Property 17: Channel ownership preserved
// Create user + channels, recover on new device, verify ownership retained.
// Validates: Requirements 14.2
// ---------------------------------------------------------------------------

#[tokio::test]
#[ignore]
#[serial]
async fn prop17_channel_ownership_after_recovery() {
    let pool = test_pool().await;
    truncate_tables(&pool).await;

    let phrase = "channel-owner-phrase";
    let dummy = phrase::generate_dummy_verifier(&test_phrase_config());

    // Register a user
    let reg = register_test_user(&pool, phrase, "device-channels").await;

    // Create two channels owned by this user
    let ch1 = channel::create_channel(&pool, reg.user_id, "channel-one")
        .await
        .unwrap();
    let ch2 = channel::create_channel(&pool, reg.user_id, "channel-two")
        .await
        .unwrap();

    assert_eq!(ch1.owner_user_id, reg.user_id);
    assert_eq!(ch2.owner_user_id, reg.user_id);

    // Recover on a new device
    let recovered = recover_account(
        &pool,
        &reg.recovery_id,
        phrase,
        "device-recovered",
        TEST_SECRET,
        TEST_ACCESS_TTL,
        TEST_REFRESH_TTL_DAYS,
        TEST_PEPPER,
        &test_phrase_config(),
        TEST_ENCRYPTION_KEY,
        &dummy,
    )
    .await
    .unwrap();

    assert_eq!(recovered.user_id, reg.user_id);
    assert_ne!(recovered.device_id, reg.device_id);

    // Verify channel ownership is retained — channels still reference user_id
    let channels = channel::list_channels(&pool, recovered.user_id)
        .await
        .unwrap();
    assert_eq!(
        channels.len(),
        2,
        "user should still have 2 channels after recovery"
    );

    let channel_ids: Vec<Uuid> = channels.iter().map(|c| c.channel_id).collect();
    assert!(channel_ids.contains(&ch1.channel_id));
    assert!(channel_ids.contains(&ch2.channel_id));

    // Verify ownership specifically
    for ch in &channels {
        assert_eq!(
            ch.owner_user_id, reg.user_id,
            "channel ownership must be preserved"
        );
    }
}
