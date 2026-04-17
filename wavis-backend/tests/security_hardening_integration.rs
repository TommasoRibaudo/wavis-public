#![cfg(feature = "test-support")]
//! Integration tests automating TESTING.md Â§27â€“Â§31 (Security Hardening).
//!
//! Covered:
//!   - Â§27d: Revoke nonexistent invite code â†’ "invite not found"
//!   - Â§27 (abuse metrics): revoke_authorization_rejections increments on unauthorized revoke
//!   - Â§28b: TLS enforcement rejects non-HTTPS connections (HTTP 403)
//!   - Â§28c: TLS enforcement accepts HTTPS via X-Forwarded-Proto header
//!   - Â§28 (abuse metrics): tls_proto_rejections increments
//!   - Â§29: Fail-closed config validation (unit-level, validate_security_config)
//!   - Â§30a: JWT media_token includes jti, nbf, iat claims
//!   - Â§30b: Key rotation â€” token signed with old secret validated via rotation
//!   - Â§30d: Previous secret too short is silently ignored
//!   - Â§31: Per-IP failed join threshold detection via abuse metrics
//!
//! NOT covered (and why):
//!   - Â§27a-c: Already covered by authorization_matrix_integration.rs
//!   - Â§28a: Startup validation (REQUIRE_TLS without TRUST_PROXY_HEADERS) — startup behavior,
//!     not a WS flow; would require spawning a separate process
//!   - Â§28d: Warning log for TRUST_PROXY_HEADERS without REQUIRE_TLS — log inspection,
//!     not a WS flow
//!   - Â§29 startup behavior: Requires spawning backend process with zero-value env vars;
//!     already covered by unit + property tests in config_validation.rs
//!   - Â§30c: Short secret rejected at startup in release builds — build-mode dependent
//!
//! Run: cargo test -p wavis-backend --test security_hardening_integration -- --test-threads=1

use futures_util::{SinkExt, StreamExt};
use serde_json::{Value, json};
use std::net::SocketAddr;
use std::sync::Arc;
use std::time::Duration;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpListener;
use tokio::time::timeout;
use tokio_tungstenite::{connect_async, tungstenite::Message};

use wavis_backend::abuse::join_rate_limiter::{JoinRateLimiter, JoinRateLimiterConfig};
use wavis_backend::app_state::AppState;
use wavis_backend::auth::auth_rate_limiter::{AuthRateLimiter, AuthRateLimiterConfig};
use wavis_backend::auth::jwt::{
    DEFAULT_JWT_ISSUER, DEFAULT_TOKEN_TTL_SECS, SFU_AUDIENCE, sign_media_token,
    validate_media_token, validate_media_token_with_rotation,
};
use wavis_backend::channel::invite::{InviteStore, InviteStoreConfig};
use wavis_backend::config_validation::{SecurityConfig, validate_security_config};
use wavis_backend::ip::IpConfig;
use wavis_backend::voice::mock_sfu_bridge::MockSfuBridge;
use wavis_backend::voice::sfu_bridge::{SfuRoomManager, SfuSignalingProxy};
use wavis_backend::ws::ws::ws_handler;

use axum::Router;
use axum::routing::get;

// ============================================================
// Server setup + WS helpers
// ============================================================

/// Standard server: no TLS enforcement, configurable invite requirement.
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

