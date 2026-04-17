#![cfg(feature = "test-support")]
//! WebSocket integration tests for multi-share screen sharing (Section 13).
//!
//! Automates the manual test steps from doc/testing/backend-manual.md Â§13:
//!   - Start share â†’ share_started broadcast
//!   - Concurrent shares from different participants
//!   - Idempotent start_share (same sender twice â†’ no-op)
//!   - Stop share â†’ share_stopped broadcast
//!   - Host-directed stop (targetParticipantId)
//!   - Non-host targeted stop â†’ permission error
//!   - Stop all shares (host) â†’ one share_stopped per sharer
//!   - Stop all shares (non-host) â†’ permission error
//!   - Late joiner receives share_state snapshot
//!   - First joiner gets empty share_state
//!   - Disconnect cleanup â†’ share_stopped only for sharer
//!   - Disconnect non-sharer â†’ no share signal
//!   - P2P room â†’ "screen sharing unavailable in P2P mode"
//!
//! NOT covered (and why):
//!   - CLI REPL commands (Section 6 of client-tests.md) â€” requires interactive terminal
//!   - Audio/media tests â€” require hardware
//!   - Stress test scenarios â€” separate harness (tools/stress/)
//!   - Client-side property test (Property 10) â€” already in clients/shared/src/room_session/
//!
//! Run: cargo test -p wavis-backend --test screen_share_ws_integration -- --test-threads=1

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
use wavis_backend::ws::ws::ws_handler;

use axum::Router;
use axum::routing::get;

type WsSink = futures_util::stream::SplitSink<
    tokio_tungstenite::WebSocketStream<tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>>,
    Message,
>;
type WsStream = futures_util::stream::SplitStream<
    tokio_tungstenite::WebSocketStream<tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>>,
>;

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

async fn ws_connect(addr: SocketAddr) -> (WsSink, WsStream) {
    let url = format!("ws://{addr}/ws");
    let (ws, _) = connect_async(&url).await.expect("WS connect failed");
    ws.split()
}

async fn ws_send(sink: &mut WsSink, msg: Value) {
    sink.send(Message::Text(msg.to_string())).await.unwrap();
}

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

async fn drain(stream: &mut WsStream) {
    while let Ok(Some(Ok(_))) = timeout(Duration::from_millis(200), stream.next()).await {
        // Continue draining
    }
}

// ============================================================
// Helpers: create SFU room (host) + join as guest
// ============================================================

/// Creates an SFU room. Returns (sink, stream, peer_id, invite_code).
async fn create_sfu_room(addr: SocketAddr, room_id: &str) -> (WsSink, WsStream, String, String) {
    let (mut sink, mut stream) = ws_connect(addr).await;
    ws_send(
        &mut sink,
        json!({"type":"create_room","roomId":room_id,"roomType":"sfu"}),
    )
    .await;
    let created = recv_type(&mut stream, "room_created").await;
    let invite_code = created["inviteCode"].as_str().unwrap().to_string();
    let peer_id = created["peerId"].as_str().unwrap().to_string();
    // Drain media_token and any other startup messages
    drain(&mut stream).await;
    (sink, stream, peer_id, invite_code)
}

/// Joins an existing SFU room via invite code. Returns (sink, stream, peer_id).
async fn join_sfu_room(
    addr: SocketAddr,
    room_id: &str,
    invite_code: &str,
) -> (WsSink, WsStream, String) {
    let (mut sink, mut stream) = ws_connect(addr).await;
    ws_send(
        &mut sink,
        json!({"type":"join","roomId":room_id,"roomType":"sfu","inviteCode":invite_code}),
    )
    .await;
    let joined = recv_type(&mut stream, "joined").await;
    let peer_id = joined["peerId"].as_str().unwrap().to_string();
    // Drain media_token, room_state, share_state, participant_joined, etc.
    drain(&mut stream).await;
    (sink, stream, peer_id)
}

// ============================================================
// Tests
// ============================================================

/// Â§13: Start share â†’ share_started broadcast to all peers.
#[tokio::test]
async fn test_13_start_share_broadcast() {
    let (addr, _state) = start_server(true).await;

    let (mut sink1, mut stream1, _host_id, invite) = create_sfu_room(addr, "share-broadcast").await;
    let (_sink2, mut stream2, _guest_id) = join_sfu_room(addr, "share-broadcast", &invite).await;

    // Host on stream1 needs to drain the participant_joined from guest joining
    drain(&mut stream1).await;

    // Host starts sharing
    ws_send(&mut sink1, json!({"type":"start_share"})).await;

    // Both peers receive share_started
    let ss1 = recv_type(&mut stream1, "share_started").await;
    let ss2 = recv_type(&mut stream2, "share_started").await;
    assert_eq!(ss1["participantId"].as_str().unwrap(), _host_id);
    assert_eq!(ss2["participantId"].as_str().unwrap(), _host_id);
}

