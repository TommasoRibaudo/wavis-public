//! Integration tests for QR/code device pairing domain layer.
//!
//! Tests Properties 8, 11 from the user-identity-recovery spec.
//! All tests require a running Postgres instance.
//! Run with: `cargo test --test pairing_integration -- --ignored`

use serial_test::serial;
use sqlx::PgPool;
use std::net::{IpAddr, Ipv4Addr};
use std::time::{Duration, Instant};

use wavis_backend::auth::auth::register_user;
use wavis_backend::auth::auth_rate_limiter::{AuthRateLimiter, AuthRateLimiterConfig};
use wavis_backend::auth::jwt::validate_access_token;
use wavis_backend::auth::pairing::{approve_pairing, finish_pairing, start_pairing};
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
// Feature: user-identity-recovery, Property 8: Pairing flow round-trip
// Start → approve → finish; verify new device under approving user, valid tokens.
// Validates: Requirements 8.1, 9.1, 9.3
// ---------------------------------------------------------------------------

#[tokio::test]
#[ignore]
#[serial]
async fn prop8_pairing_flow_round_trip() {
    let pool = test_pool().await;
    truncate_tables(&pool).await;

    // Register a trusted user (the approver)
    let approver = register_user(
        &pool,
        "approver-phrase",
        "trusted-device",
        TEST_SECRET,
        TEST_ACCESS_TTL,
        TEST_REFRESH_TTL_DAYS,
        TEST_PEPPER,
        &test_phrase_config(),
        TEST_ENCRYPTION_KEY,
    )
    .await
    .unwrap();

    // Step 1: New device starts pairing
    let (pairing_id, code) = start_pairing(&pool, "new-device", TEST_PEPPER)
        .await
        .unwrap();

    // Step 2: Trusted device approves
    approve_pairing(
        &pool,
        pairing_id,
        &code,
        approver.user_id,
        approver.device_id,
        TEST_PEPPER,
    )
    .await
    .unwrap();

    // Step 3: New device finishes pairing
    let result = finish_pairing(
        &pool,
        pairing_id,
        &code,
        TEST_PEPPER,
        TEST_SECRET,
        TEST_ACCESS_TTL,
        TEST_REFRESH_TTL_DAYS,
        TEST_PEPPER,
    )
    .await
    .unwrap();

    // Verify: new device is under the approving user
    assert_eq!(
        result.user_id, approver.user_id,
        "paired device must be under the approving user"
    );

    // Verify: new device_id differs from approver's device_id
    assert_ne!(
        result.device_id, approver.device_id,
        "paired device_id must differ from approver's"
    );

    // Verify: valid access token
    let (uid, did, _epoch) = validate_access_token(&result.access_token, TEST_SECRET).unwrap();
    assert_eq!(uid, approver.user_id);
    assert_eq!(did, result.device_id);

    // Verify: non-empty refresh token
    assert!(!result.refresh_token.is_empty());

    // Verify: the new device exists in the devices table
    let device_exists: bool = sqlx::query_scalar(
        "SELECT EXISTS(SELECT 1 FROM devices WHERE device_id = $1 AND user_id = $2)",
    )
    .bind(result.device_id)
    .bind(approver.user_id)
    .fetch_one(&pool)
    .await
    .unwrap();
    assert!(device_exists, "new device should exist in devices table");

    // Verify: the pairing is marked as used
    let used_at: Option<chrono::DateTime<chrono::Utc>> =
        sqlx::query_scalar("SELECT used_at FROM pairings WHERE pairing_id = $1")
            .bind(pairing_id)
            .fetch_one(&pool)
            .await
            .unwrap();
    assert!(used_at.is_some(), "pairing should be marked as used");
}

// ---------------------------------------------------------------------------
// Feature: user-identity-recovery, Property 11: Pairing rate limiter ceiling
// Verify 10 start/finish per IP per hour, 10 approve per user per hour.
// Validates: Requirements 7.5, 8.7, 9.7
//
// Note: Pairing rate limiting is enforced at the handler level using
// AuthRateLimiter. This test verifies the rate limiter logic directly
// with a threshold of 10 to match the pairing endpoint configuration.
// ---------------------------------------------------------------------------

#[tokio::test]
#[ignore]
#[serial]
async fn prop11_pairing_rate_limiter_ceiling() {
    // Test the rate limiter with pairing-specific thresholds (10 per hour)
    let config = AuthRateLimiterConfig {
        register_max_per_ip: 10, // Used for pairing start/finish per IP
        register_window_secs: 3600,
        refresh_max_per_ip: 10, // Used for pairing approve per user
        refresh_window_secs: 3600,
    };
    let limiter = AuthRateLimiter::new(config);
    let ip = IpAddr::V4(Ipv4Addr::new(192, 168, 1, 100));
    let now = Instant::now();

    // Verify 10 requests are allowed (simulating pairing start/finish per IP)
    for i in 0..10 {
        assert!(
            limiter.check_register(ip, now),
            "pairing request {} should be allowed",
            i
        );
        limiter.record_register(ip, now);
    }

    // 11th request should be rejected
    assert!(
        !limiter.check_register(ip, now),
        "11th pairing request should be rejected"
    );

    // After window expires, should be allowed again
    let after_window = now + Duration::from_secs(3601);
    assert!(
        limiter.check_register(ip, after_window),
        "pairing should be allowed after window expires"
    );

    // Test approve rate limiting (per user, using refresh limiter as proxy)
    let user_ip = IpAddr::V4(Ipv4Addr::new(10, 0, 0, 1));
    for i in 0..10 {
        assert!(
            limiter.check_refresh(user_ip, now),
            "approve request {} should be allowed",
            i
        );
        limiter.record_refresh(user_ip, now);
    }

    // 11th approve should be rejected
    assert!(
        !limiter.check_refresh(user_ip, now),
        "11th approve request should be rejected"
    );

    // After window expires, should be allowed again
    assert!(
        limiter.check_refresh(user_ip, after_window),
        "approve should be allowed after window expires"
    );
}
