#![cfg(feature = "test-support")]
//! HTTP-level integration tests for bug-report abuse controls (GitHub issue #140):
//! per-IP + per-user rate limiting on submission, the daily per-IP limiter on the
//! LLM-assist ("AI") endpoint, and the admin ban/unban/stats endpoints.
//!
//! All tests require a running Postgres instance.
//! Run with: `cargo test --test bug_report_admin_integration -- --ignored --test-threads=1`
//!
//! The DATABASE_URL env var must point to a test database.
//! Tables are truncated between tests for isolation. ADMIN_TOKEN is set via
//! `std::env::set_var` before each admin test — safe only because these tests
//! run single-threaded (see the `--test-threads=1` invocation above).

use axum::Router;
use axum::body::Body;
use axum::extract::ConnectInfo;
use axum::http::{Request, StatusCode};
use axum::routing::{get, post};
use serde_json::{Value, json};
use sqlx::PgPool;
use std::net::{IpAddr, Ipv4Addr, SocketAddr};
use std::sync::Arc;
use tower::ServiceExt;
use uuid::Uuid;

use wavis_backend::abuse::join_rate_limiter::{JoinRateLimiter, JoinRateLimiterConfig};
use wavis_backend::app_state::AppState;
use wavis_backend::auth::auth_rate_limiter::{AuthRateLimiter, AuthRateLimiterConfig};
use wavis_backend::auth::jwt::sign_access_token;
use wavis_backend::channel::invite::{InviteStore, InviteStoreConfig};
use wavis_backend::diagnostics::admin_routes;
use wavis_backend::diagnostics::bug_report::MockGitHubClient;
use wavis_backend::diagnostics::llm_client::MockLlmClient;
use wavis_backend::diagnostics::routes as bug_routes;
use wavis_backend::ip::IpConfig;
use wavis_backend::voice::mock_sfu_bridge::MockSfuBridge;
use wavis_backend::voice::sfu_bridge::SfuRoomManager;

const TEST_AUTH_SECRET: &[u8] = b"test-auth-secret-at-least-32-bytes!!";
const TEST_PEPPER: &[u8] = b"test-pepper-at-least-32-bytes!!!!!!";
const ADMIN_TOKEN_VALUE: &str = "test-admin-token-value";

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
    sqlx::query("TRUNCATE bug_report_bans, refresh_tokens, devices, users CASCADE")
        .execute(pool)
        .await
        .expect("Failed to truncate tables");
}

async fn register_test_user(pool: &PgPool) -> Uuid {
    let secret = TEST_AUTH_SECRET.to_vec();
    let reg = wavis_backend::auth::auth::register_device(pool, &secret, 30, 30, TEST_PEPPER)
        .await
        .expect("register_device failed");
    reg.user_id
}

async fn sign_test_token(pool: &PgPool, user_id: &Uuid) -> String {
    let device_id: Uuid = sqlx::query_scalar("SELECT device_id FROM devices WHERE user_id = $1")
        .bind(user_id)
        .fetch_one(pool)
        .await
        .expect("device lookup failed");
    sign_access_token(user_id, &device_id, TEST_AUTH_SECRET, 3600, 0)
        .expect("signing should succeed")
}

/// Build a minimal AppState backed by a real DB pool, with mock GitHub + LLM
/// clients so submissions succeed without hitting the network.
fn build_test_app_state(pool: PgPool) -> AppState {
    let mock = Arc::new(MockSfuBridge::new());
    AppState::new(
        mock.clone() as Arc<dyn SfuRoomManager>,
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
        Arc::new(MockGitHubClient::new()),
        "owner/test-repo".to_string(),
        Arc::new(MockLlmClient::new()),
    )
}

