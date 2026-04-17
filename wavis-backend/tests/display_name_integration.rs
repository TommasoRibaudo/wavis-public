#![cfg(feature = "test-support")]
//! Integration tests automating TESTING.md Â§41fâ€“Â§41i.
//!
//! Covered:
//!   - Â§41f: Existing integration tests pass with display_name field in payloads
//!   - Â§41g: Join with displayName â†’ participant_joined event carries the name
//!   - Â§41h: Join without displayName â†’ fallback to peer_id
//!   - Â§41i: CreateRoom with displayName â†’ late joiner sees name in participants
//!
//! NOT covered (and why):
//!   - Â§41aâ€“Â§41e: Already automated as unit/property tests in shared/wavis-client crates
//!   - Â§41j: Client-only REPL behavior (name set while in room) â€” no backend involvement
//!   - Â§41k: LiveKit token display name â€” requires LiveKit Cloud credentials
//!   - Â§42*: Volume control â€” entirely client-side, no backend signaling involved
//!
//! Run: cargo test -p wavis-backend --test display_name_integration -- --test-threads=1

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
// Server setup + WS helpers (same pattern as create_room_integration.rs)
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
// Â§41g: Join with displayName â†’ participant_joined carries the name
// ==========================================================================

/// Â§41g: When a client joins with a displayName, the participant_joined event
/// broadcast to existing peers carries that display name.
#[tokio::test]
async fn test41g_join_with_display_name_propagates_to_participant_joined() {
    let (addr, _state) = start_server(false).await;

    // Client 1: create a room (no display name)
    let (mut sink1, mut stream1) = ws_connect(addr).await;
    ws_send(
        &mut sink1,
        json!({
            "type": "create_room",
            "roomId": "name-test-1",
            "roomType": "sfu"
        }),
    )
    .await;
    let _created = recv_type(&mut stream1, "room_created").await;
    drain(&mut stream1).await;

    // Client 2: join with a displayName
    let (mut sink2, mut stream2) = ws_connect(addr).await;
    ws_send(
        &mut sink2,
        json!({
            "type": "join",
            "roomId": "name-test-1",
            "roomType": "sfu",
            "displayName": "Alice"
        }),
    )
    .await;

    // Client 2 should receive "joined"
    let joined = recv_type(&mut stream2, "joined").await;
    assert_eq!(joined["roomId"], "name-test-1");

    // Client 1 should receive participant_joined with displayName = "Alice"
    let pj = recv_type(&mut stream1, "participant_joined").await;
    assert_eq!(pj["displayName"], "Alice");
    assert!(pj["participantId"].is_string());
}

/// Â§41g supplement: CreateRoom with displayName â†’ creator's display name is
/// stored and visible to late joiners in the participants list.
#[tokio::test]
async fn test41g_create_room_with_display_name_visible_to_joiner() {
    let (addr, _state) = start_server(false).await;

    // Client 1: create room with displayName
    let (mut sink1, mut stream1) = ws_connect(addr).await;
    ws_send(
        &mut sink1,
        json!({
            "type": "create_room",
            "roomId": "name-test-2",
            "roomType": "sfu",
            "displayName": "Charlie"
        }),
    )
    .await;
    let created = recv_type(&mut stream1, "room_created").await;
    assert_eq!(created["roomId"], "name-test-2");
    let invite_code = created["inviteCode"].as_str().unwrap().to_string();
    drain(&mut stream1).await;

    // Client 2: join the room â€” should see "Charlie" in participants list
    let (mut sink2, mut stream2) = ws_connect(addr).await;
    ws_send(
        &mut sink2,
        json!({
            "type": "join",
            "roomId": "name-test-2",
            "roomType": "sfu",
            "inviteCode": invite_code
        }),
    )
    .await;

    let joined = recv_type(&mut stream2, "joined").await;
    assert_eq!(joined["roomId"], "name-test-2");

    // The participants list should contain the creator with displayName "Charlie"
    let participants = joined["participants"].as_array().unwrap();
    let creator = participants
        .iter()
        .find(|p| p["displayName"].as_str() == Some("Charlie"));
    assert!(
        creator.is_some(),
        "Expected to find creator with displayName 'Charlie' in participants: {:?}",
        participants
    );
}

// ==========================================================================
// Â§41h: Join without displayName â†’ fallback to peer_id
// ==========================================================================

