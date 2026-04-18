mod abuse;
mod app_state;
mod auth;
mod channel;
mod chat;
mod config_validation;
mod connections;
mod diagnostics;
mod ec2_control;
mod error;
mod ip;
mod redaction;
mod state;
mod voice;
mod ws;

use abuse::join_rate_limiter::{JoinRateLimiter, JoinRateLimiterConfig};
use app_state::AppState;
use auth::auth_rate_limiter::{AuthRateLimiter, AuthRateLimiterConfig};
use axum::extract::Request;
use axum::http::StatusCode;
use axum::middleware::Next;
use axum::response::{IntoResponse, Response};
use axum::{
    Json, Router,
    extract::State,
    routing::{delete, get, post, put},
};
use channel::invite::{InviteStore, InviteStoreConfig};
use chat::chat_persistence;
use config_validation::{SecurityConfig, validate_security_config};
use connections::ConnectionManager;
use voice::livekit_bridge::LiveKitSfuBridge;
// MockSfuBridge is test-only (§9.2). For dev builds without LiveKit,
// a lightweight DevStubSfuBridge is defined inline below.
use auth::pairing;
use auth::phrase;
use auth::recovery_rate_limiter::{RecoveryRateLimiter, RecoveryRateLimiterConfig};
use ip::IpConfig;
use redaction::Sensitive;
use serde_json::json;
use std::env;
use std::io;
use std::net::SocketAddr;
use std::sync::Arc;
use std::sync::atomic::Ordering;
use std::time::Duration;
use tracing_subscriber::EnvFilter;
use voice::sfu_bridge::{SfuHealth, SfuRoomManager, SfuSignalingProxy};
use ws::ws::ws_handler;

// ---------------------------------------------------------------------------
// Development-only SFU stub (§9.2: mock logic must never reach production)
// ---------------------------------------------------------------------------
// In debug builds without LIVEKIT_* env vars, this lightweight stub provides
// a no-op SfuRoomManager + SfuSignalingProxy so the server can start for
// local development. In release builds the server will refuse to start
// without LiveKit configured (fail-closed per §6.2).
#[cfg(debug_assertions)]
mod dev_stub_sfu {
    use async_trait::async_trait;
    use shared::signaling::IceCandidate;

    use crate::voice::sfu_bridge::{
        SfuError, SfuHealth, SfuRoomHandle, SfuRoomManager, SfuSignalingProxy,
    };

    /// Stub SFU bridge for local development without a running SFU.
    /// All operations succeed with no-op behavior.
    pub struct DevStubSfuBridge;

    #[async_trait]
    impl SfuRoomManager for DevStubSfuBridge {
        async fn create_room(&self, room_id: &str) -> Result<SfuRoomHandle, SfuError> {
            Ok(SfuRoomHandle(room_id.to_string()))
        }
        async fn destroy_room(&self, _handle: &SfuRoomHandle) -> Result<(), SfuError> {
            Ok(())
        }
        async fn add_participant(
            &self,
            _handle: &SfuRoomHandle,
            _participant_id: &str,
        ) -> Result<(), SfuError> {
            Ok(())
        }
        async fn remove_participant(
            &self,
            _handle: &SfuRoomHandle,
            _participant_id: &str,
        ) -> Result<(), SfuError> {
            Ok(())
        }
        async fn health_check(&self) -> Result<SfuHealth, SfuError> {
            Ok(SfuHealth::Available)
        }
    }

    #[async_trait]
    impl SfuSignalingProxy for DevStubSfuBridge {
        async fn forward_offer(
            &self,
            _handle: &SfuRoomHandle,
            _participant_id: &str,
            _sdp: &str,
        ) -> Result<String, SfuError> {
            Ok("stub-answer-sdp".to_string())
        }
        async fn forward_ice_candidate(
            &self,
            _handle: &SfuRoomHandle,
            _participant_id: &str,
            _candidate: &IceCandidate,
        ) -> Result<(), SfuError> {
            Ok(())
        }
        async fn poll_sfu_ice_candidates(
            &self,
            _handle: &SfuRoomHandle,
            _participant_id: &str,
        ) -> Result<Vec<IceCandidate>, SfuError> {
            Ok(vec![])
        }
    }
}

/// CloudFront origin verification middleware.
/// Rejects requests missing the `X-Origin-Verify` header when `CF_ORIGIN_SECRET` is set.
/// Skips `/health` so load balancers and Docker healthchecks still work.
async fn verify_origin(req: Request, next: Next) -> Response {
    // Skip health endpoint — must remain accessible without the header.
    if req.uri().path() == "/health" {
        return next.run(req).await;
    }

    let expected = env::var("CF_ORIGIN_SECRET").ok().filter(|s| !s.is_empty());
    if let Some(secret) = expected {
        let header_val = req
            .headers()
            .get("x-origin-verify")
            .and_then(|v| v.to_str().ok());
        match header_val {
            Some(val) if val == secret => {}
            _ => return StatusCode::FORBIDDEN.into_response(),
        }
    }
    next.run(req).await
}