/// Server with TLS enforcement enabled and proxy headers trusted.
async fn start_server_with_tls(require_invite: bool) -> (SocketAddr, AppState) {
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
        trust_proxy_headers: true,
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
    app_state.require_tls = true;

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

/// Server with custom JWT secrets (for key rotation tests).
async fn start_server_with_jwt_secrets(
    current_secret: &[u8],
    previous_secret: Option<&[u8]>,
) -> (SocketAddr, AppState) {
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
        Arc::new(current_secret.to_vec()),
        previous_secret.map(|s| Arc::new(s.to_vec())),
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

async fn drain(stream: &mut WsStream) {
    while let Ok(Some(Ok(_))) = timeout(Duration::from_millis(200), stream.next()).await {
        // Continue draining
    }
}

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

/// Send a raw HTTP request and read the response status code.
async fn http_get_status(addr: SocketAddr, path: &str, extra_headers: &str) -> u16 {
    let mut stream = tokio::net::TcpStream::connect(addr).await.unwrap();

    let request =
        format!("GET {path} HTTP/1.1\r\nHost: {addr}\r\nConnection: close\r\n{extra_headers}\r\n");
    stream.write_all(request.as_bytes()).await.unwrap();

    let mut response = Vec::new();
    let _ = timeout(Duration::from_secs(3), async {
        let mut buf = [0u8; 4096];
        loop {
            match stream.read(&mut buf).await {
                Ok(0) => break,
                Ok(n) => response.extend_from_slice(&buf[..n]),
                Err(_) => break,
            }
        }
    })
    .await;

    let response_str = String::from_utf8_lossy(&response);
    response_str
        .lines()
        .next()
        .unwrap_or("")
        .split_whitespace()
        .nth(1)
        .and_then(|s| s.parse().ok())
        .unwrap_or(0)
}

// ==========================================================================
// Â§27d: Revoke nonexistent invite code â†’ "invite not found"
// ==========================================================================

#[tokio::test]
async fn test27d_revoke_nonexistent_code() {
    let (addr, _state) = start_server(false).await;

    // Host joins
    let (mut s_host, mut r_host) = ws_connect(addr).await;
    let _host_id = join_sfu(&mut s_host, &mut r_host, "revoke-nonexist").await;

    // Try to revoke a code that was never created
    ws_send(
        &mut s_host,
        json!({"type":"invite_revoke","inviteCode":"does-not-exist"}),
    )
    .await;
    let err = recv_type(&mut r_host, "error").await;
    assert!(
        err["message"].as_str().unwrap().contains("not found"),
        "expected 'not found' error, got: {}",
        err["message"]
    );
}

// ==========================================================================
// Â§27 (abuse metrics): revoke_authorization_rejections increments
// ==========================================================================

#[tokio::test]
async fn test27_revoke_authorization_rejections_metric() {
    let (addr, state) = start_server(false).await;

    // Host joins and creates invite
    let (mut s_host, mut r_host) = ws_connect(addr).await;
    let _host_id = join_sfu(&mut s_host, &mut r_host, "revoke-metric").await;

    ws_send(&mut s_host, json!({"type":"invite_create","maxUses":5})).await;
    let created = recv_type(&mut r_host, "invite_created").await;
    let invite_code = created["inviteCode"].as_str().unwrap().to_string();
    drain(&mut r_host).await;

    // Guest joins
    let (mut s_guest, mut r_guest) = ws_connect(addr).await;
    let _guest_id = join_sfu(&mut s_guest, &mut r_guest, "revoke-metric").await;
    drain(&mut r_host).await;
    drain(&mut r_guest).await;

    let before = state
        .abuse_metrics
        .revoke_authorization_rejections
        .load(std::sync::atomic::Ordering::Relaxed);

    // Guest tries to revoke â†’ unauthorized
    ws_send(
        &mut s_guest,
        json!({"type":"invite_revoke","inviteCode": &invite_code}),
    )
    .await;
    let err = recv_type(&mut r_guest, "error").await;
    assert_eq!(err["message"], "unauthorized");

    let after = state
        .abuse_metrics
        .revoke_authorization_rejections
        .load(std::sync::atomic::Ordering::Relaxed);
    assert!(
        after > before,
        "revoke_authorization_rejections should increment (before={before}, after={after})"
    );
}

// ==========================================================================
// Â§28b: TLS enforcement rejects non-HTTPS connections (HTTP 403)
// ==========================================================================

#[tokio::test]
async fn test28b_tls_enforcement_rejects_non_https() {
    let (addr, state) = start_server_with_tls(false).await;

    // Attempt a real WebSocket upgrade without X-Forwarded-Proto header â†’ should get 403
    let url = format!("ws://127.0.0.1:{}/ws", addr.port());
    let result = connect_async(&url).await;
    // The server should reject with 403 before completing the WS handshake
    match result {
        Err(tokio_tungstenite::tungstenite::Error::Http(response)) => {
            assert_eq!(
                response.status().as_u16(),
                403,
                "non-HTTPS connection should be rejected with 403"
            );
        }
        Err(other) => panic!("expected HTTP 403 error, got: {:?}", other),
        Ok(_) => panic!("WS connection should have been rejected"),
    }

    // Verify tls_proto_rejections metric incremented
    let rejections = state
        .abuse_metrics
        .tls_proto_rejections
        .load(std::sync::atomic::Ordering::Relaxed);
    assert!(
        rejections > 0,
        "tls_proto_rejections should be > 0 after non-HTTPS rejection"
    );
}

// ==========================================================================
// Â§28c: TLS enforcement accepts HTTPS via X-Forwarded-Proto header
// ==========================================================================

#[tokio::test]
async fn test28c_tls_enforcement_accepts_https_header() {
    let (addr, _state) = start_server_with_tls(false).await;

    // Send WS upgrade with X-Forwarded-Proto: https â†’ should NOT get 403
    let status = http_get_status(
        addr,
        "/ws",
        "X-Forwarded-Proto: https\r\nUpgrade: websocket\r\nConnection: Upgrade\r\nSec-WebSocket-Key: dGhlIHNhbXBsZSBub25jZQ==\r\nSec-WebSocket-Version: 13\r\n",
    )
    .await;

    // Should pass TLS check. May get 101 (upgrade) or 400 (bad handshake) but NOT 403.
    assert_ne!(
        status, 403,
        "HTTPS connection via X-Forwarded-Proto should not be rejected with 403 (got {status})"
    );
}

// ==========================================================================
// Â§29: Fail-closed config validation (integration-level unit test)
// ==========================================================================

#[test]
fn test29a_zero_global_ws_ceiling_rejected() {
    let config = SecurityConfig {
        global_ws_per_sec: 0,
        global_joins_per_sec: 50,
        invite_ttl_secs: 86400,
        token_ttl_secs: 600,
        ban_duration_secs: 600,
        rate_limit_window_secs: 10,
        bug_report_rate_limit_max: 5,
        bug_report_rate_limit_window_secs: 3600,
        github_bug_report_token_set: true,
        github_bug_report_repo_set: true,
    };
    let err = validate_security_config(&config).unwrap_err();
    assert!(err.contains("GLOBAL_WS_UPGRADES_PER_SEC"));
}

#[test]
fn test29b_zero_global_join_ceiling_rejected() {
    let config = SecurityConfig {
        global_ws_per_sec: 100,
        global_joins_per_sec: 0,
        invite_ttl_secs: 86400,
        token_ttl_secs: 600,
        ban_duration_secs: 600,
        rate_limit_window_secs: 10,
        bug_report_rate_limit_max: 5,
        bug_report_rate_limit_window_secs: 3600,
        github_bug_report_token_set: true,
        github_bug_report_repo_set: true,
    };
    let err = validate_security_config(&config).unwrap_err();
    assert!(err.contains("GLOBAL_JOINS_PER_SEC"));
}

#[test]
fn test29c_zero_token_ttl_rejected() {
    let config = SecurityConfig {
        global_ws_per_sec: 100,
        global_joins_per_sec: 50,
        invite_ttl_secs: 86400,
        token_ttl_secs: 0,
        ban_duration_secs: 600,
        rate_limit_window_secs: 10,
        bug_report_rate_limit_max: 5,
        bug_report_rate_limit_window_secs: 3600,
        github_bug_report_token_set: true,
        github_bug_report_repo_set: true,
    };
    let err = validate_security_config(&config).unwrap_err();
    assert!(err.contains("token TTL"));
}

#[test]
fn test29d_zero_ban_duration_rejected() {
    let config = SecurityConfig {
        global_ws_per_sec: 100,
        global_joins_per_sec: 50,
        invite_ttl_secs: 86400,
        token_ttl_secs: 600,
        ban_duration_secs: 0,
        rate_limit_window_secs: 10,
        bug_report_rate_limit_max: 5,
        bug_report_rate_limit_window_secs: 3600,
        github_bug_report_token_set: true,
        github_bug_report_repo_set: true,
    };
    let err = validate_security_config(&config).unwrap_err();
    assert!(err.contains("ban duration"));
}

#[test]
fn test29e_zero_rate_limit_window_rejected() {
    let config = SecurityConfig {
        global_ws_per_sec: 100,
        global_joins_per_sec: 50,
        invite_ttl_secs: 86400,
        token_ttl_secs: 600,
        ban_duration_secs: 600,
        rate_limit_window_secs: 0,
        bug_report_rate_limit_max: 5,
        bug_report_rate_limit_window_secs: 3600,
        github_bug_report_token_set: true,
        github_bug_report_repo_set: true,
    };
    let err = validate_security_config(&config).unwrap_err();
    assert!(err.contains("rate limit window"));
}

#[test]
fn test29f_all_positive_values_accepted() {
    let config = SecurityConfig {
        global_ws_per_sec: 100,
        global_joins_per_sec: 50,
        invite_ttl_secs: 86400,
        token_ttl_secs: 600,
        ban_duration_secs: 600,
        rate_limit_window_secs: 10,
        bug_report_rate_limit_max: 5,
        bug_report_rate_limit_window_secs: 3600,
        github_bug_report_token_set: true,
        github_bug_report_repo_set: true,
    };
    assert!(validate_security_config(&config).is_ok());
}

// ==========================================================================
// Â§30a: JWT media_token includes jti, nbf, iat claims
// ==========================================================================

#[tokio::test]
async fn test30a_media_token_includes_jti_nbf_iat() {
    let (addr, state) = start_server(false).await;

    let (mut sink, mut stream) = ws_connect(addr).await;
    ws_send(
        &mut sink,
        json!({"type":"join","roomId":"jwt-claims-test","roomType":"sfu"}),
    )
    .await;
    let _joined = recv_type(&mut stream, "joined").await;

    // SFU join should produce a media_token message
    let media_token_msg = recv_type(&mut stream, "media_token").await;
    let token_str = media_token_msg["token"]
        .as_str()
        .expect("media_token should have 'token' field");

    // Validate the token and inspect claims
    let claims = validate_media_token(
        token_str,
        &state.jwt_secret,
        SFU_AUDIENCE,
        &state.jwt_issuer,
    )
    .expect("media_token should be valid");

    // Â§30a: jti is a valid UUID v4
    let parsed_uuid = uuid::Uuid::parse_str(&claims.jti).expect("jti should be a valid UUID");
    assert_eq!(
        parsed_uuid.get_version(),
        Some(uuid::Version::Random),
        "jti should be UUID v4"
    );

    // Â§30a: nbf and iat are present and equal
    assert_eq!(claims.nbf, claims.iat, "nbf should equal iat");

    // Â§30a: iat is recent (within 5 seconds of now)
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_secs();
    assert!(
        claims.iat >= now.saturating_sub(5) && claims.iat <= now + 5,
        "iat should be within 5s of now (iat={}, now={now})",
        claims.iat
    );
}

// ==========================================================================
// Â§30b: Key rotation â€” token signed with old secret validated via rotation
// ==========================================================================

#[test]
fn test30b_key_rotation_dual_secret_validation() {
    let old_secret = b"old-secret-32-bytes-minimum!!!XX";
    let new_secret = b"new-secret-32-bytes-minimum!!!YY";

    // Sign a token with the old secret
    let token = sign_media_token(
        "rotation-room",
        "peer-1",
        old_secret,
        DEFAULT_JWT_ISSUER,
        DEFAULT_TOKEN_TTL_SECS,
    )
    .expect("signing with old secret should succeed");

    // Validate with new secret as current, old as previous â†’ should succeed
    let claims = validate_media_token_with_rotation(
        &token,
        new_secret,
        Some(old_secret.as_slice()),
        SFU_AUDIENCE,
        DEFAULT_JWT_ISSUER,
    )
    .expect("rotation validation should succeed with previous secret");

    assert_eq!(claims.room_id, "rotation-room");
    assert_eq!(claims.participant_id, "peer-1");

    // Validate with new secret only (no previous) â†’ should fail
    let result = validate_media_token_with_rotation(
        &token,
        new_secret,
        None,
        SFU_AUDIENCE,
        DEFAULT_JWT_ISSUER,
    );
    assert!(
        result.is_err(),
        "validation without previous secret should fail for old-secret token"
    );
}

// ==========================================================================
// Â§30b: Integration â€” server with dual secrets issues valid tokens
// ==========================================================================

#[tokio::test]
async fn test30b_server_with_dual_secrets_issues_valid_tokens() {
    let current = b"current-secret-32-bytes-min!!!XX";
    let previous = b"previous-secret-32-bytes-min!!XX";

    let (addr, state) = start_server_with_jwt_secrets(current, Some(previous)).await;

    let (mut sink, mut stream) = ws_connect(addr).await;
    ws_send(
        &mut sink,
        json!({"type":"join","roomId":"dual-secret-test","roomType":"sfu"}),
    )
    .await;
    let _joined = recv_type(&mut stream, "joined").await;
    let media_token_msg = recv_type(&mut stream, "media_token").await;
    let token_str = media_token_msg["token"].as_str().unwrap();

    // Token should be signed with the current secret
    let claims = validate_media_token(token_str, current, SFU_AUDIENCE, &state.jwt_issuer)
        .expect("token should validate with current secret");
    assert_eq!(claims.room_id, "dual-secret-test");

    // Token should NOT validate with the previous secret alone
    // (it was signed with current, not previous)
    let result = validate_media_token(token_str, previous, SFU_AUDIENCE, &state.jwt_issuer);
    assert!(
        result.is_err(),
        "token signed with current secret should not validate with previous secret alone"
    );
}

// ==========================================================================
// Â§30d: Previous secret too short is silently ignored
// ==========================================================================

#[test]
fn test30d_short_previous_secret_ignored() {
    let current = b"current-secret-32-bytes-min!!!XX";
    let short_previous = b"short"; // < 32 bytes

    // Sign with current secret
    let token = sign_media_token(
        "room-1",
        "peer-1",
        current,
        DEFAULT_JWT_ISSUER,
        DEFAULT_TOKEN_TTL_SECS,
    )
    .unwrap();

    // validate_media_token_with_rotation with short previous should still work
    // because the current secret validates the token directly
    let claims = validate_media_token_with_rotation(
        &token,
        current,
        Some(short_previous.as_slice()),
        SFU_AUDIENCE,
        DEFAULT_JWT_ISSUER,
    )
    .expect("current secret should validate even with short previous");
    assert_eq!(claims.room_id, "room-1");

    // Sign with a different valid secret, try to validate with current + short previous
    // â†’ should fail (short previous can't validate, current doesn't match)
    let other_secret = b"other-secret-32-bytes-minimum!!X";
    let token2 = sign_media_token(
        "room-2",
        "peer-2",
        other_secret,
        DEFAULT_JWT_ISSUER,
        DEFAULT_TOKEN_TTL_SECS,
    )
    .unwrap();

    let result = validate_media_token_with_rotation(
        &token2,
        current,
        Some(short_previous.as_slice()),
        SFU_AUDIENCE,
        DEFAULT_JWT_ISSUER,
    );
    // The short previous secret will cause validate_media_token to return an error
    // (secret < 32 bytes), so the overall rotation fails
    assert!(
        result.is_err(),
        "short previous secret should not validate tokens signed with a different secret"
    );
}

// ==========================================================================
// Â§31: Per-IP failed join threshold detection
// ==========================================================================

/// Send 11 join attempts with invalid invite codes from the same IP.
/// The IpFailedJoinTracker (default threshold=10) should fire on the 11th,
/// incrementing invite_usage_anomalies.
#[tokio::test]
async fn test31_per_ip_failed_join_threshold_detection() {
    let (addr, app_state) = start_server(true).await;

    // Baseline: no anomalies yet
    let before = app_state.abuse_metrics.snapshot().invite_usage_anomalies;
    assert_eq!(before, 0, "invite_usage_anomalies should start at 0");

    // Send 11 join attempts with different bad invite codes.
    // Attempts 1-10: invite validation fails, record_failure returns None.
    // Attempt 11: rate limiter's ip_failed dimension rejects (threshold=10),
    //   record_failure is called â†’ count=11 > threshold=10 â†’ Some(11) â†’ metric incremented.
    for i in 0..11 {
        let (mut sink, mut stream) = ws_connect(addr).await;
        ws_send(
            &mut sink,
            json!({
                "type": "join",
                "roomId": "threshold-test",
                "roomType": "sfu",
                "inviteCode": format!("bad-code-{}", i)
            }),
        )
        .await;

        // Each attempt should produce a join_rejected
        let msg = recv_type(&mut stream, "join_rejected").await;
        let reason = msg["reason"].as_str().unwrap_or("");
        assert!(
            reason == "invite_invalid" || reason == "rate_limited",
            "attempt {}: expected invite_invalid or rate_limited, got: {}",
            i,
            reason
        );

        // Close the connection before the next attempt
        let _ = sink.close().await;
        // Small delay to let the server process the disconnect
        tokio::time::sleep(Duration::from_millis(20)).await;
    }

    let after = app_state.abuse_metrics.snapshot().invite_usage_anomalies;
    assert!(
        after >= 1,
        "invite_usage_anomalies should have incremented after exceeding threshold, got: {}",
        after
    );
}