/// Â§41h: When a client joins without a displayName, the backend falls back to
/// using the peer_id as the display name in participant_joined events.
#[tokio::test]
async fn test41h_join_without_display_name_falls_back_to_peer_id() {
    let (addr, _state) = start_server(false).await;

    // Client 1: create a room
    let (mut sink1, mut stream1) = ws_connect(addr).await;
    ws_send(
        &mut sink1,
        json!({
            "type": "create_room",
            "roomId": "fallback-test",
            "roomType": "sfu"
        }),
    )
    .await;
    let _created = recv_type(&mut stream1, "room_created").await;
    drain(&mut stream1).await;

    // Client 2: join WITHOUT displayName
    let (mut sink2, mut stream2) = ws_connect(addr).await;
    ws_send(
        &mut sink2,
        json!({
            "type": "join",
            "roomId": "fallback-test",
            "roomType": "sfu"
        }),
    )
    .await;

    // Client 2 receives joined
    let joined = recv_type(&mut stream2, "joined").await;
    let joiner_peer_id = joined["peerId"].as_str().unwrap().to_string();

    // Client 1 receives participant_joined â€” displayName should equal the peer_id
    let pj = recv_type(&mut stream1, "participant_joined").await;
    let pj_display_name = pj["displayName"].as_str().unwrap();
    let pj_participant_id = pj["participantId"].as_str().unwrap();
    assert_eq!(
        pj_display_name, pj_participant_id,
        "Without displayName, displayName should equal participantId"
    );
    assert_eq!(pj_participant_id, joiner_peer_id);
}

/// Â§41h supplement: CreateRoom without displayName â†’ creator's display name
/// falls back to peer_id in the participants list.
#[tokio::test]
async fn test41h_create_room_without_display_name_falls_back_to_peer_id() {
    let (addr, _state) = start_server(false).await;

    // Client 1: create room WITHOUT displayName
    let (mut sink1, mut stream1) = ws_connect(addr).await;
    ws_send(
        &mut sink1,
        json!({
            "type": "create_room",
            "roomId": "fallback-create",
            "roomType": "sfu"
        }),
    )
    .await;
    let created = recv_type(&mut stream1, "room_created").await;
    let creator_peer_id = created["peerId"].as_str().unwrap().to_string();
    let invite_code = created["inviteCode"].as_str().unwrap().to_string();
    drain(&mut stream1).await;

    // Client 2: join the room
    let (mut sink2, mut stream2) = ws_connect(addr).await;
    ws_send(
        &mut sink2,
        json!({
            "type": "join",
            "roomId": "fallback-create",
            "roomType": "sfu",
            "inviteCode": invite_code
        }),
    )
    .await;

    let joined = recv_type(&mut stream2, "joined").await;
    let participants = joined["participants"].as_array().unwrap();

    // Find the creator in the participants list
    let creator = participants
        .iter()
        .find(|p| p["participantId"].as_str() == Some(&creator_peer_id))
        .expect("Creator should be in participants list");

    // displayName should equal peer_id (fallback)
    assert_eq!(
        creator["displayName"].as_str().unwrap(),
        creator_peer_id,
        "Without displayName, creator's displayName should equal their peer_id"
    );
}

// ==========================================================================
// Â§41i: Join/CreateRoom with displayName via raw JSON
// ==========================================================================

/// Â§41i: Join with displayName in raw JSON â€” the joined response includes the
/// joiner in the participants list, and a second client sees the display name.
#[tokio::test]
async fn test41i_join_with_display_name_raw_json() {
    let (addr, _state) = start_server(false).await;

    // Client 1: create room
    let (mut sink1, mut stream1) = ws_connect(addr).await;
    ws_send(
        &mut sink1,
        json!({
            "type": "create_room",
            "roomId": "raw-json-test",
            "roomType": "sfu"
        }),
    )
    .await;
    let created = recv_type(&mut stream1, "room_created").await;
    let invite_code = created["inviteCode"].as_str().unwrap().to_string();
    drain(&mut stream1).await;

    // Client 2: join with displayName "Charlie"
    let (mut sink2, mut stream2) = ws_connect(addr).await;
    ws_send(
        &mut sink2,
        json!({
            "type": "join",
            "roomId": "raw-json-test",
            "roomType": "sfu",
            "inviteCode": invite_code,
            "displayName": "Charlie"
        }),
    )
    .await;

    let joined = recv_type(&mut stream2, "joined").await;
    assert_eq!(joined["roomId"], "raw-json-test");

    // The joiner should see themselves in the participants list with displayName "Charlie"
    let participants = joined["participants"].as_array().unwrap();
    let charlie = participants
        .iter()
        .find(|p| p["displayName"].as_str() == Some("Charlie"));
    assert!(
        charlie.is_some(),
        "Joiner should see themselves with displayName 'Charlie' in participants: {:?}",
        participants
    );

    // Client 1 should receive participant_joined with displayName "Charlie"
    let pj = recv_type(&mut stream1, "participant_joined").await;
    assert_eq!(pj["displayName"], "Charlie");
}

