#![cfg(feature = "test-support")]
//! Integration test: Invite Code Lifecycle (Test 8)
//!
//! Spins up the full Axum backend in-process with real WebSocket connections.
//! Two test functions:
//!   1. invite_generate_and_use â€” bypass mode, tests generate â†’ use â†’ room lifecycle
//!   2. invite_rejection_pipeline â€” enforcement mode, tests all rejection reasons
//!
//! Run: cargo test -p wavis-backend --test invite_lifecycle_integration

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

/// Start the backend on a random port. `require_invite` controls REQUIRE_INVITE_CODE.
/// Returns (addr, app_state) so tests can pre-populate invite store if needed.
async fn start_server(require_invite: bool) -> (SocketAddr, AppState) {
    // Set env vars for SFU config (read per-connection by SfuConfig::from_env()).
    // SAFETY: tests run with --test-threads=1, so no concurrent env var access.
    unsafe {
        std::env::set_var("SFU_JWT_SECRET", "dev-secret-32-bytes-minimum!!!XX");
        std::env::set_var("MAX_ROOM_PARTICIPANTS", "6");
        // Set REQUIRE_INVITE_CODE so AppState::new() reads the correct value.
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
    // Override require_invite_code directly â€” avoids env var race between tests.
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

use std::time::Instant;

// ==========================================================================
// Test 1: Happy path â€” generate invite, join with it, room lifecycle
// Backend runs with REQUIRE_INVITE_CODE=false (bypass mode)
// ==========================================================================
#[tokio::test]
async fn invite_generate_and_use() {
    let (addr, _state) = start_server(false).await;

    // Step 1: First peer joins without invite (bypass mode allows it)
    let (mut s1, mut r1) = ws_connect(addr).await;
    ws_send(
        &mut s1,
        json!({"type":"join","roomId":"inv-test","roomType":"sfu"}),
    )
    .await;
    let joined = recv_type(&mut r1, "joined").await;
    assert_eq!(joined["peerCount"], 1);
    let _ = recv_type(&mut r1, "media_token").await;

    // Step 2: Generate invite code (maxUses=2)
    ws_send(&mut s1, json!({"type":"invite_create","maxUses":2})).await;
    let created = recv_type(&mut r1, "invite_created").await;
    assert_eq!(created["maxUses"], 2);
    let code = created["inviteCode"].as_str().unwrap().to_string();
    assert!(!code.is_empty());

    // Step 3: Second peer joins with invite code
    let (mut s2, mut r2) = ws_connect(addr).await;
    ws_send(
        &mut s2,
        json!({"type":"join","roomId":"inv-test","roomType":"sfu","inviteCode":&code}),
    )
    .await;
    let joined2 = recv_type(&mut r2, "joined").await;
    assert_eq!(joined2["peerCount"], 2);
    let _ = recv_type(&mut r1, "participant_joined").await;
    drain(&mut r1).await;
    drain(&mut r2).await;

    // Step 4: Generate another code, revoke it, verify revoke response
    ws_send(&mut s1, json!({"type":"invite_create","maxUses":5})).await;
    let created2 = recv_type(&mut r1, "invite_created").await;
    let code2 = created2["inviteCode"].as_str().unwrap().to_string();

    ws_send(&mut s1, json!({"type":"invite_revoke","inviteCode":&code2})).await;
    let revoked = recv_type(&mut r1, "invite_revoked").await;
    assert_eq!(revoked["inviteCode"], code2);

    // Step 5: Cleanup
    ws_send(&mut s2, json!({"type":"leave"})).await;
    drain(&mut r1).await;
    drain(&mut r2).await;
    ws_send(&mut s1, json!({"type":"leave"})).await;
    drain(&mut r1).await;
}

// ==========================================================================
// Test 2: Full rejection pipeline â€” REQUIRE_INVITE_CODE=true
// Pre-populates invite store to test all rejection reasons end-to-end.
// ==========================================================================
#[tokio::test]
async fn invite_rejection_pipeline() {
    let (addr, state) = start_server(true).await;

    // Pre-populate invite store with codes for testing
    let now = Instant::now();

    // A valid code with 1 use (for successful join + exhaustion test)
    let valid_record = state
        .invite_store
        .generate("rej-test", "test-issuer", Some(1), now)
        .unwrap();
    let valid_code = valid_record.code.clone();

    // A code we'll revoke
    let revoke_record = state
        .invite_store
        .generate("rej-test", "test-issuer", Some(5), now)
        .unwrap();
    let revoke_code = revoke_record.code.clone();
    state.invite_store.revoke(&revoke_code).unwrap();

    // An exhausted code (0 remaining uses)
    let exhausted_record = state
        .invite_store
        .generate("rej-test", "test-issuer", Some(1), now)
        .unwrap();
    let exhausted_code = exhausted_record.code.clone();
    state.invite_store.consume_use(&exhausted_code); // use the 1 allowed use

    // --- invite_required: join without any invite code ---
    let (mut s1, mut r1) = ws_connect(addr).await;
    ws_send(
        &mut s1,
        json!({"type":"join","roomId":"rej-test","roomType":"sfu"}),
    )
    .await;
    let rej = recv_type(&mut r1, "join_rejected").await;
    assert_eq!(
        rej["reason"], "invite_required",
        "Missing code should be rejected"
    );
    drop(s1);

    // --- invite_invalid: join with a random code ---
    let (mut s2, mut r2) = ws_connect(addr).await;
    ws_send(
        &mut s2,
        json!({"type":"join","roomId":"rej-test","roomType":"sfu","inviteCode":"bogus-code"}),
    )
    .await;
    let rej2 = recv_type(&mut r2, "join_rejected").await;
    assert_eq!(
        rej2["reason"], "invite_invalid",
        "Unknown code should be rejected"
    );
    drop(s2);

    // --- invite_revoked: join with a revoked code ---
    let (mut s3, mut r3) = ws_connect(addr).await;
    ws_send(
        &mut s3,
        json!({"type":"join","roomId":"rej-test","roomType":"sfu","inviteCode":&revoke_code}),
    )
    .await;
    let rej3 = recv_type(&mut r3, "join_rejected").await;
    assert_eq!(
        rej3["reason"], "invite_revoked",
        "Revoked code should be rejected"
    );
    drop(s3);

    // --- invite_exhausted: join with an exhausted code ---
    let (mut s4, mut r4) = ws_connect(addr).await;
    ws_send(
        &mut s4,
        json!({"type":"join","roomId":"rej-test","roomType":"sfu","inviteCode":&exhausted_code}),
    )
    .await;
    let rej4 = recv_type(&mut r4, "join_rejected").await;
    assert_eq!(
        rej4["reason"], "invite_exhausted",
        "Exhausted code should be rejected"
    );
    drop(s4);

    // --- Valid code succeeds, then second use is exhausted ---
    let (mut s5, mut r5) = ws_connect(addr).await;
    ws_send(
        &mut s5,
        json!({"type":"join","roomId":"rej-test","roomType":"sfu","inviteCode":&valid_code}),
    )
    .await;
    let joined = recv_type(&mut r5, "joined").await;
    assert_eq!(joined["peerCount"], 1, "Valid code should allow join");
    drain(&mut r5).await;

    // Same code again â€” now exhausted
    let (mut s6, mut r6) = ws_connect(addr).await;
    ws_send(
        &mut s6,
        json!({"type":"join","roomId":"rej-test","roomType":"sfu","inviteCode":&valid_code}),
    )
    .await;
    let rej5 = recv_type(&mut r6, "join_rejected").await;
    assert_eq!(
        rej5["reason"], "invite_exhausted",
        "Used-up code should be rejected"
    );
    drop(s6);

    // Cleanup
    ws_send(&mut s5, json!({"type":"leave"})).await;
    drain(&mut r5).await;
}