#[tokio::main]
async fn main() -> io::Result<()> {
    // Load repo-local .env for local development. Existing process env still wins.
    let _ = dotenvy::dotenv();

    // Initialize structured logging. Log level comes from RUST_LOG env var.
    tracing_subscriber::fmt()
        .with_env_filter(EnvFilter::from_default_env())
        .with_target(false)
        .init();

    // Load JWT signing secret with fail-closed logic.
    // Single authoritative source — all consumers read from AppState.
    let jwt_secret = match env::var("SFU_JWT_SECRET") {
        Ok(s) => s,
        Err(_) => {
            if cfg!(debug_assertions) {
                "dev-secret-32-bytes-minimum!!!XX".to_string()
            } else {
                eprintln!("FATAL: SFU_JWT_SECRET must be set in release builds");
                std::process::exit(1);
            }
        }
    };
    if jwt_secret.len() < 32 {
        eprintln!("FATAL: SFU_JWT_SECRET must be at least 32 bytes");
        std::process::exit(1);
    }
    let jwt_secret = Arc::new(jwt_secret.into_bytes());

    // Optional previous secret for zero-downtime key rotation.
    // Silently ignored if shorter than 32 bytes (treat as "not configured").
    let jwt_secret_previous = env::var("SFU_JWT_SECRET_PREVIOUS")
        .ok()
        .filter(|s| s.len() >= 32)
        .map(|s| Arc::new(s.into_bytes()));

    let jwt_issuer = env::var("SFU_JWT_ISSUER").unwrap_or_else(|_| "wavis-backend".to_string());

    // --- Postgres connection pool + migrations ---
    let database_url = match env::var("DATABASE_URL") {
        Ok(url) => url,
        Err(_) => {
            if cfg!(debug_assertions) {
                "postgres://wavis:wavis@localhost:5432/wavis".to_string()
            } else {
                eprintln!("FATAL: DATABASE_URL must be set in release builds");
                std::process::exit(1);
            }
        }
    };

    let db_pool = sqlx::postgres::PgPoolOptions::new()
        .max_connections(10)
        .connect(&database_url)
        .await
        .unwrap_or_else(|e| {
            eprintln!("FATAL: Failed to connect to database: {e}");
            std::process::exit(1);
        });

    sqlx::migrate!().run(&db_pool).await.unwrap_or_else(|e| {
        eprintln!("FATAL: Database migration failed: {e}");
        std::process::exit(1);
    });

    // --- Auth JWT secret (separate from SFU JWT secret, Req 3.4) ---
    let auth_jwt_secret = match env::var("AUTH_JWT_SECRET") {
        Ok(s) => s,
        Err(_) => {
            if cfg!(debug_assertions) {
                "dev-auth-secret-32-bytes-min!!XX".to_string()
            } else {
                eprintln!("FATAL: AUTH_JWT_SECRET must be set in release builds");
                std::process::exit(1);
            }
        }
    };
    if auth_jwt_secret.len() < 32 {
        eprintln!("FATAL: AUTH_JWT_SECRET must be at least 32 bytes");
        std::process::exit(1);
    }
    let auth_jwt_secret = Arc::new(auth_jwt_secret.into_bytes());

    let auth_jwt_secret_previous = env::var("AUTH_JWT_SECRET_PREVIOUS")
        .ok()
        .filter(|s| s.len() >= 32)
        .map(|s| Arc::new(s.into_bytes()));

    // --- Refresh token TTL validation (Req 4.5) ---
    let refresh_token_ttl_days: u32 = env::var("REFRESH_TOKEN_TTL_DAYS")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(180);
    if let Err(msg) = auth::auth::validate_refresh_ttl(refresh_token_ttl_days) {
        eprintln!("FATAL: {msg}");
        std::process::exit(1);
    }

    let consumed_token_retention_hours: u64 = env::var("CONSUMED_TOKEN_RETENTION_HOURS")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(72);

    // --- Refresh token pepper (HMAC key for at-rest hash protection) ---
    let refresh_token_pepper = match env::var("AUTH_REFRESH_PEPPER") {
        Ok(s) => s,
        Err(_) => {
            if cfg!(debug_assertions) {
                "dev-pepper-32-bytes-minimum!!XXX".to_string()
            } else {
                eprintln!("FATAL: AUTH_REFRESH_PEPPER must be set in release builds");
                std::process::exit(1);
            }
        }
    };
    if refresh_token_pepper.len() < 32 {
        eprintln!("FATAL: AUTH_REFRESH_PEPPER must be at least 32 bytes");
        std::process::exit(1);
    }
    let refresh_token_pepper = Arc::new(refresh_token_pepper.into_bytes());

    // Optional previous pepper for zero-downtime rotation.
    let refresh_token_pepper_previous = env::var("AUTH_REFRESH_PEPPER_PREVIOUS")
        .ok()
        .filter(|s| s.len() >= 32)
        .map(|s| Arc::new(s.into_bytes()));

    // --- Auth rate limiter config ---
    let auth_rate_limiter = Arc::new(AuthRateLimiter::new(AuthRateLimiterConfig {
        register_max_per_ip: env::var("AUTH_REGISTER_RATE_LIMIT")
            .ok()
            .and_then(|v| v.parse().ok())
            .unwrap_or(5),
        register_window_secs: 3600,
        refresh_max_per_ip: env::var("AUTH_REFRESH_RATE_LIMIT")
            .ok()
            .and_then(|v| v.parse().ok())
            .unwrap_or(30),
        refresh_window_secs: 60,
    }));

    // Build SFU bridge — use LiveKitSfuBridge when LIVEKIT_* env vars are set,
    // otherwise fall back to MockSfuBridge (proxy mode for testing).
    let (sfu_room_manager, sfu_signaling_proxy, sfu_url): (
        Arc<dyn SfuRoomManager>,
        Option<Arc<dyn SfuSignalingProxy>>,
        String,
    ) = match (
        env::var("LIVEKIT_API_KEY").ok(),
        env::var("LIVEKIT_API_SECRET").ok(),
        env::var("LIVEKIT_HOST").ok(),
    ) {
        (Some(key), Some(secret), Some(host)) => {
            let bridge = LiveKitSfuBridge::from_env(&key, &secret, &host)
                .expect("Failed to create LiveKitSfuBridge — check LIVEKIT_* env vars");
            let public_host = env::var("LIVEKIT_PUBLIC_HOST").unwrap_or_else(|_| host.clone());
            tracing::info!("LiveKit mode: connecting to {host}");
            (
                Arc::new(bridge) as Arc<dyn SfuRoomManager>,
                None,
                public_host,
            )
        }
        _ => {
            #[cfg(debug_assertions)]
            {
                tracing::warn!(
                    "LIVEKIT_API_KEY/LIVEKIT_API_SECRET/LIVEKIT_HOST not all set — \
                     using DevStubSfuBridge (dev proxy mode, not for production)"
                );
                let stub = Arc::new(dev_stub_sfu::DevStubSfuBridge);
                let sfu_url = env::var("SFU_URL").unwrap_or_else(|_| "sfu://localhost".to_string());
                (
                    stub.clone() as Arc<dyn SfuRoomManager>,
                    Some(stub as Arc<dyn SfuSignalingProxy>),
                    sfu_url,
                )
            }
            #[cfg(not(debug_assertions))]
            {
                panic!(
                    "LIVEKIT_API_KEY, LIVEKIT_API_SECRET, and LIVEKIT_HOST must all be set \
                     in release builds — the server cannot start without a real SFU (§6.2)"
                );
            }
        }
    };

    let invite_store = Arc::new(InviteStore::new(InviteStoreConfig::default()));
    let join_rate_limiter = Arc::new(JoinRateLimiter::new(JoinRateLimiterConfig::default()));
    let ip_config = IpConfig::from_env();

    // Fail-closed config validation — reject zero-value security-critical settings
    // before constructing AppState (Req 6.1–6.5).
    let security_config = SecurityConfig {
        global_ws_per_sec: env::var("GLOBAL_WS_UPGRADES_PER_SEC")
            .ok()
            .and_then(|v| v.parse().ok())
            .unwrap_or(100),
        global_joins_per_sec: env::var("GLOBAL_JOINS_PER_SEC")
            .ok()
            .and_then(|v| v.parse().ok())
            .unwrap_or(50),
        invite_ttl_secs: env::var("INVITE_TTL_SECS")
            .ok()
            .and_then(|v| v.parse().ok())
            .unwrap_or(86400),
        token_ttl_secs: env::var("SFU_TOKEN_TTL_SECS")
            .ok()
            .and_then(|v| v.parse().ok())
            .unwrap_or(crate::auth::jwt::DEFAULT_TOKEN_TTL_SECS),
        ban_duration_secs: env::var("TEMP_BAN_DURATION_SECS")
            .ok()
            .and_then(|v| v.parse().ok())
            .unwrap_or(600),
        rate_limit_window_secs: env::var("WS_RATE_LIMIT_WINDOW_SECS")
            .ok()
            .and_then(|v| v.parse().ok())
            .unwrap_or(10),
        bug_report_rate_limit_max: env::var("BUG_REPORT_RATE_LIMIT_MAX")
            .ok()
            .and_then(|v| v.parse().ok())
            .unwrap_or(5),
        bug_report_rate_limit_window_secs: env::var("BUG_REPORT_RATE_LIMIT_WINDOW_SECS")
            .ok()
            .and_then(|v| v.parse().ok())
            .unwrap_or(3600),
        github_bug_report_token_set: !env::var("GITHUB_BUG_REPORT_TOKEN")
            .unwrap_or_default()
            .is_empty(),
        github_bug_report_repo_set: !env::var("GITHUB_BUG_REPORT_REPO")
            .unwrap_or_default()
            .is_empty(),
    };
    if let Err(msg) = validate_security_config(&security_config) {
        eprintln!("FATAL: {msg}");
        std::process::exit(1);
    }

    // --- Phrase encryption key (AES-256-GCM, base64-encoded in env) ---
    let phrase_encryption_key = match env::var("PHRASE_ENCRYPTION_KEY") {
        Ok(b64) => {
            use base64::Engine;
            let decoded = base64::engine::general_purpose::STANDARD
                .decode(&b64)
                .unwrap_or_else(|e| {
                    eprintln!("FATAL: PHRASE_ENCRYPTION_KEY is not valid base64: {e}");
                    std::process::exit(1);
                });
            if decoded.len() != 32 {
                eprintln!(
                    "FATAL: PHRASE_ENCRYPTION_KEY decoded length must be exactly 32 bytes, got {}",
                    decoded.len()
                );
                std::process::exit(1);
            }
            Arc::new(decoded)
        }
        Err(_) => {
            if cfg!(debug_assertions) {
                // Dev fallback: 32 zero bytes (never use in production)
                Arc::new(vec![0u8; 32])
            } else {
                eprintln!("FATAL: PHRASE_ENCRYPTION_KEY must be set in release builds");
                std::process::exit(1);
            }
        }
    };

    // --- Phrase config (Argon2id parameters) ---
    let phrase_config = Arc::new(phrase::PhraseConfig::default());

    // --- Dummy verifier for timing equalization ---
    let dummy_verifier = Arc::new(phrase::generate_dummy_verifier(&phrase_config));

    // --- Pairing code pepper ---
    let pairing_code_pepper = match env::var("PAIRING_CODE_PEPPER") {
        Ok(s) => s,
        Err(_) => {
            if cfg!(debug_assertions) {
                "dev-pairing-pepper-32-bytes!!XXX".to_string()
            } else {
                eprintln!("FATAL: PAIRING_CODE_PEPPER must be set in release builds");
                std::process::exit(1);
            }
        }
    };
    if pairing_code_pepper.len() < 32 {
        eprintln!("FATAL: PAIRING_CODE_PEPPER must be at least 32 bytes");
        std::process::exit(1);
    }
    let pairing_code_pepper = Arc::new(pairing_code_pepper.into_bytes());

    // --- Recovery rate limiter ---
    let recovery_rate_limiter = Arc::new(RecoveryRateLimiter::new(
        RecoveryRateLimiterConfig::default(),
    ));

    // --- Background cleanup retention config ---
    let pairing_retention_hours: u64 = env::var("PAIRING_RETENTION_HOURS")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(24);
    let revoked_token_retention_days: u64 = env::var("REVOKED_TOKEN_RETENTION_DAYS")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(7);

    // GitHub bug report client — token and repo loaded from env, validated by SecurityConfig.
    let github_bug_report_token = env::var("GITHUB_BUG_REPORT_TOKEN").unwrap_or_default();
    let github_bug_report_repo = env::var("GITHUB_BUG_REPORT_REPO").unwrap_or_default();
    let github_client: Arc<dyn diagnostics::bug_report::GitHubClient> = Arc::new(
        diagnostics::bug_report::RealGitHubClient::new(Sensitive(github_bug_report_token)),
    );

    // LLM client for bug report analysis — optional, degrades gracefully to NoOp.
    let llm_client: Arc<dyn diagnostics::llm_client::LlmClient> =
        match env::var("BUG_REPORT_LLM_API_KEY")
            .ok()
            .filter(|s| !s.is_empty())
        {
            Some(api_key) => {
                let model = env::var("BUG_REPORT_LLM_MODEL")
                    .unwrap_or_else(|_| "claude-sonnet-4-20250514".to_string());
                tracing::info!("Bug report LLM enabled (model: {model})");
                Arc::new(diagnostics::llm_client::RealLlmClient::new(
                    Sensitive(api_key),
                    model,
                ))
            }
            None => {
                tracing::info!("BUG_REPORT_LLM_API_KEY not set — LLM analysis disabled");
                Arc::new(diagnostics::llm_client::NoOpLlmClient)
            }
        };

    let app_state = AppState::new(
        sfu_room_manager,
        sfu_signaling_proxy,
        sfu_url,
        invite_store,
        join_rate_limiter,
        ip_config.clone(),
        jwt_secret,
        jwt_secret_previous,
        jwt_issuer,
        db_pool.clone(),
        auth_jwt_secret,
        auth_jwt_secret_previous,
        auth_rate_limiter,
        refresh_token_ttl_days,
        consumed_token_retention_hours,
        refresh_token_pepper,
        refresh_token_pepper_previous,
        dummy_verifier,
        pairing_code_pepper,
        recovery_rate_limiter,
        phrase_config,
        phrase_encryption_key,
        pairing_retention_hours,
        revoked_token_retention_days,
        github_client,
        github_bug_report_repo,
        llm_client,
    );

    // WSS-only deployment enforcement (Req 3.1–3.5).
    if app_state.require_tls && !ip_config.trust_proxy_headers {
        eprintln!(
            "FATAL: REQUIRE_TLS=true requires TRUST_PROXY_HEADERS=true \
             (backend must sit behind a TLS-terminating reverse proxy)"
        );
        std::process::exit(1);
    }
    if ip_config.trust_proxy_headers && !app_state.require_tls {
        tracing::warn!(
            "TRUST_PROXY_HEADERS is enabled but REQUIRE_TLS is false — \
             signaling traffic may be unencrypted"
        );
    }

    // Perform initial health check on startup — with a 5s timeout so a
    // slow or unreachable SFU cannot block the server from starting.
    {
        let bridge = app_state.sfu_room_manager.clone();
        let health_status = app_state.sfu_health_status.clone();
        let check_result = tokio::time::timeout(
            Duration::from_secs(5),
            bridge.health_check(),
        )
        .await;
        match check_result {
            Ok(Ok(health)) => {
                tracing::info!("SFU initial health check: {:?}", health);
                *health_status.write().await = health;
            }
            Ok(Err(e)) => {
                tracing::warn!("SFU initial health check failed: {e}");
                *health_status.write().await = SfuHealth::Unavailable(e.to_string());
            }
            Err(_) => {
                tracing::warn!("SFU initial health check timed out — starting anyway");
                *health_status.write().await = SfuHealth::Unavailable("startup timeout".to_string());
            }
        }
    }

    // Spawn background health monitoring loop.
    spawn_health_monitor(
        app_state.sfu_room_manager.clone(),
        app_state.sfu_health_status.clone(),
    );

    // Spawn preemptive token refresh monitor.
    spawn_token_refresh_monitor(app_state.clone());

    // Spawn background invite sweep task.
    spawn_invite_sweep(app_state.clone());

    // Spawn background auth token sweep task (Req 4.6).
    spawn_auth_token_sweep(app_state.clone());

    // Spawn background channel invite expiry sweep.
    spawn_channel_invite_sweep(app_state.clone());

    // Spawn background chat message purge sweep.
    spawn_chat_purge_sweep(app_state.clone());

    // Spawn background LiveKit EC2 idle shutdown scheduler.
    spawn_shutdown_scheduler(app_state.clone());

    // Keep debug routes disabled by default. They are helpful locally, risky in production.
    let debug_routes_enabled = env::var("ENABLE_DEBUG_ROUTES")
        .map(|value| matches!(value.as_str(), "1" | "true" | "TRUE" | "yes" | "YES"))
        .unwrap_or(false);

    // Build routes first, then attach shared state once at the end.
    let mut app = Router::new()
        .route("/health", get(health))
        .route("/ws", get(ws_handler))
        .route("/auth/register_device", post(auth::routes::register_device))
        .route("/auth/register", post(auth::routes::register))
        .route("/auth/refresh", post(auth::routes::refresh_token))
        .route("/auth/recover", post(auth::routes::recover))
        .route("/auth/pair/start", post(auth::routes::pair_start))
        .route("/auth/pair/approve", post(auth::routes::pair_approve))
        .route("/auth/pair/finish", post(auth::routes::pair_finish))
        .route("/auth/logout_all", post(auth::routes::logout_all))
        .route("/auth/devices", get(auth::routes::list_devices))
        .route(
            "/auth/devices/{device_id}/revoke",
            post(auth::routes::revoke_device),
        )
        .route("/auth/phrase/rotate", post(auth::routes::rotate_phrase))
        // Channel routes — /channels/join MUST come before /channels/{channel_id}
        .route(
            "/channels",
            post(channel::routes::create_channel).get(channel::routes::list_channels),
        )
        .route("/channels/join", post(channel::routes::join_channel))
        .route(
            "/channels/{channel_id}",
            get(channel::routes::get_channel).delete(channel::routes::delete_channel),
        )
        .route(
            "/channels/{channel_id}/invites",
            post(channel::routes::create_invite).get(channel::routes::list_invites),
        )
        .route(
            "/channels/{channel_id}/invites/{code}",
            delete(channel::routes::revoke_invite),
        )
        .route(
            "/channels/{channel_id}/leave",
            post(channel::routes::leave_channel),
        )
        .route(
            "/channels/{channel_id}/bans",
            get(channel::routes::list_bans),
        )
        .route(
            "/channels/{channel_id}/bans/{user_id}",
            post(channel::routes::ban_member).delete(channel::routes::unban_member),
        )
        .route(
            "/channels/{channel_id}/members/{user_id}/role",
            put(channel::routes::change_role),
        )
        .route(
            "/channels/{channel_id}/voice",
            get(channel::routes::get_voice_status),
        )
        .route("/bug-report", post(diagnostics::routes::submit_bug_report))
        .route(
            "/bug-report/analyze",
            post(diagnostics::routes::analyze_bug_report),
        )
        .route(
            "/bug-report/generate-body",
            post(diagnostics::routes::generate_bug_report_body),
        );

    if debug_routes_enabled {
        app = app.route("/debug/rooms", get(debug_rooms));
    }

    #[cfg(feature = "test-metrics")]
    {
        app = app.route(
            "/test/reset_rate_limits",
            axum::routing::post(diagnostics::test_metrics::reset_rate_limits_handler),
        );
    }

    let app = app
        .layer(axum::middleware::from_fn(verify_origin))
        .with_state(app_state.clone());

    let port: u16 = env::var("PORT")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(3000);

    let listener = tokio::net::TcpListener::bind(format!("0.0.0.0:{port}")).await?;

    #[cfg(feature = "test-metrics")]
    {
        use diagnostics::test_metrics::reset_rate_limits_handler;
        use diagnostics::test_metrics::test_metrics_handler;

        let admin_port: u16 = env::var("TEST_METRICS_PORT")
            .ok()
            .and_then(|v| v.parse().ok())
            .unwrap_or(port + 1);

        let admin_listener =
            tokio::net::TcpListener::bind(format!("127.0.0.1:{admin_port}")).await?;

        tracing::info!("Admin metrics listener on 127.0.0.1:{}", admin_port);

        let admin_app = Router::new()
            .route("/test/metrics", get(test_metrics_handler))
            .route(
                "/test/reset_rate_limits",
                axum::routing::post(reset_rate_limits_handler),
            )
            .with_state(app_state);

        tokio::spawn(async move {
            axum::serve(admin_listener, admin_app)
                .await
                .expect("admin metrics listener failed");
        });
    }

    axum::serve(
        listener,
        app.into_make_service_with_connect_info::<SocketAddr>(),
    )
    .await?;
    Ok(())
}

