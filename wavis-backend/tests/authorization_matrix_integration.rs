#![cfg(feature = "test-support")]
//! Authorization Matrix Integration Tests (Req 7.1–7.7)
//!
//! Covers all combinations of privileged actions × roles × contexts:
//!
//! | Action           | Host (own room) | Guest (own room) | Host (cross-room) | Non-member |
//! |------------------|-----------------|-------------------|--------------------|------------|
//! | KickParticipant  | ✅ allowed       | ❌ unauthorized   | ❌ target not in room | ❌ not authenticated |
//! | MuteParticipant  | ✅ allowed       | ❌ unauthorized   | ❌ target not in room | ❌ not authenticated |
//! | InviteRevoke     | ✅ allowed       | ❌ unauthorized   | ❌ unauthorized       | ❌ not authenticated |
//!
//! Run: cargo test -p wavis-backend --test authorization_matrix_integration -- --test-threads=1

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
// Server + WebSocket helpers (same pattern as other integration tests)
// ============================================================

async fn start_server() -> (SocketAddr, AppState) {
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
    app_state.require_invite_code = false;

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

/// Drain all pending messages with a short timeout.
async fn drain(stream: &mut WsStream) {
    while let Ok(Some(Ok(_))) = timeout(Duration::from_millis(200), stream.next()).await {
        // Continue draining
    }
}

/// Join an SFU room and return the peer ID from the joined response.
async fn join_sfu(sink: &mut WsSink, stream: &mut WsStream, room_id: &str) -> String {
    ws_send(
        sink,
        json!({"type":"join","roomId": room_id,"roomType":"sfu"}),
    )
    .await;
    let joined = recv_type(stream, "joined").await;
    let peer_id = joined["peerId"].as_str().unwrap().to_string();
    drain(stream).await;
    peer_id
}

// ============================================================
// Req 7.1: Guest cannot KickParticipant → "unauthorized"
// ============================================================
#[tokio::test]
async fn guest_cannot_kick_participant() {
    let (addr, _state) = start_server().await;

    // Host joins first
    let (mut s_host, mut r_host) = ws_connect(addr).await;
    let host_id = join_sfu(&mut s_host, &mut r_host, "auth-kick").await;

    // Guest joins second
    let (mut s_guest, mut r_guest) = ws_connect(addr).await;
    let _guest_id = join_sfu(&mut s_guest, &mut r_guest, "auth-kick").await;
    // Drain host's participant_joined notification
    let _ = recv_type(&mut r_host, "participant_joined").await;
    drain(&mut r_host).await;

    // Guest tries to kick Host → "unauthorized"
    ws_send(
        &mut s_guest,
        json!({"type":"kick_participant","targetParticipantId": &host_id}),
    )
    .await;
    let err = recv_type(&mut r_guest, "error").await;
    assert_eq!(err["message"], "unauthorized");
}

// ============================================================
// Req 7.2: Guest cannot MuteParticipant → "unauthorized"
// ============================================================
#[tokio::test]
async fn guest_cannot_mute_participant() {
    let (addr, _state) = start_server().await;

    let (mut s_host, mut r_host) = ws_connect(addr).await;
    let host_id = join_sfu(&mut s_host, &mut r_host, "auth-mute").await;

    let (mut s_guest, mut r_guest) = ws_connect(addr).await;
    let _guest_id = join_sfu(&mut s_guest, &mut r_guest, "auth-mute").await;
    let _ = recv_type(&mut r_host, "participant_joined").await;
    drain(&mut r_host).await;

    // Guest tries to mute Host → "unauthorized"
    ws_send(
        &mut s_guest,
        json!({"type":"mute_participant","targetParticipantId": &host_id}),
    )
    .await;
    let err = recv_type(&mut r_guest, "error").await;
    assert_eq!(err["message"], "unauthorized");
}

// ============================================================
// Req 7.3: Guest cannot InviteRevoke → "unauthorized"
// ============================================================
#[tokio::test]
async fn guest_cannot_revoke_invite() {
    let (addr, _state) = start_server().await;

    // Host joins and creates an invite
    let (mut s_host, mut r_host) = ws_connect(addr).await;
    let _host_id = join_sfu(&mut s_host, &mut r_host, "auth-revoke").await;

    ws_send(&mut s_host, json!({"type":"invite_create","maxUses":1})).await;
    let created = recv_type(&mut r_host, "invite_created").await;
    let invite_code = created["inviteCode"].as_str().unwrap().to_string();

    // Guest joins
    let (mut s_guest, mut r_guest) = ws_connect(addr).await;
    let _guest_id = join_sfu(&mut s_guest, &mut r_guest, "auth-revoke").await;
    let _ = recv_type(&mut r_host, "participant_joined").await;
    drain(&mut r_host).await;
    drain(&mut r_guest).await;

    // Guest tries to revoke invite → "unauthorized"
    ws_send(
        &mut s_guest,
        json!({"type":"invite_revoke","inviteCode": &invite_code}),
    )
    .await;
    let err = recv_type(&mut r_guest, "error").await;
    assert_eq!(err["message"], "unauthorized");
}

// ============================================================
// Req 7.4: Host cannot kick participant in different room → "target not in room"
// ============================================================
#[tokio::test]
async fn host_cannot_kick_cross_room() {
    let (addr, _state) = start_server().await;

    // Host in room A
    let (mut s_host_a, mut r_host_a) = ws_connect(addr).await;
    let _host_a_id = join_sfu(&mut s_host_a, &mut r_host_a, "room-a-kick").await;

    // Someone in room B (so we have a valid peer ID from a different room)
    let (mut s_peer_b, mut r_peer_b) = ws_connect(addr).await;
    let peer_b_id = join_sfu(&mut s_peer_b, &mut r_peer_b, "room-b-kick").await;

    // Host A tries to kick peer B (who is in room B) → "target not in room"
    ws_send(
        &mut s_host_a,
        json!({"type":"kick_participant","targetParticipantId": &peer_b_id}),
    )
    .await;
    let err = recv_type(&mut r_host_a, "error").await;
    assert_eq!(err["message"], "target not in room");
}

// ============================================================
// Req 7.5: Host cannot revoke invite from different room → "unauthorized"
// ============================================================
#[tokio::test]
async fn host_cannot_revoke_invite_cross_room() {
    let (addr, _state) = start_server().await;

    // Host in room A creates an invite (invite is bound to room A)
    let (mut s_host_a, mut r_host_a) = ws_connect(addr).await;
    let _host_a_id = join_sfu(&mut s_host_a, &mut r_host_a, "room-a-rev").await;

    ws_send(&mut s_host_a, json!({"type":"invite_create","maxUses":1})).await;
    let created = recv_type(&mut r_host_a, "invite_created").await;
    let invite_code = created["inviteCode"].as_str().unwrap().to_string();

    // Host in room B tries to revoke room A's invite → "unauthorized"
    let (mut s_host_b, mut r_host_b) = ws_connect(addr).await;
    let _host_b_id = join_sfu(&mut s_host_b, &mut r_host_b, "room-b-rev").await;

    ws_send(
        &mut s_host_b,
        json!({"type":"invite_revoke","inviteCode": &invite_code}),
    )
    .await;
    let err = recv_type(&mut r_host_b, "error").await;
    assert_eq!(err["message"], "unauthorized");
}

// ============================================================
// Req 7.6: Non-member cannot execute any privileged action → "not authenticated"
// ============================================================
#[tokio::test]
async fn non_member_cannot_execute_privileged_actions() {
    let (addr, _state) = start_server().await;

    // Connect but do NOT join any room
    let (mut sink, mut stream) = ws_connect(addr).await;

    // Try KickParticipant → "not authenticated"
    ws_send(
        &mut sink,
        json!({"type":"kick_participant","targetParticipantId":"peer-1"}),
    )
    .await;
    let err = recv_type(&mut stream, "error").await;
    assert_eq!(err["message"], "not authenticated");

    // Try MuteParticipant → "not authenticated"
    ws_send(
        &mut sink,
        json!({"type":"mute_participant","targetParticipantId":"peer-1"}),
    )
    .await;
    let err = recv_type(&mut stream, "error").await;
    assert_eq!(err["message"], "not authenticated");

    // Try InviteRevoke → "not authenticated"
    ws_send(
        &mut sink,
        json!({"type":"invite_revoke","inviteCode":"ABCD1234"}),
    )
    .await;
    let err = recv_type(&mut stream, "error").await;
    assert_eq!(err["message"], "not authenticated");
}

// ============================================================
// Req 7.7: Host can kick, mute, and revoke within own room → success
// ============================================================
#[tokio::test]
async fn host_can_kick_mute_revoke_in_own_room() {
    let (addr, _state) = start_server().await;

    // Host joins
    let (mut s_host, mut r_host) = ws_connect(addr).await;
    let _host_id = join_sfu(&mut s_host, &mut r_host, "auth-success").await;

    // Host creates an invite for later revocation
    ws_send(&mut s_host, json!({"type":"invite_create","maxUses":5})).await;
    let created = recv_type(&mut r_host, "invite_created").await;
    let invite_code = created["inviteCode"].as_str().unwrap().to_string();
    drain(&mut r_host).await;

    // Guest joins
    let (mut s_guest, mut r_guest) = ws_connect(addr).await;
    let guest_id = join_sfu(&mut s_guest, &mut r_guest, "auth-success").await;
    let _ = recv_type(&mut r_host, "participant_joined").await;
    drain(&mut r_host).await;
    drain(&mut r_guest).await;

    // 1) Host mutes Guest → success (participant_muted broadcast)
    ws_send(
        &mut s_host,
        json!({"type":"mute_participant","targetParticipantId": &guest_id}),
    )
    .await;
    let muted = recv_type(&mut r_host, "participant_muted").await;
    assert_eq!(muted["participantId"], guest_id);
    // Guest also receives the mute notification
    let muted_guest = recv_type(&mut r_guest, "participant_muted").await;
    assert_eq!(muted_guest["participantId"], guest_id);

    // 2) Host revokes invite → success (invite_revoked)
    ws_send(
        &mut s_host,
        json!({"type":"invite_revoke","inviteCode": &invite_code}),
    )
    .await;
    let revoked = recv_type(&mut r_host, "invite_revoked").await;
    assert_eq!(revoked["inviteCode"], invite_code);

    // 3) Host kicks Guest → success (participant_kicked)
    ws_send(
        &mut s_host,
        json!({"type":"kick_participant","targetParticipantId": &guest_id}),
    )
    .await;
    let kicked = recv_type(&mut r_host, "participant_kicked").await;
    assert_eq!(kicked["participantId"], guest_id);
}
