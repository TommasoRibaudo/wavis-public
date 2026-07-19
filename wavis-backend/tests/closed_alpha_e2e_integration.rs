#![cfg(feature = "test-support")]
//! Release-gate, end-to-end verification of the closed-alpha access path (#269,
//! parent #239): invite redemption -> token issuance -> WS ticket issuance ->
//! room connection, plus the unauthorized/replay/double-redemption boundaries
//! around it.
//!
//! This file drives the real HTTP + WebSocket surface of a single running
//! server (`POST /auth/register`, `POST /auth/refresh`, `POST /auth/ws-ticket`,
//! `GET /ws`) end-to-end, chaining requests the way a real client does. It
//! intentionally does not re-test invariants already covered at the
//! unit/domain level elsewhere:
//!   - Concurrent redemption of the last invite slot admits exactly one
//!     winner: `auth_integration.rs::concurrent_redemption_of_last_slot_admits_exactly_one`
//!     (tests the `SELECT ... FOR UPDATE` row lock directly — the right level
//!     for a concurrency invariant; re-driving it over real HTTP would only
//!     add network jitter, not additional coverage).
//!   - WS ticket replay/expiry at the store level:
//!     `ws_ticket_gate_integration.rs`, `auth::ws_ticket::tests`.
//!   - `register_device` returning 410 Gone:
//!     `auth/routes.rs::register_device_returns_gone_without_consuming_register_limit`.
//!   - Refresh-token rotation/reuse-detection invariants:
//!     `auth_integration.rs::prop8_refresh_token_rotation_invariant`,
//!     `prop13_refresh_token_reuse_detection`.
//!
//! All tests require a running Postgres instance.
//! Run: cargo test -p wavis-backend --test closed_alpha_e2e_integration --features test-support -- --ignored --test-threads=1

use axum::Router;
use axum::routing::{get, post};
use futures_util::{SinkExt, StreamExt};
use serde_json::{Value, json};
use serial_test::serial;
use sqlx::PgPool;
use std::net::SocketAddr;
use std::sync::Arc;
use std::time::Duration;
use tokio::net::TcpListener;
use tokio::time::timeout;
use tokio_tungstenite::{connect_async, tungstenite::Message};
use uuid::Uuid;

use wavis_backend::abuse::join_rate_limiter::{JoinRateLimiter, JoinRateLimiterConfig};
use wavis_backend::app_state::AppState;
use wavis_backend::auth::alpha_invite::hash_invite_code;
use wavis_backend::auth::auth_rate_limiter::{AuthRateLimiter, AuthRateLimiterConfig};
use wavis_backend::auth::routes as auth_routes;
use wavis_backend::channel::invite::{InviteStore, InviteStoreConfig};
use wavis_backend::ip::IpConfig;
use wavis_backend::voice::mock_sfu_bridge::MockSfuBridge;
use wavis_backend::voice::sfu_bridge::{SfuRoomManager, SfuSignalingProxy};
use wavis_backend::ws::ws::ws_handler;

const TEST_AUTH_SECRET: &[u8] = b"test-auth-secret-at-least-32-bytes!!";
const TEST_REFRESH_PEPPER: &[u8] = b"test-pepper-at-least-32-bytes!!!!!!";
const TEST_ALPHA_PEPPER: &[u8] = b"e2e-closed-alpha-invite-pepper-32b!";
const TEST_SFU_SECRET: &str = "dev-secret-32-bytes-minimum!!!XX";

// ============================================================
// DB helpers
// ============================================================

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

async fn truncate_auth_tables(pool: &PgPool) {
    sqlx::query(
        "TRUNCATE alpha_invite_redemptions, alpha_invites, refresh_tokens, devices, users CASCADE",
    )
    .execute(pool)
    .await
    .expect("Failed to truncate auth tables");
}

async fn seed_alpha_invite(pool: &PgPool, code: &str, max_redemptions: i32) -> Uuid {
    let hash = hash_invite_code(code, TEST_ALPHA_PEPPER).unwrap();
    sqlx::query_scalar(
        "INSERT INTO alpha_invites (code_hash, max_redemptions) VALUES ($1, $2) \
         RETURNING invite_id",
    )
    .bind(&hash)
    .bind(max_redemptions)
    .fetch_one(pool)
    .await
    .unwrap()
}

async fn user_count(pool: &PgPool) -> i64 {
    sqlx::query_scalar("SELECT COUNT(*) FROM users")
        .fetch_one(pool)
        .await
        .unwrap()
}