/// Spawn a background task that polls SFU health at a configurable interval.
/// Default interval: 30 seconds, overridden by `SFU_HEALTH_CHECK_INTERVAL_SECS`.
pub fn spawn_health_monitor(
    bridge: Arc<dyn SfuRoomManager>,
    health_status: Arc<tokio::sync::RwLock<SfuHealth>>,
) {
    let interval_secs = env::var("SFU_HEALTH_CHECK_INTERVAL_SECS")
        .ok()
        .and_then(|v| v.parse::<u64>().ok())
        .unwrap_or(30);

    tokio::spawn(async move {
        let mut interval = tokio::time::interval(Duration::from_secs(interval_secs));
        // Skip the first tick (fires immediately) — startup check already done.
        interval.tick().await;

        loop {
            interval.tick().await;

            let new_health = match bridge.health_check().await {
                Ok(h) => h,
                Err(e) => {
                    tracing::warn!("SFU health check failed: {e}");
                    SfuHealth::Unavailable(e.to_string())
                }
            };

            let mut guard = health_status.write().await;
            let old = guard.clone();

            if let SfuHealth::Starting { since } = &old {
                match &new_health {
                    SfuHealth::Available => {
                        tracing::info!(
                            "SFU transitioned Starting -> Available after {:.1}s",
                            since.elapsed().as_secs_f64()
                        );
                        *guard = SfuHealth::Available;
                    }
                    _ if since.elapsed() > Duration::from_secs(300) => {
                        tracing::error!("SFU cold start timed out after 5 minutes");
                        *guard = SfuHealth::Unavailable("EC2 start timed out".to_string());
                    }
                    _ => {
                        tracing::debug!("SFU health check: still cold-starting");
                    }
                }
                continue;
            }

            match (&old, &new_health) {
                (SfuHealth::Available, SfuHealth::Unavailable(reason)) => {
                    tracing::warn!(
                        "SFU transitioned Available → Unavailable: {reason}. \
                         Rejecting new multi-party joins; existing rooms unaffected."
                    );
                }
                (SfuHealth::Unavailable(_), SfuHealth::Available) => {
                    tracing::info!("SFU transitioned Unavailable → Available. Resuming joins.");
                }
                _ => {
                    tracing::debug!("SFU health check: {:?}", new_health);
                }
            }

            *guard = new_health;
        }
    });
}