/// Â§41i: CreateRoom with displayName "Dana" â€” second client joining sees "Dana"
/// in the participants list.
#[tokio::test]
async fn test41i_create_room_with_display_name_raw_json() {
    let (addr, _state) = start_server(false).await;

    // Client 1: create room with displayName "Dana"
    let (mut sink1, mut stream1) = ws_connect(addr).await;
    ws_send(
        &mut sink1,
        json!({
            "type": "create_room",
            "roomId": "raw-create-test",
            "roomType": "sfu",
            "displayName": "Dana"
        }),
    )
    .await;
    let created = recv_type(&mut stream1, "room_created").await;
    assert_eq!(created["roomId"], "raw-create-test");
    let invite_code = created["inviteCode"].as_str().unwrap().to_string();
    drain(&mut stream1).await;

    // Client 2: join the room
    let (mut sink2, mut stream2) = ws_connect(addr).await;
    ws_send(
        &mut sink2,
        json!({
            "type": "join",
            "roomId": "raw-create-test",
            "roomType": "sfu",
            "inviteCode": invite_code
        }),
    )
    .await;

    let joined = recv_type(&mut stream2, "joined").await;
    let participants = joined["participants"].as_array().unwrap();

    // Should see "Dana" in the participants list
    let dana = participants
        .iter()
        .find(|p| p["displayName"].as_str() == Some("Dana"));
    assert!(
        dana.is_some(),
        "Expected to find 'Dana' in participants list: {:?}",
        participants
    );
}

// ==========================================================================
// Â§41g/41i: Both clients have display names â€” bidirectional verification
// ==========================================================================

/// Both creator and joiner set display names. Verify the full round-trip:
/// - Creator's name appears in joiner's participants list
/// - Joiner's name appears in creator's participant_joined event
#[tokio::test]
async fn test41_both_clients_with_display_names() {
    let (addr, _state) = start_server(false).await;

    // Client 1: create room as "Alice"
    let (mut sink1, mut stream1) = ws_connect(addr).await;
    ws_send(
        &mut sink1,
        json!({
            "type": "create_room",
            "roomId": "both-names",
            "roomType": "sfu",
            "displayName": "Alice"
        }),
    )
    .await;
    let created = recv_type(&mut stream1, "room_created").await;
    let invite_code = created["inviteCode"].as_str().unwrap().to_string();
    drain(&mut stream1).await;

    // Client 2: join as "Bob"
    let (mut sink2, mut stream2) = ws_connect(addr).await;
    ws_send(
        &mut sink2,
        json!({
            "type": "join",
            "roomId": "both-names",
            "roomType": "sfu",
            "inviteCode": invite_code,
            "displayName": "Bob"
        }),
    )
    .await;

    // Client 2 (Bob) should see Alice in participants
    let joined = recv_type(&mut stream2, "joined").await;
    let participants = joined["participants"].as_array().unwrap();
    let alice = participants
        .iter()
        .find(|p| p["displayName"].as_str() == Some("Alice"));
    assert!(
        alice.is_some(),
        "Bob should see Alice in participants: {:?}",
        participants
    );

    // Client 1 (Alice) should receive participant_joined with displayName "Bob"
    let pj = recv_type(&mut stream1, "participant_joined").await;
    assert_eq!(pj["displayName"], "Bob");
}

/// Â§41h edge case: Join with empty displayName string â†’ falls back to peer_id
#[tokio::test]
async fn test41h_empty_display_name_falls_back_to_peer_id() {
    let (addr, _state) = start_server(false).await;

    // Client 1: create room
    let (mut sink1, mut stream1) = ws_connect(addr).await;
    ws_send(
        &mut sink1,
        json!({
            "type": "create_room",
            "roomId": "empty-name-test",
            "roomType": "sfu"
        }),
    )
    .await;
    let _created = recv_type(&mut stream1, "room_created").await;
    drain(&mut stream1).await;

    // Client 2: join with empty displayName
    let (mut sink2, mut stream2) = ws_connect(addr).await;
    ws_send(
        &mut sink2,
        json!({
            "type": "join",
            "roomId": "empty-name-test",
            "roomType": "sfu",
            "displayName": ""
        }),
    )
    .await;

    let joined = recv_type(&mut stream2, "joined").await;
    let joiner_peer_id = joined["peerId"].as_str().unwrap().to_string();

    // Client 1 receives participant_joined â€” displayName should equal peer_id
    let pj = recv_type(&mut stream1, "participant_joined").await;
    assert_eq!(
        pj["displayName"].as_str().unwrap(),
        pj["participantId"].as_str().unwrap(),
        "Empty displayName should fall back to peer_id"
    );
    assert_eq!(pj["participantId"].as_str().unwrap(), joiner_peer_id);
}