// ============================================================
// Server setup — the full closed-alpha route surface on one AppState
// ============================================================

async fn start_server(pool: PgPool) -> (SocketAddr, AppState) {
    unsafe {
        std::env::set_var("SFU_JWT_SECRET", TEST_SFU_SECRET);
        std::env::set_var("MAX_ROOM_PARTICIPANTS", "6");
        std::env::set_var("REQUIRE_INVITE_CODE", "false");
        std::env::set_var("ALPHA_INVITE_CODE_PEPPER", "unused-32-bytes-min-not-read!!!X");
        std::env::set_var("WS_TICKET_PEPPER", "e2e-closed-alpha-ws-ticket-pepper!");
        std::env::remove_var("TURN_SHARED_SECRET");
        std::env::remove_var("TURN_SHARED_SECRET_PREVIOUS");
    }

    let mock = Arc::new(MockSfuBridge::new());
    let invite_store = Arc::new(InviteStore::new(InviteStoreConfig::default()));
    let join_rate_limiter = Arc::new(JoinRateLimiter::new(JoinRateLimiterConfig::default()));
    let ip_config = IpConfig {
        trust_proxy_headers: false,
        trusted_proxy_cidrs: vec![],
    };

    let mut app_state = AppState::new(
        mock.clone() as Arc<dyn SfuRoomManager>,
        Some(mock as Arc<dyn SfuSignalingProxy>),
        "sfu://localhost".to_string(),
        invite_store,
        join_rate_limiter,
        ip_config,
        Arc::new(TEST_SFU_SECRET.as_bytes().to_vec()),
        None,
        "wavis-backend".to_string(),
        pool,
        Arc::new(TEST_AUTH_SECRET.to_vec()),
        None,
        Arc::new(AuthRateLimiter::new(AuthRateLimiterConfig::default())),
        30,
        72,
        Arc::new(TEST_REFRESH_PEPPER.to_vec()),
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
    );
    app_state.require_invite_code = false;
    // AppState::new() reads ALPHA_INVITE_CODE_PEPPER from the env var set above,
    // but that value only matters if it's what invites were hashed with. Every
    // test seeds invites with TEST_ALPHA_PEPPER via hash_invite_code directly and
    // this override makes the running server use the exact same pepper.
    app_state.alpha_invite_code_pepper = Arc::new(TEST_ALPHA_PEPPER.to_vec());

    {
        let health = app_state.sfu_room_manager.health_check().await.unwrap();
        *app_state.sfu_health_status.write().await = health;
    }

    let app = Router::new()
        .route("/auth/register", post(auth_routes::register))
        .route("/auth/refresh", post(auth_routes::refresh_token))
        .route("/auth/ws-ticket", post(auth_routes::issue_ws_ticket))
        .route("/ws", get(ws_handler))
        .with_state(app_state.clone());

    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();

    tokio::spawn(async move {
        axum::serve(
            listener,
            app.into_make_service_with_connect_info::<SocketAddr>(),
        )
        .await
        .unwrap();
    });

    tokio::time::sleep(Duration::from_millis(50)).await;
    (addr, app_state)
}

// ============================================================
// HTTP helpers
// ============================================================

async fn post_json(addr: SocketAddr, path: &str, body: Value, bearer: Option<&str>) -> (u16, Value) {
    let client = reqwest::Client::new();
    let mut req = client.post(format!("http://{addr}{path}")).json(&body);
    if let Some(token) = bearer {
        req = req.bearer_auth(token);
    }
    let resp = req.send().await.unwrap();
    let status = resp.status().as_u16();
    let body: Value = resp.json().await.unwrap_or(Value::Null);
    (status, body)
}

async fn register(addr: SocketAddr, phrase: &str, username: &str, invite_code: Option<&str>) -> (u16, Value) {
    let mut body = json!({"phrase": phrase, "username": username});
    if let Some(code) = invite_code {
        body["invite_code"] = json!(code);
    }
    post_json(addr, "/auth/register", body, None).await
}

async fn issue_ws_ticket(addr: SocketAddr, access_token: &str) -> (u16, Value) {
    let client = reqwest::Client::new();
    let resp = client
        .post(format!("http://{addr}/auth/ws-ticket"))
        .bearer_auth(access_token)
        .send()
        .await
        .unwrap();
    let status = resp.status().as_u16();
    let body: Value = resp.json().await.unwrap_or(Value::Null);
    (status, body)
}

