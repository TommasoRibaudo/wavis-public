//! Integration tests for background cleanup (sweep) operations.
//!
//! Tests Properties 21, 22 from the user-identity-recovery spec.
//! All tests require a running Postgres instance.
//! Run with: `cargo test --test cleanup_integration -- --ignored`

use serial_test::serial;
use sqlx::PgPool;
use uuid::Uuid;

use wavis_backend::auth::auth::{
    generate_refresh_token, hash_refresh_token, register_user, sweep_consumed_tokens,
};
use wavis_backend::auth::pairing::{start_pairing, sweep_expired_pairings};
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

// ---------------------------------------------------------------------------
// Feature: user-identity-recovery, Property 21: Sweep removes expired pairings
// Insert pairings with various expiry times, sweep, verify only expired ones removed.
// Validates: Requirements 20.1
// ---------------------------------------------------------------------------

#[tokio::test]
#[ignore]
#[serial]
async fn prop21_sweep_expired_pairings() {
    let pool = test_pool().await;
    truncate_tables(&pool).await;

    // Create a fresh pairing (expires in 5 minutes — should survive sweep)
    let (fresh_id, _code) = start_pairing(&pool, "fresh-device", TEST_PEPPER)
        .await
        .unwrap();

    // Create another fresh pairing
    let (fresh_id2, _code2) = start_pairing(&pool, "fresh-device-2", TEST_PEPPER)
        .await
        .unwrap();

    // Manually insert an expired pairing (expired 48 hours ago)
    let expired_id = Uuid::new_v4();
    sqlx::query(
        "INSERT INTO pairings (pairing_id, code_hash, request_device_name, expires_at) \
         VALUES ($1, $2, 'expired-device', now() - interval '48 hours')",
    )
    .bind(expired_id)
    .bind(vec![0u8; 32]) // dummy hash
    .execute(&pool)
    .await
    .unwrap();

    // Insert another expired pairing (expired 25 hours ago)
    let expired_id2 = Uuid::new_v4();
    sqlx::query(
        "INSERT INTO pairings (pairing_id, code_hash, request_device_name, expires_at) \
         VALUES ($1, $2, 'expired-device-2', now() - interval '25 hours')",
    )
    .bind(expired_id2)
    .bind(vec![0u8; 32])
    .execute(&pool)
    .await
    .unwrap();

    // Insert a recently expired pairing (expired 1 hour ago — within 24h retention)
    let recent_expired_id = Uuid::new_v4();
    sqlx::query(
        "INSERT INTO pairings (pairing_id, code_hash, request_device_name, expires_at) \
         VALUES ($1, $2, 'recent-expired', now() - interval '1 hour')",
    )
    .bind(recent_expired_id)
    .bind(vec![0u8; 32])
    .execute(&pool)
    .await
    .unwrap();

    // Verify we have 5 pairings total
    let total: (i64,) = sqlx::query_as("SELECT COUNT(*) FROM pairings")
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(total.0, 5, "should have 5 pairings before sweep");

    // Sweep with 24-hour retention
    let swept = sweep_expired_pairings(&pool, 24).await.unwrap();
    assert_eq!(
        swept, 2,
        "should sweep 2 pairings expired beyond 24h retention"
    );

    // Verify remaining pairings
    let remaining: (i64,) = sqlx::query_as("SELECT COUNT(*) FROM pairings")
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(remaining.0, 3, "should have 3 pairings after sweep");

    // Fresh pairings should still exist
    let fresh_exists: bool =
        sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM pairings WHERE pairing_id = $1)")
            .bind(fresh_id)
            .fetch_one(&pool)
            .await
            .unwrap();
    assert!(fresh_exists, "fresh pairing should survive sweep");

    let fresh2_exists: bool =
        sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM pairings WHERE pairing_id = $1)")
            .bind(fresh_id2)
            .fetch_one(&pool)
            .await
            .unwrap();
    assert!(fresh2_exists, "fresh pairing 2 should survive sweep");

    // Recently expired pairing (within retention) should survive
    let recent_exists: bool =
        sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM pairings WHERE pairing_id = $1)")
            .bind(recent_expired_id)
            .fetch_one(&pool)
            .await
            .unwrap();
    assert!(
        recent_exists,
        "recently expired pairing should survive 24h retention"
    );

    // Expired pairings beyond retention should be gone
    let expired_exists: bool =
        sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM pairings WHERE pairing_id = $1)")
            .bind(expired_id)
            .fetch_one(&pool)
            .await
            .unwrap();
    assert!(!expired_exists, "expired pairing (48h) should be swept");

    let expired2_exists: bool =
        sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM pairings WHERE pairing_id = $1)")
            .bind(expired_id2)
            .fetch_one(&pool)
            .await
            .unwrap();
    assert!(!expired2_exists, "expired pairing (25h) should be swept");
}

// ---------------------------------------------------------------------------
// Feature: user-identity-recovery, Property 22: Sweep consumed/revoked tokens
// Insert tokens with various consumed/revoked times, sweep, verify correct
// removal; active tokens unaffected.
// Validates: Requirements 20.2
// ---------------------------------------------------------------------------