pub fn spawn_shutdown_scheduler(app_state: AppState) {
    if app_state.ec2_controller.is_none() {
        return;
    }

    tracing::info!("Shutdown scheduler started");

    tokio::spawn(async move {
        loop {
            let wait = duration_until_next_1am_costa_rica();
            tokio::time::sleep(wait).await;

            {
                let health = app_state.sfu_health_status.read().await;
                if matches!(*health, SfuHealth::Starting { .. }) {
                    tracing::info!(
                        "Shutdown scheduler: EC2 is starting; skipping until next day"
                    );
                    continue;
                }
            }

            let active = app_state.room_state.active_room_count();
            if active == 0 {
                trigger_ec2_stop(&app_state).await;
            } else {
                tracing::info!(
                    "Shutdown scheduler: {active} room(s) active; setting pending_shutdown"
                );
                app_state.pending_shutdown.store(true, Ordering::Release);
            }
        }
    });
}

/// Returns the duration from now until the next 01:00:00 Costa Rica time (UTC-6).
///
/// Costa Rica does not observe daylight saving time, so 01:00 Costa Rica is 07:00 UTC.
/// If it is currently before 07:00 UTC, the next 01:00 Costa Rica is today.
/// If it is at or after 07:00 UTC, the next 01:00 Costa Rica is tomorrow.
/// The "at exactly 07:00 UTC" case is handled by using `>=`, so the scheduler
/// always waits at least 24 hours minus a small margin before firing again.
fn duration_until_next_1am_costa_rica() -> Duration {
    use chrono::{Duration as ChronoDuration, Utc};

    let now = Utc::now();
    let today_1am_costa_rica = now
        .date_naive()
        .and_hms_opt(7, 0, 0)
        .expect("valid time")
        .and_utc();

    let next_1am_costa_rica = if now >= today_1am_costa_rica {
        today_1am_costa_rica + ChronoDuration::days(1)
    } else {
        today_1am_costa_rica
    };

    (next_1am_costa_rica - now)
        .to_std()
        .unwrap_or(Duration::from_secs(3600))
}