/// Â§13: Concurrent shares from different participants both succeed.
#[tokio::test]
async fn test_13_concurrent_shares() {
    let (addr, _state) = start_server(true).await;

    let (mut sink1, mut stream1, host_id, invite) =
        create_sfu_room(addr, "concurrent-shares").await;
    let (mut sink2, mut stream2, guest_id) =
        join_sfu_room(addr, "concurrent-shares", &invite).await;
    drain(&mut stream1).await;

    // Host starts sharing
    ws_send(&mut sink1, json!({"type":"start_share"})).await;
    let ss1 = recv_type(&mut stream1, "share_started").await;
    assert_eq!(ss1["participantId"].as_str().unwrap(), host_id);
    drain(&mut stream2).await; // guest drains host's share_started

    // Guest starts sharing while host is still sharing â†’ should succeed
    ws_send(&mut sink2, json!({"type":"start_share"})).await;
    let ss2 = recv_type(&mut stream2, "share_started").await;
    assert_eq!(ss2["participantId"].as_str().unwrap(), guest_id);

    // Host also receives guest's share_started
    let ss1b = recv_type(&mut stream1, "share_started").await;
    assert_eq!(ss1b["participantId"].as_str().unwrap(), guest_id);
}

/// Â§13: Same sender's second start_share â†’ no-op (no error, no broadcast).
#[tokio::test]
async fn test_13_idempotent_start_share() {
    let (addr, _state) = start_server(true).await;

    let (mut sink1, mut stream1, _host_id, invite) =
        create_sfu_room(addr, "idempotent-share").await;
    let (_sink2, mut stream2, _guest_id) = join_sfu_room(addr, "idempotent-share", &invite).await;
    drain(&mut stream1).await;

    // First start_share â†’ broadcast
    ws_send(&mut sink1, json!({"type":"start_share"})).await;
    recv_type(&mut stream1, "share_started").await;
    recv_type(&mut stream2, "share_started").await;

    // Second start_share from same sender â†’ no-op
    ws_send(&mut sink1, json!({"type":"start_share"})).await;

    // Neither peer should receive another share_started or error
    let extra = try_recv_type(&mut stream1, "share_started", 500).await;
    assert!(extra.is_none(), "second start_share should be no-op");
    let err = try_recv_type(&mut stream1, "error", 200).await;
    assert!(err.is_none(), "second start_share should not produce error");
}

/// Â§13: Stop share â†’ share_stopped broadcast to all peers.
#[tokio::test]
async fn test_13_stop_share_broadcast() {
    let (addr, _state) = start_server(true).await;

    let (mut sink1, mut stream1, host_id, invite) = create_sfu_room(addr, "stop-share").await;
    let (_sink2, mut stream2, _guest_id) = join_sfu_room(addr, "stop-share", &invite).await;
    drain(&mut stream1).await;

    // Start sharing
    ws_send(&mut sink1, json!({"type":"start_share"})).await;
    recv_type(&mut stream1, "share_started").await;
    drain(&mut stream2).await;

    // Stop sharing
    ws_send(&mut sink1, json!({"type":"stop_share"})).await;

    let stopped1 = recv_type(&mut stream1, "share_stopped").await;
    let stopped2 = recv_type(&mut stream2, "share_stopped").await;
    assert_eq!(stopped1["participantId"].as_str().unwrap(), host_id);
    assert_eq!(stopped2["participantId"].as_str().unwrap(), host_id);
}

/// Â§13: Host-directed stop (targetParticipantId) â†’ share_stopped for target only.
#[tokio::test]
async fn test_13_host_directed_stop() {
    let (addr, _state) = start_server(true).await;

    let (mut sink1, mut stream1, _host_id, invite) = create_sfu_room(addr, "host-stop").await;
    let (mut sink2, mut stream2, guest_id) = join_sfu_room(addr, "host-stop", &invite).await;
    drain(&mut stream1).await;

    // Guest starts sharing
    ws_send(&mut sink2, json!({"type":"start_share"})).await;
    recv_type(&mut stream2, "share_started").await;
    drain(&mut stream1).await;

    // Host stops guest's share via targetParticipantId
    ws_send(
        &mut sink1,
        json!({"type":"stop_share","targetParticipantId": guest_id}),
    )
    .await;

    let stopped1 = recv_type(&mut stream1, "share_stopped").await;
    let stopped2 = recv_type(&mut stream2, "share_stopped").await;
    assert_eq!(stopped1["participantId"].as_str().unwrap(), guest_id);
    assert_eq!(stopped2["participantId"].as_str().unwrap(), guest_id);
}