// ============================================================
// WebSocket helpers
// ============================================================

type WsSink = futures_util::stream::SplitSink<
    tokio_tungstenite::WebSocketStream<tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>>,
    Message,
>;
type WsStream = futures_util::stream::SplitStream<
    tokio_tungstenite::WebSocketStream<tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>>,
>;

async fn ws_send(sink: &mut WsSink, msg: Value) {
    sink.send(Message::Text(msg.to_string())).await.unwrap();
}

async fn recv_type(stream: &mut WsStream, target_type: &str) -> Value {
    timeout(Duration::from_secs(5), async {
        while let Some(Ok(msg)) = stream.next().await {
            if let Message::Text(text) = msg {
                let v: Value = serde_json::from_str(&text).unwrap();
                if v["type"].as_str().unwrap_or("unknown") == target_type {
                    return v;
                }
            }
        }
        panic!("WS closed without '{target_type}'");
    })
    .await
    .unwrap_or_else(|_| panic!("Timeout waiting for '{target_type}'"))
}

/// Join a room over an already-ticket-authenticated connection and return the peerId.
async fn join_room(sink: &mut WsSink, stream: &mut WsStream, room_id: &str) -> String {
    ws_send(
        sink,
        json!({"type": "join", "roomId": room_id, "roomType": "sfu"}),
    )
    .await;
    let joined = recv_type(stream, "joined").await;
    joined["peerId"].as_str().unwrap().to_string()
}

// ============================================================
// Tests
// ============================================================

/// AC: "End-to-end flow covers invite redemption, token issuance, WebSocket
/// ticket issuance, and successful room connection."
#[tokio::test]
#[ignore]
#[serial]
async fn full_closed_alpha_path_invite_to_room_connection() {
    let pool = test_pool().await;
    truncate_auth_tables(&pool).await;
    seed_alpha_invite(&pool, "full-path-invite", 1).await;
    let (addr, _state) = start_server(pool).await;

    // 1. Invite redemption + token issuance.
    let (status, body) = register(addr, "full-path-phrase-1234", "alice", Some("full-path-invite")).await;
    assert_eq!(status, 201, "register with a valid invite must succeed: {body:?}");
    let access_token = body["access_token"].as_str().unwrap();

    // 2. WebSocket ticket issuance.
    let (status, body) = issue_ws_ticket(addr, access_token).await;
    assert_eq!(status, 200, "ws-ticket for a freshly-registered user must succeed: {body:?}");
    let ticket = body["ticket"].as_str().unwrap();

    // 3. Successful room connection.
    let url = format!("ws://{addr}/ws?ticket={ticket}");
    let (ws, _) = connect_async(&url)
        .await
        .expect("a valid ticket must be accepted at upgrade");
    let (mut sink, mut stream) = ws.split();
    let peer_id = join_room(&mut sink, &mut stream, "full-path-room").await;
    assert!(!peer_id.is_empty(), "joining must return a real peer id");
}

/// AC: "Registered alpha user can refresh and reconnect."
#[tokio::test]
#[ignore]
#[serial]
async fn registered_alpha_user_can_refresh_and_reconnect() {
    let pool = test_pool().await;
    truncate_auth_tables(&pool).await;
    seed_alpha_invite(&pool, "refresh-reconnect-invite", 1).await;
    let (addr, _state) = start_server(pool).await;

    let (status, body) = register(addr, "refresh-phrase-1234", "bob", Some("refresh-reconnect-invite")).await;
    assert_eq!(status, 201, "{body:?}");
    let original_refresh = body["refresh_token"].as_str().unwrap().to_string();

    let (status, body) = post_json(
        addr,
        "/auth/refresh",
        json!({"refresh_token": original_refresh}),
        None,
    )
    .await;
    assert_eq!(status, 200, "refresh with a just-issued token must succeed: {body:?}");
    let new_access = body["access_token"].as_str().unwrap().to_string();
    let new_refresh = body["refresh_token"].as_str().unwrap().to_string();
    assert_ne!(new_refresh, original_refresh, "refresh must rotate the token");

    // Reconnect using the post-refresh access token.
    let (status, body) = issue_ws_ticket(addr, &new_access).await;
    assert_eq!(status, 200, "{body:?}");
    let ticket = body["ticket"].as_str().unwrap();

    let url = format!("ws://{addr}/ws?ticket={ticket}");
    let (ws, _) = connect_async(&url)
        .await
        .expect("reconnecting with a post-refresh ticket must succeed");
    let (mut sink, mut stream) = ws.split();
    let peer_id = join_room(&mut sink, &mut stream, "refresh-reconnect-room").await;
    assert!(!peer_id.is_empty());
}