// trigger_ec2_stop lives in ec2_control.rs, shared by both the scheduler and
// the last-room-close hook in ws_session.rs.
use ec2_control::trigger_ec2_stop;

/// Spawn a background task that proactively refreshes MediaTokens for participants
/// who have not completed SFU media connection within 75% of the token TTL.
///
/// In proxy mode (dev stub): uses `sign_media_token` with `SFU_JWT_SECRET`,
/// refresh threshold is 75% of TOKEN_TTL_SECS (450s).
///
/// In LiveKit mode (no signaling proxy): uses `sign_livekit_token` with
/// LIVEKIT_API_KEY/SECRET, refresh threshold is 75% of LIVEKIT_TOKEN_TTL_SECS (450s).
pub fn spawn_token_refresh_monitor(app_state: AppState) {
    use crate::auth::jwt::{
        LIVEKIT_TOKEN_TTL_SECS, TOKEN_TTL_SECS, sign_livekit_token, sign_media_token,
    };
    use shared::signaling::{MediaTokenPayload, SignalingMessage};

    const CHECK_INTERVAL_SECS: u64 = 10;

    let is_livekit_mode = app_state.sfu_signaling_proxy.is_none();
    let sfu_url = app_state.sfu_url.clone();

    // Capture credentials for the appropriate mode at spawn time.
    // JWT secret and issuer are read from AppState (centralized, loaded once at startup).
    let livekit_api_key = env::var("LIVEKIT_API_KEY").unwrap_or_default();
    let livekit_api_secret = env::var("LIVEKIT_API_SECRET").unwrap_or_default();
    let jwt_secret = app_state.jwt_secret.as_ref().clone();
    let jwt_issuer = app_state.jwt_issuer.clone();

    let (ttl_secs, refresh_threshold_secs) = if is_livekit_mode {
        let ttl = LIVEKIT_TOKEN_TTL_SECS;
        (ttl, (ttl as f64 * 0.75) as u64) // 1350s
    } else {
        let ttl = TOKEN_TTL_SECS;
        (ttl, (ttl as f64 * 0.75) as u64) // 90s
    };

    tokio::spawn(async move {
        let mut interval = tokio::time::interval(Duration::from_secs(CHECK_INTERVAL_SECS));
        interval.tick().await; // skip immediate first tick

        loop {
            interval.tick().await;

            let rooms = app_state
                .room_state
                .rooms_needing_token_refresh(refresh_threshold_secs);

            for (room_id, _handle, peers) in rooms {
                // Look up display names for LiveKit token refresh
                let participants = if is_livekit_mode {
                    app_state
                        .room_state
                        .get_room_info(&room_id)
                        .map(|info| info.participants)
                        .unwrap_or_default()
                } else {
                    vec![]
                };

                for peer_id in peers {
                    let token_result = if is_livekit_mode {
                        let display_name = participants
                            .iter()
                            .find(|p| p.participant_id == peer_id)
                            .map(|p| p.display_name.as_str())
                            .unwrap_or(&peer_id);
                        sign_livekit_token(
                            &room_id,
                            &peer_id,
                            display_name,
                            &livekit_api_key,
                            &livekit_api_secret,
                            ttl_secs,
                        )
                    } else {
                        sign_media_token(&room_id, &peer_id, &jwt_secret, &jwt_issuer, ttl_secs)
                    };

                    match token_result {
                        Ok(token) => {
                            tracing::info!(
                                "Preemptive token refresh for peer {peer_id} in room {room_id}"
                            );
                            let msg = SignalingMessage::MediaToken(MediaTokenPayload {
                                token,
                                sfu_url: sfu_url.clone(),
                            });
                            app_state.connections.send_to(&peer_id, &msg);
                            app_state.room_state.update_room_info(&room_id, |info| {
                                info.record_token_issued(&peer_id);
                            });
                        }
                        Err(e) => {
                            tracing::error!("Failed to sign refresh token for peer {peer_id}: {e}");
                        }
                    }
                }
            }
        }
    });
}

