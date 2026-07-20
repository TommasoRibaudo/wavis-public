#![cfg(feature = "test-support")]
//! Integration tests for account recovery domain layer.
//!
//! Tests Properties 5, 6, 17 from the user-identity-recovery spec.
//! All tests require a running Postgres instance.
//! Run with: `cargo test --features test-support --test recovery_integration -- --ignored`
//!
//! Gated on `test-support` (added for P1-6's handler-level anti-oracle test,
//! which needs `MockSfuBridge` to build a full `AppState`) — matches the
//! pattern in `bug_report_admin_integration.rs` / `channel_rest_integration.rs`.

use axum::Router;
use axum::body::Body;
use axum::extract::ConnectInfo;
use axum::http::{Request, StatusCode};
use axum::routing::post;
use serde_json::{Value, json};
use serial_test::serial;
use sqlx::PgPool;
use std::net::{IpAddr, Ipv4Addr, SocketAddr};
use std::sync::Arc;
use tower::ServiceExt;
use uuid::Uuid;

use wavis_backend::abuse::join_rate_limiter::{JoinRateLimiter, JoinRateLimiterConfig};
use wavis_backend::app_state::AppState;
use wavis_backend::auth::auth::{
    AuthError, DeviceRegistration, recover_account, register_user, rotate_phrase,
};
use wavis_backend::auth::auth_rate_limiter::{AuthRateLimiter, AuthRateLimiterConfig};
use wavis_backend::auth::jwt::validate_access_token;
use wavis_backend::auth::phrase::{self, PhraseConfig};
use wavis_backend::auth::recovery_rate_limiter::{RecoveryRateLimiter, RecoveryRateLimiterConfig};
use wavis_backend::auth::routes as auth_routes;
use wavis_backend::channel::channel;
use wavis_backend::ip::IpConfig;
use wavis_backend::voice::mock_sfu_bridge::MockSfuBridge;
use wavis_backend::voice::sfu_bridge::SfuRoomManager;

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
// Feature: user-identity-recovery, Property 5: Username preserved on recovery
// A stale display name from another device must never clobber the account's
// server-side username. Regression test for fix/device-name-persistence-105.
// Validates: Requirements 5.1 (recovery does not mutate account identity)
// ---------------------------------------------------------------------------

