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

// ---------------------------------------------------------------------------
// P0-2: Pairing lockout — real production behavior, not a reimplemented
// threshold check. 5 total failed attempts must permanently lock the pairing
// (until TTL expiry), even for the correct code.
// ---------------------------------------------------------------------------

#[tokio::test]
#[ignore]
#[serial]
async fn lockout_after_five_mismatches_persists_for_correct_code() {
    let pool = test_pool().await;
    truncate_tables(&pool).await;

    let approver = register_user(
        &pool,
        "lockout-approver-phrase",
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

    let (pairing_id, code) = start_pairing(&pool, "attacker-device", TEST_PEPPER)
        .await
        .unwrap();

    // Attempts 1-4 with the wrong code: CodeMismatch.
    for attempt in 1..=4 {
        let result = approve_pairing(
            &pool,
            pairing_id,
            "WRONGCODE",
            approver.user_id,
            approver.device_id,
            TEST_PEPPER,
        )
        .await;
        assert!(
            matches!(
                result,
                Err(wavis_backend::auth::pairing::PairingError::CodeMismatch)
            ),
            "attempt {attempt} should be CodeMismatch, got {result:?}"
        );
    }

    // Attempt 5 with the wrong code crosses the threshold: LockedOut, not CodeMismatch.
    let fifth = approve_pairing(
        &pool,
        pairing_id,
        "WRONGCODE",
        approver.user_id,
        approver.device_id,
        TEST_PEPPER,
    )
    .await;
    assert!(
        matches!(
            fifth,
            Err(wavis_backend::auth::pairing::PairingError::LockedOut)
        ),
        "5th attempt should be LockedOut, got {fifth:?}"
    );

    // Attempt 6 with the CORRECT code: still LockedOut — lockout is not
    // bypassable by finally guessing right.
    let sixth = approve_pairing(
        &pool,
        pairing_id,
        &code,
        approver.user_id,
        approver.device_id,
        TEST_PEPPER,
    )
    .await;
    assert!(
        matches!(
            sixth,
            Err(wavis_backend::auth::pairing::PairingError::LockedOut)
        ),
        "6th attempt with correct code should still be LockedOut, got {sixth:?}"
    );

    let attempt_count: i32 =
        sqlx::query_scalar("SELECT attempt_count FROM pairings WHERE pairing_id = $1")
            .bind(pairing_id)
            .fetch_one(&pool)
            .await
            .unwrap();
    assert_eq!(
        attempt_count, 5,
        "attempt_count must stop at 5, not overshoot"
    );

    let approved_at: Option<chrono::DateTime<chrono::Utc>> =
        sqlx::query_scalar("SELECT approved_at FROM pairings WHERE pairing_id = $1")
            .bind(pairing_id)
            .fetch_one(&pool)
            .await
            .unwrap();
    assert!(
        approved_at.is_none(),
        "a locked-out pairing must never end up approved, even with the correct code"
    );
}

// ---------------------------------------------------------------------------
// P0-2: finish_pairing error paths — Expired, NotApproved, AlreadyUsed.
// ---------------------------------------------------------------------------

#[tokio::test]
#[ignore]
#[serial]
async fn finish_rejects_expired_used_unapproved() {
    let pool = test_pool().await;
    truncate_tables(&pool).await;

    let approver = register_user(
        &pool,
        "finish-approver-phrase",
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

    // --- NotApproved: never approved, finish fails before any code check ---
    let (not_approved_id, not_approved_code) =
        start_pairing(&pool, "device-a", TEST_PEPPER).await.unwrap();
    let result = finish_pairing(
        &pool,
        not_approved_id,
        &not_approved_code,
        TEST_PEPPER,
        TEST_SECRET,
        TEST_ACCESS_TTL,
        TEST_REFRESH_TTL_DAYS,
        TEST_PEPPER,
    )
    .await;
    assert!(
        matches!(
            result,
            Err(wavis_backend::auth::pairing::PairingError::NotApproved)
        ),
        "unapproved pairing must reject finish with NotApproved, got {result:?}"
    );

    // --- Expired: approved but TTL elapsed ---
    let (expired_id, expired_code) = start_pairing(&pool, "device-b", TEST_PEPPER).await.unwrap();
    approve_pairing(
        &pool,
        expired_id,
        &expired_code,
        approver.user_id,
        approver.device_id,
        TEST_PEPPER,
    )
    .await
    .unwrap();
    sqlx::query(
        "UPDATE pairings SET expires_at = now() - interval '1 minute' WHERE pairing_id = $1",
    )
    .bind(expired_id)
    .execute(&pool)
    .await
    .unwrap();
    let result = finish_pairing(
        &pool,
        expired_id,
        &expired_code,
        TEST_PEPPER,
        TEST_SECRET,
        TEST_ACCESS_TTL,
        TEST_REFRESH_TTL_DAYS,
        TEST_PEPPER,
    )
    .await;
    assert!(
        matches!(
            result,
            Err(wavis_backend::auth::pairing::PairingError::Expired)
        ),
        "expired pairing must reject finish with Expired, got {result:?}"
    );

    // --- AlreadyUsed: finish twice ---
    let (used_id, used_code) = start_pairing(&pool, "device-c", TEST_PEPPER).await.unwrap();
    approve_pairing(
        &pool,
        used_id,
        &used_code,
        approver.user_id,
        approver.device_id,
        TEST_PEPPER,
    )
    .await
    .unwrap();
    finish_pairing(
        &pool,
        used_id,
        &used_code,
        TEST_PEPPER,
        TEST_SECRET,
        TEST_ACCESS_TTL,
        TEST_REFRESH_TTL_DAYS,
        TEST_PEPPER,
    )
    .await
    .unwrap();
    let result = finish_pairing(
        &pool,
        used_id,
        &used_code,
        TEST_PEPPER,
        TEST_SECRET,
        TEST_ACCESS_TTL,
        TEST_REFRESH_TTL_DAYS,
        TEST_PEPPER,
    )
    .await;
    assert!(
        matches!(
            result,
            Err(wavis_backend::auth::pairing::PairingError::AlreadyUsed)
        ),
        "already-used pairing must reject a second finish with AlreadyUsed, got {result:?}"
    );
}

#[tokio::test]
#[ignore]
#[serial]
async fn approve_rejects_already_approved() {
    let pool = test_pool().await;
    truncate_tables(&pool).await;

    let approver = register_user(
        &pool,
        "double-approve-phrase",
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

    let (pairing_id, code) = start_pairing(&pool, "device-x", TEST_PEPPER).await.unwrap();
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

    let second = approve_pairing(
        &pool,
        pairing_id,
        &code,
        approver.user_id,
        approver.device_id,
        TEST_PEPPER,
    )
    .await;
    assert!(
        matches!(
            second,
            Err(wavis_backend::auth::pairing::PairingError::AlreadyApproved)
        ),
        "a second approve on an already-approved pairing must fail with AlreadyApproved, got {second:?}"
    );
}

// ---------------------------------------------------------------------------
// P0-2: finish_pairing is transactional — a downstream failure after used_at
// is set must roll back, not leave the pairing half-consumed.
// ---------------------------------------------------------------------------

#[tokio::test]
#[ignore]
#[serial]
async fn finish_pairing_rolls_back_used_at_on_downstream_failure() {
    let pool = test_pool().await;
    truncate_tables(&pool).await;

    let approver = register_user(
        &pool,
        "rollback-approver-phrase",
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

    let (pairing_id, code) = start_pairing(&pool, "device-y", TEST_PEPPER).await.unwrap();
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

    // Delete the approving user out from under the pairing. approved_user_id
    // has no FK (so the pairing row survives), but the devices.user_id FK
    // will reject the INSERT in step 3b, forcing a downstream failure inside
    // the finish_pairing transaction.
    sqlx::query("DELETE FROM users WHERE user_id = $1")
        .bind(approver.user_id)
        .execute(&pool)
        .await
        .unwrap();

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
    .await;
    assert!(
        matches!(
            result,
            Err(wavis_backend::auth::pairing::PairingError::DatabaseError(_))
        ),
        "finish_pairing should surface the downstream FK failure, got {result:?}"
    );

    let used_at: Option<chrono::DateTime<chrono::Utc>> =
        sqlx::query_scalar("SELECT used_at FROM pairings WHERE pairing_id = $1")
            .bind(pairing_id)
            .fetch_one(&pool)
            .await
            .unwrap();
    assert!(
        used_at.is_none(),
        "used_at must be rolled back when a downstream step in finish_pairing fails"
    );
}

// ---------------------------------------------------------------------------
// P1-3: finish_pairing / approve_pairing concurrency — exactly one winner.
// ---------------------------------------------------------------------------

#[tokio::test]
#[ignore]
#[serial]
async fn concurrent_finish_pairing_single_winner() {
    let pool = test_pool().await;
    truncate_tables(&pool).await;

    let approver = register_user(
        &pool,
        "concurrent-finish-approver",
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

    let (pairing_id, code) = start_pairing(&pool, "device-z", TEST_PEPPER).await.unwrap();
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

    let devices_before: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM devices WHERE user_id = $1")
        .bind(approver.user_id)
        .fetch_one(&pool)
        .await
        .unwrap();
    let refresh_before: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM refresh_tokens rt JOIN devices d ON rt.device_id = d.device_id WHERE d.user_id = $1",
    )
    .bind(approver.user_id)
    .fetch_one(&pool)
    .await
    .unwrap();

    let pool_a = pool.clone();
    let pool_b = pool.clone();
    let code_a = code.clone();
    let code_b = code.clone();

    let (result_a, result_b) = tokio::join!(
        finish_pairing(
            &pool_a,
            pairing_id,
            &code_a,
            TEST_PEPPER,
            TEST_SECRET,
            TEST_ACCESS_TTL,
            TEST_REFRESH_TTL_DAYS,
            TEST_PEPPER,
        ),
        finish_pairing(
            &pool_b,
            pairing_id,
            &code_b,
            TEST_PEPPER,
            TEST_SECRET,
            TEST_ACCESS_TTL,
            TEST_REFRESH_TTL_DAYS,
            TEST_PEPPER,
        )
    );

    let ok_count = [&result_a, &result_b].iter().filter(|r| r.is_ok()).count();
    let already_used_count = [&result_a, &result_b]
        .iter()
        .filter(|r| {
            matches!(
                r,
                Err(wavis_backend::auth::pairing::PairingError::AlreadyUsed)
            )
        })
        .count();
    assert_eq!(
        ok_count, 1,
        "exactly one concurrent finish must win: {result_a:?} / {result_b:?}"
    );
    assert_eq!(
        already_used_count, 1,
        "the loser must see AlreadyUsed, not a duplicate success: {result_a:?} / {result_b:?}"
    );

    let devices_after: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM devices WHERE user_id = $1")
        .bind(approver.user_id)
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(
        devices_after - devices_before,
        1,
        "a race must create exactly one device, never two"
    );

    let refresh_after: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM refresh_tokens rt JOIN devices d ON rt.device_id = d.device_id WHERE d.user_id = $1",
    )
    .bind(approver.user_id)
    .fetch_one(&pool)
    .await
    .unwrap();
    assert_eq!(
        refresh_after - refresh_before,
        1,
        "a race must create exactly one new refresh token"
    );
}

#[tokio::test]
#[ignore]
#[serial]
async fn concurrent_approve_single_winner() {
    let pool = test_pool().await;
    truncate_tables(&pool).await;

    let approver_a = register_user(
        &pool,
        "concurrent-approve-a",
        "trusted-device-a",
        TEST_SECRET,
        TEST_ACCESS_TTL,
        TEST_REFRESH_TTL_DAYS,
        TEST_PEPPER,
        &test_phrase_config(),
        TEST_ENCRYPTION_KEY,
    )
    .await
    .unwrap();
    let approver_b = register_user(
        &pool,
        "concurrent-approve-b",
        "trusted-device-b",
        TEST_SECRET,
        TEST_ACCESS_TTL,
        TEST_REFRESH_TTL_DAYS,
        TEST_PEPPER,
        &test_phrase_config(),
        TEST_ENCRYPTION_KEY,
    )
    .await
    .unwrap();

    let (pairing_id, code) = start_pairing(&pool, "device-race", TEST_PEPPER)
        .await
        .unwrap();

    let pool_a = pool.clone();
    let pool_b = pool.clone();
    let code_a = code.clone();
    let code_b = code.clone();

    let (result_a, result_b) = tokio::join!(
        approve_pairing(
            &pool_a,
            pairing_id,
            &code_a,
            approver_a.user_id,
            approver_a.device_id,
            TEST_PEPPER,
        ),
        approve_pairing(
            &pool_b,
            pairing_id,
            &code_b,
            approver_b.user_id,
            approver_b.device_id,
            TEST_PEPPER,
        )
    );

    let ok_count = [&result_a, &result_b].iter().filter(|r| r.is_ok()).count();
    let already_approved_count = [&result_a, &result_b]
        .iter()
        .filter(|r| {
            matches!(
                r,
                Err(wavis_backend::auth::pairing::PairingError::AlreadyApproved)
            )
        })
        .count();
    assert_eq!(
        ok_count, 1,
        "exactly one concurrent approve must win: {result_a:?} / {result_b:?}"
    );
    assert_eq!(
        already_approved_count, 1,
        "the loser must see AlreadyApproved: {result_a:?} / {result_b:?}"
    );

    let winner_user_id = if result_a.is_ok() {
        approver_a.user_id
    } else {
        approver_b.user_id
    };
    let approved_user_id: uuid::Uuid =
        sqlx::query_scalar("SELECT approved_user_id FROM pairings WHERE pairing_id = $1")
            .bind(pairing_id)
            .fetch_one(&pool)
            .await
            .unwrap();
    assert_eq!(
        approved_user_id, winner_user_id,
        "the persisted approver must match whichever call actually won"
    );
}