/// Spawn a background task that periodically sweeps expired invite codes and prunes
/// stale rate limiter entries. Interval is configurable via `INVITE_SWEEP_INTERVAL_SECS`
/// (default: 60 seconds).
pub fn spawn_invite_sweep(app_state: AppState) {
    let interval_secs = env::var("INVITE_SWEEP_INTERVAL_SECS")
        .ok()
        .and_then(|v| v.parse::<u64>().ok())
        .unwrap_or(60);

    tokio::spawn(async move {
        let mut interval = tokio::time::interval(Duration::from_secs(interval_secs));
        // Skip the first tick (fires immediately at spawn time).
        interval.tick().await;

        loop {
            interval.tick().await;

            let now = std::time::Instant::now();
            let removed = app_state.invite_store.sweep_expired(now);
            let pruned = app_state.join_rate_limiter.prune_all(now);
            let bans_pruned = app_state.temp_ban_list.prune_expired();
            if removed > 0 || pruned > 0 || bans_pruned > 0 {
                tracing::debug!(removed, pruned, bans_pruned, "invite sweep complete");
            } else {
                tracing::debug!("invite sweep: nothing to remove");
            }
        }
    });
}
/// Spawn a background task that periodically deletes expired channel invites.
/// Default interval: 1 hour, configurable via CHANNEL_INVITE_SWEEP_INTERVAL_SECS.
pub fn spawn_channel_invite_sweep(app_state: AppState) {
    let interval_secs: u64 = env::var("CHANNEL_INVITE_SWEEP_INTERVAL_SECS")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(3600);

    tokio::spawn(async move {
        let mut interval = tokio::time::interval(Duration::from_secs(interval_secs));
        // Skip the first tick (fires immediately at spawn time).
        interval.tick().await;

        loop {
            interval.tick().await;
            match channel::channel::sweep_expired_invites(&app_state.db_pool).await {
                Ok(count) => {
                    if count > 0 {
                        tracing::info!(count, "swept expired channel invites");
                    } else {
                        tracing::debug!("channel invite sweep: 0 expired");
                    }
                }
                Err(e) => {
                    tracing::error!(error = %e, "channel invite sweep failed");
                }
            }
            // Prune stale channel rate limiter entries
            let pruned = app_state
                .channel_rate_limiter
                .prune_stale(std::time::Instant::now());
            if pruned > 0 {
                tracing::debug!(pruned, "pruned stale channel rate limiter entries");
            }
            // Prune stale bug report rate limiter entries
            let pruned = app_state
                .bug_report_rate_limiter
                .prune_stale(std::time::Instant::now());
            if pruned > 0 {
                tracing::debug!(pruned, "pruned stale bug report rate limiter entries");
            }
        }
    });
}

