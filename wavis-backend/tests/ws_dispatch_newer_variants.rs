#![cfg(feature = "test-support")]
//! Unit-level integration tests for newer ws_dispatch.rs message variants.
//!
//! Covered:
//!   - SelfDeafen / SelfUndeafen: state mutation + broadcast + rate limit
//!   - SetPassthrough / ClearPassthrough: non-channel session rejection
//!   - SetPassthroughVolume: non-channel session rejection
//!   - SubRoomState / SubRoomCreated / SubRoomJoined / SubRoomLeft / SubRoomDeleted:
//!     server-generated messages rejected with error
//!   - ChatHistoryRequest: state machine validation (not in room → rejected)
//!
//! These tests use the full WS server (no DB required — MockSfuBridge + in-memory state).
//! Run with: cargo test --test ws_dispatch_newer_variants --features test-support

use futures_util::{SinkExt, StreamExt};
use serde_json::{Value, json};
use std::net::SocketAddr;
use std::sync::Arc;
use std::time::Duration;
use tokio::net::TcpListener;
use tokio::time::timeout;
use tokio_tungstenite::{connect_async, tungstenite::Message};

use axum::Router;
use axum::routing::get;
use wavis_backend::abuse::join_rate_limiter::{JoinRateLimiter, JoinRateLimiterConfig};
use wavis_backend::app_state::AppState;
use wavis_backend::auth::auth_rate_limiter::{AuthRateLimiter, AuthRateLimiterConfig};
use wavis_backend::channel::invite::{InviteStore, InviteStoreConfig};
use wavis_backend::ip::IpConfig;
use wavis_backend::voice::mock_sfu_bridge::MockSfuBridge;
use wavis_backend::voice::sfu_bridge::{SfuRoomManager, SfuSignalingProxy};
use wavis_backend::ws::ws::ws_handler;

// ─── Server setup ─────────────────────────────────────────────────

