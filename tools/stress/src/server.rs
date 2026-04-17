use std::net::SocketAddr;
use std::sync::Arc;

use wavis_backend::app_state::AppState;
use wavis_backend::domain::auth_rate_limiter::{AuthRateLimiter, AuthRateLimiterConfig};
use wavis_backend::domain::bug_report::MockGitHubClient;
use wavis_backend::domain::invite::{InviteStore, InviteStoreConfig};
use wavis_backend::domain::join_rate_limiter::{JoinRateLimiter, JoinRateLimiterConfig};
use wavis_backend::domain::llm_client::NoOpLlmClient;
use wavis_backend::domain::mock_sfu_bridge::MockSfuBridge;
use wavis_backend::domain::sfu_bridge::{SfuRoomManager, SfuSignalingProxy};
use wavis_backend::handlers::ip::IpConfig;
use wavis_backend::handlers::ws::ws_handler;

/// Handle to the in-process backend server.
pub struct InProcessServer {
    pub app_state: AppState,
    pub ws_url: String,
    pub metrics_url: String,
    pub ws_port: u16,
    pub admin_port: u16,
}

/// Start the backend in-process on a dedicated tokio runtime (separate OS thread).
/// Returns an `InProcessServer` with the `AppState` and URLs for the harness to use.
///
/// The backend uses `MockSfuBridge` (no LiveKit required in CI).
/// The `test-metrics` feature must be enabled for this to compile.
pub async fn start_in_process(
    ws_port: u16,
    admin_port: u16,
    metrics_token: &str,
) -> InProcessServer {
    // Set required env vars before constructing AppState so they are picked up
    // by the internal env-var reads inside AppState::new().
    //
    // SAFETY: These are test-only env vars set before any threads read them.
    // We use set_var here because AppState::new() reads them synchronously.
    //
    // NOTE: We unconditionally set REQUIRE_INVITE_CODE and MAX_CONNECTIONS_PER_IP
    // because the shell environment may have stale values from previous runs
    // (e.g. REQUIRE_INVITE_CODE=false from a dev session) that would break
    // stress scenarios. The harness MUST control these values.
    unsafe {
        // Enable invite code requirement â€” most stress scenarios test invite
        // validation, rate limiting, and exhaustion. Scenarios that don't need
        // invite codes create them explicitly via app_state.invite_store.
        std::env::set_var("REQUIRE_INVITE_CODE", "true");
        // Provide a test JWT secret if not already set.
        if std::env::var("SFU_JWT_SECRET").is_err() {
            std::env::set_var("SFU_JWT_SECRET", "stress-test-secret-32-bytes-min!!");
        }
        // Raise per-IP connection cap for stress tests â€” many scenarios spawn
        // 20-100 concurrent clients from 127.0.0.1 which would hit the default
        // cap of 10 and get HTTP 429 before reaching the WebSocket handler.
        std::env::set_var("MAX_CONNECTIONS_PER_IP", "200");
        // Raise global join ceiling so high-concurrency scenarios are not
        // throttled by the global join limiter. The global WS upgrade ceiling
        // is left at the default (100) so message-flood can test it.
        std::env::set_var("GLOBAL_JOINS_PER_SEC", "500");
        // Set the test metrics bearer token.
        std::env::set_var("TEST_METRICS_TOKEN", metrics_token);
    }

    // Build MockSfuBridge â€” no LiveKit required in CI.
    let mock = Arc::new(MockSfuBridge::new());
    let sfu_url = "sfu://localhost".to_string();

    let invite_store = Arc::new(InviteStore::new(InviteStoreConfig::default()));
    let join_rate_limiter = Arc::new(JoinRateLimiter::new(JoinRateLimiterConfig::default()));
    let ip_config = IpConfig::from_env();

    let app_state = AppState::new(
        mock.clone() as Arc<dyn SfuRoomManager>,
        Some(mock as Arc<dyn SfuSignalingProxy>),
        sfu_url,
        invite_store,
        join_rate_limiter,
        ip_config,
        Arc::new(b"dev-secret-32-bytes-minimum!!!XX".to_vec()),
        None,
        "wavis-backend".to_string(),
        // Device-auth fields â€” stress harness does not test auth REST endpoints,
        // so we use a lazy dummy Postgres pool and permissive defaults.
        // Use a real Postgres pool when DATABASE_URL is set, otherwise fall back
        // to a lazy dummy pool (auth scenarios will be skipped).
        {
            match std::env::var("DATABASE_URL") {
                Ok(url) if !url.is_empty() && url != "postgres://dummy" => {
                    eprintln!("[harness] Connecting to real Postgres: {url}");
                    let pool = sqlx::postgres::PgPoolOptions::new()
                        .max_connections(5)
                        .connect(&url)
                        .await
                        .expect("failed to connect to Postgres");
                    // Run migrations so tables exist.
                    sqlx::migrate!("../../wavis-backend/migrations")
                        .run(&pool)
                        .await
                        .expect("database migration failed");
                    eprintln!("[harness] Postgres connected and migrations applied");
                    pool
                }
                _ => {
                    eprintln!(
                        "[harness] No DATABASE_URL — using dummy pool (auth scenarios will skip)"
                    );
                    sqlx::postgres::PgPoolOptions::new()
                        .connect_lazy("postgres://dummy")
                        .unwrap()
                }
            }
        },
        Arc::new(b"stress-auth-secret-at-least-32-bytes!".to_vec()),
        None,
        Arc::new(AuthRateLimiter::new(AuthRateLimiterConfig::default())),
        30, // refresh_token_ttl_days (stress tests use shorter TTL)
        72, // consumed_token_retention_hours
        Arc::new(b"stress-pepper-at-least-32-bytes!!!!!".to_vec()),
        None,
        Arc::new(wavis_backend::domain::phrase::generate_dummy_verifier(
            &wavis_backend::domain::phrase::PhraseConfig::default(),
        )),
        Arc::new(b"stress-pairing-pepper-32-bytes!X".to_vec()),
        Arc::new(
            wavis_backend::domain::recovery_rate_limiter::RecoveryRateLimiter::new(
                wavis_backend::domain::recovery_rate_limiter::RecoveryRateLimiterConfig::default(),
            ),
        ),
        Arc::new(wavis_backend::domain::phrase::PhraseConfig::default()),
        Arc::new(vec![0u8; 32]),
        24,
        7,
        Arc::new(MockGitHubClient::new()),
        "owner/test-repo".to_string(),
        Arc::new(NoOpLlmClient),
    );

    // Mark SFU as available â€” MockSfuBridge is always ready, but AppState
    // initialises sfu_health_status to Unavailable("not yet checked").
    // Without this, any join with room_type="sfu" gets "SFU unavailable".
    {
        let mut health = app_state.sfu_health_status.write().await;
        *health = wavis_backend::domain::sfu_bridge::SfuHealth::Available;
    }

    // Debug: verify critical config values are set correctly.
    eprintln!(
        "[harness] require_invite_code={}, ip_connection_tracker.max_per_ip={}",
        app_state.require_invite_code,
        app_state.ip_connection_tracker.max_per_ip(),
    );

    // Clone app_state for the server thread â€” AppState is Arc-based and Clone.
    let server_app_state = app_state.clone();

    // Bind listeners on the calling (harness) runtime before handing off to the
    // server thread, so we can propagate bind errors immediately.
    let ws_listener = tokio::net::TcpListener::bind(format!("127.0.0.1:{ws_port}"))
        .await
        .unwrap_or_else(|e| panic!("Failed to bind WS listener on 127.0.0.1:{ws_port}: {e}"));

    let admin_listener = tokio::net::TcpListener::bind(format!("127.0.0.1:{admin_port}"))
        .await
        .unwrap_or_else(|e| panic!("Failed to bind admin listener on 127.0.0.1:{admin_port}: {e}"));

    // Spawn the backend on a dedicated multi-thread runtime (separate OS thread).
    // This prevents harness task pressure from starving the backend's event loop.
    std::thread::spawn(move || {
        let server_runtime = tokio::runtime::Builder::new_multi_thread()
            .worker_threads(4)
            .enable_all()
            .build()
            .expect("failed to build server runtime");

        server_runtime.block_on(async move {
            // Build the main WS router.
            let app = axum::Router::new()
                .route("/ws", axum::routing::get(ws_handler))
                .with_state(server_app_state.clone())
                .into_make_service_with_connect_info::<SocketAddr>();

            // Spawn the admin metrics listener (always enabled â€” wavis-backend dep
            // is always compiled with the test-metrics feature in this crate).
            {
                use wavis_backend::handlers::test_metrics::reset_rate_limits_handler;
                use wavis_backend::handlers::test_metrics::test_metrics_handler;

                let admin_app = axum::Router::new()
                    .route("/test/metrics", axum::routing::get(test_metrics_handler))
                    .route(
                        "/test/reset_rate_limits",
                        axum::routing::post(reset_rate_limits_handler),
                    )
                    .with_state(server_app_state);

                tokio::spawn(async move {
                    axum::serve(admin_listener, admin_app)
                        .await
                        .expect("admin metrics listener failed");
                });
            }

            axum::serve(ws_listener, app)
                .await
                .expect("WS server failed");
        });
    });

    // Give the server a moment to start accepting connections.
    tokio::time::sleep(std::time::Duration::from_millis(100)).await;

    InProcessServer {
        app_state,
        ws_url: format!("ws://127.0.0.1:{ws_port}/ws"),
        metrics_url: format!("http://127.0.0.1:{admin_port}/test/metrics"),
        ws_port,
        admin_port,
    }
}