fn build_bug_report_router(state: AppState) -> Router {
    Router::new()
        .route("/bug-report", post(bug_routes::submit_bug_report))
        .route("/bug-report/analyze", post(bug_routes::analyze_bug_report))
        .route(
            "/admin/bug-report/stats",
            get(admin_routes::get_bug_report_stats),
        )
        .route(
            "/admin/bug-report/bans/{user_id}",
            post(admin_routes::ban_user).delete(admin_routes::unban_user),
        )
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

fn with_connect_info(mut req: Request<Body>, ip: IpAddr) -> Request<Body> {
    req.extensions_mut()
        .insert(ConnectInfo(SocketAddr::new(ip, 12345)));
    req
}

fn bug_report_body() -> Value {
    json!({
        "title": "Audio drops mid-call",
        "body": "Reproduced by muting then unmuting twice in a row.",
        "category": "audio",
        "screenshot": null,
    })
}

fn submit_bug_report_request(ip: IpAddr, token: Option<&str>) -> Request<Body> {
    let mut builder = Request::builder()
        .method("POST")
        .uri("/bug-report")
        .header("content-type", "application/json");
    if let Some(t) = token {
        builder = builder.header("authorization", format!("Bearer {t}"));
    }
    let req = builder
        .body(Body::from(serde_json::to_vec(&bug_report_body()).unwrap()))
        .unwrap();
    with_connect_info(req, ip)
}

fn analyze_request(ip: IpAddr) -> Request<Body> {
    let body = json!({
        "description": "The app crashed when I clicked mute during a call.",
        "context": {
            "js_console_logs": [],
            "rust_logs": [],
            "ws_messages": [],
            "app_state": {
                "route": "/voice-room",
                "ws_status": "connected",
                "voice_room_state": "active",
                "platform": "windows",
            },
        },
        "previous_answers": null,
    });
    let req = Request::builder()
        .method("POST")
        .uri("/bug-report/analyze")
        .header("content-type", "application/json")
        .body(Body::from(serde_json::to_vec(&body).unwrap()))
        .unwrap();
    with_connect_info(req, ip)
}

fn admin_request(method: &str, uri: &str, token: &str) -> Request<Body> {
    Request::builder()
        .method(method)
        .uri(uri)
        .header("authorization", format!("Bearer {token}"))
        .body(Body::empty())
        .unwrap()
}

// ===========================================================================
// Requirement 2: bug report submission is rate-limited per IP (spammy accounts)
// ===========================================================================

#[tokio::test]
#[ignore]
async fn sixth_bug_report_from_same_ip_is_rejected() {
    let pool = test_pool().await;
    truncate_tables(&pool).await;
    let app = build_bug_report_router(build_test_app_state(pool));
    let ip = IpAddr::V4(Ipv4Addr::new(203, 0, 113, 10));

    for i in 0..5 {
        let (status, body) = send_json(&app, submit_bug_report_request(ip, None)).await;
        assert_eq!(
            status,
            StatusCode::CREATED,
            "submission {} should succeed: {body:?}",
            i + 1
        );
    }

    let (status, body) = send_json(&app, submit_bug_report_request(ip, None)).await;
    assert_eq!(
        status,
        StatusCode::TOO_MANY_REQUESTS,
        "6th submission from same IP should be rate limited: {body:?}"
    );
}

// ===========================================================================
// Requirement 1: AI (LLM-assist) endpoint is rate-limited 5/day per IP
// ===========================================================================

#[tokio::test]
#[ignore]
async fn sixth_ai_analyze_call_from_same_ip_is_rejected() {
    let pool = test_pool().await;
    truncate_tables(&pool).await;
    let app = build_bug_report_router(build_test_app_state(pool));
    let ip = IpAddr::V4(Ipv4Addr::new(203, 0, 113, 11));

    for i in 0..5 {
        let (status, body) = send_json(&app, analyze_request(ip)).await;
        assert_eq!(
            status,
            StatusCode::OK,
            "analyze call {} should succeed: {body:?}",
            i + 1
        );
    }

    let (status, body) = send_json(&app, analyze_request(ip)).await;
    assert_eq!(
        status,
        StatusCode::TOO_MANY_REQUESTS,
        "6th AI analyze call from same IP within the day should be rate limited: {body:?}"
    );
}

// ===========================================================================
// Requirement 3: admin can ban a specific spamming user; ban is enforced;
// unban restores access.
// ===========================================================================

#[tokio::test]
#[ignore]
async fn banned_user_is_rejected_then_unban_restores_access() {
    unsafe {
        std::env::set_var("ADMIN_TOKEN", ADMIN_TOKEN_VALUE);
    }
    let pool = test_pool().await;
    truncate_tables(&pool).await;
    let user_id = register_test_user(&pool).await;
    let token = sign_test_token(&pool, &user_id).await;
    let app = build_bug_report_router(build_test_app_state(pool));
    let ip = IpAddr::V4(Ipv4Addr::new(203, 0, 113, 12));

    // Sanity: user can submit before being banned.
    let (status, body) = send_json(&app, submit_bug_report_request(ip, Some(&token))).await;
    assert_eq!(status, StatusCode::CREATED, "pre-ban submission: {body:?}");

    // Admin bans the user.
    let ban_req = admin_request(
        "POST",
        &format!("/admin/bug-report/bans/{user_id}"),
        ADMIN_TOKEN_VALUE,
    );
    let mut ban_req = ban_req;
    *ban_req.body_mut() = Body::from(serde_json::to_vec(&json!({"reason": "spam"})).unwrap());
    ban_req
        .headers_mut()
        .insert("content-type", "application/json".parse().unwrap());
    let (status, body) = send_json(&app, ban_req).await;
    assert_eq!(
        status,
        StatusCode::OK,
        "ban request should succeed: {body:?}"
    );

    // A fresh IP so we isolate the ban check from the per-IP rate limiter.
    let ip2 = IpAddr::V4(Ipv4Addr::new(203, 0, 113, 13));
    let (status, body) = send_json(&app, submit_bug_report_request(ip2, Some(&token))).await;
    assert_eq!(
        status,
        StatusCode::FORBIDDEN,
        "banned user's submission should be rejected: {body:?}"
    );

    // Admin unbans the user.
    let unban_req = admin_request(
        "DELETE",
        &format!("/admin/bug-report/bans/{user_id}"),
        ADMIN_TOKEN_VALUE,
    );
    let (status, body) = send_json(&app, unban_req).await;
    assert_eq!(
        status,
        StatusCode::OK,
        "unban request should succeed: {body:?}"
    );

    let ip3 = IpAddr::V4(Ipv4Addr::new(203, 0, 113, 14));
    let (status, body) = send_json(&app, submit_bug_report_request(ip3, Some(&token))).await;
    assert_eq!(
        status,
        StatusCode::CREATED,
        "unbanned user's submission should succeed: {body:?}"
    );
}

#[tokio::test]
#[ignore]
async fn admin_endpoints_reject_missing_or_wrong_token() {
    unsafe {
        std::env::set_var("ADMIN_TOKEN", ADMIN_TOKEN_VALUE);
    }
    let pool = test_pool().await;
    truncate_tables(&pool).await;
    let app = build_bug_report_router(build_test_app_state(pool));

    let no_token = Request::builder()
        .method("GET")
        .uri("/admin/bug-report/stats")
        .body(Body::empty())
        .unwrap();
    let (status, _) = send_json(&app, no_token).await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);

    let wrong_token = admin_request("GET", "/admin/bug-report/stats", "wrong-token");
    let (status, _) = send_json(&app, wrong_token).await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
}

