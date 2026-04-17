#![cfg(feature = "test-support")]
//! HTTP-level integration tests for Channel Membership REST endpoints.
//!
//! Automates the manual test steps from doc/testing/backend-manual.md Section 27.
//! All tests require a running Postgres instance.
//! Run with: `cargo test --test channel_rest_integration -- --ignored --test-threads=1`
//!
//! The DATABASE_URL env var must point to a test database.
//! Tables are truncated between tests for isolation.

use axum::Router;
use axum::body::Body;
use axum::http::{Request, StatusCode};
use axum::routing::{delete, get, post, put};
use serde_json::Value;
use sqlx::PgPool;
use std::sync::Arc;
use tower::ServiceExt;
use uuid::Uuid;

use wavis_backend::abuse::join_rate_limiter::{JoinRateLimiter, JoinRateLimiterConfig};
use wavis_backend::app_state::AppState;
use wavis_backend::auth::auth_rate_limiter::{AuthRateLimiter, AuthRateLimiterConfig};
use wavis_backend::auth::jwt::sign_access_token;
use wavis_backend::channel::invite::{InviteStore, InviteStoreConfig};
use wavis_backend::channel::routes as channel_routes;
use wavis_backend::ip::IpConfig;
use wavis_backend::voice::mock_sfu_bridge::MockSfuBridge;
use wavis_backend::voice::sfu_bridge::SfuRoomManager;

const TEST_AUTH_SECRET: &[u8] = b"test-auth-secret-at-least-32-bytes!!";

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
    sqlx::query(
        "TRUNCATE channel_invites, channel_memberships, channels, \
         refresh_tokens, devices, users CASCADE",
    )
    .execute(pool)
    .await
    .expect("Failed to truncate tables");
}

/// Register a test device and return its user_id.
const TEST_PEPPER: &[u8] = b"test-pepper-at-least-32-bytes!!!!!!";

async fn register_test_user(pool: &PgPool) -> Uuid {
    let secret = TEST_AUTH_SECRET.to_vec();
    let reg = wavis_backend::auth::auth::register_device(pool, &secret, 30, 30, TEST_PEPPER)
        .await
        .expect("register_device failed");
    reg.user_id
}

/// Sign an access token for a test user.
fn sign_test_token(user_id: &Uuid) -> String {
    sign_access_token(user_id, &Uuid::nil(), TEST_AUTH_SECRET, 3600, 0)
        .expect("signing should succeed")
}

/// Build a minimal AppState backed by a real DB pool.
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
        Arc::new(wavis_backend::diagnostics::bug_report::MockGitHubClient::new()),
        "owner/test-repo".to_string(),
        Arc::new(wavis_backend::diagnostics::llm_client::NoOpLlmClient),
    )
}

/// Build the channel REST router matching main.rs route layout.
fn build_channel_router(state: AppState) -> Router {
    Router::new()
        .route(
            "/channels",
            post(channel_routes::create_channel).get(channel_routes::list_channels),
        )
        .route("/channels/join", post(channel_routes::join_channel))
        .route(
            "/channels/{channel_id}",
            get(channel_routes::get_channel).delete(channel_routes::delete_channel),
        )
        .route(
            "/channels/{channel_id}/invites",
            post(channel_routes::create_invite).get(channel_routes::list_invites),
        )
        .route(
            "/channels/{channel_id}/invites/{code}",
            delete(channel_routes::revoke_invite),
        )
        .route(
            "/channels/{channel_id}/leave",
            post(channel_routes::leave_channel),
        )
        .route(
            "/channels/{channel_id}/bans",
            get(channel_routes::list_bans),
        )
        .route(
            "/channels/{channel_id}/bans/{user_id}",
            post(channel_routes::ban_member).delete(channel_routes::unban_member),
        )
        .route(
            "/channels/{channel_id}/members/{user_id}/role",
            put(channel_routes::change_role),
        )
        .route(
            "/channels/{channel_id}/voice",
            get(channel_routes::get_voice_status),
        )
        .with_state(state)
}

/// Helper: send a request and return (status, body as serde_json::Value).
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

/// Build a JSON POST request with auth.
fn post_json(uri: &str, token: &str, body: &Value) -> Request<Body> {
    Request::builder()
        .method("POST")
        .uri(uri)
        .header("authorization", format!("Bearer {}", token))
        .header("content-type", "application/json")
        .body(Body::from(serde_json::to_vec(body).unwrap()))
        .unwrap()
}

/// Build a JSON PUT request with auth.
fn put_json(uri: &str, token: &str, body: &Value) -> Request<Body> {
    Request::builder()
        .method("PUT")
        .uri(uri)
        .header("authorization", format!("Bearer {}", token))
        .header("content-type", "application/json")
        .body(Body::from(serde_json::to_vec(body).unwrap()))
        .unwrap()
}

/// Build a GET request with auth.
fn get_auth(uri: &str, token: &str) -> Request<Body> {
    Request::builder()
        .method("GET")
        .uri(uri)
        .header("authorization", format!("Bearer {}", token))
        .body(Body::empty())
        .unwrap()
}

/// Build a DELETE request with auth.
fn delete_auth(uri: &str, token: &str) -> Request<Body> {
    Request::builder()
        .method("DELETE")
        .uri(uri)
        .header("authorization", format!("Bearer {}", token))
        .body(Body::empty())
        .unwrap()
}

/// Build a POST request with auth but no body.
fn post_empty(uri: &str, token: &str) -> Request<Body> {
    Request::builder()
        .method("POST")
        .uri(uri)
        .header("authorization", format!("Bearer {}", token))
        .body(Body::empty())
        .unwrap()
}

// ===========================================================================
// 27.1 Create a channel
// ===========================================================================

#[tokio::test]
#[ignore]
async fn test_27_1_create_channel_happy_path() {
    let pool = test_pool().await;
    truncate_tables(&pool).await;

    let owner = register_test_user(&pool).await;
    let token = sign_test_token(&owner);
    let app = build_channel_router(build_test_app_state(pool));

    let (status, body) = send_json(
        &app,
        post_json(
            "/channels",
            &token,
            &serde_json::json!({"name": "Test Channel"}),
        ),
    )
    .await;

    assert_eq!(status, StatusCode::CREATED);
    assert_eq!(body["name"], "Test Channel");
    assert!(!body["channel_id"].as_str().unwrap().is_empty());
    assert_eq!(body["owner_user_id"], owner.to_string());
    assert!(body["created_at"].as_str().is_some());
}

#[tokio::test]
#[ignore]
async fn test_27_1_create_channel_empty_name() {
    let pool = test_pool().await;
    truncate_tables(&pool).await;

    let owner = register_test_user(&pool).await;
    let token = sign_test_token(&owner);
    let app = build_channel_router(build_test_app_state(pool));

    let (status, body) = send_json(
        &app,
        post_json("/channels", &token, &serde_json::json!({"name": ""})),
    )
    .await;

    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert!(
        body["error"]
            .as_str()
            .unwrap()
            .contains("invalid channel name")
    );
}

#[tokio::test]
#[ignore]
async fn test_27_1_create_channel_name_too_long() {
    let pool = test_pool().await;
    truncate_tables(&pool).await;

    let owner = register_test_user(&pool).await;
    let token = sign_test_token(&owner);
    let app = build_channel_router(build_test_app_state(pool));

    let long_name = "x".repeat(101);
    let (status, _body) = send_json(
        &app,
        post_json("/channels", &token, &serde_json::json!({"name": long_name})),
    )
    .await;

    assert_eq!(status, StatusCode::BAD_REQUEST);
}