/// Â§13: Non-host targeted stop â†’ permission error.
#[tokio::test]
async fn test_13_non_host_targeted_stop_rejected() {
    let (addr, _state) = start_server(true).await;

    let (mut sink1, mut stream1, host_id, invite) = create_sfu_room(addr, "nonhost-stop").await;
    let (mut sink2, mut stream2, _guest_id) = join_sfu_room(addr, "nonhost-stop", &invite).await;
    drain(&mut stream1).await;

    // Host starts sharing
    ws_send(&mut sink1, json!({"type":"start_share"})).await;
    recv_type(&mut stream1, "share_started").await;
    drain(&mut stream2).await;

    // Guest tries to stop host's share â†’ error
    ws_send(
        &mut sink2,
        json!({"type":"stop_share","targetParticipantId": host_id}),
    )
    .await;

    let err = recv_type(&mut stream2, "error").await;
    let msg = err["message"].as_str().unwrap();
    assert!(
        msg.contains("only host"),
        "expected permission error, got: {msg}"
    );

    // Host's share should still be active â€” no share_stopped broadcast
    let spurious = try_recv_type(&mut stream1, "share_stopped", 500).await;
    assert!(spurious.is_none(), "host's share should still be active");
}

/// Â§13: Stop all shares (host) â†’ one share_stopped per active sharer.
#[tokio::test]
async fn test_13_stop_all_shares_host() {
    let (addr, _state) = start_server(true).await;

    let (mut sink1, mut stream1, host_id, invite) = create_sfu_room(addr, "stop-all-host").await;
    let (mut sink2, mut stream2, guest_id) = join_sfu_room(addr, "stop-all-host", &invite).await;
    drain(&mut stream1).await;

    // Both start sharing
    ws_send(&mut sink1, json!({"type":"start_share"})).await;
    recv_type(&mut stream1, "share_started").await;
    drain(&mut stream2).await;

    ws_send(&mut sink2, json!({"type":"start_share"})).await;
    recv_type(&mut stream2, "share_started").await;
    drain(&mut stream1).await;

    // Host stops all shares
    ws_send(&mut sink1, json!({"type":"stop_all_shares"})).await;

    // Collect two share_stopped messages on stream1
    let s1a = recv_type(&mut stream1, "share_stopped").await;
    let s1b = recv_type(&mut stream1, "share_stopped").await;

    let mut stopped_ids: Vec<String> = vec![
        s1a["participantId"].as_str().unwrap().to_string(),
        s1b["participantId"].as_str().unwrap().to_string(),
    ];
    stopped_ids.sort();

    let mut expected = vec![host_id.clone(), guest_id.clone()];
    expected.sort();
    assert_eq!(stopped_ids, expected, "should stop both shares");
}

/// Â§13: Stop all shares (non-host) â†’ permission error.
#[tokio::test]
async fn test_13_stop_all_shares_non_host_rejected() {
    let (addr, _state) = start_server(true).await;

    let (mut sink1, mut stream1, _host_id, invite) = create_sfu_room(addr, "stop-all-guest").await;
    let (mut sink2, mut stream2, _guest_id) = join_sfu_room(addr, "stop-all-guest", &invite).await;
    drain(&mut stream1).await;

    // Host starts sharing
    ws_send(&mut sink1, json!({"type":"start_share"})).await;
    recv_type(&mut stream1, "share_started").await;
    drain(&mut stream2).await;

    // Guest tries stop_all_shares â†’ error
    ws_send(&mut sink2, json!({"type":"stop_all_shares"})).await;

    let err = recv_type(&mut stream2, "error").await;
    let msg = err["message"].as_str().unwrap();
    assert!(
        msg.contains("only host"),
        "expected permission error, got: {msg}"
    );

    // No share_stopped should be broadcast
    let spurious = try_recv_type(&mut stream1, "share_stopped", 500).await;
    assert!(spurious.is_none(), "shares should still be active");
}

/// Â§13: Late joiner receives share_state snapshot with active sharers.
#[tokio::test]
async fn test_13_late_joiner_share_state() {
    let (addr, _state) = start_server(true).await;

    let (mut sink1, mut stream1, host_id, invite) = create_sfu_room(addr, "late-join-state").await;

    // Host starts sharing before anyone else joins
    ws_send(&mut sink1, json!({"type":"start_share"})).await;
    recv_type(&mut stream1, "share_started").await;

    // Late joiner connects â€” should receive share_state after joined/media_token
    let (mut _sink2, mut stream2) = ws_connect(addr).await;
    ws_send(
        &mut _sink2,
        json!({"type":"join","roomId":"late-join-state","roomType":"sfu","inviteCode":invite}),
    )
    .await;

    // Skip to share_state (comes after joined, media_token, room_state)
    let share_state = recv_type(&mut stream2, "share_state").await;
    let sharers = share_state["participantIds"]
        .as_array()
        .expect("participantIds should be array");
    let sharer_ids: Vec<&str> = sharers.iter().map(|v| v.as_str().unwrap()).collect();
    assert!(
        sharer_ids.contains(&host_id.as_str()),
        "share_state should contain host's peer_id, got: {sharer_ids:?}"
    );
}