/// Spawn a background task that periodically sweeps expired and consumed auth tokens,
/// and expired pairing rows.
pub fn spawn_auth_token_sweep(app_state: AppState) {
    let sweep_interval = Duration::from_secs(3600); // 1 hour
    tokio::spawn(async move {
        loop {
            tokio::time::sleep(sweep_interval).await;
            match auth::auth::sweep_expired_tokens(&app_state.db_pool).await {
                Ok(count) if count > 0 => {
                    tracing::info!(count, "swept expired refresh tokens");
                }
                Ok(_) => {}
                Err(e) => {
                    tracing::warn!(error = %e, "failed to sweep expired refresh tokens");
                }
            }
            match auth::auth::sweep_consumed_tokens(
                &app_state.db_pool,
                app_state.consumed_token_retention_hours,
            )
            .await
            {
                Ok(count) if count > 0 => {
                    tracing::info!(count, "swept consumed/revoked refresh tokens");
                }
                Ok(_) => {}
                Err(e) => {
                    tracing::warn!(error = %e, "failed to sweep consumed/revoked refresh tokens");
                }
            }
            // Sweep expired pairing rows (Req 20.1).
            match pairing::sweep_expired_pairings(
                &app_state.db_pool,
                app_state.pairing_retention_hours,
            )
            .await
            {
                Ok(count) if count > 0 => {
                    tracing::info!(count, "swept expired pairing rows");
                }
                Ok(_) => {}
                Err(e) => {
                    tracing::warn!(error = %e, "failed to sweep expired pairing rows");
                }
            }
            // Also prune stale rate limiter entries
            let pruned = app_state
                .auth_rate_limiter
                .prune_stale(std::time::Instant::now());
            if pruned > 0 {
                tracing::debug!(pruned, "pruned stale auth rate limiter entries");
            }
        }
    });
}

