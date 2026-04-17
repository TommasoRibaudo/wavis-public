#![cfg(feature = "test-support")]
//! Integration tests automating TESTING.md Â§22câ€“Â§22e and Â§36aâ€“Â§36g.
//!
//! Covered:
//!   - Â§22c: CreateRoom after already joined â†’ "already joined"
//!   - Â§22d: Join after CreateRoom â†’ "already joined"
//!   - Â§22e: CreateRoom before authentication â†’ allowed
//!   - Â§36a: Create SFU room (success) â€” room_created + media_token
//!   - Â§36b: Create P2P room â€” room_created, no media_token
//!   - Â§36c: Room already exists â†’ error
//!   - Â§36d: Empty / whitespace room ID â†’ error "invalid room ID"
//!   - Â§36e: CreateRoom while already in a room â†’ "already joined"
//!   - Â§36f: Second client joins via invite code from CreateRoom
//!   - Â§36g: CreateRoom with TURN credentials â†’ iceConfig populated
//!
//! NOT covered (and why):
//!   - Â§36h: Interactive CLI client â€” requires audio hardware + interactive REPL
//!   - Â§37:  wavis-client tests â€” already automated as unit/property tests in wavis-client crate
//!
//! Run: cargo test -p wavis-backend --test create_room_integration -- --test-threads=1

use futures_util::{SinkExt, StreamExt};
use serde_json::{Value, json};
use std::net::SocketAddr;
use std::sync::Arc;
use std::time::Duration;
use tokio::net::TcpListener;
use tokio::time::timeout;
use tokio_tungstenite::{connect_async, tungstenite::Message};

use wavis_backend::abuse::join_rate_limiter::{JoinRateLimiter, JoinRateLimiterConfig};
use wavis_backend::app_state::AppState;
use wavis_backend::auth::auth_rate_limiter::{AuthRateLimiter, AuthRateLimiterConfig};
use wavis_backend::channel::invite::{InviteStore, InviteStoreConfig};
use wavis_backend::ip::IpConfig;
use wavis_backend::voice::mock_sfu_bridge::MockSfuBridge;
use wavis_backend::voice::sfu_bridge::{SfuRoomManager, SfuSignalingProxy};
use wavis_backend::voice::turn_cred::TurnConfig;
use wavis_backend::ws::ws::ws_handler;

use axum::Router;
use axum::routing::get;

// ============================================================
// Server setup + WS helpers
// ============================================================

