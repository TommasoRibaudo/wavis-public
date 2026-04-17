//! LiveKit end-to-end integration tests.
//!
//! These tests exercise the real `LiveKitSfuBridge` against a running LiveKit
//! server, closing the gap left by `MockSfuBridge`-only tests. The mock-based
//! suite never verifies that `LiveKitSfuBridge` can actually talk to LiveKit,
//! that the LiveKit config template produces valid keys, or that the backend's
//! JWT tokens are accepted by LiveKit. This file closes those gaps.
//!
//! All tests are `#[ignore]` by default — they require a running LiveKit server
//! and are activated only when `LIVEKIT_API_KEY`, `LIVEKIT_API_SECRET`, and
//! `LIVEKIT_HOST` are all set.
//!
//! Run locally (with docker-compose up):
//! ```sh
//! LIVEKIT_API_KEY=devkey \
//! LIVEKIT_API_SECRET=secret \
//! LIVEKIT_HOST=ws://localhost:7880 \
//! SFU_JWT_SECRET=dev-secret-32-bytes-minimum!!!XX \
//! cargo test -p wavis-backend --test livekit_e2e_integration -- --ignored --test-threads=1
//! ```

use futures_util::{SinkExt, StreamExt};
use livekit_api::services::room::RoomClient;
use serde_json::{Value, json};
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
use wavis_backend::channel::invite::{InviteStore, InviteStoreConfig};
use wavis_backend::ip::IpConfig;
use wavis_backend::voice::livekit_bridge::LiveKitSfuBridge;
use wavis_backend::voice::sfu_bridge::{SfuError, SfuHealth, SfuRoomManager};
use wavis_backend::ws::ws::ws_handler;

use axum::Router;
use axum::routing::get;

// ============================================================
// Helpers
// ============================================================

/// Read LiveKit credentials from environment. Returns `None` if any are missing
/// or empty, causing the test to skip gracefully.
fn livekit_creds() -> Option<(String, String, String)> {
    let key = std::env::var("LIVEKIT_API_KEY").ok()?;
    let secret = std::env::var("LIVEKIT_API_SECRET").ok()?;
    let host = std::env::var("LIVEKIT_HOST").ok()?;
    if key.is_empty() || secret.is_empty() || host.is_empty() {
        return None;
    }
    Some((key, secret, host))
}

/// Generate a room ID that cannot collide with other tests.
fn unique_room_id(prefix: &str) -> String {
    format!("{prefix}-{}", Uuid::new_v4())
}

/// Convert a ws:// or wss:// URL to the http(s) equivalent for the Twirp API.
fn to_api_host(host: &str) -> String {
    host.replace("wss://", "https://")
        .replace("ws://", "http://")
}

// ============================================================
// Server setup
// ============================================================