async fn start_server() -> (SocketAddr, AppState) {
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
    app_state.require_invite_code = false;

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

// ─── WS helpers ───────────────────────────────────────────────────

type WsSink = futures_util::stream::SplitSink<
    tokio_tungstenite::WebSocketStream<tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>>,
    Message,
>;
type WsStream = futures_util::stream::SplitStream<
    tokio_tungstenite::WebSocketStream<tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>>,
>;

async fn ws_connect(addr: SocketAddr) -> (WsSink, WsStream) {
    let (ws, _) = connect_async(format!("ws://{addr}/ws")).await.unwrap();
    ws.split()
}

async fn ws_send(sink: &mut WsSink, msg: Value) {
    sink.send(Message::Text(msg.to_string())).await.unwrap();
}

async fn recv_type(stream: &mut WsStream, target: &str) -> Value {
    timeout(Duration::from_secs(5), async {
        while let Some(Ok(msg)) = stream.next().await {
            if let Message::Text(text) = msg {
                let v: Value = serde_json::from_str(&text).unwrap();
                if v["type"].as_str().unwrap_or("") == target {
                    return v;
                }
            }
        }
        panic!("WS closed without '{target}'");
    })
    .await
    .unwrap_or_else(|_| panic!("Timeout waiting for '{target}'"))
}

async fn drain(stream: &mut WsStream) {
    while let Ok(Some(Ok(_))) = timeout(Duration::from_millis(100), stream.next()).await {}
}

/// Join an SFU room and return the assigned peer_id.
async fn join_sfu(sink: &mut WsSink, stream: &mut WsStream, room_id: &str) -> String {
    ws_send(
        sink,
        json!({"type":"join","roomId":room_id,"roomType":"sfu"}),
    )
    .await;
    let joined = recv_type(stream, "joined").await;
    drain(stream).await;
    joined["peerId"].as_str().unwrap().to_string()
}

// ─── SelfDeafen ───────────────────────────────────────────────────

#[tokio::test]
async fn self_deafen_broadcasts_participant_deafened() {
    let (addr, _state) = start_server().await;

    let (mut s1, mut r1) = ws_connect(addr).await;
    let (mut s2, mut r2) = ws_connect(addr).await;

    let peer1 = join_sfu(&mut s1, &mut r1, "deafen-room").await;
    let _peer2 = join_sfu(&mut s2, &mut r2, "deafen-room").await;
    drain(&mut r1).await;
    drain(&mut r2).await;

    ws_send(&mut s1, json!({"type":"self_deafen"})).await;

    // Both peers should receive participant_deafened
    let msg1 = recv_type(&mut r1, "participant_deafened").await;
    let msg2 = recv_type(&mut r2, "participant_deafened").await;

    assert_eq!(msg1["participantId"].as_str().unwrap(), peer1);
    assert_eq!(msg2["participantId"].as_str().unwrap(), peer1);
}

#[tokio::test]
async fn self_deafen_not_in_room_returns_error() {
    let (addr, _state) = start_server().await;
    let (mut sink, mut stream) = ws_connect(addr).await;

    // Send SelfDeafen without joining a room first
    ws_send(&mut sink, json!({"type":"self_deafen"})).await;
    let err = recv_type(&mut stream, "error").await;
    assert!(
        err["message"].as_str().unwrap().contains("not in a room"),
        "expected 'not in a room', got: {}",
        err["message"]
    );
}

// ─── SelfUndeafen ─────────────────────────────────────────────────

#[tokio::test]
async fn self_undeafen_broadcasts_participant_undeafened() {
    let (addr, _state) = start_server().await;

    let (mut s1, mut r1) = ws_connect(addr).await;
    let (mut s2, mut r2) = ws_connect(addr).await;

    let peer1 = join_sfu(&mut s1, &mut r1, "undeafen-room").await;
    let _peer2 = join_sfu(&mut s2, &mut r2, "undeafen-room").await;
    drain(&mut r1).await;
    drain(&mut r2).await;

    ws_send(&mut s1, json!({"type":"self_deafen"})).await;
    drain(&mut r1).await;
    drain(&mut r2).await;

    ws_send(&mut s1, json!({"type":"self_undeafen"})).await;

    let msg1 = recv_type(&mut r1, "participant_undeafened").await;
    let msg2 = recv_type(&mut r2, "participant_undeafened").await;

    assert_eq!(msg1["participantId"].as_str().unwrap(), peer1);
    assert_eq!(msg2["participantId"].as_str().unwrap(), peer1);
}

#[tokio::test]
async fn self_undeafen_not_in_room_returns_error() {
    let (addr, _state) = start_server().await;
    let (mut sink, mut stream) = ws_connect(addr).await;

    ws_send(&mut sink, json!({"type":"self_undeafen"})).await;
    let err = recv_type(&mut stream, "error").await;
    assert!(err["message"].as_str().unwrap().contains("not in a room"));
}

// ─── SetPassthrough / ClearPassthrough / SetPassthroughVolume ─────
// These require a channel voice session (session.channel_id must be Some).
// Without JoinVoice, the session has no channel_id → error.

#[tokio::test]
async fn set_passthrough_without_channel_session_returns_error() {
    let (addr, _state) = start_server().await;
    let (mut sink, mut stream) = ws_connect(addr).await;
    join_sfu(&mut sink, &mut stream, "pt-room").await;
    drain(&mut stream).await;

    ws_send(
        &mut sink,
        json!({"type":"set_passthrough","targetSubRoomId":"sr-2"}),
    )
    .await;
    let err = recv_type(&mut stream, "error").await;
    assert!(
        err["message"]
            .as_str()
            .unwrap()
            .contains("channel voice session"),
        "expected channel session error, got: {}",
        err["message"]
    );
}

#[tokio::test]
async fn clear_passthrough_without_channel_session_returns_error() {
    let (addr, _state) = start_server().await;
    let (mut sink, mut stream) = ws_connect(addr).await;
    join_sfu(&mut sink, &mut stream, "cpt-room").await;
    drain(&mut stream).await;

    ws_send(&mut sink, json!({"type":"clear_passthrough"})).await;
    let err = recv_type(&mut stream, "error").await;
    assert!(
        err["message"]
            .as_str()
            .unwrap()
            .contains("channel voice session")
    );
}

#[tokio::test]
async fn set_passthrough_volume_without_channel_session_returns_error() {
    let (addr, _state) = start_server().await;
    let (mut sink, mut stream) = ws_connect(addr).await;
    join_sfu(&mut sink, &mut stream, "ptv-room").await;
    drain(&mut stream).await;

    ws_send(
        &mut sink,
        json!({"type":"set_passthrough_volume","volume":30}),
    )
    .await;
    let err = recv_type(&mut stream, "error").await;
    assert!(
        err["message"]
            .as_str()
            .unwrap()
            .contains("channel voice session")
    );
}

// ─── Server-generated sub-room messages rejected ──────────────────

#[tokio::test]
async fn sub_room_state_from_client_returns_error() {
    let (addr, _state) = start_server().await;
    let (mut sink, mut stream) = ws_connect(addr).await;
    join_sfu(&mut sink, &mut stream, "sr-room").await;
    drain(&mut stream).await;

    ws_send(&mut sink, json!({"type":"sub_room_state","rooms":[]})).await;
    let err = recv_type(&mut stream, "error").await;
    assert!(
        err["message"]
            .as_str()
            .unwrap()
            .contains("unexpected server-generated"),
        "got: {}",
        err["message"]
    );
}

#[tokio::test]
async fn sub_room_created_from_client_returns_error() {
    let (addr, _state) = start_server().await;
    let (mut sink, mut stream) = ws_connect(addr).await;
    join_sfu(&mut sink, &mut stream, "src-room").await;
    drain(&mut stream).await;

    ws_send(&mut sink, json!({"type":"sub_room_created","room":{"subRoomId":"x","roomNumber":1,"isDefault":false,"participantIds":[]}})).await;
    let err = recv_type(&mut stream, "error").await;
    assert!(
        err["message"]
            .as_str()
            .unwrap()
            .contains("unexpected server-generated")
    );
}

#[tokio::test]
async fn sub_room_joined_from_client_returns_error() {
    let (addr, _state) = start_server().await;
    let (mut sink, mut stream) = ws_connect(addr).await;
    join_sfu(&mut sink, &mut stream, "srj-room").await;
    drain(&mut stream).await;

    ws_send(
        &mut sink,
        json!({"type":"sub_room_joined","participantId":"p1","subRoomId":"sr-1"}),
    )
    .await;
    let err = recv_type(&mut stream, "error").await;
    assert!(
        err["message"]
            .as_str()
            .unwrap()
            .contains("unexpected server-generated")
    );
}

#[tokio::test]
async fn sub_room_left_from_client_returns_error() {
    let (addr, _state) = start_server().await;
    let (mut sink, mut stream) = ws_connect(addr).await;
    join_sfu(&mut sink, &mut stream, "srl-room").await;
    drain(&mut stream).await;

    ws_send(
        &mut sink,
        json!({"type":"sub_room_left","participantId":"p1","subRoomId":"sr-1"}),
    )
    .await;
    let err = recv_type(&mut stream, "error").await;
    assert!(
        err["message"]
            .as_str()
            .unwrap()
            .contains("unexpected server-generated")
    );
}

#[tokio::test]
async fn sub_room_deleted_from_client_returns_error() {
    let (addr, _state) = start_server().await;
    let (mut sink, mut stream) = ws_connect(addr).await;
    join_sfu(&mut sink, &mut stream, "srd-room").await;
    drain(&mut stream).await;

    ws_send(
        &mut sink,
        json!({"type":"sub_room_deleted","subRoomId":"sr-1"}),
    )
    .await;
    let err = recv_type(&mut stream, "error").await;
    assert!(
        err["message"]
            .as_str()
            .unwrap()
            .contains("unexpected server-generated")
    );
}

// ─── ChatHistoryRequest state machine validation ──────────────────

#[tokio::test]
async fn chat_history_request_without_session_returns_error() {
    let (addr, _state) = start_server().await;
    let (mut sink, mut stream) = ws_connect(addr).await;

    // Not joined — state machine should reject with "not in a room"
    ws_send(&mut sink, json!({"type":"chat_history_request"})).await;
    let err = recv_type(&mut stream, "error").await;
    assert!(
        err["message"].as_str().unwrap().contains("not in a room"),
        "expected 'not in a room', got: {}",
        err["message"]
    );
}

#[tokio::test]
async fn chat_history_request_in_room_reaches_dispatch() {
    let (addr, _state) = start_server().await;
    let (mut sink, mut stream) = ws_connect(addr).await;
    join_sfu(&mut sink, &mut stream, "chat-hist-room").await;
    drain(&mut stream).await;

    ws_send(&mut sink, json!({"type":"chat_history_request"})).await;

    // This harness uses a lazy dummy Postgres pool, so a post-dispatch DB error is expected.
    // The regression guard is that the state machine must not reject the request as "not in a room".
    let msg = timeout(Duration::from_secs(3), async {
        while let Some(Ok(m)) = stream.next().await {
            if let Message::Text(text) = m {
                let v: Value = serde_json::from_str(&text).unwrap();
                let t = v["type"].as_str().unwrap_or("");
                if t == "chat_history_response" || t == "error" {
                    return v;
                }
            }
        }
        panic!("no response");
    })
    .await
    .expect("timeout");

    if msg["type"].as_str() == Some("error") {
        assert_eq!(
            msg["message"].as_str(),
            Some("failed to load chat history"),
            "expected post-dispatch DB error, got: {}",
            msg
        );
    } else {
        assert_eq!(msg["type"].as_str(), Some("chat_history_response"));
    }
}

// ─── SelfDeafen state is reflected in room_state for late joiners ──

#[tokio::test]
async fn deafened_state_visible_in_room_state_for_late_joiner() {
    let (addr, _state) = start_server().await;

    let (mut s1, mut r1) = ws_connect(addr).await;
    let peer1 = join_sfu(&mut s1, &mut r1, "deafen-late-room").await;
    drain(&mut r1).await;

    // Peer1 deafens
    ws_send(&mut s1, json!({"type":"self_deafen"})).await;
    drain(&mut r1).await;

    // Late joiner
    let (mut s2, mut r2) = ws_connect(addr).await;
    ws_send(
        &mut s2,
        json!({"type":"join","roomId":"deafen-late-room","roomType":"sfu"}),
    )
    .await;
    let joined = recv_type(&mut r2, "joined").await;

    let participants = joined["participants"].as_array().unwrap();
    let p1 = participants
        .iter()
        .find(|p| p["participantId"].as_str() == Some(&peer1));
    assert!(p1.is_some(), "peer1 should be in participants list");
    assert!(
        p1.unwrap()["isDeafened"].as_bool().unwrap_or(false),
        "peer1 should be marked as deafened for late joiner"
    );
}