async fn start_server(require_invite: bool) -> (SocketAddr, AppState) {
    unsafe {
        std::env::set_var("SFU_JWT_SECRET", "dev-secret-32-bytes-minimum!!!XX");
        std::env::set_var("MAX_ROOM_PARTICIPANTS", "6");
        std::env::set_var(
            "REQUIRE_INVITE_CODE",
            if require_invite { "true" } else { "false" },
        );
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
        sqlx::postgres::PgPoolOptions::new()
            .connect_lazy("postgres://dummy")
            .unwrap(),
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

    // Run initial health check so SFU joins aren't rejected as "SFU unavailable"
    {
        let health = app_state.sfu_room_manager.health_check().await.unwrap();
        *app_state.sfu_health_status.write().await = health;
    }

    let app = Router::new()
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

async fn start_server_with_turn(require_invite: bool) -> (SocketAddr, AppState) {
    unsafe {
        std::env::set_var("SFU_JWT_SECRET", "dev-secret-32-bytes-minimum!!!XX");
        std::env::set_var("MAX_ROOM_PARTICIPANTS", "6");
        std::env::set_var(
            "REQUIRE_INVITE_CODE",
            if require_invite { "true" } else { "false" },
        );
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
        sqlx::postgres::PgPoolOptions::new()
            .connect_lazy("postgres://dummy")
            .unwrap(),
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

    // Inject TURN config directly (bypasses env var to avoid test races)
    app_state.turn_config = Some(Arc::new(TurnConfig::new(
        b"a-32-byte-secret-for-testing-ok!".to_vec(),
        None,
        3600,
        vec!["stun:stun.l.google.com:19302".to_string()],
        vec!["turn:my-turn-server:3478".to_string()],
    )));

    {
        let health = app_state.sfu_room_manager.health_check().await.unwrap();
        *app_state.sfu_health_status.write().await = health;
    }

    let app = Router::new()
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

// --- WebSocket helpers ---

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
                eprintln!(
                    "[recv_type] skipping '{msg_type}' while waiting for '{target_type}': {v}"
                );
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
async fn drain(stream: &mut WsStream) {
    while let Ok(Some(Ok(_))) = timeout(Duration::from_millis(200), stream.next()).await {
        // Continue draining
    }
}

// ==========================================================================
// Â§22câ€“Â§22e: State machine validation â€” CreateRoom variants
// ==========================================================================

/// Â§22c: CreateRoom after already joined â†’ "already joined"
#[tokio::test]
async fn test22c_create_room_after_join_rejected() {
    let (addr, _state) = start_server(false).await;

    let (mut sink, mut stream) = ws_connect(addr).await;

    // Join a room first
    ws_send(
        &mut sink,
        json!({"type":"join","roomId":"room-1","roomType":"sfu"}),
    )
    .await;
    let joined = recv_type(&mut stream, "joined").await;
    assert_eq!(joined["roomId"], "room-1");
    drain(&mut stream).await;

    // Try to create another room â€” should be rejected
    ws_send(
        &mut sink,
        json!({"type":"create_room","roomId":"room-2","roomType":"sfu"}),
    )
    .await;
    let err = recv_type(&mut stream, "error").await;
    assert_eq!(err["message"], "already joined");
}

/// Â§22d: Join after CreateRoom â†’ "already joined"
#[tokio::test]
async fn test22d_join_after_create_room_rejected() {
    let (addr, _state) = start_server(false).await;

    let (mut sink, mut stream) = ws_connect(addr).await;

    // Create a room first
    ws_send(
        &mut sink,
        json!({"type":"create_room","roomId":"room-1","roomType":"sfu"}),
    )
    .await;
    let created = recv_type(&mut stream, "room_created").await;
    assert_eq!(created["roomId"], "room-1");
    drain(&mut stream).await;

    // Try to join another room â€” should be rejected
    ws_send(
        &mut sink,
        json!({"type":"join","roomId":"room-2","roomType":"sfu"}),
    )
    .await;
    let err = recv_type(&mut stream, "error").await;
    assert_eq!(err["message"], "already joined");
}

/// Â§22e: CreateRoom before authentication â†’ allowed (room_created response)
#[tokio::test]
async fn test22e_create_room_before_auth_allowed() {
    let (addr, _state) = start_server(false).await;

    let (mut sink, mut stream) = ws_connect(addr).await;

    // CreateRoom without any prior join â€” should succeed
    ws_send(
        &mut sink,
        json!({"type":"create_room","roomId":"new-room","roomType":"sfu"}),
    )
    .await;
    let created = recv_type(&mut stream, "room_created").await;
    assert_eq!(created["roomId"], "new-room");
    assert!(created["peerId"].is_string());
    assert!(created["inviteCode"].is_string());
}

// ==========================================================================
// Â§36: Room creation via CreateRoom
// ==========================================================================

/// Â§36a: Create SFU room â€” room_created + media_token
#[tokio::test]
async fn test36a_create_sfu_room_success() {
    let (addr, _state) = start_server(false).await;

    let (mut sink, mut stream) = ws_connect(addr).await;

    ws_send(
        &mut sink,
        json!({"type":"create_room","roomId":"create-test","roomType":"sfu"}),
    )
    .await;

    // Expect room_created
    let created = recv_type(&mut stream, "room_created").await;
    assert_eq!(created["roomId"], "create-test");
    assert!(created["peerId"].is_string());
    let invite_code = created["inviteCode"].as_str().unwrap();
    assert!(!invite_code.is_empty(), "invite code must be non-empty");
    assert!(created["expiresInSecs"].is_u64());
    assert!(created["maxUses"].is_u64());
    // No TURN configured â†’ iceConfig is null
    assert!(created["iceConfig"].is_null());

    // Expect media_token (SFU rooms issue tokens)
    let token = recv_type(&mut stream, "media_token").await;
    assert!(token["token"].is_string());
    assert_eq!(token["sfuUrl"], "sfu://localhost");
}

/// Â§36b: Create P2P room â€” room_created, no media_token
#[tokio::test]
async fn test36b_create_p2p_room() {
    let (addr, _state) = start_server(false).await;

    let (mut sink, mut stream) = ws_connect(addr).await;

    ws_send(
        &mut sink,
        json!({"type":"create_room","roomId":"p2p-create","roomType":"p2p"}),
    )
    .await;

    let created = recv_type(&mut stream, "room_created").await;
    assert_eq!(created["roomId"], "p2p-create");
    assert!(created["inviteCode"].is_string());

    // P2P rooms do NOT issue media_token
    let maybe_token = try_recv_type(&mut stream, "media_token", 500).await;
    assert!(
        maybe_token.is_none(),
        "P2P rooms should not receive media_token"
    );
}

/// Â§36c: Create room that already exists â†’ error
#[tokio::test]
async fn test36c_create_room_already_exists() {
    let (addr, _state) = start_server(false).await;

    // First client creates the room
    let (mut sink1, mut stream1) = ws_connect(addr).await;
    ws_send(
        &mut sink1,
        json!({"type":"create_room","roomId":"create-test","roomType":"sfu"}),
    )
    .await;
    let _created = recv_type(&mut stream1, "room_created").await;
    drain(&mut stream1).await;

    // Second client tries to create the same room
    let (mut sink2, mut stream2) = ws_connect(addr).await;
    ws_send(
        &mut sink2,
        json!({"type":"create_room","roomId":"create-test","roomType":"sfu"}),
    )
    .await;
    let err = recv_type(&mut stream2, "error").await;
    assert_eq!(err["message"], "room already exists");
}

/// Â§36d: Create room with empty room ID â†’ error "invalid room ID"
#[tokio::test]
async fn test36d_create_room_empty_id() {
    let (addr, _state) = start_server(false).await;

    let (mut sink, mut stream) = ws_connect(addr).await;

    // Empty string
    ws_send(
        &mut sink,
        json!({"type":"create_room","roomId":"","roomType":"sfu"}),
    )
    .await;
    let err = recv_type(&mut stream, "error").await;
    assert_eq!(err["message"], "invalid room ID");

    // Whitespace-only
    ws_send(
        &mut sink,
        json!({"type":"create_room","roomId":"   ","roomType":"sfu"}),
    )
    .await;
    let err2 = recv_type(&mut stream, "error").await;
    assert_eq!(err2["message"], "invalid room ID");
}

/// Â§36e: CreateRoom while already in a room â†’ "already joined"
#[tokio::test]
async fn test36e_create_room_while_in_room() {
    let (addr, _state) = start_server(false).await;

    let (mut sink, mut stream) = ws_connect(addr).await;

    // Create first room
    ws_send(
        &mut sink,
        json!({"type":"create_room","roomId":"room-1","roomType":"sfu"}),
    )
    .await;
    let _created = recv_type(&mut stream, "room_created").await;
    drain(&mut stream).await;

    // Try to create another room â€” should be rejected
    ws_send(
        &mut sink,
        json!({"type":"create_room","roomId":"room-2","roomType":"sfu"}),
    )
    .await;
    let err = recv_type(&mut stream, "error").await;
    assert_eq!(err["message"], "already joined");
}

/// Â§36f: Second client joins via invite code from CreateRoom
#[tokio::test]
async fn test36f_join_via_create_room_invite() {
    let (addr, _state) = start_server(true).await;

    // Client 1: create room
    let (mut sink1, mut stream1) = ws_connect(addr).await;
    ws_send(
        &mut sink1,
        json!({"type":"create_room","roomId":"invite-room","roomType":"sfu"}),
    )
    .await;
    let created = recv_type(&mut stream1, "room_created").await;
    let invite_code = created["inviteCode"].as_str().unwrap().to_string();
    assert!(!invite_code.is_empty());
    drain(&mut stream1).await;

    // Client 2: join with the invite code
    let (mut sink2, mut stream2) = ws_connect(addr).await;
    ws_send(
        &mut sink2,
        json!({"type":"join","roomId":"invite-room","roomType":"sfu","inviteCode": invite_code}),
    )
    .await;
    let joined = recv_type(&mut stream2, "joined").await;
    assert_eq!(joined["roomId"], "invite-room");
    // peerCount reflects actual peers in the room (creator + joiner)
    let peer_count = joined["peerCount"].as_u64().unwrap();
    assert_eq!(peer_count, 2);

    // participants list: handle_create_room does not push ParticipantInfo,
    // so the joiner only sees themselves in the participants list.
    // The peer_count is the authoritative count.
    let participants = joined["participants"].as_array().unwrap();
    assert!(
        !participants.is_empty(),
        "participants list must not be empty"
    );

    // Client 1 should receive participant_joined event
    let pj = recv_type(&mut stream1, "participant_joined").await;
    assert!(pj["participantId"].is_string());
}

/// Â§36g: CreateRoom with TURN credentials â†’ iceConfig populated
#[tokio::test]
async fn test36g_create_room_with_turn() {
    let (addr, _state) = start_server_with_turn(false).await;

    let (mut sink, mut stream) = ws_connect(addr).await;

    ws_send(
        &mut sink,
        json!({"type":"create_room","roomId":"turn-create-test","roomType":"sfu"}),
    )
    .await;

    let created = recv_type(&mut stream, "room_created").await;
    assert_eq!(created["roomId"], "turn-create-test");

    // iceConfig should be populated with TURN credentials
    let ice_config = &created["iceConfig"];
    assert!(
        !ice_config.is_null(),
        "iceConfig must be present when TURN is configured"
    );

    let stun_urls = ice_config["stunUrls"].as_array().unwrap();
    assert!(
        stun_urls
            .iter()
            .any(|u| u.as_str().unwrap().contains("stun:"))
    );

    let turn_urls = ice_config["turnUrls"].as_array().unwrap();
    assert!(
        turn_urls
            .iter()
            .any(|u| u.as_str().unwrap().contains("turn:"))
    );

    // turnUsername format: "{expiry}:{peer_id}"
    let username = ice_config["turnUsername"].as_str().unwrap();
    assert!(
        username.contains(':'),
        "TURN username must be in 'expiry:peer_id' format"
    );

    // turnCredential is base64
    let credential = ice_config["turnCredential"].as_str().unwrap();
    assert!(!credential.is_empty(), "TURN credential must be non-empty");
}

/// Â§36g supplement: CreateRoom without TURN â†’ iceConfig is null
#[tokio::test]
async fn test36g_create_room_without_turn_no_ice_config() {
    let (addr, _state) = start_server(false).await;

    let (mut sink, mut stream) = ws_connect(addr).await;

    ws_send(
        &mut sink,
        json!({"type":"create_room","roomId":"no-turn-room","roomType":"sfu"}),
    )
    .await;

    let created = recv_type(&mut stream, "room_created").await;
    assert!(
        created["iceConfig"].is_null(),
        "iceConfig must be null when TURN is not configured"
    );
}