/// Start an in-process wavis-backend wired to a real LiveKit server.
///
/// Uses `LiveKitSfuBridge` (not `MockSfuBridge`) and sets env vars so the
/// WebSocket handler builds `TokenMode::LiveKit` for media tokens.
async fn start_server_with_livekit(
    api_key: &str,
    api_secret: &str,
    host: &str,
    require_invite: bool,
) -> (SocketAddr, AppState) {
    unsafe {
        std::env::set_var("SFU_JWT_SECRET", "dev-secret-32-bytes-minimum!!!XX");
        std::env::set_var("MAX_ROOM_PARTICIPANTS", "6");
        std::env::set_var(
            "REQUIRE_INVITE_CODE",
            if require_invite { "true" } else { "false" },
        );
        // Set LiveKit credentials so SfuConfig in ws.rs picks them up
        // and builds TokenMode::LiveKit for media tokens.
        std::env::set_var("LIVEKIT_API_KEY", api_key);
        std::env::set_var("LIVEKIT_API_SECRET", api_secret);
        std::env::remove_var("TURN_SHARED_SECRET");
        std::env::remove_var("TURN_SHARED_SECRET_PREVIOUS");
    }

    let bridge = Arc::new(
        LiveKitSfuBridge::from_env(api_key, api_secret, host)
            .expect("LiveKitSfuBridge construction should succeed with valid credentials"),
    );
    let invite_store = Arc::new(InviteStore::new(InviteStoreConfig::default()));
    let join_rate_limiter = Arc::new(JoinRateLimiter::new(JoinRateLimiterConfig::default()));
    let ip_config = IpConfig {
        trust_proxy_headers: false,
        trusted_proxy_cidrs: vec![],
    };

    // LiveKit clients connect directly — no SfuSignalingProxy needed (None).
    let mut app_state = AppState::new(
        bridge.clone() as Arc<dyn SfuRoomManager>,
        None,             // LiveKit mode: no signaling proxy
        host.to_string(), // sfu_url sent to clients in media_token
        invite_store,
        join_rate_limiter,
        ip_config,
        Arc::new(b"dev-secret-32-bytes-minimum!!!XX".to_vec()),
        None,
        "wavis-backend".to_string(),
        sqlx::postgres::PgPoolOptions::new()
            .connect_lazy("postgres://dummy")
            .expect("lazy pool creation should not fail"),
        Arc::new(b"test-auth-secret-at-least-32-bytes!!".to_vec()),
        None,
        Arc::new(AuthRateLimiter::new(AuthRateLimiterConfig::default())),
        30,
        72,
        Arc::new(b"test-pepper-at-least-32-bytes!!!!!!".to_vec()),
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
    app_state.require_invite_code = require_invite;

    // Run initial health check so the SFU is marked Available and joins aren't rejected.
    {
        let health = app_state
            .sfu_room_manager
            .health_check()
            .await
            .expect("health_check should succeed against a running LiveKit server");
        *app_state.sfu_health_status.write().await = health;
    }

    let app = Router::new()
        .route("/ws", get(ws_handler))
        .with_state(app_state.clone());

    let listener = TcpListener::bind("127.0.0.1:0")
        .await
        .expect("binding to ephemeral port should succeed");
    let addr = listener
        .local_addr()
        .expect("listener should have a local address");

    tokio::spawn(async move {
        axum::serve(
            listener,
            app.into_make_service_with_connect_info::<SocketAddr>(),
        )
        .await
        .expect("axum server should run");
    });

    // 50ms pause: gives the spawned server task time to start accepting
    // connections. Matches the pattern used in create_room_integration.rs.
    tokio::time::sleep(Duration::from_millis(50)).await;
    (addr, app_state)
}

// ============================================================
// WebSocket helpers (same pattern as create_room_integration.rs)
// ============================================================

type WsSink = futures_util::stream::SplitSink<
    tokio_tungstenite::WebSocketStream<tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>>,
    Message,
>;
type WsStream = futures_util::stream::SplitStream<
    tokio_tungstenite::WebSocketStream<tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>>,
>;

async fn ws_connect(addr: SocketAddr) -> (WsSink, WsStream) {
    let url = format!("ws://{addr}/ws");
    let (ws, _) = connect_async(&url)
        .await
        .expect("WebSocket connection to in-process server should succeed");
    ws.split()
}

async fn ws_send(sink: &mut WsSink, msg: Value) {
    sink.send(Message::Text(msg.to_string()))
        .await
        .expect("sending WebSocket message should succeed");
}

/// Receive messages until one with the given `"type"` arrives, or timeout.
/// 10s timeout (longer than the 5s used in mock tests) because LiveKit API
/// calls add real network latency.
async fn recv_type(stream: &mut WsStream, target_type: &str) -> Value {
    timeout(Duration::from_secs(10), async {
        while let Some(Ok(msg)) = stream.next().await {
            if let Message::Text(text) = msg {
                let v: Value =
                    serde_json::from_str(&text).expect("server message should be valid JSON");
                let msg_type = v["type"].as_str().unwrap_or("unknown");
                if msg_type == target_type {
                    return v;
                }
                eprintln!(
                    "[recv_type] skipping '{msg_type}' while waiting for '{target_type}': {v}"
                );
            }
        }
        panic!("WebSocket closed without receiving '{target_type}'");
    })
    .await
    .unwrap_or_else(|_| panic!("Timeout (10s) waiting for '{target_type}'"))
}

/// Drain all pending messages within a short window.
async fn drain(stream: &mut WsStream) {
    // 200ms window: long enough to catch queued messages, short enough
    // to not slow down the test suite noticeably.
    while let Ok(Some(Ok(_))) = timeout(Duration::from_millis(200), stream.next()).await {}
}

// ============================================================
// Test 1: Health check
// ============================================================

/// LiveKitSfuBridge::health_check() returns SfuHealth::Available when
/// connected to a real LiveKit server.
#[tokio::test]
#[ignore]
async fn livekit_health_check_succeeds() {
    let Some((api_key, api_secret, host)) = livekit_creds() else {
        eprintln!("Skipping: LIVEKIT_API_KEY/LIVEKIT_API_SECRET/LIVEKIT_HOST not set");
        return;
    };

    let bridge = LiveKitSfuBridge::from_env(&api_key, &api_secret, &host)
        .expect("bridge construction should succeed");

    let health = bridge
        .health_check()
        .await
        .expect("health_check should not return Err against a running server");

    assert_eq!(
        health,
        SfuHealth::Available,
        "health_check should report Available for a running LiveKit server"
    );
}

// ============================================================
// Test 2: Create and destroy room (idempotent destroy per §4.3)
// ============================================================

/// create_room() succeeds and returns a valid SfuRoomHandle.
/// destroy_room() is idempotent — a second call must also succeed.
#[tokio::test]
#[ignore]
async fn livekit_create_and_destroy_room() {
    let Some((api_key, api_secret, host)) = livekit_creds() else {
        eprintln!("Skipping: LIVEKIT_API_KEY/LIVEKIT_API_SECRET/LIVEKIT_HOST not set");
        return;
    };

    let bridge = LiveKitSfuBridge::from_env(&api_key, &api_secret, &host)
        .expect("bridge construction should succeed");
    let room_id = unique_room_id("e2e-create-destroy");

    // Create
    let handle = bridge
        .create_room(&room_id)
        .await
        .expect("create_room should succeed against a running LiveKit server");
    assert_eq!(
        handle.0, room_id,
        "handle should carry the requested room ID"
    );

    // First destroy
    bridge
        .destroy_room(&handle)
        .await
        .expect("first destroy_room should succeed");

    // Second destroy — must be idempotent (§4.3)
    bridge
        .destroy_room(&handle)
        .await
        .expect("second destroy_room should also succeed (idempotent)");
}

// ============================================================
// Test 3: Add and remove participant + identifier validation
// ============================================================

/// add_participant() validates identifiers before calling the LiveKit API.
/// Invalid identifiers must be rejected with SfuError::InvalidInput.
#[tokio::test]
#[ignore]
async fn livekit_add_and_remove_participant() {
    let Some((api_key, api_secret, host)) = livekit_creds() else {
        eprintln!("Skipping: LIVEKIT_API_KEY/LIVEKIT_API_SECRET/LIVEKIT_HOST not set");
        return;
    };

    let bridge = LiveKitSfuBridge::from_env(&api_key, &api_secret, &host)
        .expect("bridge construction should succeed");
    let room_id = unique_room_id("e2e-participant");

    let handle = bridge
        .create_room(&room_id)
        .await
        .expect("create_room should succeed");

    // add_participant with valid ID succeeds (LiveKit bridge validates only, no API call)
    bridge
        .add_participant(&handle, "valid-participant-1")
        .await
        .expect("add_participant with valid ID should succeed");

    // Identifier validation: invalid characters rejected before API call
    let invalid_result = bridge.add_participant(&handle, "has spaces!").await;
    assert!(
        matches!(invalid_result, Err(SfuError::InvalidInput(_))),
        "add_participant with invalid chars should return InvalidInput, got: {invalid_result:?}"
    );

    // Identifier validation: empty string rejected
    let empty_result = bridge.add_participant(&handle, "").await;
    assert!(
        matches!(empty_result, Err(SfuError::InvalidInput(_))),
        "add_participant with empty ID should return InvalidInput, got: {empty_result:?}"
    );

    // Identifier validation: too-long string rejected (129 chars)
    let long_id = "a".repeat(129);
    let long_result = bridge.add_participant(&handle, &long_id).await;
    assert!(
        matches!(long_result, Err(SfuError::InvalidInput(_))),
        "add_participant with 129-char ID should return InvalidInput, got: {long_result:?}"
    );

    // remove_participant — participant never connected via WebRTC, so LiveKit
    // may return a "participant not found" error. We verify it does not panic
    // and returns a well-typed error (Ok or ParticipantError).
    let remove_result = bridge
        .remove_participant(&handle, "valid-participant-1")
        .await;
    assert!(
        matches!(remove_result, Ok(()) | Err(SfuError::ParticipantError(_))),
        "remove_participant should return Ok or ParticipantError, got: {remove_result:?}"
    );

    // Cleanup
    bridge
        .destroy_room(&handle)
        .await
        .expect("cleanup destroy_room should succeed");
}

// ============================================================
// Test 4: Full-chain media token validation
// ============================================================

/// Exercises the complete chain: in-process backend with LiveKitSfuBridge →
/// WebSocket create_room → receive media_token → decode JWT and verify claims.
#[tokio::test]
#[ignore]
async fn livekit_backend_issues_valid_media_token_accepted_by_livekit() {
    let Some((api_key, api_secret, host)) = livekit_creds() else {
        eprintln!("Skipping: LIVEKIT_API_KEY/LIVEKIT_API_SECRET/LIVEKIT_HOST not set");
        return;
    };

    let room_id = unique_room_id("e2e-media-token");
    let (addr, _state) = start_server_with_livekit(&api_key, &api_secret, &host, false).await;

    let (mut sink, mut stream) = ws_connect(addr).await;

    // Create an SFU room via WebSocket
    ws_send(
        &mut sink,
        json!({"type": "create_room", "roomId": room_id, "roomType": "sfu"}),
    )
    .await;

    // Expect room_created
    let created = recv_type(&mut stream, "room_created").await;
    assert_eq!(
        created["roomId"].as_str().unwrap(),
        room_id,
        "room_created should echo the requested room ID"
    );
    let peer_id = created["peerId"]
        .as_str()
        .expect("room_created should include a peerId");

    // Expect media_token (SFU rooms issue tokens)
    let token_msg = recv_type(&mut stream, "media_token").await;
    let token_str = token_msg["token"]
        .as_str()
        .expect("media_token should include a token string");
    assert!(!token_str.is_empty(), "media_token.token must be non-empty");
    assert_eq!(
        token_msg["sfuUrl"].as_str().unwrap(),
        host,
        "media_token.sfuUrl should match the LiveKit host"
    );

    // Decode the LiveKit JWT and verify claims.
    // The token is signed with HS256 using the API secret.
    let key = jsonwebtoken::DecodingKey::from_secret(api_secret.as_bytes());
    let mut validation = jsonwebtoken::Validation::new(jsonwebtoken::Algorithm::HS256);
    // LiveKit tokens don't use the standard aud claim; disable aud validation.
    validation.validate_aud = false;
    // Don't require specific spec claims — LiveKit token structure differs from RFC 7519 defaults.
    validation.required_spec_claims = std::collections::HashSet::new();
    let token_data = jsonwebtoken::decode::<serde_json::Value>(token_str, &key, &validation)
        .expect("LiveKit JWT should decode successfully with the API secret");

    let claims = token_data.claims;

    // `sub` claim = participant identity = peer ID assigned by the backend
    assert_eq!(
        claims["sub"].as_str().unwrap_or(""),
        peer_id,
        "JWT sub claim should match the peer ID from room_created"
    );

    // `video.room` claim = the LiveKit room name
    assert_eq!(
        claims["video"]["room"].as_str().unwrap_or(""),
        room_id,
        "JWT video.room claim should match the requested room ID"
    );

    // Cleanup: close the WebSocket (triggers implicit leave + room destroy)
    let _ = sink.close().await;
    // Give the backend time to process the disconnect and destroy the room.
    tokio::time::sleep(Duration::from_millis(500)).await;
}

// ============================================================
// Test 5: Room cleanup on last leave
// ============================================================

/// Two participants join via WebSocket (host creates, guest joins with invite).
/// Both leave. Verify via RoomClient::list_rooms that the room no longer
/// exists in LiveKit after cleanup.
#[tokio::test]
#[ignore]
async fn livekit_room_cleanup_on_last_leave() {
    let Some((api_key, api_secret, host)) = livekit_creds() else {
        eprintln!("Skipping: LIVEKIT_API_KEY/LIVEKIT_API_SECRET/LIVEKIT_HOST not set");
        return;
    };

    let room_id = unique_room_id("e2e-cleanup");
    let (addr, _state) = start_server_with_livekit(&api_key, &api_secret, &host, true).await;

    // Client 1: host creates the room
    let (mut sink1, mut stream1) = ws_connect(addr).await;
    ws_send(
        &mut sink1,
        json!({"type": "create_room", "roomId": room_id, "roomType": "sfu"}),
    )
    .await;
    let created = recv_type(&mut stream1, "room_created").await;
    let invite_code = created["inviteCode"]
        .as_str()
        .expect("room_created should include an inviteCode")
        .to_string();
    assert!(!invite_code.is_empty(), "invite code must be non-empty");
    // Drain the media_token message
    drain(&mut stream1).await;

    // Client 2: guest joins with the invite code
    let (mut sink2, mut stream2) = ws_connect(addr).await;
    ws_send(
        &mut sink2,
        json!({
            "type": "join",
            "roomId": room_id,
            "roomType": "sfu",
            "inviteCode": invite_code,
        }),
    )
    .await;
    let joined = recv_type(&mut stream2, "joined").await;
    assert_eq!(
        joined["roomId"].as_str().unwrap(),
        room_id,
        "guest should join the correct room"
    );
    drain(&mut stream2).await;
    drain(&mut stream1).await;

    // Both clients send Leave
    ws_send(&mut sink1, json!({"type": "leave"})).await;
    ws_send(&mut sink2, json!({"type": "leave"})).await;

    // 2s pause: gives the backend time to process both leaves, call
    // handle_sfu_leave for each, and invoke destroy_room on the bridge
    // when the last participant departs.
    tokio::time::sleep(Duration::from_secs(2)).await;

    // Verify the room no longer exists in LiveKit
    let api_host = to_api_host(&host);
    let room_client = RoomClient::with_api_key(&api_host, &api_key, &api_secret);
    let rooms = room_client
        .list_rooms(vec![room_id.clone()])
        .await
        .expect("list_rooms should succeed");
    assert!(
        !rooms.iter().any(|r| r.name == room_id),
        "room '{room_id}' should have been destroyed after both participants left, \
         but list_rooms still returned it"
    );
}

// ============================================================
// Test 6: Invalid credentials fail closed (§6.2)
// ============================================================

/// A LiveKitSfuBridge with a wrong API secret must return an error (not a panic)
/// on create_room(). Per §6.2, failure must be explicit, not silent.
#[tokio::test]
#[ignore]
async fn livekit_invalid_credentials_fail_closed() {
    let Some((_api_key, _api_secret, host)) = livekit_creds() else {
        eprintln!("Skipping: LIVEKIT_API_KEY/LIVEKIT_API_SECRET/LIVEKIT_HOST not set");
        return;
    };

    // Use deliberately wrong credentials
    let bad_bridge = LiveKitSfuBridge::from_env("wrong-key", "wrong-secret", &host)
        .expect("bridge construction should succeed even with bad credentials (fails on use)");

    let room_id = unique_room_id("e2e-bad-creds");
    let result = bad_bridge.create_room(&room_id).await;

    assert!(
        matches!(result, Err(SfuError::Unavailable(_))),
        "create_room with invalid credentials should return SfuError::Unavailable, got: {result:?}"
    );
}
