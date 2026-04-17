#![cfg(feature = "test-support")]
//! Integration tests automating README manual tests 9â€“13.
//!
//! Test 8 (Invite Code Lifecycle) is already covered by invite_lifecycle_integration.rs.
//! Tests 1â€“7 are either pre-existing, audio-based, or require LiveKit credentials.
//!
//! Covered here:
//!   - Test 9:  Join Rate Limiting (rapid-fire bad codes â†’ rate_limited)
//!   - Test 10: Pre-Join Authentication Gate (non-join messages â†’ "not authenticated")
//!   - Test 11: Kick Participant (Host moderation flow)
//!   - Test 12: SDP and ICE Candidate Size Limits
//!   - Test 13: Room Cleanup on Last Peer Leave (invites removed)
//!
//! Run: cargo test -p wavis-backend --test readme_manual_tests_integration -- --test-threads=1

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
// Server setup + WS helpers (same pattern as invite_lifecycle)
// ============================================================

async fn start_server(require_invite: bool) -> (SocketAddr, AppState) {
    unsafe {
        std::env::set_var("SFU_JWT_SECRET", "dev-secret-32-bytes-minimum!!!XX");
        std::env::set_var("MAX_ROOM_PARTICIPANTS", "6");
        std::env::set_var(
            "REQUIRE_INVITE_CODE",
            if require_invite { "true" } else { "false" },
        );
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

/// Start server with a custom rate limiter config (for test 9).
async fn start_server_with_rate_limiter(
    require_invite: bool,
    rl_config: JoinRateLimiterConfig,
) -> (SocketAddr, AppState) {
    unsafe {
        std::env::set_var("SFU_JWT_SECRET", "dev-secret-32-bytes-minimum!!!XX");
        std::env::set_var("MAX_ROOM_PARTICIPANTS", "6");
        std::env::set_var(
            "REQUIRE_INVITE_CODE",
            if require_invite { "true" } else { "false" },
        );
    }

    let mock = Arc::new(MockSfuBridge::new());
    let invite_store = Arc::new(InviteStore::new(InviteStoreConfig::default()));
    let join_rate_limiter = Arc::new(JoinRateLimiter::new(rl_config));
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

/// Drain all pending messages.
async fn drain(stream: &mut WsStream) {
    while let Ok(Some(Ok(_))) = timeout(Duration::from_millis(200), stream.next()).await {
        // Continue draining
    }
}

// ==========================================================================
// Test 9: Join Rate Limiting
// README: Send 11+ joins with bad invite codes â†’ first N get invite_invalid,
// then rate_limited kicks in.
// We use a custom rate limiter with ip_failed_threshold=3 to keep it fast.
// ==========================================================================
#[tokio::test]
async fn test9_join_rate_limiting() {
    let rl_config = JoinRateLimiterConfig {
        ip_failed_threshold: 3,
        ip_failed_window: Duration::from_secs(60),
        // Keep other thresholds high so only ip_failed triggers
        ip_total_threshold: 100,
        ip_total_window: Duration::from_secs(60),
        code_threshold: 100,
        code_window: Duration::from_secs(60),
        room_threshold: 100,
        room_window: Duration::from_secs(60),
        connection_threshold: 100,
        connection_window: Duration::from_secs(60),
        cooldown: Duration::from_secs(60),
    };
    let (addr, _state) = start_server_with_rate_limiter(true, rl_config).await;

    // Send bad invite codes â€” first 3 should be invite_invalid, then rate_limited
    for i in 0..5 {
        let (mut sink, mut stream) = ws_connect(addr).await;
        ws_send(
            &mut sink,
            json!({"type":"join","roomId":"rate-test","roomType":"sfu","inviteCode":format!("bad-{i}")}),
        )
        .await;
        let rej = recv_type(&mut stream, "join_rejected").await;
        let reason = rej["reason"].as_str().unwrap();
        if i < 3 {
            assert_eq!(
                reason, "invite_invalid",
                "attempt {i} should be invite_invalid"
            );
        } else {
            assert_eq!(reason, "rate_limited", "attempt {i} should be rate_limited");
        }
        drop(sink);
    }
}

// ==========================================================================
// Test 10: Pre-Join Authentication Gate
// README: Non-join messages before joining â†’ "not authenticated"
// ==========================================================================
#[tokio::test]
async fn test10_pre_join_auth_gate() {
    let (addr, _state) = start_server(false).await;
    let (mut sink, mut stream) = ws_connect(addr).await;

    // 10a: Offer before join â†’ "not authenticated"
    ws_send(
        &mut sink,
        json!({"type":"offer","sessionDescription":{"sdp":"v=0\r\n","type":"offer"}}),
    )
    .await;
    let err = recv_type(&mut stream, "error").await;
    assert_eq!(err["message"], "not authenticated");

    // 10b: InviteCreate before join â†’ "not authenticated"
    ws_send(&mut sink, json!({"type":"invite_create","maxUses":3})).await;
    let err = recv_type(&mut stream, "error").await;
    assert_eq!(err["message"], "not authenticated");

    // 10c: Leave before join â†’ "not authenticated"
    ws_send(&mut sink, json!({"type":"leave"})).await;
    let err = recv_type(&mut stream, "error").await;
    assert_eq!(err["message"], "not authenticated");

    // 10d: After joining, the same messages work (or at least don't return "not authenticated")
    ws_send(
        &mut sink,
        json!({"type":"join","roomId":"auth-test","roomType":"sfu"}),
    )
    .await;
    let joined = recv_type(&mut stream, "joined").await;
    assert_eq!(joined["roomId"], "auth-test");
    drain(&mut stream).await;

    // invite_create should work now
    ws_send(&mut sink, json!({"type":"invite_create","maxUses":3})).await;
    let created = recv_type(&mut stream, "invite_created").await;
    assert!(created["inviteCode"].as_str().is_some());

    // leave should work now (closes connection)
    ws_send(&mut sink, json!({"type":"leave"})).await;
    // Connection closes after leave â€” no error expected
}

// ==========================================================================
// Test 11: Kick Participant (Host Moderation)
// README: First joiner = Host, second = Guest. Guest can't kick. Host can.
// ==========================================================================
#[tokio::test]
async fn test11_kick_participant() {
    let (addr, _state) = start_server(false).await;

    // Host joins first
    let (mut s_host, mut r_host) = ws_connect(addr).await;
    ws_send(
        &mut s_host,
        json!({"type":"join","roomId":"kick-test","roomType":"sfu"}),
    )
    .await;
    let joined_host = recv_type(&mut r_host, "joined").await;
    assert_eq!(joined_host["peerCount"], 1);
    drain(&mut r_host).await;

    // Guest joins second
    let (mut s_guest, mut r_guest) = ws_connect(addr).await;
    ws_send(
        &mut s_guest,
        json!({"type":"join","roomId":"kick-test","roomType":"sfu"}),
    )
    .await;
    let joined_guest = recv_type(&mut r_guest, "joined").await;
    assert_eq!(joined_guest["peerCount"], 2);
    let guest_peer_id = joined_guest["peerId"].as_str().unwrap().to_string();
    let host_peer_id = joined_host["peerId"].as_str().unwrap().to_string();
    drain(&mut r_guest).await;
    // Host receives participant_joined
    let _ = recv_type(&mut r_host, "participant_joined").await;
    drain(&mut r_host).await;

    // 11a: Guest tries to kick Host â†’ "unauthorized"
    ws_send(
        &mut s_guest,
        json!({"type":"kick_participant","targetParticipantId": &host_peer_id}),
    )
    .await;
    let err = recv_type(&mut r_guest, "error").await;
    assert_eq!(err["message"], "unauthorized");

    // 11b: Host kicks Guest â†’ participant_kicked broadcast
    ws_send(
        &mut s_host,
        json!({"type":"kick_participant","targetParticipantId": &guest_peer_id}),
    )
    .await;
    let kicked = recv_type(&mut r_host, "participant_kicked").await;
    assert_eq!(kicked["participantId"], guest_peer_id);
    assert_eq!(kicked["reason"], "kicked");

    // Verify room now has only the host
    assert_eq!(
        _state.room_state.peer_count("kick-test"),
        1,
        "only host should remain after kick"
    );

    // Cleanup
    ws_send(&mut s_host, json!({"type":"leave"})).await;
    drain(&mut r_host).await;
}

// ==========================================================================
// Test 12: SDP and ICE Candidate Size Limits
// README: Oversized SDP (>64KB) â†’ "sdp too large"
//         Oversized ICE candidate (>2KB) â†’ "ice candidate too large"
// ==========================================================================
#[tokio::test]
async fn test12_sdp_and_ice_size_limits() {
    let (addr, _state) = start_server(false).await;

    let (mut sink, mut stream) = ws_connect(addr).await;
    // Join first (pre-join gate would block us)
    ws_send(
        &mut sink,
        json!({"type":"join","roomId":"size-test","roomType":"sfu"}),
    )
    .await;
    let _ = recv_type(&mut stream, "joined").await;
    drain(&mut stream).await;

    // 12a: Oversized SDP offer (>64KB total frame)
    // Both MAX_TEXT_MESSAGE_BYTES and MAX_SDP_BYTES are 64KB, so the frame-level
    // check fires first with "message too large". This is the actual server behavior
    // documented in README test 12.
    let big_sdp = "v=0\r\n".to_string() + &"x".repeat(70_000);
    ws_send(
        &mut sink,
        json!({
            "type": "offer",
            "sessionDescription": {"sdp": big_sdp, "type": "offer"}
        }),
    )
    .await;
    // Server closes connection after oversized frame â€” reconnect needed
    // The close message or no further messages indicates rejection
    let close_result = timeout(Duration::from_secs(2), stream.next()).await;
    match close_result {
        Ok(Some(Ok(Message::Text(text)))) => {
            let v: Value = serde_json::from_str(&text).unwrap();
            assert!(
                v["message"] == "message too large" || v["message"] == "sdp too large",
                "expected size rejection, got: {v}"
            );
        }
        Ok(Some(Ok(Message::Close(_)))) => {
            // Connection closed â€” expected for oversized frame
        }
        _ => {
            // Connection dropped â€” also acceptable
        }
    }

    // Reconnect after the oversized frame closed the connection
    let (mut sink, mut stream) = ws_connect(addr).await;
    ws_send(
        &mut sink,
        json!({"type":"join","roomId":"size-test-2","roomType":"sfu"}),
    )
    .await;
    let _ = recv_type(&mut stream, "joined").await;
    drain(&mut stream).await;

    // 12c: Oversized ICE candidate (>2KB)
    let big_candidate = "x".repeat(3_000);
    ws_send(
        &mut sink,
        json!({
            "type": "ice_candidate",
            "candidate": {
                "candidate": big_candidate,
                "sdpMid": "0",
                "sdpMLineIndex": 0
            }
        }),
    )
    .await;
    let err = recv_type(&mut stream, "error").await;
    let err_msg = err["message"].as_str().unwrap_or("");
    assert!(
        err_msg == "ice candidate too large"
            || err_msg.contains("candidate") && err_msg.contains("too long"),
        "expected ICE candidate size rejection, got: {err_msg}"
    );

    // 12d: Normal-sized SDP passes through without size rejection.
    // In SFU mode with MockSfuBridge, the offer is proxied and we get a mock answer back.
    ws_send(
        &mut sink,
        json!({
            "type": "offer",
            "sessionDescription": {"sdp": "v=0\r\n", "type": "offer"}
        }),
    )
    .await;
    // We should get an answer (mock SFU) â€” NOT "sdp too large" or "message too large"
    let msg = timeout(Duration::from_secs(2), async {
        while let Some(Ok(msg)) = stream.next().await {
            if let Message::Text(text) = msg {
                let v: Value = serde_json::from_str(&text).unwrap();
                let t = v["type"].as_str().unwrap_or("");
                if t == "error" {
                    assert_ne!(
                        v["message"], "sdp too large",
                        "normal SDP should not be rejected"
                    );
                    assert_ne!(
                        v["message"], "message too large",
                        "normal SDP should not be rejected"
                    );
                }
                return v;
            }
        }
        panic!("WS closed");
    })
    .await
    .expect("timeout waiting for response to normal SDP");
    // Accept answer or any non-size error
    let msg_type = msg["type"].as_str().unwrap_or("");
    assert!(
        msg_type == "answer" || msg_type == "error",
        "expected answer or error, got: {msg}"
    );

    // Cleanup
    ws_send(&mut sink, json!({"type":"leave"})).await;
}

// ==========================================================================
// Test 13: Room Cleanup on Last Peer Leave
// README: When last peer leaves, invite codes for that room are removed.
// ==========================================================================
#[tokio::test]
async fn test13_room_cleanup_removes_invites() {
    let (addr, state) = start_server(false).await;

    // Join and create an invite
    let (mut sink, mut stream) = ws_connect(addr).await;
    ws_send(
        &mut sink,
        json!({"type":"join","roomId":"cleanup-test","roomType":"sfu"}),
    )
    .await;
    let _ = recv_type(&mut stream, "joined").await;
    drain(&mut stream).await;

    ws_send(&mut sink, json!({"type":"invite_create","maxUses":5})).await;
    let created = recv_type(&mut stream, "invite_created").await;
    let code = created["inviteCode"].as_str().unwrap().to_string();

    // Verify invite is valid before leave
    let now = std::time::Instant::now();
    assert!(
        state
            .invite_store
            .validate(&code, "cleanup-test", now)
            .is_ok(),
        "invite should be valid before room cleanup"
    );

    // Verify room exists
    assert!(
        state.room_state.peer_count("cleanup-test") > 0,
        "room should have a peer"
    );

    // Leave â€” last peer, triggers room cleanup
    ws_send(&mut sink, json!({"type":"leave"})).await;
    drain(&mut stream).await;

    // Give the server a moment to process the leave + cleanup
    tokio::time::sleep(Duration::from_millis(200)).await;

    // Room should be gone
    assert_eq!(
        state.room_state.peer_count("cleanup-test"),
        0,
        "room should be empty after last peer leaves"
    );

    // Invite code should be cleaned up â€” validate returns an error
    let validation = state.invite_store.validate(&code, "cleanup-test", now);
    assert!(
        validation.is_err(),
        "invite should be invalid after room cleanup, got: {:?}",
        validation
    );
}