// ===========================================================================
// Requirement 4: admin can see who is being a spammy user
// ===========================================================================

#[tokio::test]
#[ignore]
async fn admin_stats_shows_active_ip_counts_and_banned_users() {
    unsafe {
        std::env::set_var("ADMIN_TOKEN", ADMIN_TOKEN_VALUE);
    }
    let pool = test_pool().await;
    truncate_tables(&pool).await;
    let user_id = register_test_user(&pool).await;
    let app = build_bug_report_router(build_test_app_state(pool));
    let ip = IpAddr::V4(Ipv4Addr::new(203, 0, 113, 15));

    for _ in 0..2 {
        let (status, body) = send_json(&app, submit_bug_report_request(ip, None)).await;
        assert_eq!(status, StatusCode::CREATED, "seed submission: {body:?}");
    }

    let ban_req = admin_request(
        "POST",
        &format!("/admin/bug-report/bans/{user_id}"),
        ADMIN_TOKEN_VALUE,
    );
    let mut ban_req = ban_req;
    *ban_req.body_mut() = Body::from(serde_json::to_vec(&json!({"reason": "spam"})).unwrap());
    ban_req
        .headers_mut()
        .insert("content-type", "application/json".parse().unwrap());
    send_json(&app, ban_req).await;

    let stats_req = admin_request("GET", "/admin/bug-report/stats", ADMIN_TOKEN_VALUE);
    let (status, body) = send_json(&app, stats_req).await;
    assert_eq!(
        status,
        StatusCode::OK,
        "stats request should succeed: {body:?}"
    );

    let ip_counts = body["active_ip_counts"].as_array().expect("array");
    assert!(
        ip_counts
            .iter()
            .any(|e| e["ip"] == ip.to_string() && e["count"] == 2),
        "expected ip {ip} with count 2 in {ip_counts:?}"
    );

    let banned = body["banned_users"].as_array().expect("array");
    assert!(
        banned.iter().any(|e| e["user_id"] == user_id.to_string()),
        "expected banned user {user_id} in {banned:?}"
    );
}
