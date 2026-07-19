#![cfg(feature = "test-support")]
//! Integration tests for issue #267: the `/ws` pre-auth ticket gate.
//!
//! Covers the WS-level acceptance criteria:
//!   - `/ws` without a ticket fails before upgrade
//!   - `/ws` with an unknown/invalid ticket fails before upgrade
//!   - `/ws` with a replayed (already-consumed) ticket fails before upgrade
//!   - `/ws` with a valid ticket succeeds once, and the ticket is consumed
//!   - a successfully-ticketed connection is pre-authenticated (no separate
//!     `Auth` message required — sending one is rejected as "already authenticated")
//!
//! Ticket expiry is covered at the unit level (`auth::ws_ticket::tests`, with
//! clock injection) rather than here — waiting out a real 60s TTL in an
//! integration test would be slow for no additional coverage.
//!
//! No Postgres required: `WsTicketStore` is in-memory, and the dummy lazy
//! pool below is never queried by anything this file exercises.
//!
//! Run: cargo test -p wavis-backend --test ws_ticket_gate_integration --features test-support -- --test-threads=1

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
use wavis_backend::voice::mock_sfu_bridge::MockSfuBridge;
use wavis_backend::voice::sfu_bridge::{SfuRoomManager, SfuSignalingProxy};
use wavis_backend::ws::ws::ws_handler;

use axum::Router;
use axum::routing::get;

use futures_util::{SinkExt, StreamExt};

// ============================================================
// Server setup helper (mirrors abuse_controls_integration.rs)
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

    let app_state = AppState::new(
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

// ============================================================
// Tests
// ============================================================

#[tokio::test]
async fn ws_without_ticket_fails_before_upgrade() {
    let (addr, _state) = start_server().await;
    let url = format!("ws://{addr}/ws");

    let result = connect_async(&url).await;
    assert!(
        result.is_err(),
        "connection without a ticket query param must be rejected before upgrade"
    );
}

#[tokio::test]
async fn ws_with_empty_ticket_fails_before_upgrade() {
    let (addr, _state) = start_server().await;
    let url = format!("ws://{addr}/ws?ticket=");

    let result = connect_async(&url).await;
    assert!(
        result.is_err(),
        "connection with an empty ticket value must be rejected before upgrade"
    );
}

#[tokio::test]
async fn ws_with_unknown_ticket_fails_before_upgrade() {
    let (addr, _state) = start_server().await;
    let url = format!("ws://{addr}/ws?ticket=totally-made-up-ticket-value-not-issued");

    let result = connect_async(&url).await;
    assert!(
        result.is_err(),
        "connection with a ticket the server never issued must be rejected before upgrade"
    );
}

#[tokio::test]
async fn ws_with_valid_ticket_succeeds_once_then_replay_fails() {
    let (addr, state) = start_server().await;
    let ticket = state.ws_ticket_store.issue(Uuid::new_v4(), Uuid::new_v4());
    let url = format!("ws://{addr}/ws?ticket={ticket}");

    // First use of a freshly-issued, valid ticket must succeed.
    let first = connect_async(&url).await;
    assert!(first.is_ok(), "first use of a valid ticket must succeed");
    drop(first);

    // Replaying the same (now-consumed) ticket must be rejected.
    let second = connect_async(&url).await;
    assert!(
        second.is_err(),
        "a replayed (already-consumed) ticket must be rejected before upgrade"
    );
}

#[tokio::test]
async fn ws_ticket_authenticates_connection_without_a_separate_auth_message() {
    let (addr, state) = start_server().await;
    let ticket = state.ws_ticket_store.issue(Uuid::new_v4(), Uuid::new_v4());
    let url = format!("ws://{addr}/ws?ticket={ticket}");

    let (ws, _) = connect_async(&url)
        .await
        .expect("a valid ticket must be accepted");
    let (mut sink, mut stream) = ws.split();

    // The connection is already authenticated by the ticket at upgrade time,
    // so a client-sent Auth message must be rejected as redundant — proving
    // the server didn't wait for one to mark the session authenticated.
    sink.send(Message::Text(
        json!({"type": "auth", "accessToken": "irrelevant"}).to_string(),
    ))
    .await
    .unwrap();

    let msg = timeout(Duration::from_secs(3), stream.next())
        .await
        .expect("expected a response before timeout")
        .expect("stream closed unexpectedly")
        .expect("ws-level error");

    let Message::Text(text) = msg else {
        panic!("expected a text message, got {msg:?}");
    };
    let v: Value = serde_json::from_str(&text).unwrap();
    assert_eq!(v["type"], "error");
    assert_eq!(
        v["message"], "already authenticated",
        "ticket-derived auth must already be in effect at connection start"
    );
}
