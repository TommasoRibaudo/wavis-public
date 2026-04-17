#![cfg(feature = "test-support")]
//! Reconnect / Drop Matrix — integration test scaffolding.
//!
//! Tests WebSocket disconnection at various points in the signaling lifecycle
//! to verify no ghost participants, correct capacity restoration, and clean
//! room state after drops.
//!
//! All disconnects are injected deterministically via explicit WS close —
//! no timeouts or sleeps for disconnect injection.
//!
//! Run: cargo test -p wavis-backend --test reconnect_drop_matrix

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

// ============================================================
// Type aliases for WebSocket split halves
// ============================================================

type WsSink = futures_util::stream::SplitSink<
    tokio_tungstenite::WebSocketStream<tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>>,
    Message,
>;
type WsStream = futures_util::stream::SplitStream<
    tokio_tungstenite::WebSocketStream<tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>>,
>;

// ============================================================
// Server setup
// ============================================================

/// Start the backend on a random port with invite codes required.
/// Returns (addr, app_state) so tests can create invite codes and assert room state.
async fn start_server() -> (SocketAddr, AppState) {
    // SAFETY: integration tests run with --test-threads=1.
    unsafe {
        std::env::set_var("SFU_JWT_SECRET", "dev-secret-32-bytes-minimum!!!XX");
        std::env::set_var("MAX_ROOM_PARTICIPANTS", "6");
        std::env::set_var("REQUIRE_INVITE_CODE", "false");
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
    // Bypass invite requirement — reconnect/drop tests focus on lifecycle, not invites.
    app_state.require_invite_code = false;

    // Mark SFU as available so SFU joins aren't rejected.
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

    // Brief pause for the server to start accepting connections.
    tokio::time::sleep(Duration::from_millis(50)).await;
    (addr, app_state)
}

// ============================================================
// WebSocket helpers
// ============================================================

/// Connect a new WebSocket client to the server. Returns split (sink, stream).
async fn ws_connect(addr: SocketAddr) -> (WsSink, WsStream) {
    let url = format!("ws://{addr}/ws");
    let (ws, _) = connect_async(&url).await.expect("WS connect failed");
    ws.split()
}

/// Send a JSON message over the WebSocket.
async fn ws_send(sink: &mut WsSink, msg: Value) {
    sink.send(Message::Text(msg.to_string())).await.unwrap();
}

/// Receive messages until one with the given `"type"` arrives, or timeout after 5s.
async fn recv_type(stream: &mut WsStream, target_type: &str) -> Value {
    timeout(Duration::from_secs(5), async {
        while let Some(Ok(msg)) = stream.next().await {
            if let Message::Text(text) = msg {
                let v: Value = serde_json::from_str(&text).unwrap();
                let msg_type = v["type"].as_str().unwrap_or("unknown");
                if msg_type == target_type {
                    return v;
                }
                eprintln!("[recv_type] skipping '{msg_type}' while waiting for '{target_type}'");
            }
        }
        panic!("WS closed without receiving '{target_type}'");
    })
    .await
    .unwrap_or_else(|_| panic!("Timeout waiting for '{target_type}'"))
}
/// Join a room as a P2P peer. Sends the Join message and waits for Joined response.
/// Returns the `Joined` payload.
async fn join_room(sink: &mut WsSink, stream: &mut WsStream, room_id: &str) -> Value {
    ws_send(sink, json!({"type": "join", "roomId": room_id})).await;
    recv_type(stream, "joined").await
}

/// Explicitly close the WebSocket connection (deterministic disconnect).
async fn ws_close(sink: &mut WsSink) {
    let _ = sink.close().await;
}

/// Send an explicit Leave and wait for the server to close the socket.
///
/// `Leave` dispatch is synchronous on the server: by the time the close frame
/// is observed, `handle_leave()` and the underlying room-state mutation have
/// both completed, so callers can assert state immediately without an extra
/// sleep. The 2s timeout is intentionally generous for loopback CI.
async fn ws_leave(sink: &mut WsSink, stream: &mut WsStream) {
    ws_send(sink, json!({"type": "leave"})).await;
    let _ = timeout(Duration::from_secs(2), async {
        while let Some(msg) = stream.next().await {
            if matches!(msg, Ok(Message::Close(_)) | Err(_)) {
                break;
            }
        }
    })
    .await;
}

// ============================================================
// Room state assertion helpers
// ============================================================

/// Assert the number of peers currently in a room.
fn assert_peer_count(app_state: &AppState, room_id: &str, expected: usize) {
    let actual = app_state.room_state.peer_count(room_id);
    assert_eq!(
        actual, expected,
        "Expected {expected} peers in room '{room_id}', got {actual}"
    );
}

/// Assert that a room has no participants (either empty or doesn't exist).
fn assert_room_empty(app_state: &AppState, room_id: &str) {
    let count = app_state.room_state.peer_count(room_id);
    assert_eq!(
        count, 0,
        "Expected room '{room_id}' to be empty, but found {count} peers"
    );
}

/// Assert total number of active rooms across the backend.
fn assert_active_rooms(app_state: &AppState, expected: usize) {
    let actual = app_state.room_state.active_room_count();
    assert_eq!(
        actual, expected,
        "Expected {expected} active rooms, got {actual}"
    );
}

/// Assert total participant count across all rooms.
fn assert_total_participants(app_state: &AppState, expected: usize) {
    let actual = app_state.room_state.total_participant_count();
    assert_eq!(
        actual, expected,
        "Expected {expected} total participants, got {actual}"
    );
}

/// Assert no ghost participants: snapshot all rooms and verify each room's
/// peer count matches the number of peers in the snapshot.
fn assert_no_ghosts(app_state: &AppState) {
    let snapshot = app_state.room_state.snapshot_rooms();
    for (room_id, peers) in &snapshot {
        let count = app_state.room_state.peer_count(room_id);
        assert_eq!(
            count,
            peers.len(),
            "Ghost detected in room '{room_id}': peer_count={count} but snapshot has {} peers",
            peers.len()
        );
    }
}

// ============================================================
// Smoke test — verifies scaffolding works end-to-end
// ============================================================

/// Basic smoke test: start server, connect a client, join a room, verify state,
/// disconnect, verify cleanup. This validates all helper functions work correctly.
#[tokio::test]
async fn scaffolding_smoke_test() {
    let (addr, app_state) = start_server().await;

    // No rooms initially.
    assert_active_rooms(&app_state, 0);
    assert_total_participants(&app_state, 0);

    // Connect and join a room.
    let (mut sink, mut stream) = ws_connect(addr).await;
    let joined = join_room(&mut sink, &mut stream, "smoke-room").await;
    assert_eq!(joined["peerCount"], 1);

    // Verify room state via app_state.
    assert_peer_count(&app_state, "smoke-room", 1);
    assert_active_rooms(&app_state, 1);
    assert_total_participants(&app_state, 1);
    assert_no_ghosts(&app_state);

    // Explicitly close the WebSocket.
    ws_close(&mut sink).await;

    // Give the server a moment to process the disconnect.
    tokio::time::sleep(Duration::from_millis(100)).await;

    // Room should be cleaned up (last peer left).
    assert_room_empty(&app_state, "smoke-room");
    assert_active_rooms(&app_state, 0);
    assert_total_participants(&app_state, 0);
    assert_no_ghosts(&app_state);
}

/// Drop before join: connect a WS client, close it immediately without sending
/// any Join message. Room state must remain unchanged — no ghost participants.
///
/// Validates: Requirements 3.1, 3.5
#[tokio::test]
async fn drop_before_join() {
    let (addr, app_state) = start_server().await;

    // Baseline: no rooms, no participants.
    assert_active_rooms(&app_state, 0);
    assert_total_participants(&app_state, 0);

    // Connect and immediately close without sending Join.
    let (mut sink, _stream) = ws_connect(addr).await;
    ws_close(&mut sink).await;

    // Give the server a moment to process the disconnect.
    tokio::time::sleep(Duration::from_millis(100)).await;

    // Nothing should have changed.
    assert_active_rooms(&app_state, 0);
    assert_total_participants(&app_state, 0);
    assert_no_ghosts(&app_state);
}

/// Join a room explicitly as P2P. Sends Join with `roomType: "p2p"` and waits
/// for the Joined response.
async fn join_room_p2p(sink: &mut WsSink, stream: &mut WsStream, room_id: &str) -> Value {
    ws_send(
        sink,
        json!({"type": "join", "roomId": room_id, "roomType": "p2p"}),
    )
    .await;
    recv_type(stream, "joined").await
}

/// Drop after join, before offer: two clients join a room, then one drops
/// before sending an Offer. The dropped peer must be removed from room state,
/// the remaining peer must receive PeerLeft, and capacity must be restored.
///
/// Validates: Requirements 3.2, 3.5, 3.6
#[tokio::test]
async fn drop_after_join_before_offer() {
    let (addr, app_state) = start_server().await;

    // Client A joins "drop-room" as P2P — creates the room.
    let (mut sink_a, mut stream_a) = ws_connect(addr).await;
    let joined_a = join_room_p2p(&mut sink_a, &mut stream_a, "drop-room").await;
    assert_eq!(joined_a["peerCount"], 1);

    // Client B joins "drop-room" as P2P.
    let (mut sink_b, mut stream_b) = ws_connect(addr).await;
    let joined_b = join_room_p2p(&mut sink_b, &mut stream_b, "drop-room").await;
    assert_eq!(joined_b["peerCount"], 2);

    // In P2P mode, Client A receives a second "joined" notification when Client B joins.
    let peer_joined = recv_type(&mut stream_a, "joined").await;
    assert_eq!(peer_joined["peerCount"], 2);

    // Verify both peers are in the room.
    assert_peer_count(&app_state, "drop-room", 2);
    assert_no_ghosts(&app_state);

    // Client B closes WS without ever sending an Offer.
    ws_close(&mut sink_b).await;

    // Give the server a moment to process the disconnect.
    tokio::time::sleep(Duration::from_millis(100)).await;

    // Dropped peer removed — only Client A remains.
    assert_peer_count(&app_state, "drop-room", 1);
    assert_total_participants(&app_state, 1);
    assert_no_ghosts(&app_state);

    // Client A should receive peer_left for Client B.
    let peer_left = recv_type(&mut stream_a, "peer_left").await;
    assert_eq!(peer_left["type"], "peer_left");

    // Capacity restored: a new client can join (room max is 6).
    let (mut sink_c, mut stream_c) = ws_connect(addr).await;
    let joined_c = join_room_p2p(&mut sink_c, &mut stream_c, "drop-room").await;
    assert_eq!(joined_c["peerCount"], 2);
    assert_peer_count(&app_state, "drop-room", 2);

    // Cleanup.
    ws_close(&mut sink_c).await;
    ws_close(&mut sink_a).await;
}

/// Drop during ICE exchange: two clients join a P2P room, exchange Offer/Answer,
/// Client A sends an ICE candidate, then Client B drops during the ICE exchange.
/// The dropped peer must be removed from room state, the remaining peer must
/// receive peer_left, and capacity must be restored.
///
/// Validates: Requirements 3.3, 3.5, 3.6
#[tokio::test]
async fn drop_during_ice_exchange() {
    let (addr, app_state) = start_server().await;

    // Client A joins "ice-room" as P2P — creates the room.
    let (mut sink_a, mut stream_a) = ws_connect(addr).await;
    let joined_a = join_room_p2p(&mut sink_a, &mut stream_a, "ice-room").await;
    assert_eq!(joined_a["peerCount"], 1);

    // Client B joins "ice-room" as P2P.
    let (mut sink_b, mut stream_b) = ws_connect(addr).await;
    let joined_b = join_room_p2p(&mut sink_b, &mut stream_b, "ice-room").await;
    assert_eq!(joined_b["peerCount"], 2);

    // Client A receives a "joined" notification when Client B joins.
    let peer_joined = recv_type(&mut stream_a, "joined").await;
    assert_eq!(peer_joined["peerCount"], 2);

    // Verify both peers are in the room.
    assert_peer_count(&app_state, "ice-room", 2);
    assert_no_ghosts(&app_state);

    // Client A sends an Offer to Client B (relayed via signaling server).
    ws_send(
        &mut sink_a,
        json!({
            "type": "offer",
            "sessionDescription": {
                "sdp": "fake-sdp-offer",
                "type": "offer"
            }
        }),
    )
    .await;

    // Client B receives the relayed Offer.
    let offer = recv_type(&mut stream_b, "offer").await;
    assert_eq!(offer["sessionDescription"]["sdp"], "fake-sdp-offer");

    // Client B sends an Answer back.
    ws_send(
        &mut sink_b,
        json!({
            "type": "answer",
            "sessionDescription": {
                "sdp": "fake-sdp-answer",
                "type": "answer"
            }
        }),
    )
    .await;

    // Client A receives the relayed Answer.
    let answer = recv_type(&mut stream_a, "answer").await;
    assert_eq!(answer["sessionDescription"]["sdp"], "fake-sdp-answer");

    // Client A sends an ICE candidate.
    ws_send(
        &mut sink_a,
        json!({
            "type": "ice_candidate",
            "candidate": {
                "candidate": "fake-candidate",
                "sdpMid": "0",
                "sdpMLineIndex": 0
            }
        }),
    )
    .await;

    // Client B receives the relayed ICE candidate.
    let ice = recv_type(&mut stream_b, "ice_candidate").await;
    assert_eq!(ice["candidate"]["candidate"], "fake-candidate");

    // Client B drops during the ICE exchange.
    ws_close(&mut sink_b).await;

    // Give the server a moment to process the disconnect.
    tokio::time::sleep(Duration::from_millis(100)).await;

    // Dropped peer removed — only Client A remains.
    assert_peer_count(&app_state, "ice-room", 1);
    assert_total_participants(&app_state, 1);
    assert_no_ghosts(&app_state);

    // Client A should receive peer_left for Client B.
    let peer_left = recv_type(&mut stream_a, "peer_left").await;
    assert_eq!(peer_left["type"], "peer_left");

    // Capacity restored: a new client can join (room max is 6).
    let (mut sink_c, mut stream_c) = ws_connect(addr).await;
    let joined_c = join_room_p2p(&mut sink_c, &mut stream_c, "ice-room").await;
    assert_eq!(joined_c["peerCount"], 2);
    assert_peer_count(&app_state, "ice-room", 2);

    // Cleanup.
    ws_close(&mut sink_c).await;
    ws_close(&mut sink_a).await;
}

/// Rejoin after drop: a client joins a P2P room, drops, then reconnects with
/// a new WebSocket connection and joins the same room again. The new join must
/// succeed with no stale state blocking the rejoin.
///
/// Peer IDs are ephemeral and server-assigned, so the reconnected client gets
/// a new peer ID. The key invariant is that cleanup from the first connection
/// fully completes before the second join is attempted.
///
/// Validates: Requirements 3.4
#[tokio::test]
async fn rejoin_after_drop() {
    let (addr, app_state) = start_server().await;

    // --- Phase 1: Client connects and joins "rejoin-room" as P2P ---
    let (mut sink1, mut stream1) = ws_connect(addr).await;
    let joined1 = join_room_p2p(&mut sink1, &mut stream1, "rejoin-room").await;
    assert_eq!(joined1["peerCount"], 1);
    assert_peer_count(&app_state, "rejoin-room", 1);
    assert_no_ghosts(&app_state);

    // --- Phase 2: Client drops (explicit WS close) ---
    ws_close(&mut sink1).await;

    // Give the server time to process the disconnect and clean up.
    tokio::time::sleep(Duration::from_millis(100)).await;

    // Room should be empty — last peer left.
    assert_room_empty(&app_state, "rejoin-room");
    assert_active_rooms(&app_state, 0);
    assert_total_participants(&app_state, 0);
    assert_no_ghosts(&app_state);

    // --- Phase 3: Client reconnects with a NEW WebSocket connection ---
    let (mut sink2, mut stream2) = ws_connect(addr).await;

    // --- Phase 4: Client joins the same "rejoin-room" again ---
    let joined2 = join_room_p2p(&mut sink2, &mut stream2, "rejoin-room").await;

    // Join must succeed — no stale state blocking the rejoin.
    assert_eq!(joined2["peerCount"], 1);
    assert_peer_count(&app_state, "rejoin-room", 1);
    assert_active_rooms(&app_state, 1);
    assert_total_participants(&app_state, 1);
    assert_no_ghosts(&app_state);

    // The new connection gets a different peer ID (ephemeral, server-assigned).
    let peer_id_1 = joined1["peerId"].as_str().unwrap_or("");
    let peer_id_2 = joined2["peerId"].as_str().unwrap_or("");
    assert_ne!(
        peer_id_1, peer_id_2,
        "Reconnected client should get a new peer ID, got same: {peer_id_1}"
    );

    // Cleanup.
    ws_close(&mut sink2).await;
}

/// Rapid connect-disconnect cycle: 3 iterations of connect → join → drop on
/// the same room. After each cycle the room must be empty with no ghosts.
/// After all 3 cycles, final assertion: no active rooms, no participants.
///
/// This tests that rapid connect/disconnect cycles don't leave stale state
/// or ghost participants.
///
/// Validates: Requirements 3.7, 3.5
#[tokio::test]
async fn rapid_connect_disconnect_cycle() {
    let (addr, app_state) = start_server().await;

    // Baseline: clean slate.
    assert_active_rooms(&app_state, 0);
    assert_total_participants(&app_state, 0);

    for i in 0..3 {
        // --- Connect ---
        let (mut sink, mut stream) = ws_connect(addr).await;

        // --- Join "cycle-room" as P2P ---
        let joined = join_room_p2p(&mut sink, &mut stream, "cycle-room").await;
        assert_eq!(
            joined["peerCount"], 1,
            "Cycle {i}: expected peerCount=1 after join"
        );
        assert_peer_count(&app_state, "cycle-room", 1);

        // --- Drop (explicit WS close) ---
        ws_close(&mut sink).await;

        // Give the server a moment to process the disconnect.
        tokio::time::sleep(Duration::from_millis(100)).await;

        // After each cycle: room empty, no active rooms (last peer left), no ghosts.
        assert_room_empty(&app_state, "cycle-room");
        assert_active_rooms(&app_state, 0);
        assert_total_participants(&app_state, 0);
        assert_no_ghosts(&app_state);
    }

    // Final assertion after all 3 cycles: completely clean state.
    assert_active_rooms(&app_state, 0);
    assert_total_participants(&app_state, 0);
    assert_no_ghosts(&app_state);
}

/// Full clean lifecycle: connect, join, send Leave, then confirm the backend
/// has already released all room state when the close frame is observed.
///
/// Validates: §10.4 integration boundary, §4.3 idempotent teardown
#[tokio::test]
async fn test_clean_session_lifecycle() {
    let (addr, app_state) = start_server().await;

    assert_active_rooms(&app_state, 0);
    assert_total_participants(&app_state, 0);

    let (mut sink, mut stream) = ws_connect(addr).await;
    let joined = join_room_p2p(&mut sink, &mut stream, "lifecycle-room").await;
    assert_eq!(joined["peerCount"], 1);

    assert_peer_count(&app_state, "lifecycle-room", 1);
    assert_active_rooms(&app_state, 1);
    assert_total_participants(&app_state, 1);
    assert_no_ghosts(&app_state);

    ws_leave(&mut sink, &mut stream).await;

    assert_room_empty(&app_state, "lifecycle-room");
    assert_active_rooms(&app_state, 0);
    assert_total_participants(&app_state, 0);
    assert_no_ghosts(&app_state);
}

/// Abrupt disconnect after join: cleanup must release the room slot and leave
/// no ghost state behind so the same room can be joined again immediately.
///
/// Validates: §10.4 integration boundary, §4.3 idempotent teardown
#[tokio::test]
async fn test_unclean_disconnect_cleans_up() {
    let (addr, app_state) = start_server().await;

    assert_active_rooms(&app_state, 0);
    assert_total_participants(&app_state, 0);

    let (mut sink, mut stream) = ws_connect(addr).await;
    let joined = join_room_p2p(&mut sink, &mut stream, "unclean-room").await;
    assert_eq!(joined["peerCount"], 1);

    assert_peer_count(&app_state, "unclean-room", 1);
    assert_active_rooms(&app_state, 1);
    assert_total_participants(&app_state, 1);
    assert_no_ghosts(&app_state);

    ws_close(&mut sink).await;

    // Keep the same proven-stable delay as the existing disconnect tests in
    // this file so the backend task can observe the closed socket and finish
    // cleanup before assertions run.
    tokio::time::sleep(Duration::from_millis(100)).await;

    assert_room_empty(&app_state, "unclean-room");
    assert_active_rooms(&app_state, 0);
    assert_total_participants(&app_state, 0);
    assert_no_ghosts(&app_state);

    let (mut sink2, mut stream2) = ws_connect(addr).await;
    let joined2 = join_room_p2p(&mut sink2, &mut stream2, "unclean-room").await;
    assert_eq!(joined2["peerCount"], 1);

    assert_peer_count(&app_state, "unclean-room", 1);
    assert_active_rooms(&app_state, 1);
    assert_total_participants(&app_state, 1);
    assert_no_ghosts(&app_state);

    ws_close(&mut sink2).await;
}