#[tokio::test]
#[ignore]
#[serial]
async fn prop5_recovery_preserves_existing_username() {
    let pool = test_pool().await;
    truncate_tables(&pool).await;

    let phrase = "my-recovery-phrase";
    let dummy = phrase::generate_dummy_verifier(&test_phrase_config());

    // Register with a known username.
    let reg = register_test_user(&pool, phrase, "original-name").await;

    // Confirm the server stored the original username.
    let stored: Option<String> =
        sqlx::query_scalar("SELECT username FROM users WHERE user_id = $1")
            .bind(reg.user_id)
            .fetch_optional(&pool)
            .await
            .unwrap();
    assert_eq!(stored.as_deref(), Some("original-name"));

    // Recovery call supplies a *different* (stale/leaked) username — simulating
    // what happens when a second device has a cached display name from a
    // different account logged in previously.
    let _recovered = recover_account(
        &pool,
        &reg.recovery_id,
        phrase,
        "stale-leaked-name",
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

    // The account username must NOT have been overwritten.
    let after: Option<String> = sqlx::query_scalar("SELECT username FROM users WHERE user_id = $1")
        .bind(reg.user_id)
        .fetch_optional(&pool)
        .await
        .unwrap();
    assert_eq!(
        after.as_deref(),
        Some("original-name"),
        "recovery must not overwrite an existing username (got: {:?})",
        after,
    );
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

// ---------------------------------------------------------------------------
// P1-6: Recovery rate-limiter anti-oracle (handler-level, not domain-level).
//
// `POST /auth/recover` must never let an attacker distinguish "recovery_id
// doesn't exist" from "recovery_id exists but budget exhausted" by watching
// the rate limiter. The per-recovery_id attempt is only recorded when the ID
// was found in the DB (success or PhraseVerificationFailed) — never for
// RecoveryIdNotFound (auth/routes.rs recover(), lines ~403-429).
// ---------------------------------------------------------------------------

/// Build a minimal AppState backed by a real DB pool, matching main.rs wiring
/// closely enough to exercise the `recover` handler's rate-limiter behavior.
fn build_test_app_state(pool: PgPool) -> AppState {
    use wavis_backend::channel::invite::{InviteStore, InviteStoreConfig};
    use wavis_backend::diagnostics::bug_report::MockGitHubClient;
    use wavis_backend::diagnostics::llm_client::NoOpLlmClient;

    let mock = Arc::new(MockSfuBridge::new());
    AppState::new(
        mock as Arc<dyn SfuRoomManager>,
        None,
        "sfu://localhost".to_string(),
        Arc::new(InviteStore::new(InviteStoreConfig::default())),
        Arc::new(JoinRateLimiter::new(JoinRateLimiterConfig::default())),
        IpConfig {
            trust_proxy_headers: false,
            trusted_proxy_cidrs: vec![],
        },
        Arc::new(b"dev-secret-32-bytes-minimum!!!XX".to_vec()),
        None,
        "wavis-backend".to_string(),
        pool,
        Arc::new(TEST_SECRET.to_vec()),
        None,
        Arc::new(AuthRateLimiter::new(AuthRateLimiterConfig::default())),
        TEST_REFRESH_TTL_DAYS,
        72,
        Arc::new(TEST_PEPPER.to_vec()),
        None,
        Arc::new(phrase::generate_dummy_verifier(&test_phrase_config())),
        Arc::new(b"test-pairing-pepper-32-bytes!!XX".to_vec()),
        Arc::new(RecoveryRateLimiter::new(
            RecoveryRateLimiterConfig::default(),
        )),
        Arc::new(test_phrase_config()),
        Arc::new(TEST_ENCRYPTION_KEY.to_vec()),
        24,
        7,
        Arc::new(MockGitHubClient::new()),
        "owner/test-repo".to_string(),
        Arc::new(NoOpLlmClient),
    )
}

fn build_recover_router(state: AppState) -> Router {
    Router::new()
        .route("/auth/recover", post(auth_routes::recover))
        .with_state(state)
}

fn with_connect_info(mut req: Request<Body>, ip: IpAddr) -> Request<Body> {
    req.extensions_mut()
        .insert(ConnectInfo(SocketAddr::new(ip, 12345)));
    req
}

fn recover_request(ip: IpAddr, recovery_id: &str, phrase: &str) -> Request<Body> {
    let body = json!({ "recovery_id": recovery_id, "phrase": phrase });
    let req = Request::builder()
        .method("POST")
        .uri("/auth/recover")
        .header("content-type", "application/json")
        .body(Body::from(serde_json::to_vec(&body).unwrap()))
        .unwrap();
    with_connect_info(req, ip)
}

async fn send_recover(app: &Router, req: Request<Body>) -> (StatusCode, Value) {
    let response = app.clone().oneshot(req).await.unwrap();
    let status = response.status();
    let body_bytes = axum::body::to_bytes(response.into_body(), 1024 * 64)
        .await
        .unwrap();
    let body: Value = if body_bytes.is_empty() {
        Value::Null
    } else {
        serde_json::from_slice(&body_bytes).unwrap_or(Value::Null)
    };
    (status, body)
}

#[tokio::test]
#[ignore]
#[serial]
async fn unknown_recovery_id_attempts_do_not_consume_per_rid_budget() {
    let pool = test_pool().await;
    truncate_tables(&pool).await;

    let phrase = "budget-anti-oracle-phrase";
    let reg = register_test_user(&pool, phrase, "budget-device").await;

    let app = build_recover_router(build_test_app_state(pool));
    let ip = IpAddr::V4(Ipv4Addr::new(198, 51, 100, 7));

    // 5 attempts against a recovery_id that does not exist. per_rid_max is 3,
    // but since the ID is never found, the per-rid attempt must never be
    // recorded — all 5 must return 401, none must ever hit 429.
    for attempt in 1..=5 {
        let (status, _body) = send_recover(
            &app,
            recover_request(ip, "wvs-XXXX-FAKE", "irrelevant-phrase"),
        )
        .await;
        assert_eq!(
            status,
            StatusCode::UNAUTHORIZED,
            "unknown-recovery_id attempt {attempt} must be 401, never 429 (would leak that the ID is unknown but budget-exhausted)"
        );
    }

    // Now spend the real recovery_id's budget (per_rid_max = 3) with a wrong
    // phrase. Each of the first 3 must be 401 (found, phrase wrong) — proving
    // the 5 unknown-ID attempts above left this budget untouched.
    for attempt in 1..=3 {
        let (status, _body) =
            send_recover(&app, recover_request(ip, &reg.recovery_id, "wrong-phrase")).await;
        assert_eq!(
            status,
            StatusCode::UNAUTHORIZED,
            "real recovery_id attempt {attempt}/3 must still be within budget (401), got {status}"
        );
    }

    // The 4th attempt against the real ID must now be rate-limited — proving
    // the budget is real (3, not inflated by the unknown-ID noise) and that
    // it was consumed only by the real-ID attempts.
    let (status, _body) =
        send_recover(&app, recover_request(ip, &reg.recovery_id, "wrong-phrase")).await;
    assert_eq!(
        status,
        StatusCode::TOO_MANY_REQUESTS,
        "4th attempt against the real recovery_id must be 429 once its own budget (3) is exhausted"
    );
}