#[tokio::test]
#[ignore]
async fn test_27_1_create_channel_no_auth() {
    let pool = test_pool().await;
    truncate_tables(&pool).await;
    let app = build_channel_router(build_test_app_state(pool));

    let req = Request::builder()
        .method("POST")
        .uri("/channels")
        .header("content-type", "application/json")
        .body(Body::from(r#"{"name":"No Auth"}"#))
        .unwrap();

    let (status, body) = send_json(&app, req).await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
    assert_eq!(body["error"], "authentication failed");
}

// ===========================================================================
// 27.2 List channels
// ===========================================================================

#[tokio::test]
#[ignore]
async fn test_27_2_list_channels() {
    let pool = test_pool().await;
    truncate_tables(&pool).await;

    let owner = register_test_user(&pool).await;
    let token = sign_test_token(&owner);
    let app = build_channel_router(build_test_app_state(pool.clone()));

    // Create two channels
    send_json(
        &app,
        post_json("/channels", &token, &serde_json::json!({"name": "Ch1"})),
    )
    .await;
    send_json(
        &app,
        post_json("/channels", &token, &serde_json::json!({"name": "Ch2"})),
    )
    .await;

    let (status, body) = send_json(&app, get_auth("/channels", &token)).await;
    assert_eq!(status, StatusCode::OK);

    let arr = body.as_array().expect("should be array");
    assert_eq!(arr.len(), 2);
    // Owner should have "owner" role on both
    for item in arr {
        assert_eq!(item["role"], "owner");
    }
}

// ===========================================================================
// 27.3 Get channel detail
// ===========================================================================

#[tokio::test]
#[ignore]
async fn test_27_3_get_channel_detail() {
    let pool = test_pool().await;
    truncate_tables(&pool).await;

    let owner = register_test_user(&pool).await;
    let token = sign_test_token(&owner);
    let app = build_channel_router(build_test_app_state(pool));

    let (_, create_body) = send_json(
        &app,
        post_json(
            "/channels",
            &token,
            &serde_json::json!({"name": "Detail Test"}),
        ),
    )
    .await;
    let channel_id = create_body["channel_id"].as_str().unwrap();

    let (status, body) =
        send_json(&app, get_auth(&format!("/channels/{}", channel_id), &token)).await;

    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["name"], "Detail Test");
    let members = body["members"].as_array().unwrap();
    assert_eq!(members.len(), 1);
    assert_eq!(members[0]["role"], "owner");
}

#[tokio::test]
#[ignore]
async fn test_27_3_get_channel_non_member() {
    let pool = test_pool().await;
    truncate_tables(&pool).await;

    let owner = register_test_user(&pool).await;
    let outsider = register_test_user(&pool).await;
    let owner_token = sign_test_token(&owner);
    let outsider_token = sign_test_token(&outsider);
    let app = build_channel_router(build_test_app_state(pool));

    let (_, create_body) = send_json(
        &app,
        post_json(
            "/channels",
            &owner_token,
            &serde_json::json!({"name": "Private"}),
        ),
    )
    .await;
    let channel_id = create_body["channel_id"].as_str().unwrap();

    let (status, _) = send_json(
        &app,
        get_auth(&format!("/channels/{}", channel_id), &outsider_token),
    )
    .await;
    assert_eq!(status, StatusCode::FORBIDDEN);
}

#[tokio::test]
#[ignore]
async fn test_27_3_get_channel_not_found() {
    let pool = test_pool().await;
    truncate_tables(&pool).await;

    let user = register_test_user(&pool).await;
    let token = sign_test_token(&user);
    let app = build_channel_router(build_test_app_state(pool));

    let fake_id = Uuid::new_v4();
    let (status, _) = send_json(&app, get_auth(&format!("/channels/{}", fake_id), &token)).await;
    // Non-member of non-existent channel â†’ opaque error (403 or 404)
    assert!(status == StatusCode::NOT_FOUND || status == StatusCode::FORBIDDEN);
}

// ===========================================================================
// 27.4 Create channel invite
// ===========================================================================

#[tokio::test]
#[ignore]
async fn test_27_4_create_invite_happy_path() {
    let pool = test_pool().await;
    truncate_tables(&pool).await;

    let owner = register_test_user(&pool).await;
    let token = sign_test_token(&owner);
    let app = build_channel_router(build_test_app_state(pool));

    let (_, create_body) = send_json(
        &app,
        post_json(
            "/channels",
            &token,
            &serde_json::json!({"name": "Invite Test"}),
        ),
    )
    .await;
    let channel_id = create_body["channel_id"].as_str().unwrap();

    let (status, body) = send_json(
        &app,
        post_json(
            &format!("/channels/{}/invites", channel_id),
            &token,
            &serde_json::json!({"expires_in_secs": 3600, "max_uses": 5}),
        ),
    )
    .await;

    assert_eq!(status, StatusCode::CREATED);
    assert!(!body["code"].as_str().unwrap().is_empty());
    assert_eq!(body["max_uses"], 5);
    assert_eq!(body["uses"], 0);
}

#[tokio::test]
#[ignore]
async fn test_27_4_create_invite_member_rejected() {
    let pool = test_pool().await;
    truncate_tables(&pool).await;

    let owner = register_test_user(&pool).await;
    let member = register_test_user(&pool).await;
    let owner_token = sign_test_token(&owner);
    let member_token = sign_test_token(&member);
    let app = build_channel_router(build_test_app_state(pool));

    // Create channel and invite member
    let (_, create_body) = send_json(
        &app,
        post_json(
            "/channels",
            &owner_token,
            &serde_json::json!({"name": "Auth Test"}),
        ),
    )
    .await;
    let channel_id = create_body["channel_id"].as_str().unwrap();

    let (_, invite_body) = send_json(
        &app,
        post_json(
            &format!("/channels/{}/invites", channel_id),
            &owner_token,
            &serde_json::json!({"expires_in_secs": 3600, "max_uses": 5}),
        ),
    )
    .await;
    let code = invite_body["code"].as_str().unwrap();

    // Member joins
    send_json(
        &app,
        post_json(
            "/channels/join",
            &member_token,
            &serde_json::json!({"code": code}),
        ),
    )
    .await;

    // Member tries to create invite â†’ 403
    let (status, _) = send_json(
        &app,
        post_json(
            &format!("/channels/{}/invites", channel_id),
            &member_token,
            &serde_json::json!({"expires_in_secs": 3600, "max_uses": 3}),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::FORBIDDEN);
}

// ===========================================================================
// 27.5 Join channel via invite
// ===========================================================================

#[tokio::test]
#[ignore]
async fn test_27_5_join_channel_happy_path() {
    let pool = test_pool().await;
    truncate_tables(&pool).await;

    let owner = register_test_user(&pool).await;
    let joiner = register_test_user(&pool).await;
    let owner_token = sign_test_token(&owner);
    let joiner_token = sign_test_token(&joiner);
    let app = build_channel_router(build_test_app_state(pool));

    let (_, create_body) = send_json(
        &app,
        post_json(
            "/channels",
            &owner_token,
            &serde_json::json!({"name": "Join Test"}),
        ),
    )
    .await;
    let channel_id = create_body["channel_id"].as_str().unwrap();

    let (_, invite_body) = send_json(
        &app,
        post_json(
            &format!("/channels/{}/invites", channel_id),
            &owner_token,
            &serde_json::json!({"expires_in_secs": 3600, "max_uses": 5}),
        ),
    )
    .await;
    let code = invite_body["code"].as_str().unwrap();

    let (status, body) = send_json(
        &app,
        post_json(
            "/channels/join",
            &joiner_token,
            &serde_json::json!({"code": code}),
        ),
    )
    .await;

    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["channel_id"], channel_id);
    assert_eq!(body["role"], "member");
}

#[tokio::test]
#[ignore]
async fn test_27_5_join_invalid_code() {
    let pool = test_pool().await;
    truncate_tables(&pool).await;

    let user = register_test_user(&pool).await;
    let token = sign_test_token(&user);
    let app = build_channel_router(build_test_app_state(pool));

    let (status, body) = send_json(
        &app,
        post_json(
            "/channels/join",
            &token,
            &serde_json::json!({"code": "bogus-code-123"}),
        ),
    )
    .await;

    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert!(body["error"].as_str().unwrap().contains("invalid invite"));
}

#[tokio::test]
#[ignore]
async fn test_27_5_join_already_member() {
    let pool = test_pool().await;
    truncate_tables(&pool).await;

    let owner = register_test_user(&pool).await;
    let joiner = register_test_user(&pool).await;
    let owner_token = sign_test_token(&owner);
    let joiner_token = sign_test_token(&joiner);
    let app = build_channel_router(build_test_app_state(pool));

    let (_, create_body) = send_json(
        &app,
        post_json(
            "/channels",
            &owner_token,
            &serde_json::json!({"name": "Dup Test"}),
        ),
    )
    .await;
    let channel_id = create_body["channel_id"].as_str().unwrap();

    let (_, invite_body) = send_json(
        &app,
        post_json(
            &format!("/channels/{}/invites", channel_id),
            &owner_token,
            &serde_json::json!({"expires_in_secs": 3600, "max_uses": 5}),
        ),
    )
    .await;
    let code = invite_body["code"].as_str().unwrap();

    // First join succeeds
    send_json(
        &app,
        post_json(
            "/channels/join",
            &joiner_token,
            &serde_json::json!({"code": code}),
        ),
    )
    .await;

    // Second join â†’ 409
    let (status, body) = send_json(
        &app,
        post_json(
            "/channels/join",
            &joiner_token,
            &serde_json::json!({"code": code}),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::CONFLICT);
    assert!(body["error"].as_str().unwrap().contains("already a member"));
}

#[tokio::test]
#[ignore]
async fn test_27_5_join_banned_user() {
    let pool = test_pool().await;
    truncate_tables(&pool).await;

    let owner = register_test_user(&pool).await;
    let target = register_test_user(&pool).await;
    let owner_token = sign_test_token(&owner);
    let target_token = sign_test_token(&target);
    let app = build_channel_router(build_test_app_state(pool));

    let (_, create_body) = send_json(
        &app,
        post_json(
            "/channels",
            &owner_token,
            &serde_json::json!({"name": "Ban Join Test"}),
        ),
    )
    .await;
    let channel_id = create_body["channel_id"].as_str().unwrap();

    // Create invite, join, then ban
    let (_, invite_body) = send_json(
        &app,
        post_json(
            &format!("/channels/{}/invites", channel_id),
            &owner_token,
            &serde_json::json!({"expires_in_secs": 3600, "max_uses": 10}),
        ),
    )
    .await;
    let code = invite_body["code"].as_str().unwrap();

    send_json(
        &app,
        post_json(
            "/channels/join",
            &target_token,
            &serde_json::json!({"code": code}),
        ),
    )
    .await;

    send_json(
        &app,
        post_empty(
            &format!("/channels/{}/bans/{}", channel_id, target),
            &owner_token,
        ),
    )
    .await;

    // Banned user tries to rejoin â†’ 403
    let (status, body) = send_json(
        &app,
        post_json(
            "/channels/join",
            &target_token,
            &serde_json::json!({"code": code}),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::FORBIDDEN);
    assert_eq!(body["error"], "forbidden");
}

#[tokio::test]
#[ignore]
async fn test_27_5_join_max_uses_exhausted() {
    let pool = test_pool().await;
    truncate_tables(&pool).await;

    let owner = register_test_user(&pool).await;
    let owner_token = sign_test_token(&owner);
    let app = build_channel_router(build_test_app_state(pool.clone()));

    let (_, create_body) = send_json(
        &app,
        post_json(
            "/channels",
            &owner_token,
            &serde_json::json!({"name": "Exhaust Test"}),
        ),
    )
    .await;
    let channel_id = create_body["channel_id"].as_str().unwrap();

    // Create invite with max_uses=1
    let (_, invite_body) = send_json(
        &app,
        post_json(
            &format!("/channels/{}/invites", channel_id),
            &owner_token,
            &serde_json::json!({"expires_in_secs": 3600, "max_uses": 1}),
        ),
    )
    .await;
    let code = invite_body["code"].as_str().unwrap();

    // First user joins (uses up the invite)
    let user1 = register_test_user(&pool).await;
    let token1 = sign_test_token(&user1);
    let (status, _) = send_json(
        &app,
        post_json(
            "/channels/join",
            &token1,
            &serde_json::json!({"code": code}),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::OK);

    // Second user tries â†’ invite exhausted
    let user2 = register_test_user(&pool).await;
    let token2 = sign_test_token(&user2);
    let (status, body) = send_json(
        &app,
        post_json(
            "/channels/join",
            &token2,
            &serde_json::json!({"code": code}),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert!(body["error"].as_str().unwrap().contains("invalid invite"));
}

// ===========================================================================
// 27.6 Leave channel
// ===========================================================================

#[tokio::test]
#[ignore]
async fn test_27_6_leave_channel_happy_path() {
    let pool = test_pool().await;
    truncate_tables(&pool).await;

    let owner = register_test_user(&pool).await;
    let member = register_test_user(&pool).await;
    let owner_token = sign_test_token(&owner);
    let member_token = sign_test_token(&member);
    let app = build_channel_router(build_test_app_state(pool));

    let (_, create_body) = send_json(
        &app,
        post_json(
            "/channels",
            &owner_token,
            &serde_json::json!({"name": "Leave Test"}),
        ),
    )
    .await;
    let channel_id = create_body["channel_id"].as_str().unwrap();

    // Add member via invite
    let (_, invite_body) = send_json(
        &app,
        post_json(
            &format!("/channels/{}/invites", channel_id),
            &owner_token,
            &serde_json::json!({"expires_in_secs": 3600, "max_uses": 5}),
        ),
    )
    .await;
    let code = invite_body["code"].as_str().unwrap();
    send_json(
        &app,
        post_json(
            "/channels/join",
            &member_token,
            &serde_json::json!({"code": code}),
        ),
    )
    .await;

    // Member leaves
    let (status, _) = send_json(
        &app,
        post_empty(&format!("/channels/{}/leave", channel_id), &member_token),
    )
    .await;
    assert_eq!(status, StatusCode::NO_CONTENT);

    // Verify member no longer listed
    let (_, detail) = send_json(
        &app,
        get_auth(&format!("/channels/{}", channel_id), &owner_token),
    )
    .await;
    let members = detail["members"].as_array().unwrap();
    assert_eq!(members.len(), 1); // only owner remains
}

#[tokio::test]
#[ignore]
async fn test_27_6_owner_cannot_leave() {
    let pool = test_pool().await;
    truncate_tables(&pool).await;

    let owner = register_test_user(&pool).await;
    let token = sign_test_token(&owner);
    let app = build_channel_router(build_test_app_state(pool));

    let (_, create_body) = send_json(
        &app,
        post_json(
            "/channels",
            &token,
            &serde_json::json!({"name": "Owner Leave"}),
        ),
    )
    .await;
    let channel_id = create_body["channel_id"].as_str().unwrap();

    let (status, body) = send_json(
        &app,
        post_empty(&format!("/channels/{}/leave", channel_id), &token),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert!(
        body["error"]
            .as_str()
            .unwrap()
            .contains("owner cannot leave")
    );
}

// ===========================================================================
// 27.7 Ban a member
// ===========================================================================

#[tokio::test]
#[ignore]
async fn test_27_7_ban_member_happy_path() {
    let pool = test_pool().await;
    truncate_tables(&pool).await;

    let owner = register_test_user(&pool).await;
    let member = register_test_user(&pool).await;
    let owner_token = sign_test_token(&owner);
    let member_token = sign_test_token(&member);
    let app = build_channel_router(build_test_app_state(pool));

    let (_, create_body) = send_json(
        &app,
        post_json(
            "/channels",
            &owner_token,
            &serde_json::json!({"name": "Ban Test"}),
        ),
    )
    .await;
    let channel_id = create_body["channel_id"].as_str().unwrap();

    // Add member
    let (_, invite_body) = send_json(
        &app,
        post_json(
            &format!("/channels/{}/invites", channel_id),
            &owner_token,
            &serde_json::json!({"expires_in_secs": 3600, "max_uses": 5}),
        ),
    )
    .await;
    let code = invite_body["code"].as_str().unwrap();
    send_json(
        &app,
        post_json(
            "/channels/join",
            &member_token,
            &serde_json::json!({"code": code}),
        ),
    )
    .await;

    // Ban member
    let (status, body) = send_json(
        &app,
        post_empty(
            &format!("/channels/{}/bans/{}", channel_id, member),
            &owner_token,
        ),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["user_id"], member.to_string());
    assert!(body["banned_at"].as_str().is_some());
}

#[tokio::test]
#[ignore]
async fn test_27_7_ban_member_role_rejected() {
    let pool = test_pool().await;
    truncate_tables(&pool).await;

    let owner = register_test_user(&pool).await;
    let member1 = register_test_user(&pool).await;
    let member2 = register_test_user(&pool).await;
    let owner_token = sign_test_token(&owner);
    let member1_token = sign_test_token(&member1);
    let member2_token = sign_test_token(&member2);
    let app = build_channel_router(build_test_app_state(pool));

    let (_, create_body) = send_json(
        &app,
        post_json(
            "/channels",
            &owner_token,
            &serde_json::json!({"name": "Ban Auth"}),
        ),
    )
    .await;
    let channel_id = create_body["channel_id"].as_str().unwrap();

    let (_, invite_body) = send_json(
        &app,
        post_json(
            &format!("/channels/{}/invites", channel_id),
            &owner_token,
            &serde_json::json!({"expires_in_secs": 3600, "max_uses": 5}),
        ),
    )
    .await;
    let code = invite_body["code"].as_str().unwrap();
    send_json(
        &app,
        post_json(
            "/channels/join",
            &member1_token,
            &serde_json::json!({"code": code}),
        ),
    )
    .await;
    send_json(
        &app,
        post_json(
            "/channels/join",
            &member2_token,
            &serde_json::json!({"code": code}),
        ),
    )
    .await;

    // Member tries to ban another member â†’ 403
    let (status, _) = send_json(
        &app,
        post_empty(
            &format!("/channels/{}/bans/{}", channel_id, member2),
            &member1_token,
        ),
    )
    .await;
    assert_eq!(status, StatusCode::FORBIDDEN);
}

#[tokio::test]
#[ignore]
async fn test_27_7_ban_owner_rejected() {
    let pool = test_pool().await;
    truncate_tables(&pool).await;

    let owner = register_test_user(&pool).await;
    let admin = register_test_user(&pool).await;
    let owner_token = sign_test_token(&owner);
    let admin_token = sign_test_token(&admin);
    let app = build_channel_router(build_test_app_state(pool));

    let (_, create_body) = send_json(
        &app,
        post_json(
            "/channels",
            &owner_token,
            &serde_json::json!({"name": "Ban Owner"}),
        ),
    )
    .await;
    let channel_id = create_body["channel_id"].as_str().unwrap();

    let (_, invite_body) = send_json(
        &app,
        post_json(
            &format!("/channels/{}/invites", channel_id),
            &owner_token,
            &serde_json::json!({"expires_in_secs": 3600, "max_uses": 5}),
        ),
    )
    .await;
    let code = invite_body["code"].as_str().unwrap();
    send_json(
        &app,
        post_json(
            "/channels/join",
            &admin_token,
            &serde_json::json!({"code": code}),
        ),
    )
    .await;

    // Promote to admin
    send_json(
        &app,
        put_json(
            &format!("/channels/{}/members/{}/role", channel_id, admin),
            &owner_token,
            &serde_json::json!({"role": "admin"}),
        ),
    )
    .await;

    // Admin tries to ban owner â†’ 403
    let (status, body) = send_json(
        &app,
        post_empty(
            &format!("/channels/{}/bans/{}", channel_id, owner),
            &admin_token,
        ),
    )
    .await;
    assert_eq!(status, StatusCode::FORBIDDEN);
    assert!(body["error"].as_str().unwrap().contains("cannot ban"));
}

#[tokio::test]
#[ignore]
async fn test_27_7_self_ban_rejected() {
    let pool = test_pool().await;
    truncate_tables(&pool).await;

    let owner = register_test_user(&pool).await;
    let token = sign_test_token(&owner);
    let app = build_channel_router(build_test_app_state(pool));

    let (_, create_body) = send_json(
        &app,
        post_json(
            "/channels",
            &token,
            &serde_json::json!({"name": "Self Ban"}),
        ),
    )
    .await;
    let channel_id = create_body["channel_id"].as_str().unwrap();

    let (status, body) = send_json(
        &app,
        post_empty(&format!("/channels/{}/bans/{}", channel_id, owner), &token),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert!(
        body["error"]
            .as_str()
            .unwrap()
            .contains("cannot ban yourself")
    );
}

#[tokio::test]
#[ignore]
async fn test_27_7_already_banned() {
    let pool = test_pool().await;
    truncate_tables(&pool).await;

    let owner = register_test_user(&pool).await;
    let member = register_test_user(&pool).await;
    let owner_token = sign_test_token(&owner);
    let member_token = sign_test_token(&member);
    let app = build_channel_router(build_test_app_state(pool));

    let (_, create_body) = send_json(
        &app,
        post_json(
            "/channels",
            &owner_token,
            &serde_json::json!({"name": "Double Ban"}),
        ),
    )
    .await;
    let channel_id = create_body["channel_id"].as_str().unwrap();

    let (_, invite_body) = send_json(
        &app,
        post_json(
            &format!("/channels/{}/invites", channel_id),
            &owner_token,
            &serde_json::json!({"expires_in_secs": 3600, "max_uses": 5}),
        ),
    )
    .await;
    let code = invite_body["code"].as_str().unwrap();
    send_json(
        &app,
        post_json(
            "/channels/join",
            &member_token,
            &serde_json::json!({"code": code}),
        ),
    )
    .await;

    // First ban
    send_json(
        &app,
        post_empty(
            &format!("/channels/{}/bans/{}", channel_id, member),
            &owner_token,
        ),
    )
    .await;

    // Second ban â†’ 409
    let (status, body) = send_json(
        &app,
        post_empty(
            &format!("/channels/{}/bans/{}", channel_id, member),
            &owner_token,
        ),
    )
    .await;
    assert_eq!(status, StatusCode::CONFLICT);
    assert!(body["error"].as_str().unwrap().contains("already banned"));
}

// ===========================================================================
// 27.8 Unban a member
// ===========================================================================

#[tokio::test]
#[ignore]
async fn test_27_8_unban_happy_path() {
    let pool = test_pool().await;
    truncate_tables(&pool).await;

    let owner = register_test_user(&pool).await;
    let member = register_test_user(&pool).await;
    let owner_token = sign_test_token(&owner);
    let member_token = sign_test_token(&member);
    let app = build_channel_router(build_test_app_state(pool));

    let (_, create_body) = send_json(
        &app,
        post_json(
            "/channels",
            &owner_token,
            &serde_json::json!({"name": "Unban Test"}),
        ),
    )
    .await;
    let channel_id = create_body["channel_id"].as_str().unwrap();

    let (_, invite_body) = send_json(
        &app,
        post_json(
            &format!("/channels/{}/invites", channel_id),
            &owner_token,
            &serde_json::json!({"expires_in_secs": 3600, "max_uses": 5}),
        ),
    )
    .await;
    let code = invite_body["code"].as_str().unwrap();
    send_json(
        &app,
        post_json(
            "/channels/join",
            &member_token,
            &serde_json::json!({"code": code}),
        ),
    )
    .await;

    // Ban then unban
    send_json(
        &app,
        post_empty(
            &format!("/channels/{}/bans/{}", channel_id, member),
            &owner_token,
        ),
    )
    .await;

    let (status, _) = send_json(
        &app,
        delete_auth(
            &format!("/channels/{}/bans/{}", channel_id, member),
            &owner_token,
        ),
    )
    .await;
    assert_eq!(status, StatusCode::NO_CONTENT);
}

#[tokio::test]
#[ignore]
async fn test_27_8_unban_not_banned() {
    let pool = test_pool().await;
    truncate_tables(&pool).await;

    let owner = register_test_user(&pool).await;
    let member = register_test_user(&pool).await;
    let owner_token = sign_test_token(&owner);
    let member_token = sign_test_token(&member);
    let app = build_channel_router(build_test_app_state(pool));

    let (_, create_body) = send_json(
        &app,
        post_json(
            "/channels",
            &owner_token,
            &serde_json::json!({"name": "Unban NB"}),
        ),
    )
    .await;
    let channel_id = create_body["channel_id"].as_str().unwrap();

    let (_, invite_body) = send_json(
        &app,
        post_json(
            &format!("/channels/{}/invites", channel_id),
            &owner_token,
            &serde_json::json!({"expires_in_secs": 3600, "max_uses": 5}),
        ),
    )
    .await;
    let code = invite_body["code"].as_str().unwrap();
    send_json(
        &app,
        post_json(
            "/channels/join",
            &member_token,
            &serde_json::json!({"code": code}),
        ),
    )
    .await;

    // Unban without banning first â†’ 404
    let (status, _) = send_json(
        &app,
        delete_auth(
            &format!("/channels/{}/bans/{}", channel_id, member),
            &owner_token,
        ),
    )
    .await;
    assert_eq!(status, StatusCode::NOT_FOUND);
}

#[tokio::test]
#[ignore]
async fn test_27_8_unban_member_role_rejected() {
    let pool = test_pool().await;
    truncate_tables(&pool).await;

    let owner = register_test_user(&pool).await;
    let member1 = register_test_user(&pool).await;
    let member2 = register_test_user(&pool).await;
    let owner_token = sign_test_token(&owner);
    let member1_token = sign_test_token(&member1);
    let member2_token = sign_test_token(&member2);
    let app = build_channel_router(build_test_app_state(pool));

    let (_, create_body) = send_json(
        &app,
        post_json(
            "/channels",
            &owner_token,
            &serde_json::json!({"name": "Unban Auth"}),
        ),
    )
    .await;
    let channel_id = create_body["channel_id"].as_str().unwrap();

    let (_, invite_body) = send_json(
        &app,
        post_json(
            &format!("/channels/{}/invites", channel_id),
            &owner_token,
            &serde_json::json!({"expires_in_secs": 3600, "max_uses": 5}),
        ),
    )
    .await;
    let code = invite_body["code"].as_str().unwrap();
    send_json(
        &app,
        post_json(
            "/channels/join",
            &member1_token,
            &serde_json::json!({"code": code}),
        ),
    )
    .await;
    send_json(
        &app,
        post_json(
            "/channels/join",
            &member2_token,
            &serde_json::json!({"code": code}),
        ),
    )
    .await;

    // Ban member2
    send_json(
        &app,
        post_empty(
            &format!("/channels/{}/bans/{}", channel_id, member2),
            &owner_token,
        ),
    )
    .await;

    // Member1 tries to unban â†’ 403
    let (status, _) = send_json(
        &app,
        delete_auth(
            &format!("/channels/{}/bans/{}", channel_id, member2),
            &member1_token,
        ),
    )
    .await;
    assert_eq!(status, StatusCode::FORBIDDEN);
}

// ===========================================================================
// 27.9 Change member role
// ===========================================================================

#[tokio::test]
#[ignore]
async fn test_27_9_change_role_happy_path() {
    let pool = test_pool().await;
    truncate_tables(&pool).await;

    let owner = register_test_user(&pool).await;
    let member = register_test_user(&pool).await;
    let owner_token = sign_test_token(&owner);
    let member_token = sign_test_token(&member);
    let app = build_channel_router(build_test_app_state(pool));

    let (_, create_body) = send_json(
        &app,
        post_json(
            "/channels",
            &owner_token,
            &serde_json::json!({"name": "Role Test"}),
        ),
    )
    .await;
    let channel_id = create_body["channel_id"].as_str().unwrap();

    let (_, invite_body) = send_json(
        &app,
        post_json(
            &format!("/channels/{}/invites", channel_id),
            &owner_token,
            &serde_json::json!({"expires_in_secs": 3600, "max_uses": 5}),
        ),
    )
    .await;
    let code = invite_body["code"].as_str().unwrap();
    send_json(
        &app,
        post_json(
            "/channels/join",
            &member_token,
            &serde_json::json!({"code": code}),
        ),
    )
    .await;

    // Promote to admin
    let (status, body) = send_json(
        &app,
        put_json(
            &format!("/channels/{}/members/{}/role", channel_id, member),
            &owner_token,
            &serde_json::json!({"role": "admin"}),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["role"], "admin");

    // Demote back to member
    let (status, body) = send_json(
        &app,
        put_json(
            &format!("/channels/{}/members/{}/role", channel_id, member),
            &owner_token,
            &serde_json::json!({"role": "member"}),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["role"], "member");
}

#[tokio::test]
#[ignore]
async fn test_27_9_change_role_non_owner_rejected() {
    let pool = test_pool().await;
    truncate_tables(&pool).await;

    let owner = register_test_user(&pool).await;
    let admin = register_test_user(&pool).await;
    let member = register_test_user(&pool).await;
    let owner_token = sign_test_token(&owner);
    let admin_token = sign_test_token(&admin);
    let member_token = sign_test_token(&member);
    let app = build_channel_router(build_test_app_state(pool));

    let (_, create_body) = send_json(
        &app,
        post_json(
            "/channels",
            &owner_token,
            &serde_json::json!({"name": "Role Auth"}),
        ),
    )
    .await;
    let channel_id = create_body["channel_id"].as_str().unwrap();

    let (_, invite_body) = send_json(
        &app,
        post_json(
            &format!("/channels/{}/invites", channel_id),
            &owner_token,
            &serde_json::json!({"expires_in_secs": 3600, "max_uses": 5}),
        ),
    )
    .await;
    let code = invite_body["code"].as_str().unwrap();
    send_json(
        &app,
        post_json(
            "/channels/join",
            &admin_token,
            &serde_json::json!({"code": code}),
        ),
    )
    .await;
    send_json(
        &app,
        post_json(
            "/channels/join",
            &member_token,
            &serde_json::json!({"code": code}),
        ),
    )
    .await;

    // Promote admin
    send_json(
        &app,
        put_json(
            &format!("/channels/{}/members/{}/role", channel_id, admin),
            &owner_token,
            &serde_json::json!({"role": "admin"}),
        ),
    )
    .await;

    // Admin tries to change member's role â†’ 403
    let (status, _) = send_json(
        &app,
        put_json(
            &format!("/channels/{}/members/{}/role", channel_id, member),
            &admin_token,
            &serde_json::json!({"role": "admin"}),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::FORBIDDEN);
}

#[tokio::test]
#[ignore]
async fn test_27_9_change_role_to_owner_rejected() {
    let pool = test_pool().await;
    truncate_tables(&pool).await;

    let owner = register_test_user(&pool).await;
    let member = register_test_user(&pool).await;
    let owner_token = sign_test_token(&owner);
    let member_token = sign_test_token(&member);
    let app = build_channel_router(build_test_app_state(pool));

    let (_, create_body) = send_json(
        &app,
        post_json(
            "/channels",
            &owner_token,
            &serde_json::json!({"name": "Role Owner"}),
        ),
    )
    .await;
    let channel_id = create_body["channel_id"].as_str().unwrap();

    let (_, invite_body) = send_json(
        &app,
        post_json(
            &format!("/channels/{}/invites", channel_id),
            &owner_token,
            &serde_json::json!({"expires_in_secs": 3600, "max_uses": 5}),
        ),
    )
    .await;
    let code = invite_body["code"].as_str().unwrap();
    send_json(
        &app,
        post_json(
            "/channels/join",
            &member_token,
            &serde_json::json!({"code": code}),
        ),
    )
    .await;

    // Try to set role to "owner" â†’ 400
    let (status, body) = send_json(
        &app,
        put_json(
            &format!("/channels/{}/members/{}/role", channel_id, member),
            &owner_token,
            &serde_json::json!({"role": "owner"}),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert!(body["error"].as_str().unwrap().contains("invalid role"));
}

#[tokio::test]
#[ignore]
async fn test_27_9_change_role_unknown_role() {
    let pool = test_pool().await;
    truncate_tables(&pool).await;

    let owner = register_test_user(&pool).await;
    let member = register_test_user(&pool).await;
    let owner_token = sign_test_token(&owner);
    let member_token = sign_test_token(&member);
    let app = build_channel_router(build_test_app_state(pool));

    let (_, create_body) = send_json(
        &app,
        post_json(
            "/channels",
            &owner_token,
            &serde_json::json!({"name": "Role Unknown"}),
        ),
    )
    .await;
    let channel_id = create_body["channel_id"].as_str().unwrap();

    let (_, invite_body) = send_json(
        &app,
        post_json(
            &format!("/channels/{}/invites", channel_id),
            &owner_token,
            &serde_json::json!({"expires_in_secs": 3600, "max_uses": 5}),
        ),
    )
    .await;
    let code = invite_body["code"].as_str().unwrap();
    send_json(
        &app,
        post_json(
            "/channels/join",
            &member_token,
            &serde_json::json!({"code": code}),
        ),
    )
    .await;

    let (status, body) = send_json(
        &app,
        put_json(
            &format!("/channels/{}/members/{}/role", channel_id, member),
            &owner_token,
            &serde_json::json!({"role": "superadmin"}),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert!(body["error"].as_str().unwrap().contains("invalid role"));
}

// ===========================================================================
// 27.10 Revoke invite
// ===========================================================================

#[tokio::test]
#[ignore]
async fn test_27_10_revoke_invite_happy_path() {
    let pool = test_pool().await;
    truncate_tables(&pool).await;

    let owner = register_test_user(&pool).await;
    let token = sign_test_token(&owner);
    let app = build_channel_router(build_test_app_state(pool.clone()));

    let (_, create_body) = send_json(
        &app,
        post_json(
            "/channels",
            &token,
            &serde_json::json!({"name": "Revoke Test"}),
        ),
    )
    .await;
    let channel_id = create_body["channel_id"].as_str().unwrap();

    let (_, invite_body) = send_json(
        &app,
        post_json(
            &format!("/channels/{}/invites", channel_id),
            &token,
            &serde_json::json!({"expires_in_secs": 3600, "max_uses": 5}),
        ),
    )
    .await;
    let code = invite_body["code"].as_str().unwrap();

    let (status, _) = send_json(
        &app,
        delete_auth(
            &format!("/channels/{}/invites/{}", channel_id, code),
            &token,
        ),
    )
    .await;
    assert_eq!(status, StatusCode::NO_CONTENT);

    // Revoked invite should no longer work
    let joiner = register_test_user(&pool).await;
    let joiner_token = sign_test_token(&joiner);
    let (status, body) = send_json(
        &app,
        post_json(
            "/channels/join",
            &joiner_token,
            &serde_json::json!({"code": code}),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert!(body["error"].as_str().unwrap().contains("invalid invite"));
}

#[tokio::test]
#[ignore]
async fn test_27_10_revoke_invite_member_rejected() {
    let pool = test_pool().await;
    truncate_tables(&pool).await;

    let owner = register_test_user(&pool).await;
    let member = register_test_user(&pool).await;
    let owner_token = sign_test_token(&owner);
    let member_token = sign_test_token(&member);
    let app = build_channel_router(build_test_app_state(pool));

    let (_, create_body) = send_json(
        &app,
        post_json(
            "/channels",
            &owner_token,
            &serde_json::json!({"name": "Revoke Auth"}),
        ),
    )
    .await;
    let channel_id = create_body["channel_id"].as_str().unwrap();

    let (_, invite_body) = send_json(
        &app,
        post_json(
            &format!("/channels/{}/invites", channel_id),
            &owner_token,
            &serde_json::json!({"expires_in_secs": 3600, "max_uses": 5}),
        ),
    )
    .await;
    let code = invite_body["code"].as_str().unwrap();

    // Add member
    send_json(
        &app,
        post_json(
            "/channels/join",
            &member_token,
            &serde_json::json!({"code": code}),
        ),
    )
    .await;

    // Create a new invite to revoke (the first one was used)
    let (_, invite_body2) = send_json(
        &app,
        post_json(
            &format!("/channels/{}/invites", channel_id),
            &owner_token,
            &serde_json::json!({"expires_in_secs": 3600, "max_uses": 5}),
        ),
    )
    .await;
    let code2 = invite_body2["code"].as_str().unwrap();

    // Member tries to revoke â†’ 403
    let (status, _) = send_json(
        &app,
        delete_auth(
            &format!("/channels/{}/invites/{}", channel_id, code2),
            &member_token,
        ),
    )
    .await;
    assert_eq!(status, StatusCode::FORBIDDEN);
}

#[tokio::test]
#[ignore]
async fn test_27_10_revoke_nonexistent_code() {
    let pool = test_pool().await;
    truncate_tables(&pool).await;

    let owner = register_test_user(&pool).await;
    let token = sign_test_token(&owner);
    let app = build_channel_router(build_test_app_state(pool));

    let (_, create_body) = send_json(
        &app,
        post_json(
            "/channels",
            &token,
            &serde_json::json!({"name": "Revoke NF"}),
        ),
    )
    .await;
    let channel_id = create_body["channel_id"].as_str().unwrap();

    let (status, _) = send_json(
        &app,
        delete_auth(
            &format!("/channels/{}/invites/nonexistent-code", channel_id),
            &token,
        ),
    )
    .await;
    assert_eq!(status, StatusCode::NOT_FOUND);
}

// ===========================================================================
// 27.11 Delete channel
// ===========================================================================

#[tokio::test]
#[ignore]
async fn test_27_11_delete_channel_happy_path() {
    let pool = test_pool().await;
    truncate_tables(&pool).await;

    let owner = register_test_user(&pool).await;
    let token = sign_test_token(&owner);
    let app = build_channel_router(build_test_app_state(pool));

    let (_, create_body) = send_json(
        &app,
        post_json(
            "/channels",
            &token,
            &serde_json::json!({"name": "Delete Test"}),
        ),
    )
    .await;
    let channel_id = create_body["channel_id"].as_str().unwrap();

    let (status, _) = send_json(
        &app,
        delete_auth(&format!("/channels/{}", channel_id), &token),
    )
    .await;
    assert_eq!(status, StatusCode::NO_CONTENT);

    // Channel should no longer be listed
    let (_, list_body) = send_json(&app, get_auth("/channels", &token)).await;
    let arr = list_body.as_array().unwrap();
    assert!(arr.is_empty());
}

#[tokio::test]
#[ignore]
async fn test_27_11_delete_channel_non_owner() {
    let pool = test_pool().await;
    truncate_tables(&pool).await;

    let owner = register_test_user(&pool).await;
    let member = register_test_user(&pool).await;
    let owner_token = sign_test_token(&owner);
    let member_token = sign_test_token(&member);
    let app = build_channel_router(build_test_app_state(pool));

    let (_, create_body) = send_json(
        &app,
        post_json(
            "/channels",
            &owner_token,
            &serde_json::json!({"name": "Delete Auth"}),
        ),
    )
    .await;
    let channel_id = create_body["channel_id"].as_str().unwrap();

    let (_, invite_body) = send_json(
        &app,
        post_json(
            &format!("/channels/{}/invites", channel_id),
            &owner_token,
            &serde_json::json!({"expires_in_secs": 3600, "max_uses": 5}),
        ),
    )
    .await;
    let code = invite_body["code"].as_str().unwrap();
    send_json(
        &app,
        post_json(
            "/channels/join",
            &member_token,
            &serde_json::json!({"code": code}),
        ),
    )
    .await;

    // Member tries to delete â†’ 403
    let (status, _) = send_json(
        &app,
        delete_auth(&format!("/channels/{}", channel_id), &member_token),
    )
    .await;
    assert_eq!(status, StatusCode::FORBIDDEN);
}

#[tokio::test]
#[ignore]
async fn test_27_11_delete_nonexistent_channel() {
    let pool = test_pool().await;
    truncate_tables(&pool).await;

    let user = register_test_user(&pool).await;
    let token = sign_test_token(&user);
    let app = build_channel_router(build_test_app_state(pool));

    let fake_id = Uuid::new_v4();
    let (status, _) = send_json(&app, delete_auth(&format!("/channels/{}", fake_id), &token)).await;
    assert_eq!(status, StatusCode::NOT_FOUND);
}

// ===========================================================================
// 27.12 Channel rate limiting
// ===========================================================================

#[tokio::test]
#[ignore]
async fn test_27_12_channel_rate_limiting() {
    let pool = test_pool().await;
    truncate_tables(&pool).await;

    let owner = register_test_user(&pool).await;
    let token = sign_test_token(&owner);

    // Build AppState with a very tight rate limit (3 per 60s)
    let mut state = build_test_app_state(pool);
    state.channel_rate_limiter = Arc::new(
        wavis_backend::channel::channel_rate_limiter::ChannelRateLimiter::new(
            wavis_backend::channel::channel_rate_limiter::ChannelRateLimiterConfig {
                max_per_user: 3,
                window_secs: 60,
            },
        ),
    );
    let app = build_channel_router(state);

    // First 3 requests should succeed
    for i in 0..3 {
        let (status, _) = send_json(
            &app,
            post_json(
                "/channels",
                &token,
                &serde_json::json!({"name": format!("RL-{}", i)}),
            ),
        )
        .await;
        assert_eq!(status, StatusCode::CREATED, "request {} should succeed", i);
    }

    // 4th request should be rate-limited
    let (status, body) = send_json(
        &app,
        post_json(
            "/channels",
            &token,
            &serde_json::json!({"name": "RL-overflow"}),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::TOO_MANY_REQUESTS);
    assert!(
        body["error"]
            .as_str()
            .unwrap()
            .contains("too many requests")
    );

    // Read-only endpoint should still work (not rate-limited)
    let (status, _) = send_json(&app, get_auth("/channels", &token)).await;
    assert_eq!(status, StatusCode::OK);
}

// ===========================================================================
// List Bans endpoint â€” GET /channels/{channel_id}/bans
// ===========================================================================

#[tokio::test]
#[ignore]
async fn test_list_bans_owner_gets_200_with_banned_list() {
    let pool = test_pool().await;
    truncate_tables(&pool).await;

    let owner = register_test_user(&pool).await;
    let member = register_test_user(&pool).await;
    let owner_token = sign_test_token(&owner);
    let member_token = sign_test_token(&member);
    let app = build_channel_router(build_test_app_state(pool));

    // Create channel + add member
    let (_, create_body) = send_json(
        &app,
        post_json(
            "/channels",
            &owner_token,
            &serde_json::json!({"name": "Bans Test"}),
        ),
    )
    .await;
    let channel_id = create_body["channel_id"].as_str().unwrap();

    let (_, invite_body) = send_json(
        &app,
        post_json(
            &format!("/channels/{}/invites", channel_id),
            &owner_token,
            &serde_json::json!({"expires_in_secs": 3600, "max_uses": 5}),
        ),
    )
    .await;
    let code = invite_body["code"].as_str().unwrap();
    send_json(
        &app,
        post_json(
            "/channels/join",
            &member_token,
            &serde_json::json!({"code": code}),
        ),
    )
    .await;

    // Ban the member
    send_json(
        &app,
        post_empty(
            &format!("/channels/{}/bans/{}", channel_id, member),
            &owner_token,
        ),
    )
    .await;

    // List bans â€” owner should see the banned member
    let (status, body) = send_json(
        &app,
        get_auth(&format!("/channels/{}/bans", channel_id), &owner_token),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    let banned = body["banned"].as_array().unwrap();
    assert_eq!(banned.len(), 1);
    assert_eq!(banned[0]["user_id"], member.to_string());
    assert!(banned[0]["banned_at"].as_str().is_some());
}

#[tokio::test]
#[ignore]
async fn test_list_bans_admin_gets_200() {
    let pool = test_pool().await;
    truncate_tables(&pool).await;

    let owner = register_test_user(&pool).await;
    let admin = register_test_user(&pool).await;
    let target = register_test_user(&pool).await;
    let owner_token = sign_test_token(&owner);
    let admin_token = sign_test_token(&admin);
    let target_token = sign_test_token(&target);
    let app = build_channel_router(build_test_app_state(pool));

    // Create channel
    let (_, create_body) = send_json(
        &app,
        post_json(
            "/channels",
            &owner_token,
            &serde_json::json!({"name": "Admin Bans"}),
        ),
    )
    .await;
    let channel_id = create_body["channel_id"].as_str().unwrap();

    // Add admin + target via invite
    let (_, invite_body) = send_json(
        &app,
        post_json(
            &format!("/channels/{}/invites", channel_id),
            &owner_token,
            &serde_json::json!({"expires_in_secs": 3600, "max_uses": 10}),
        ),
    )
    .await;
    let code = invite_body["code"].as_str().unwrap();
    send_json(
        &app,
        post_json(
            "/channels/join",
            &admin_token,
            &serde_json::json!({"code": code}),
        ),
    )
    .await;
    send_json(
        &app,
        post_json(
            "/channels/join",
            &target_token,
            &serde_json::json!({"code": code}),
        ),
    )
    .await;

    // Promote admin
    send_json(
        &app,
        put_json(
            &format!("/channels/{}/members/{}/role", channel_id, admin),
            &owner_token,
            &serde_json::json!({"role": "admin"}),
        ),
    )
    .await;

    // Ban target (as owner)
    send_json(
        &app,
        post_empty(
            &format!("/channels/{}/bans/{}", channel_id, target),
            &owner_token,
        ),
    )
    .await;

    // Admin should be able to list bans
    let (status, body) = send_json(
        &app,
        get_auth(&format!("/channels/{}/bans", channel_id), &admin_token),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    let banned = body["banned"].as_array().unwrap();
    assert_eq!(banned.len(), 1);
    assert_eq!(banned[0]["user_id"], target.to_string());
}

#[tokio::test]
#[ignore]
async fn test_list_bans_member_gets_403() {
    let pool = test_pool().await;
    truncate_tables(&pool).await;

    let owner = register_test_user(&pool).await;
    let member = register_test_user(&pool).await;
    let owner_token = sign_test_token(&owner);
    let member_token = sign_test_token(&member);
    let app = build_channel_router(build_test_app_state(pool));

    // Create channel + add member
    let (_, create_body) = send_json(
        &app,
        post_json(
            "/channels",
            &owner_token,
            &serde_json::json!({"name": "Member Bans"}),
        ),
    )
    .await;
    let channel_id = create_body["channel_id"].as_str().unwrap();

    let (_, invite_body) = send_json(
        &app,
        post_json(
            &format!("/channels/{}/invites", channel_id),
            &owner_token,
            &serde_json::json!({"expires_in_secs": 3600, "max_uses": 5}),
        ),
    )
    .await;
    let code = invite_body["code"].as_str().unwrap();
    send_json(
        &app,
        post_json(
            "/channels/join",
            &member_token,
            &serde_json::json!({"code": code}),
        ),
    )
    .await;

    // Member should get 403
    let (status, body) = send_json(
        &app,
        get_auth(&format!("/channels/{}/bans", channel_id), &member_token),
    )
    .await;
    assert_eq!(status, StatusCode::FORBIDDEN);
    assert_eq!(body["error"], "forbidden");
}

#[tokio::test]
#[ignore]
async fn test_list_bans_non_member_gets_403() {
    let pool = test_pool().await;
    truncate_tables(&pool).await;

    let owner = register_test_user(&pool).await;
    let outsider = register_test_user(&pool).await;
    let owner_token = sign_test_token(&owner);
    let outsider_token = sign_test_token(&outsider);
    let app = build_channel_router(build_test_app_state(pool));

    // Create channel (outsider never joins)
    let (_, create_body) = send_json(
        &app,
        post_json(
            "/channels",
            &owner_token,
            &serde_json::json!({"name": "Outsider Bans"}),
        ),
    )
    .await;
    let channel_id = create_body["channel_id"].as_str().unwrap();

    // Non-member should get 403
    let (status, body) = send_json(
        &app,
        get_auth(&format!("/channels/{}/bans", channel_id), &outsider_token),
    )
    .await;
    assert_eq!(status, StatusCode::FORBIDDEN);
    assert_eq!(body["error"], "forbidden");
}

#[tokio::test]
#[ignore]
async fn test_list_bans_empty_returns_empty_array() {
    let pool = test_pool().await;
    truncate_tables(&pool).await;

    let owner = register_test_user(&pool).await;
    let owner_token = sign_test_token(&owner);
    let app = build_channel_router(build_test_app_state(pool));

    // Create channel with no bans
    let (_, create_body) = send_json(
        &app,
        post_json(
            "/channels",
            &owner_token,
            &serde_json::json!({"name": "Empty Bans"}),
        ),
    )
    .await;
    let channel_id = create_body["channel_id"].as_str().unwrap();

    // Should return empty banned array
    let (status, body) = send_json(
        &app,
        get_auth(&format!("/channels/{}/bans", channel_id), &owner_token),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    let banned = body["banned"].as_array().unwrap();
    assert!(banned.is_empty());
}

// â”€â”€â”€ List Invites Tests â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€

#[tokio::test]
#[ignore]
async fn test_list_invites_owner_gets_200_with_active_invites() {
    let pool = test_pool().await;
    truncate_tables(&pool).await;

    let owner = register_test_user(&pool).await;
    let owner_token = sign_test_token(&owner);
    let app = build_channel_router(build_test_app_state(pool));

    // Create channel
    let (_, create_body) = send_json(
        &app,
        post_json(
            "/channels",
            &owner_token,
            &serde_json::json!({"name": "Invites Test"}),
        ),
    )
    .await;
    let channel_id = create_body["channel_id"].as_str().unwrap();

    // Create two invites
    let (_, inv1) = send_json(
        &app,
        post_json(
            &format!("/channels/{}/invites", channel_id),
            &owner_token,
            &serde_json::json!({"expires_in_secs": 3600, "max_uses": 5}),
        ),
    )
    .await;
    let (_, inv2) = send_json(
        &app,
        post_json(
            &format!("/channels/{}/invites", channel_id),
            &owner_token,
            &serde_json::json!({}),
        ),
    )
    .await;

    // List invites â€” owner should see both
    let (status, body) = send_json(
        &app,
        get_auth(&format!("/channels/{}/invites", channel_id), &owner_token),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    let invites = body.as_array().unwrap();
    assert_eq!(invites.len(), 2);

    // Most recent first (created_at DESC)
    assert_eq!(invites[0]["code"], inv2["code"]);
    assert_eq!(invites[1]["code"], inv1["code"]);

    // Verify fields present
    assert!(invites[1]["channel_id"].as_str().is_some());
    assert_eq!(invites[1]["max_uses"], 5);
    assert_eq!(invites[1]["uses"], 0);
}

#[tokio::test]
#[ignore]
async fn test_list_invites_admin_gets_200() {
    let pool = test_pool().await;
    truncate_tables(&pool).await;

    let owner = register_test_user(&pool).await;
    let admin = register_test_user(&pool).await;
    let owner_token = sign_test_token(&owner);
    let admin_token = sign_test_token(&admin);
    let app = build_channel_router(build_test_app_state(pool));

    // Create channel
    let (_, create_body) = send_json(
        &app,
        post_json(
            "/channels",
            &owner_token,
            &serde_json::json!({"name": "Admin Invites"}),
        ),
    )
    .await;
    let channel_id = create_body["channel_id"].as_str().unwrap();

    // Add admin via invite
    let (_, invite_body) = send_json(
        &app,
        post_json(
            &format!("/channels/{}/invites", channel_id),
            &owner_token,
            &serde_json::json!({"expires_in_secs": 3600, "max_uses": 10}),
        ),
    )
    .await;
    let code = invite_body["code"].as_str().unwrap();
    send_json(
        &app,
        post_json(
            "/channels/join",
            &admin_token,
            &serde_json::json!({"code": code}),
        ),
    )
    .await;

    // Promote to admin
    send_json(
        &app,
        put_json(
            &format!("/channels/{}/members/{}/role", channel_id, admin),
            &owner_token,
            &serde_json::json!({"role": "admin"}),
        ),
    )
    .await;

    // Admin should be able to list invites
    let (status, body) = send_json(
        &app,
        get_auth(&format!("/channels/{}/invites", channel_id), &admin_token),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    let invites = body.as_array().unwrap();
    // The invite used to join was max_uses=10, uses=1 â€” still active
    assert!(!invites.is_empty());
}

#[tokio::test]
#[ignore]
async fn test_list_invites_member_gets_403() {
    let pool = test_pool().await;
    truncate_tables(&pool).await;

    let owner = register_test_user(&pool).await;
    let member = register_test_user(&pool).await;
    let owner_token = sign_test_token(&owner);
    let member_token = sign_test_token(&member);
    let app = build_channel_router(build_test_app_state(pool));

    // Create channel + add member
    let (_, create_body) = send_json(
        &app,
        post_json(
            "/channels",
            &owner_token,
            &serde_json::json!({"name": "Member Invites"}),
        ),
    )
    .await;
    let channel_id = create_body["channel_id"].as_str().unwrap();

    let (_, invite_body) = send_json(
        &app,
        post_json(
            &format!("/channels/{}/invites", channel_id),
            &owner_token,
            &serde_json::json!({"expires_in_secs": 3600, "max_uses": 5}),
        ),
    )
    .await;
    let code = invite_body["code"].as_str().unwrap();
    send_json(
        &app,
        post_json(
            "/channels/join",
            &member_token,
            &serde_json::json!({"code": code}),
        ),
    )
    .await;

    // Member should get 403
    let (status, body) = send_json(
        &app,
        get_auth(&format!("/channels/{}/invites", channel_id), &member_token),
    )
    .await;
    assert_eq!(status, StatusCode::FORBIDDEN);
    assert_eq!(body["error"], "forbidden");
}

#[tokio::test]
#[ignore]
async fn test_list_invites_excludes_exhausted() {
    let pool = test_pool().await;
    truncate_tables(&pool).await;

    let owner = register_test_user(&pool).await;
    let joiner = register_test_user(&pool).await;
    let owner_token = sign_test_token(&owner);
    let joiner_token = sign_test_token(&joiner);
    let app = build_channel_router(build_test_app_state(pool));

    // Create channel
    let (_, create_body) = send_json(
        &app,
        post_json(
            "/channels",
            &owner_token,
            &serde_json::json!({"name": "Exhausted Invites"}),
        ),
    )
    .await;
    let channel_id = create_body["channel_id"].as_str().unwrap();

    // Create invite with max_uses=1
    let (_, invite_body) = send_json(
        &app,
        post_json(
            &format!("/channels/{}/invites", channel_id),
            &owner_token,
            &serde_json::json!({"max_uses": 1}),
        ),
    )
    .await;
    let code = invite_body["code"].as_str().unwrap();

    // Also create a permanent invite (no max_uses, no expiry)
    send_json(
        &app,
        post_json(
            &format!("/channels/{}/invites", channel_id),
            &owner_token,
            &serde_json::json!({}),
        ),
    )
    .await;

    // Use the max_uses=1 invite
    send_json(
        &app,
        post_json(
            "/channels/join",
            &joiner_token,
            &serde_json::json!({"code": code}),
        ),
    )
    .await;

    // List invites â€” exhausted invite should be excluded, permanent one remains
    let (status, body) = send_json(
        &app,
        get_auth(&format!("/channels/{}/invites", channel_id), &owner_token),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    let invites = body.as_array().unwrap();
    assert_eq!(invites.len(), 1);
    // The remaining invite should NOT be the exhausted one
    assert_ne!(invites[0]["code"].as_str().unwrap(), code);
}