/// AC: "Unauthorized users cannot register ... without tokens."
#[tokio::test]
#[ignore]
#[serial]
async fn non_alpha_user_cannot_create_an_account() {
    let pool = test_pool().await;
    truncate_auth_tables(&pool).await;
    let (addr, _state) = start_server(pool.clone()).await;

    let (status, _body) = register(addr, "no-invite-phrase-1234", "carol", None).await;
    assert_eq!(status, 401, "register with no invite_code must be rejected");

    let (status, _body) = register(addr, "bad-invite-phrase-1234", "dave", Some("does-not-exist")).await;
    assert_eq!(status, 401, "register with an unknown invite_code must be rejected");

    assert_eq!(
        user_count(&pool).await,
        0,
        "no user row may exist after either rejected registration attempt"
    );
}

/// AC: "Unauthorized users cannot ... refresh without tokens."
#[tokio::test]
#[ignore]
#[serial]
async fn unauthorized_clients_cannot_refresh_without_a_valid_token() {
    let pool = test_pool().await;
    truncate_auth_tables(&pool).await;
    let (addr, _state) = start_server(pool).await;

    let (status, _body) = post_json(
        addr,
        "/auth/refresh",
        json!({"refresh_token": "not-a-real-refresh-token"}),
        None,
    )
    .await;
    assert_eq!(status, 401, "refresh with a garbage token must be rejected");
}

/// AC: "Unauthorized users cannot ... open WebSockets."
#[tokio::test]
#[ignore]
#[serial]
async fn unauthorized_clients_cannot_open_websockets_on_the_full_route_surface() {
    let pool = test_pool().await;
    truncate_auth_tables(&pool).await;
    let (addr, _state) = start_server(pool).await;

    let result = connect_async(format!("ws://{addr}/ws")).await;
    assert!(result.is_err(), "a connection with no ticket must be rejected before upgrade");
}

/// AC / Test: "One invite cannot be redeemed twice when max redemptions is 1."
/// (Complements `auth_integration.rs::alpha_invite_registration_exhausts_limited_invite`,
/// which exercises the same invariant at the domain-function level, with a
/// full HTTP round trip.)
#[tokio::test]
#[ignore]
#[serial]
async fn single_use_invite_cannot_be_redeemed_twice_over_http() {
    let pool = test_pool().await;
    truncate_auth_tables(&pool).await;
    seed_alpha_invite(&pool, "single-use-http-invite", 1).await;
    let (addr, _state) = start_server(pool.clone()).await;

    let (status, body) = register(addr, "first-phrase-1234", "erin", Some("single-use-http-invite")).await;
    assert_eq!(status, 201, "the first redemption must succeed: {body:?}");

    let (status, _body) = register(addr, "second-phrase-1234", "frank", Some("single-use-http-invite")).await;
    assert_eq!(status, 401, "redeeming the same single-use invite twice must be rejected");

    assert_eq!(user_count(&pool).await, 1, "exactly one user must have been created");
}

/// AC / Test: "Consumed WebSocket ticket cannot be replayed."
/// (Complements `ws_ticket_gate_integration.rs`'s store-seeded replay test with
/// a full register -> ws-ticket -> connect -> replay round trip.)
#[tokio::test]
#[ignore]
#[serial]
async fn consumed_ws_ticket_cannot_be_replayed_over_http() {
    let pool = test_pool().await;
    truncate_auth_tables(&pool).await;
    seed_alpha_invite(&pool, "ticket-replay-invite", 1).await;
    let (addr, _state) = start_server(pool).await;

    let (status, body) = register(addr, "replay-phrase-1234", "grace", Some("ticket-replay-invite")).await;
    assert_eq!(status, 201, "{body:?}");
    let access_token = body["access_token"].as_str().unwrap();

    let (status, body) = issue_ws_ticket(addr, access_token).await;
    assert_eq!(status, 200, "{body:?}");
    let ticket = body["ticket"].as_str().unwrap().to_string();

    let url = format!("ws://{addr}/ws?ticket={ticket}");
    let first = connect_async(&url).await;
    assert!(first.is_ok(), "first use of a freshly-issued ticket must succeed");
    drop(first);

    let second = connect_async(&url).await;
    assert!(second.is_err(), "replaying an already-consumed ticket must be rejected");
}