#[tokio::test]
#[ignore]
#[serial]
async fn prop22_sweep_consumed_revoked_tokens() {
    let pool = test_pool().await;
    truncate_tables(&pool).await;

    // Register a user to get a valid device
    let reg = register_user(
        &pool,
        "sweep-token-phrase",
        "sweep-device",
        TEST_SECRET,
        TEST_ACCESS_TTL,
        TEST_REFRESH_TTL_DAYS,
        TEST_PEPPER,
        &test_phrase_config(),
        TEST_ENCRYPTION_KEY,
    )
    .await
    .unwrap();

    // The registration created 1 active token. Create more tokens with various states.

    // Token 2: consumed recently (within retention) — should survive
    let hash2 = hash_refresh_token(&generate_refresh_token(), TEST_PEPPER);
    sqlx::query(
        "INSERT INTO refresh_tokens (refresh_id, device_id, token_hash, family_id, expires_at, consumed_at) \
         VALUES (gen_random_uuid(), $1, $2, gen_random_uuid(), now() + interval '30 days', now() - interval '1 hour')",
    )
    .bind(reg.device_id)
    .bind(&hash2)
    .execute(&pool)
    .await
    .unwrap();

    // Token 3: consumed long ago (beyond retention) — should be swept
    let hash3 = hash_refresh_token(&generate_refresh_token(), TEST_PEPPER);
    sqlx::query(
        "INSERT INTO refresh_tokens (refresh_id, device_id, token_hash, family_id, expires_at, consumed_at) \
         VALUES (gen_random_uuid(), $1, $2, gen_random_uuid(), now() + interval '30 days', now() - interval '200 hours')",
    )
    .bind(reg.device_id)
    .bind(&hash3)
    .execute(&pool)
    .await
    .unwrap();

    // Token 4: revoked recently (within retention) — should survive
    let hash4 = hash_refresh_token(&generate_refresh_token(), TEST_PEPPER);
    sqlx::query(
        "INSERT INTO refresh_tokens (refresh_id, device_id, token_hash, family_id, expires_at, revoked_at) \
         VALUES (gen_random_uuid(), $1, $2, gen_random_uuid(), now() + interval '30 days', now() - interval '2 hours')",
    )
    .bind(reg.device_id)
    .bind(&hash4)
    .execute(&pool)
    .await
    .unwrap();

    // Token 5: revoked long ago (beyond retention) — should be swept
    let hash5 = hash_refresh_token(&generate_refresh_token(), TEST_PEPPER);
    sqlx::query(
        "INSERT INTO refresh_tokens (refresh_id, device_id, token_hash, family_id, expires_at, revoked_at) \
         VALUES (gen_random_uuid(), $1, $2, gen_random_uuid(), now() + interval '30 days', now() - interval '200 hours')",
    )
    .bind(reg.device_id)
    .bind(&hash5)
    .execute(&pool)
    .await
    .unwrap();

    // Token 6: active (no consumed_at, no revoked_at) — should survive
    let hash6 = hash_refresh_token(&generate_refresh_token(), TEST_PEPPER);
    sqlx::query(
        "INSERT INTO refresh_tokens (refresh_id, device_id, token_hash, family_id, expires_at) \
         VALUES (gen_random_uuid(), $1, $2, gen_random_uuid(), now() + interval '30 days')",
    )
    .bind(reg.device_id)
    .bind(&hash6)
    .execute(&pool)
    .await
    .unwrap();

    // Verify total: 6 tokens (1 from registration + 5 manually inserted)
    let total: (i64,) = sqlx::query_as("SELECT COUNT(*) FROM refresh_tokens WHERE device_id = $1")
        .bind(reg.device_id)
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(total.0, 6, "should have 6 tokens before sweep");

    // Sweep with 168-hour (7-day) retention
    let swept = sweep_consumed_tokens(&pool, 168).await.unwrap();
    assert_eq!(
        swept, 2,
        "should sweep 2 tokens (consumed + revoked beyond 168h)"
    );

    // Verify remaining tokens
    let remaining: (i64,) =
        sqlx::query_as("SELECT COUNT(*) FROM refresh_tokens WHERE device_id = $1")
            .bind(reg.device_id)
            .fetch_one(&pool)
            .await
            .unwrap();
    assert_eq!(remaining.0, 4, "should have 4 tokens after sweep");

    // Active tokens (registration token + token 6) should survive
    let active: (i64,) = sqlx::query_as(
        "SELECT COUNT(*) FROM refresh_tokens \
         WHERE device_id = $1 AND consumed_at IS NULL AND revoked_at IS NULL",
    )
    .bind(reg.device_id)
    .fetch_one(&pool)
    .await
    .unwrap();
    assert_eq!(active.0, 2, "2 active tokens should survive sweep");

    // Recently consumed token should survive
    let recent_consumed: bool =
        sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM refresh_tokens WHERE token_hash = $1)")
            .bind(&hash2)
            .fetch_one(&pool)
            .await
            .unwrap();
    assert!(recent_consumed, "recently consumed token should survive");

    // Old consumed token should be gone
    let old_consumed: bool =
        sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM refresh_tokens WHERE token_hash = $1)")
            .bind(&hash3)
            .fetch_one(&pool)
            .await
            .unwrap();
    assert!(!old_consumed, "old consumed token should be swept");

    // Recently revoked token should survive
    let recent_revoked: bool =
        sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM refresh_tokens WHERE token_hash = $1)")
            .bind(&hash4)
            .fetch_one(&pool)
            .await
            .unwrap();
    assert!(recent_revoked, "recently revoked token should survive");

    // Old revoked token should be gone
    let old_revoked: bool =
        sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM refresh_tokens WHERE token_hash = $1)")
            .bind(&hash5)
            .fetch_one(&pool)
            .await
            .unwrap();
    assert!(!old_revoked, "old revoked token should be swept");
}