/// Spawn a background task that periodically purges expired chat messages.
/// Default interval: 15 minutes, configurable via CHAT_PURGE_INTERVAL_SECS.
/// Default retention: 24 hours, configurable via CHAT_RETENTION_HOURS.
/// Default batch size: 1000, configurable via CHAT_PURGE_BATCH_SIZE.
pub fn spawn_chat_purge_sweep(app_state: AppState) {
    let interval_secs: u64 = env::var("CHAT_PURGE_INTERVAL_SECS")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(900);
    let retention_hours: u64 = env::var("CHAT_RETENTION_HOURS")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(24);
    let batch_size: i64 = env::var("CHAT_PURGE_BATCH_SIZE")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(1000);

    tokio::spawn(async move {
        loop {
            tokio::time::sleep(Duration::from_secs(interval_secs)).await;
            let mut total_deleted: u64 = 0;
            loop {
                match chat_persistence::purge_expired_messages(
                    &app_state.db_pool,
                    retention_hours,
                    batch_size,
                )
                .await
                {
                    Ok(count) => {
                        total_deleted += count;
                        if (count as i64) < batch_size {
                            break;
                        }
                        tokio::task::yield_now().await;
                    }
                    Err(e) => {
                        tracing::warn!(error = %e, "chat purge sweep failed mid-drain");
                        break;
                    }
                }
            }
            if total_deleted > 0 {
                tracing::info!(count = total_deleted, "purged expired chat messages");
            }
        }
    });
}

// Lightweight endpoint used by health checks/load balancers.
// Includes SFU health status.
async fn health(State(app_state): State<AppState>) -> Json<serde_json::Value> {
    let sfu_health = app_state.sfu_health_status.read().await;
    let (sfu_ok, sfu_reason) = match &*sfu_health {
        SfuHealth::Available => (true, None::<String>),
        SfuHealth::Unavailable(reason) => (false, Some(reason.clone())),
        SfuHealth::Starting { since } => (
            false,
            Some(format!(
                "starting ({:.0}s elapsed)",
                since.elapsed().as_secs_f64()
            )),
        ),
    };

    let active_rooms = app_state.room_state.active_room_count();
    let total_participants = app_state.room_state.total_participant_count();

    Json(json!({
        "ok": true,
        "sfu": {
            "available": sfu_ok,
            "reason": sfu_reason,
        },
        "metrics": {
            "active_rooms": active_rooms,
            "total_participants": total_participants,
        }
    }))
}

// Debug endpoint to inspect current in-memory room membership.
async fn debug_rooms(State(app_state): State<AppState>) -> Json<serde_json::Value> {
    let rooms = app_state.room_state.snapshot_rooms();
    Json(json!({ "rooms": rooms }))
}
