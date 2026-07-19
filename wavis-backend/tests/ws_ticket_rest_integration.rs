#![cfg(feature = "test-support")]
//! HTTP-level integration tests for `POST /auth/ws-ticket` (issue #267).
//!
//! Covers the REST-level acceptance criteria:
//!   - Authenticated ticket request succeeds
//!   - Unauthenticated ticket request fails
//!
//! All tests require a running Postgres instance.
//! Run with: `cargo test --test ws_ticket_rest_integration --features test-support -- --ignored --test-threads=1`

use axum::Router;
use axum::body::Body;
use axum::http::{Request, StatusCode};
use axum::routing::post;
use serde_json::Value;
use serial_test::serial;
use sqlx::PgPool;
use std::sync::Arc;
use tower::ServiceExt;
use uuid::Uuid;

use wavis_backend::abuse::join_rate_limiter::{JoinRateLimiter, JoinRateLimiterConfig};
use wavis_backend::app_state::AppState;
use wavis_backend::auth::auth_rate_limiter::{AuthRateLimiter, AuthRateLimiterConfig};
use wavis_backend::auth::jwt::sign_access_token;
use wavis_backend::auth::routes as auth_routes;
use wavis_backend::ip::IpConfig;
use wavis_backend::voice::mock_sfu_bridge::MockSfuBridge;
use wavis_backend::voice::sfu_bridge::SfuRoomManager;

const TEST_AUTH_SECRET: &[u8] = b"test-auth-secret-at-least-32-bytes!!";
const TEST_PEPPER: &[u8] = b"test-pepper-at-least-32-bytes!!!!!!";

// ---------------------------------------------------------------------------
// Test helpers
// ---------------------------------------------------------------------------

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
    sqlx::query("TRUNCATE refresh_tokens, devices, users CASCADE")
        .execute(pool)
        .await
        .expect("Failed to truncate tables");
}

/// Register a test device and return (user_id, device_id).
async fn register_test_user(pool: &PgPool) -> (Uuid, Uuid) {
    let reg =
        wavis_backend::auth::auth::register_device(pool, TEST_AUTH_SECRET, 30, 30, TEST_PEPPER)
            .await
            .expect("register_device failed");
    (reg.user_id, reg.device_id)
}

fn sign_test_token(user_id: &Uuid, device_id: &Uuid) -> String {
    sign_access_token(user_id, device_id, TEST_AUTH_SECRET, 3600, 0)
        .expect("signing should succeed")
}

/// Build a minimal AppState backed by a real DB pool (mirrors channel_rest_integration.rs).
fn build_test_app_state(pool: PgPool) -> AppState {
    let mock = Arc::new(MockSfuBridge::new());
    AppState::new(
        mock.clone() as Arc<dyn SfuRoomManager>,
        None,
        "sfu://localhost".to_string(),
        Arc::new(wavis_backend::channel::invite::InviteStore::new(
            wavis_backend::channel::invite::InviteStoreConfig::default(),
        )),
        Arc::new(JoinRateLimiter::new(JoinRateLimiterConfig::default())),
        IpConfig {
            trust_proxy_headers: false,
            trusted_proxy_cidrs: vec![],
        },
        Arc::new(b"dev-secret-32-bytes-minimum!!!XX".to_vec()),
        None,
        "wavis-backend".to_string(),
        pool,
        Arc::new(TEST_AUTH_SECRET.to_vec()),
        None,
        Arc::new(AuthRateLimiter::new(AuthRateLimiterConfig::default())),
        30,
        72,
        Arc::new(TEST_PEPPER.to_vec()),
        None,
        Arc::new(wavis_backend::auth::phrase::generate_dummy_verifier(
            &wavis_backend::auth::phrase::PhraseConfig::default(),
        )),
        Arc::new(b"test-pairing-pepper-32-bytes!!XX".to_vec()),
        Arc::new(
            wavis_backend::auth::recovery_rate_limiter::RecoveryRateLimiter::new(
                wavis_backend::auth::recovery_rate_limiter::RecoveryRateLimiterConfig::default(),
            ),
        ),
        Arc::new(wavis_backend::auth::phrase::PhraseConfig::default()),
        Arc::new(vec![0u8; 32]),
        24,
        7,
        Arc::new(wavis_backend::diagnostics::bug_report::MockGitHubClient::new()),
        "owner/test-repo".to_string(),
        Arc::new(wavis_backend::diagnostics::llm_client::NoOpLlmClient),
    )
}

fn build_router(state: AppState) -> Router {
    Router::new()
        .route("/auth/ws-ticket", post(auth_routes::issue_ws_ticket))
        .with_state(state)
}

async fn send_json(app: &Router, req: Request<Body>) -> (StatusCode, Value) {
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

fn post_empty_with_auth(uri: &str, token: Option<&str>) -> Request<Body> {
    let mut builder = Request::builder().method("POST").uri(uri);
    if let Some(t) = token {
        builder = builder.header("authorization", format!("Bearer {t}"));
    }
    builder.body(Body::empty()).unwrap()
}

// ---------------------------------------------------------------------------
// Authenticated ticket request succeeds
// ---------------------------------------------------------------------------

#[tokio::test]
#[ignore]
#[serial]
async fn authenticated_ws_ticket_request_succeeds_and_consumes_once() {
    let pool = test_pool().await;
    truncate_tables(&pool).await;

    let (user_id, device_id) = register_test_user(&pool).await;
    let token = sign_test_token(&user_id, &device_id);

    let state = build_test_app_state(pool);
    let app = build_router(state.clone());

    let (status, body) =
        send_json(&app, post_empty_with_auth("/auth/ws-ticket", Some(&token))).await;

    assert_eq!(status, StatusCode::OK);
    let ticket = body["ticket"]
        .as_str()
        .expect("response must include a ticket string")
        .to_string();
    assert!(!ticket.is_empty(), "ticket must not be empty");
    assert!(
        body["expires_in"].as_u64().unwrap() > 0,
        "expires_in must be positive"
    );

    // The minted ticket must resolve to the authenticated caller's identity,
    // and be consumable exactly once.
    let resolved = state
        .ws_ticket_store
        .validate_and_consume(&ticket)
        .expect("freshly-issued ticket must validate");
    assert_eq!(resolved, (user_id, device_id));

    let replay = state.ws_ticket_store.validate_and_consume(&ticket);
    assert!(replay.is_err(), "ticket must not be consumable twice");
}

// ---------------------------------------------------------------------------
// Unauthenticated ticket request fails
// ---------------------------------------------------------------------------

#[tokio::test]
#[ignore]
#[serial]
async fn unauthenticated_ws_ticket_request_fails() {
    let pool = test_pool().await;
    truncate_tables(&pool).await;

    let state = build_test_app_state(pool);
    let app = build_router(state);

    let (status, _body) = send_json(&app, post_empty_with_auth("/auth/ws-ticket", None)).await;

    assert_eq!(
        status,
        StatusCode::UNAUTHORIZED,
        "missing Authorization header must be rejected"
    );
}

#[tokio::test]
#[ignore]
#[serial]
async fn ws_ticket_request_with_garbage_bearer_token_fails() {
    let pool = test_pool().await;
    truncate_tables(&pool).await;

    let state = build_test_app_state(pool);
    let app = build_router(state);

    let (status, _body) = send_json(
        &app,
        post_empty_with_auth("/auth/ws-ticket", Some("not-a-real-jwt")),
    )
    .await;

    assert_eq!(
        status,
        StatusCode::UNAUTHORIZED,
        "an invalid Bearer token must be rejected"
    );
}
