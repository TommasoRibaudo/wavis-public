#![cfg(feature = "test-support")]
//! End-to-end WebSocket + REST integration tests for Channel Voice Orchestration.
//!
//! Automates the manual test steps from doc/testing/backend-manual.md Section 28.
//! All tests require a running Postgres instance.
//! Run with: `cargo test --test voice_ws_integration -- --ignored --test-threads=1`
//!
//! The DATABASE_URL env var must point to a test database.
//! Tables are truncated between tests for isolation.
//!
//! Covered:
//!   28.1  JoinVoice â€” happy path (auth â†’ join_voice â†’ joined + media_token)
//!   28.2  JoinVoice â€” requires authentication
//!   28.3  JoinVoice â€” non-member rejection (opaque not_authorized)
//!   28.4  JoinVoice â€” banned user rejection (opaque not_authorized)
//!   28.5  JoinVoice â€” non-existent channel (opaque not_authorized)
//!   28.6  JoinVoice â€” invalid channel ID format (opaque not_authorized)
//!   28.7  JoinVoice â€” field-length validation (channelId > 64, displayName > 64)
//!   28.8  JoinVoice â€” already joined
//!   28.9  Active voice query â€” happy path (REST)
//!   28.10 Active voice query â€” no active session (REST)
//!   28.11 Active voice query â€” non-member (REST)
//!   28.12 Ban eject from voice (REST ban â†’ WS participant_kicked)
//!   28.13 Room cleanup on last leave (leave â†’ voice query returns inactive)
//!
//! NOT covered (and why):
//!   28.14 Lazy role enforcement â€” requires mid-session kick which needs a third
//!         participant and complex WS orchestration; covered by domain-level tests
//!         in voice_orchestration_integration.rs (Property 11).
//!   28.15 Rate limiting â€” JoinVoice uses the same rate limiter as Join; already
//!         covered by existing rate limiter tests and Property 17.

use axum::Router;
use axum::routing::{delete, get, post, put};
use futures_util::{SinkExt, StreamExt};
use serde_json::{Value, json};
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
use wavis_backend::auth::auth_rate_limiter::{AuthRateLimiter, AuthRateLimiterConfig};
use wavis_backend::auth::jwt::sign_access_token;
use wavis_backend::channel::invite::{InviteStore, InviteStoreConfig};
use wavis_backend::channel::routes as channel_routes;
use wavis_backend::ip::IpConfig;
use wavis_backend::voice::mock_sfu_bridge::MockSfuBridge;
use wavis_backend::voice::sfu_bridge::{SfuRoomManager, SfuSignalingProxy};
use wavis_backend::ws::ws::ws_handler;

const TEST_AUTH_SECRET: &[u8] = b"test-auth-secret-at-least-32-bytes!!";

// ---------------------------------------------------------------------------
// Test helpers â€” DB
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

const TEST_PEPPER: &[u8] = b"test-pepper-at-least-32-bytes!!!!!!";

async fn register_test_user(pool: &PgPool) -> Uuid {
    let secret = TEST_AUTH_SECRET.to_vec();
    let reg = wavis_backend::auth::auth::register_device(pool, &secret, 30, 30, TEST_PEPPER)
        .await
        .expect("register_device failed");
    reg.user_id
}

fn sign_test_token(user_id: &Uuid) -> String {
    sign_access_token(user_id, &Uuid::nil(), TEST_AUTH_SECRET, 3600, 0)
        .expect("signing should succeed")
}

async fn create_test_channel(pool: &PgPool, owner_id: Uuid, name: &str) -> Uuid {
    let ch = wavis_backend::channel::channel::create_channel(pool, owner_id, name)
        .await
        .expect("create_channel failed");
    ch.channel_id
}

async fn add_member(pool: &PgPool, channel_id: Uuid, owner_id: Uuid, member_id: Uuid) {
    let invite =
        wavis_backend::channel::channel::create_invite(pool, channel_id, owner_id, None, None)
            .await
            .expect("create_invite failed");
    wavis_backend::channel::channel::join_channel_by_invite(pool, member_id, &invite.code)
        .await
        .expect("join_channel_by_invite failed");
}