/// Â§13: First joiner (via join, not create_room) gets empty share_state.
///
/// Note: `create_room` does NOT send `share_state` â€” only `handle_sfu_join`
/// does. So we test the first *joiner* via the join path (second peer into
/// a room where no shares are active).
#[tokio::test]
async fn test_13_first_joiner_empty_share_state() {
    let (addr, _state) = start_server(true).await;

    // Creator makes the room (no share_state sent on create_room path)
    let (_sink1, mut _stream1, _host_id, invite) = create_sfu_room(addr, "empty-share-state").await;

    // Second peer joins â€” no shares active, should get empty share_state
    let (mut _sink2, mut stream2) = ws_connect(addr).await;
    ws_send(
        &mut _sink2,
        json!({"type":"join","roomId":"empty-share-state","roomType":"sfu","inviteCode":invite}),
    )
    .await;

    let share_state = recv_type(&mut stream2, "share_state").await;
    let sharers = share_state["participantIds"]
        .as_array()
        .expect("participantIds should be array");
    assert!(
        sharers.is_empty(),
        "joiner should get empty share_state when no shares active, got: {sharers:?}"
    );
}

/// Â§13: Disconnect cleanup â€” sharing peer disconnects â†’ share_stopped broadcast.
#[tokio::test]
async fn test_13_disconnect_cleanup() {
    let (addr, _state) = start_server(true).await;

    let (_sink1, mut stream1, _host_id, invite) = create_sfu_room(addr, "disconnect-cleanup").await;
    let (mut sink2, mut stream2, guest_id) =
        join_sfu_room(addr, "disconnect-cleanup", &invite).await;
    drain(&mut stream1).await;

    // Guest starts sharing
    ws_send(&mut sink2, json!({"type":"start_share"})).await;
    recv_type(&mut stream2, "share_started").await;
    drain(&mut stream1).await;

    // Guest disconnects
    sink2.close().await.ok();
    drop(sink2);
    drop(stream2);

    // Host should receive share_stopped for the disconnected guest
    let stopped = recv_type(&mut stream1, "share_stopped").await;
    assert_eq!(stopped["participantId"].as_str().unwrap(), guest_id);
}

/// Â§13: Disconnect non-sharer â†’ no share_stopped signal.
#[tokio::test]
async fn test_13_disconnect_non_sharer_no_signal() {
    let (addr, _state) = start_server(true).await;

    let (mut sink1, mut stream1, _host_id, invite) =
        create_sfu_room(addr, "disconnect-noshare").await;
    let (mut sink2, mut stream2, _guest_id) =
        join_sfu_room(addr, "disconnect-noshare", &invite).await;
    drain(&mut stream1).await;

    // Host starts sharing (guest does NOT share)
    ws_send(&mut sink1, json!({"type":"start_share"})).await;
    recv_type(&mut stream1, "share_started").await;
    drain(&mut stream2).await;

    // Guest (non-sharer) disconnects
    sink2.close().await.ok();
    drop(sink2);
    drop(stream2);

    // Host should receive participant_left but NOT share_stopped
    // (host's own share is still active)
    let share_stopped = try_recv_type(&mut stream1, "share_stopped", 1000).await;
    assert!(
        share_stopped.is_none(),
        "non-sharer disconnect should not produce share_stopped"
    );
}

/// Â§13: P2P room â†’ start_share returns "screen sharing unavailable in P2P mode".
#[tokio::test]
async fn test_13_p2p_room_rejected() {
    let (addr, _state) = start_server(true).await;

    // Create a P2P room
    let (mut sink1, mut stream1) = ws_connect(addr).await;
    ws_send(
        &mut sink1,
        json!({"type":"create_room","roomId":"p2p-share-test","roomType":"p2p"}),
    )
    .await;
    recv_type(&mut stream1, "room_created").await;
    drain(&mut stream1).await;

    // Try to start share in P2P room
    ws_send(&mut sink1, json!({"type":"start_share"})).await;

    let err = recv_type(&mut stream1, "error").await;
    let msg = err["message"].as_str().unwrap();
    assert!(msg.contains("P2P"), "expected P2P rejection, got: {msg}");
}