async fn ban_member(pool: &PgPool, channel_id: Uuid, banner_id: Uuid, target_id: Uuid) {
    wavis_backend::channel::channel::ban_member(pool, channel_id, banner_id, target_id)
        .await
        .expect("ban_member failed");
}

// ---------------------------------------------------------------------------
// Server setup â€” full router with WS + REST + real DB
// ---------------------------------------------------------------------------

/// Start a full server with WS handler + channel REST routes + voice query,
/// backed by a real Postgres pool. Returns (addr, app_state).
async fn start_server(pool: PgPool) -> (SocketAddr, AppState) {
    unsafe {
        std::env::set_var("SFU_JWT_SECRET", "dev-secret-32-bytes-minimum!!!XX");
        std::env::set_var("MAX_ROOM_PARTICIPANTS", "6");
        std::env::set_var("REQUIRE_INVITE_CODE", "false");
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
    );
    app_state.require_invite_code = false;

    // Run initial health check so SFU joins aren't rejected as "SFU unavailable"
    {
        let health = app_state.sfu_room_manager.health_check().await.unwrap();
        *app_state.sfu_health_status.write().await = health;
    }

    let app = Router::new()
        .route("/ws", get(ws_handler))
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
            post(channel_routes::create_invite),
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

// ---------------------------------------------------------------------------
// WebSocket helpers
// ---------------------------------------------------------------------------

type WsSink = futures_util::stream::SplitSink<
    tokio_tungstenite::WebSocketStream<tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>>,
    Message,
>;
type WsStream = futures_util::stream::SplitStream<
    tokio_tungstenite::WebSocketStream<tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>>,
>;

async fn ws_connect(addr: SocketAddr) -> (WsSink, WsStream) {
    let url = format!("ws://{addr}/ws");
    let (ws, _) = connect_async(&url).await.expect("WS connect failed");
    ws.split()
}

async fn ws_send(sink: &mut WsSink, msg: Value) {
    sink.send(Message::Text(msg.to_string())).await.unwrap();
}

/// Receive messages until we find one with the given "type", or timeout after 5s.
async fn recv_type(stream: &mut WsStream, target_type: &str) -> Value {
    timeout(Duration::from_secs(5), async {
        while let Some(Ok(msg)) = stream.next().await {
            if let Message::Text(text) = msg {
                let v: Value = serde_json::from_str(&text).unwrap();
                let msg_type = v["type"].as_str().unwrap_or("unknown");
                if msg_type == target_type {
                    return v;
                }
            }
        }
        panic!("WS closed without '{target_type}'");
    })
    .await
    .unwrap_or_else(|_| panic!("Timeout waiting for '{target_type}'"))
}

/// Try to receive a message of the given type within a short timeout.
async fn try_recv_type(stream: &mut WsStream, target_type: &str, timeout_ms: u64) -> Option<Value> {
    timeout(Duration::from_millis(timeout_ms), async {
        while let Some(Ok(msg)) = stream.next().await {
            if let Message::Text(text) = msg {
                let v: Value = serde_json::from_str(&text).unwrap();
                let msg_type = v["type"].as_str().unwrap_or("unknown");
                if msg_type == target_type {
                    return Some(v);
                }
            }
        }
        None
    })
    .await
    .unwrap_or(None)
}

/// Drain all pending messages.
#[allow(dead_code)]
async fn drain(stream: &mut WsStream) {
    while let Ok(Some(Ok(_))) = timeout(Duration::from_millis(200), stream.next()).await {
        // Continue draining
    }
}

/// Authenticate a WS connection. Sends Auth and waits for auth_success.
async fn ws_auth(sink: &mut WsSink, stream: &mut WsStream, token: &str) {
    ws_send(sink, json!({"type": "auth", "accessToken": token})).await;
    let resp = recv_type(stream, "auth_success").await;
    assert!(
        resp["userId"].as_str().is_some(),
        "auth_success must have userId"
    );
}

/// Send join_voice and wait for joined + media_token. Returns the joined payload.
async fn ws_join_voice(
    sink: &mut WsSink,
    stream: &mut WsStream,
    channel_id: &str,
    display_name: &str,
) -> Value {
    ws_send(
        sink,
        json!({
            "type": "join_voice",
            "channelId": channel_id,
            "displayName": display_name
        }),
    )
    .await;
    let joined = recv_type(stream, "joined").await;
    // Also consume the media_token that follows
    let _media_token = recv_type(stream, "media_token").await;
    joined
}

// ---------------------------------------------------------------------------
// REST helpers (raw HTTP/1.1 over TCP â€” no extra dependencies needed)
// ---------------------------------------------------------------------------

use tokio::io::{AsyncReadExt, AsyncWriteExt};

async fn http_request(
    addr: SocketAddr,
    method: &str,
    path: &str,
    token: &str,
    body: Option<&str>,
) -> (u16, Value) {
    let mut stream = tokio::net::TcpStream::connect(addr)
        .await
        .expect("TCP connect failed");

    let content = body.unwrap_or("");
    let content_headers = if body.is_some() {
        format!(
            "Content-Type: application/json\r\nContent-Length: {}\r\n",
            content.len()
        )
    } else {
        "Content-Length: 0\r\n".to_string()
    };

    let request = format!(
        "{method} {path} HTTP/1.1\r\n\
         Host: {addr}\r\n\
         Authorization: Bearer {token}\r\n\
         {content_headers}\
         Connection: close\r\n\
         \r\n\
         {content}"
    );

    stream
        .write_all(request.as_bytes())
        .await
        .expect("write failed");

    let mut response = Vec::new();
    // Connection: close tells the server to close after responding.
    // Do NOT call stream.shutdown() before reading â€” on Windows this can
    // cause the server to reset the connection before sending a response.
    let _ = timeout(Duration::from_secs(5), stream.read_to_end(&mut response)).await;
    let response_str = String::from_utf8_lossy(&response);

    // Parse status code from first line
    let status_line = response_str.lines().next().unwrap_or("");
    let status: u16 = status_line
        .split_whitespace()
        .nth(1)
        .and_then(|s| s.parse().ok())
        .unwrap_or(0);

    // Parse body â€” find the JSON after headers.
    // Handle both regular and chunked transfer encoding.
    let body_json = if let Some(idx) = response_str.find("\r\n\r\n") {
        let raw_body = &response_str[idx + 4..];
        // If chunked, the body starts with a hex chunk size line.
        // Try to extract JSON from the raw body by finding the first '{'.
        if let Some(json_start) = raw_body.find('{') {
            let json_candidate = &raw_body[json_start..];
            // Find the matching closing brace
            let mut depth = 0i32;
            let mut end = 0;
            for (i, ch) in json_candidate.char_indices() {
                match ch {
                    '{' => depth += 1,
                    '}' => {
                        depth -= 1;
                        if depth == 0 {
                            end = i + 1;
                            break;
                        }
                    }
                    _ => {}
                }
            }
            if end > 0 {
                serde_json::from_str(&json_candidate[..end]).unwrap_or(Value::Null)
            } else {
                serde_json::from_str(raw_body).unwrap_or(Value::Null)
            }
        } else {
            serde_json::from_str(raw_body).unwrap_or(Value::Null)
        }
    } else {
        Value::Null
    };

    (status, body_json)
}

async fn rest_get(addr: SocketAddr, path: &str, token: &str) -> (u16, Value) {
    http_request(addr, "GET", path, token, None).await
}

async fn rest_post_empty(addr: SocketAddr, path: &str, token: &str) -> (u16, Value) {
    http_request(addr, "POST", path, token, None).await
}

// ===========================================================================
// 28.1 JoinVoice â€” happy path
// ===========================================================================

#[tokio::test]
#[ignore]
async fn test_28_1_join_voice_happy_path() {
    let pool = test_pool().await;
    truncate_tables(&pool).await;

    let owner = register_test_user(&pool).await;
    let member = register_test_user(&pool).await;
    let channel_id = create_test_channel(&pool, owner, "voice-happy").await;
    add_member(&pool, channel_id, owner, member).await;

    let (addr, _state) = start_server(pool).await;

    // Owner connects, authenticates, joins voice
    let (mut sink1, mut stream1) = ws_connect(addr).await;
    ws_auth(&mut sink1, &mut stream1, &sign_test_token(&owner)).await;

    let joined1 = ws_join_voice(&mut sink1, &mut stream1, &channel_id.to_string(), "Owner").await;

    // Verify joined payload
    let room_id = joined1["roomId"].as_str().unwrap();
    assert!(
        room_id.starts_with("channel-"),
        "roomId should start with 'channel-', got: {room_id}"
    );
    assert!(joined1["peerId"].as_str().is_some());
    assert_eq!(joined1["peerCount"].as_u64().unwrap(), 1);

    // Member connects, authenticates, joins same channel voice
    let (mut sink2, mut stream2) = ws_connect(addr).await;
    ws_auth(&mut sink2, &mut stream2, &sign_test_token(&member)).await;

    let joined2 = ws_join_voice(&mut sink2, &mut stream2, &channel_id.to_string(), "Member").await;

    // Both should be in the same room
    let room_id2 = joined2["roomId"].as_str().unwrap();
    assert_eq!(room_id, room_id2, "both users must be in the same room");
    assert_eq!(joined2["peerCount"].as_u64().unwrap(), 2);

    // Owner should have received participant_joined for the member
    let pj = recv_type(&mut stream1, "participant_joined").await;
    assert_eq!(pj["displayName"].as_str().unwrap(), "Member");
}

// ===========================================================================
// 28.2 JoinVoice â€” requires authentication
// ===========================================================================

#[tokio::test]
#[ignore]
async fn test_28_2_join_voice_requires_auth() {
    let pool = test_pool().await;
    truncate_tables(&pool).await;

    let owner = register_test_user(&pool).await;
    let channel_id = create_test_channel(&pool, owner, "voice-noauth").await;

    let (addr, _state) = start_server(pool).await;

    // Connect WITHOUT authenticating, send join_voice
    let (mut sink, mut stream) = ws_connect(addr).await;
    ws_send(
        &mut sink,
        json!({
            "type": "join_voice",
            "channelId": channel_id.to_string()
        }),
    )
    .await;

    let err = recv_type(&mut stream, "error").await;
    assert_eq!(err["message"].as_str().unwrap(), "not authenticated");
}

// ===========================================================================
// 28.3 JoinVoice â€” non-member rejection
// ===========================================================================

#[tokio::test]
#[ignore]
async fn test_28_3_join_voice_non_member() {
    let pool = test_pool().await;
    truncate_tables(&pool).await;

    let owner = register_test_user(&pool).await;
    let outsider = register_test_user(&pool).await;
    let channel_id = create_test_channel(&pool, owner, "voice-nonmember").await;

    let (addr, _state) = start_server(pool).await;

    let (mut sink, mut stream) = ws_connect(addr).await;
    ws_auth(&mut sink, &mut stream, &sign_test_token(&outsider)).await;

    ws_send(
        &mut sink,
        json!({
            "type": "join_voice",
            "channelId": channel_id.to_string()
        }),
    )
    .await;

    let rejected = recv_type(&mut stream, "join_rejected").await;
    assert_eq!(rejected["reason"].as_str().unwrap(), "not_authorized");
}

// ===========================================================================
// 28.4 JoinVoice â€” banned user rejection
// ===========================================================================

#[tokio::test]
#[ignore]
async fn test_28_4_join_voice_banned_user() {
    let pool = test_pool().await;
    truncate_tables(&pool).await;

    let owner = register_test_user(&pool).await;
    let target = register_test_user(&pool).await;
    let channel_id = create_test_channel(&pool, owner, "voice-banned").await;
    add_member(&pool, channel_id, owner, target).await;
    ban_member(&pool, channel_id, owner, target).await;

    let (addr, _state) = start_server(pool).await;

    let (mut sink, mut stream) = ws_connect(addr).await;
    ws_auth(&mut sink, &mut stream, &sign_test_token(&target)).await;

    ws_send(
        &mut sink,
        json!({
            "type": "join_voice",
            "channelId": channel_id.to_string()
        }),
    )
    .await;

    let rejected = recv_type(&mut stream, "join_rejected").await;
    assert_eq!(
        rejected["reason"].as_str().unwrap(),
        "not_authorized",
        "banned user must get same opaque rejection as non-member"
    );
}

// ===========================================================================
// 28.5 JoinVoice â€” non-existent channel
// ===========================================================================

#[tokio::test]
#[ignore]
async fn test_28_5_join_voice_nonexistent_channel() {
    let pool = test_pool().await;
    truncate_tables(&pool).await;

    let user = register_test_user(&pool).await;

    let (addr, _state) = start_server(pool).await;

    let (mut sink, mut stream) = ws_connect(addr).await;
    ws_auth(&mut sink, &mut stream, &sign_test_token(&user)).await;

    let fake_channel = Uuid::new_v4();
    ws_send(
        &mut sink,
        json!({
            "type": "join_voice",
            "channelId": fake_channel.to_string()
        }),
    )
    .await;

    let rejected = recv_type(&mut stream, "join_rejected").await;
    assert_eq!(rejected["reason"].as_str().unwrap(), "not_authorized");
}

// ===========================================================================
// 28.6 JoinVoice â€” invalid channel ID format
// ===========================================================================

#[tokio::test]
#[ignore]
async fn test_28_6_join_voice_invalid_channel_id() {
    let pool = test_pool().await;
    truncate_tables(&pool).await;

    let user = register_test_user(&pool).await;

    let (addr, _state) = start_server(pool).await;

    let (mut sink, mut stream) = ws_connect(addr).await;
    ws_auth(&mut sink, &mut stream, &sign_test_token(&user)).await;

    ws_send(
        &mut sink,
        json!({
            "type": "join_voice",
            "channelId": "not-a-uuid"
        }),
    )
    .await;

    let rejected = recv_type(&mut stream, "join_rejected").await;
    assert_eq!(rejected["reason"].as_str().unwrap(), "not_authorized");
}

// ===========================================================================
// 28.7 JoinVoice â€” field-length validation
// ===========================================================================

#[tokio::test]
#[ignore]
async fn test_28_7_join_voice_field_length_validation() {
    let pool = test_pool().await;
    truncate_tables(&pool).await;

    let user = register_test_user(&pool).await;
    let channel_id = create_test_channel(&pool, user, "voice-fieldlen").await;

    let (addr, _state) = start_server(pool).await;

    let (mut sink, mut stream) = ws_connect(addr).await;
    ws_auth(&mut sink, &mut stream, &sign_test_token(&user)).await;

    // channelId > 64 chars â†’ field-length error
    let long_channel_id = "x".repeat(65);
    ws_send(
        &mut sink,
        json!({
            "type": "join_voice",
            "channelId": long_channel_id
        }),
    )
    .await;

    let err = recv_type(&mut stream, "error").await;
    let msg = err["message"].as_str().unwrap();
    assert!(
        msg.contains("channelId") && msg.contains("too long"),
        "expected field-length error for channelId, got: {msg}"
    );

    // Connection should still be open â€” send another message with long displayName
    let long_display = "d".repeat(65);
    ws_send(
        &mut sink,
        json!({
            "type": "join_voice",
            "channelId": channel_id.to_string(),
            "displayName": long_display
        }),
    )
    .await;

    let err2 = recv_type(&mut stream, "error").await;
    let msg2 = err2["message"].as_str().unwrap();
    assert!(
        msg2.contains("displayName") && msg2.contains("too long"),
        "expected field-length error for displayName, got: {msg2}"
    );
}

// ===========================================================================
// 28.8 JoinVoice â€” already joined
// ===========================================================================

#[tokio::test]
#[ignore]
async fn test_28_8_join_voice_already_joined() {
    let pool = test_pool().await;
    truncate_tables(&pool).await;

    let owner = register_test_user(&pool).await;
    let channel_id = create_test_channel(&pool, owner, "voice-alreadyjoined").await;

    let (addr, _state) = start_server(pool).await;

    let (mut sink, mut stream) = ws_connect(addr).await;
    ws_auth(&mut sink, &mut stream, &sign_test_token(&owner)).await;

    // First join_voice succeeds
    ws_join_voice(&mut sink, &mut stream, &channel_id.to_string(), "Owner").await;

    // Second join_voice â†’ "already joined"
    ws_send(
        &mut sink,
        json!({
            "type": "join_voice",
            "channelId": channel_id.to_string(),
            "displayName": "Owner2"
        }),
    )
    .await;

    let err = recv_type(&mut stream, "error").await;
    assert_eq!(err["message"].as_str().unwrap(), "already joined");
}

// ===========================================================================
// 28.9 Active voice query â€” happy path (REST)
// ===========================================================================

#[tokio::test]
#[ignore]
async fn test_28_9_voice_query_happy_path() {
    let pool = test_pool().await;
    truncate_tables(&pool).await;

    let owner = register_test_user(&pool).await;
    let channel_id = create_test_channel(&pool, owner, "voice-query").await;

    let (addr, _state) = start_server(pool).await;

    // Owner joins voice via WS
    let (mut sink, mut stream) = ws_connect(addr).await;
    ws_auth(&mut sink, &mut stream, &sign_test_token(&owner)).await;
    ws_join_voice(&mut sink, &mut stream, &channel_id.to_string(), "OwnerName").await;

    // REST query voice status
    let token = sign_test_token(&owner);
    let (status, body) = rest_get(addr, &format!("/channels/{channel_id}/voice"), &token).await;

    assert_eq!(status, 200, "expected 200, got {status}: {body}");
    assert!(body["active"].as_bool().unwrap());
    assert_eq!(body["participant_count"].as_u64().unwrap(), 1);

    let participants = body["participants"].as_array().unwrap();
    assert_eq!(participants.len(), 1);
    assert_eq!(
        participants[0]["display_name"].as_str().unwrap(),
        "OwnerName"
    );
}

// ===========================================================================
// 28.10 Active voice query â€” no active session (REST)
// ===========================================================================

#[tokio::test]
#[ignore]
async fn test_28_10_voice_query_no_active_session() {
    let pool = test_pool().await;
    truncate_tables(&pool).await;

    let owner = register_test_user(&pool).await;
    let channel_id = create_test_channel(&pool, owner, "voice-inactive").await;

    let (addr, _state) = start_server(pool).await;

    let token = sign_test_token(&owner);
    let (status, body) = rest_get(addr, &format!("/channels/{channel_id}/voice"), &token).await;

    assert_eq!(status, 200, "expected 200, got {status}: {body}");
    assert!(!body["active"].as_bool().unwrap());
    // No participant_count or participants fields when inactive
    assert!(body.get("participant_count").is_none() || body["participant_count"].is_null());
}

// ===========================================================================
// 28.11 Active voice query â€” non-member (REST)
// ===========================================================================

#[tokio::test]
#[ignore]
async fn test_28_11_voice_query_non_member() {
    let pool = test_pool().await;
    truncate_tables(&pool).await;

    let owner = register_test_user(&pool).await;
    let outsider = register_test_user(&pool).await;
    let channel_id = create_test_channel(&pool, owner, "voice-nonmember-query").await;

    let (addr, _state) = start_server(pool).await;

    let token = sign_test_token(&outsider);
    let (status, body) = rest_get(addr, &format!("/channels/{channel_id}/voice"), &token).await;

    assert_eq!(status, 403, "expected 403, got {status}: {body}");
    assert_eq!(body["error"].as_str().unwrap(), "forbidden");
}

// ===========================================================================
// 28.12 Ban eject from voice (REST ban â†’ WS participant_kicked)
// ===========================================================================

#[tokio::test]
#[ignore]
async fn test_28_12_ban_eject_from_voice() {
    let pool = test_pool().await;
    truncate_tables(&pool).await;

    let owner = register_test_user(&pool).await;
    let member = register_test_user(&pool).await;
    let channel_id = create_test_channel(&pool, owner, "voice-ban-eject").await;
    add_member(&pool, channel_id, owner, member).await;

    let (addr, _state) = start_server(pool).await;

    // Owner joins voice
    let (mut sink1, mut stream1) = ws_connect(addr).await;
    ws_auth(&mut sink1, &mut stream1, &sign_test_token(&owner)).await;
    ws_join_voice(&mut sink1, &mut stream1, &channel_id.to_string(), "Owner").await;

    // Member joins voice
    let (mut sink2, mut stream2) = ws_connect(addr).await;
    ws_auth(&mut sink2, &mut stream2, &sign_test_token(&member)).await;
    ws_join_voice(&mut sink2, &mut stream2, &channel_id.to_string(), "Member").await;

    // Owner should have received participant_joined for member
    let _pj = recv_type(&mut stream1, "participant_joined").await;

    // Ban the member via REST
    let owner_token = sign_test_token(&owner);
    let (status, _body) = rest_post_empty(
        addr,
        &format!("/channels/{channel_id}/bans/{member}"),
        &owner_token,
    )
    .await;
    assert_eq!(status, 200, "ban should succeed, got {status}");

    // Owner should receive participant_kicked for the member
    let kicked = recv_type(&mut stream1, "participant_kicked").await;
    assert!(
        kicked["participantId"].as_str().is_some(),
        "participant_kicked must have participantId"
    );

    // Member should receive participant_kicked (for themselves) or the connection may close
    // The kicked user gets a participant_kicked signal too
    let member_kicked = try_recv_type(&mut stream2, "participant_kicked", 3000).await;
    assert!(
        member_kicked.is_some(),
        "member should receive participant_kicked"
    );
}

// ===========================================================================
// 28.13 Room cleanup on last leave
// ===========================================================================

#[tokio::test]
#[ignore]
async fn test_28_13_room_cleanup_on_last_leave() {
    let pool = test_pool().await;
    truncate_tables(&pool).await;

    let owner = register_test_user(&pool).await;
    let channel_id = create_test_channel(&pool, owner, "voice-cleanup").await;

    let (addr, _state) = start_server(pool).await;

    // Join voice
    let (mut sink, mut stream) = ws_connect(addr).await;
    ws_auth(&mut sink, &mut stream, &sign_test_token(&owner)).await;
    let joined = ws_join_voice(&mut sink, &mut stream, &channel_id.to_string(), "Owner").await;
    let room_id_1 = joined["roomId"].as_str().unwrap().to_string();

    // Send leave
    ws_send(&mut sink, json!({"type": "leave"})).await;

    // Give the server a moment to clean up
    tokio::time::sleep(Duration::from_millis(200)).await;

    // REST voice query should show inactive
    let token = sign_test_token(&owner);
    let (status, body) = rest_get(addr, &format!("/channels/{channel_id}/voice"), &token).await;
    assert_eq!(status, 200);
    assert!(
        !body["active"].as_bool().unwrap(),
        "room should be cleaned up after last leave"
    );

    // Re-join voice â€” should get a different roomId
    // Need a new WS connection since the old session is in a post-leave state
    let (mut sink2, mut stream2) = ws_connect(addr).await;
    ws_auth(&mut sink2, &mut stream2, &sign_test_token(&owner)).await;
    let joined2 = ws_join_voice(&mut sink2, &mut stream2, &channel_id.to_string(), "Owner").await;
    let room_id_2 = joined2["roomId"].as_str().unwrap().to_string();

    assert_ne!(
        room_id_1, room_id_2,
        "re-join after cleanup should get a new room"
    );
}
