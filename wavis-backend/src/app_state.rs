//! Shared application state injected into all Axum handlers.
//!
//! **Owns:** the `AppState` struct that holds every piece of server-wide
//! shared state: room state, connection registry, rate limiters, invite
//! store, SFU bridge handles, JWT secrets, database pool, abuse-protection
//! structures, and configuration values loaded once at startup.
//!
//! **Does not own:** business logic or request handling. `AppState` is a
//! data holder and configuration surface — mutations flow through domain
//! functions that receive the relevant `Arc`-wrapped fields.
//!
//! **Key invariants:**
//! - All configuration derived from environment variables is read once in
//!   `AppState::new()` and never re-read at request time.
//! - Lock ordering must be respected across all state:
//!   `active_room_map` (0) → `rooms` (1) → per-room (2) → `peer_to_room` (3).
//!   Violating this order risks deadlocks.
//! - Security-critical secrets (JWT keys, peppers) are validated at startup;
//!   missing or weak values panic to fail closed (§6.2).
//! - `AppState` is `Clone` (all fields are `Arc`-wrapped or `Copy`), so
//!   Axum can inject it into concurrent handlers cheaply.
//!
//! **Layering:** infrastructure layer. Constructed in `main`, consumed by
//! handlers via `State<Arc<AppState>>`. Domain functions receive individual
//! fields, not the full `AppState`.

use crate::abuse::abuse_metrics::{AbuseMetrics, IpFailedJoinTracker};
use crate::abuse::global_rate_limiter::GlobalRateLimiter;
use crate::abuse::ip_tracker::IpConnectionTracker;
use crate::abuse::join_rate_limiter::JoinRateLimiter;
use crate::abuse::temp_ban::{TempBanConfig, TempBanList};
use crate::auth::auth_rate_limiter::AuthRateLimiter;
use crate::auth::phrase;
use crate::auth::recovery_rate_limiter::RecoveryRateLimiter;
use crate::channel::channel_rate_limiter::{ChannelRateLimiter, ChannelRateLimiterConfig};
use crate::channel::invite::InviteStore;
use crate::connections::LiveConnections;
use crate::diagnostics::bug_report::GitHubClient;
use crate::diagnostics::bug_report_rate_limiter::{
    BugReportRateLimiter, BugReportRateLimiterConfig,
};
use crate::diagnostics::llm_client::LlmClient;
use crate::ec2_control::Ec2InstanceController;
use crate::ip::IpConfig;
use crate::state::InMemoryRoomState;
use crate::voice::sfu_bridge::{SfuHealth, SfuRoomManager, SfuSignalingProxy};
use crate::voice::turn_cred::TurnConfig;
use crate::ws::ws_rate_limit::WsRateLimitConfig;
use std::collections::HashMap;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use tokio::sync::RwLock;
use uuid::Uuid;

/// Thread-safe mapping from Channel_ID to the currently active Room_ID.
/// A Channel has at most one active Room at any time.
///
/// Lock ordering: active_room_map (0) → rooms (1) → per-room (2) → peer_to_room (3)
/// No room-level locks shall be held when acquiring this lock.
/// Cleanup (last-leave) acquires this lock only after all room locks are released.
pub type ActiveRoomMap = Arc<RwLock<HashMap<Uuid, String>>>;

// Shared application state injected into Axum handlers.
// Arc is used so many concurrent websocket tasks can read/write safely.
#[derive(Clone)]
pub struct AppState {
    pub room_state: Arc<InMemoryRoomState>,
    pub connections: Arc<LiveConnections>,
    // Monotonic counter for per-connection peer IDs (peer-1, peer-2, ...).
    next_peer_id: Arc<AtomicU64>,
    /// Current SFU health status, updated by the background health monitor.
    pub sfu_health_status: Arc<RwLock<SfuHealth>>,
    /// Room lifecycle operations (always present).
    pub sfu_room_manager: Arc<dyn SfuRoomManager>,
    /// SDP/ICE proxy operations (None for LiveKit — clients connect directly).
    pub sfu_signaling_proxy: Option<Arc<dyn SfuSignalingProxy>>,
    /// SFU server URL sent to clients in MediaToken payloads.
    pub sfu_url: String,
    /// Optional EC2 controller for the LiveKit instance. Absent in local dev.
    pub ec2_controller: Option<Arc<Ec2InstanceController>>,
    /// Set when idle shutdown is deferred until the last active room closes.
    pub pending_shutdown: Arc<AtomicBool>,
    /// Invite code store — tracks active invite codes and enforces limits.
    pub invite_store: Arc<InviteStore>,
    /// Multi-dimensional join rate limiter.
    pub join_rate_limiter: Arc<JoinRateLimiter>,
    /// IP extraction configuration (proxy header trust).
    pub ip_config: IpConfig,
    /// When true, the join handler requires a valid invite code.
    /// Read from `REQUIRE_INVITE_CODE` env var at startup; stored here so tests
    /// can override per-server without global env var races.
    pub require_invite_code: bool,
    /// Abuse metrics counters — shared across all connections.
    pub abuse_metrics: Arc<AbuseMetrics>,
    /// Temporary IP ban list — checked at upgrade time.
    pub temp_ban_list: Arc<TempBanList>,
    /// Per-IP connection tracker — checked at upgrade time.
    pub ip_connection_tracker: Arc<IpConnectionTracker>,
    /// WS rate limit config — read once at startup, cloned into each connection.
    pub ws_rate_limit_config: WsRateLimitConfig,
    /// TURN credential config — None if TURN_SHARED_SECRET not set.
    pub turn_config: Option<Arc<TurnConfig>>,
    /// Global ceiling on WebSocket upgrade requests per second.
    pub global_ws_limiter: Arc<GlobalRateLimiter>,
    /// Global ceiling on join attempts per second.
    pub global_join_limiter: Arc<GlobalRateLimiter>,
    /// Centralized JWT signing secret (loaded once at startup, ≥ 32 bytes).
    pub jwt_secret: Arc<Vec<u8>>,
    /// Previous JWT signing secret for zero-downtime key rotation.
    /// `None` when no rotation is in progress.
    pub jwt_secret_previous: Option<Arc<Vec<u8>>>,
    /// JWT issuer claim for MediaTokens.
    pub jwt_issuer: String,
    /// When true, enforce TLS termination (REQUIRE_TLS env var).
    /// Default: false in debug, true in release.
    pub require_tls: bool,
    /// Per-IP failed join tracker — detects abuse patterns.
    pub ip_failed_join_tracker: Arc<IpFailedJoinTracker>,
    /// Postgres connection pool for device-auth persistence.
    pub db_pool: sqlx::PgPool,
    /// JWT signing secret for device-auth access tokens (separate from SFU JWT secret).
    pub auth_jwt_secret: Arc<Vec<u8>>,
    /// Previous auth JWT secret for zero-downtime key rotation.
    pub auth_jwt_secret_previous: Option<Arc<Vec<u8>>>,
    /// Per-IP rate limiter for auth REST endpoints (register + refresh).
    pub auth_rate_limiter: Arc<AuthRateLimiter>,
    /// Refresh token TTL in days (validated at startup: 1..=365).
    pub refresh_token_ttl_days: u32,
    /// How long consumed token hashes are retained for reuse detection (hours).
    pub consumed_token_retention_hours: u64,
    /// Per-user rate limiter for channel REST endpoints.
    pub channel_rate_limiter: Arc<ChannelRateLimiter>,
    /// HMAC pepper for refresh token hashing. Provides defense-in-depth
    /// against DB leaks — attacker cannot brute-force hashes without the pepper.
    pub refresh_token_pepper: Arc<Vec<u8>>,
    /// Previous pepper for zero-downtime pepper rotation.
    pub refresh_token_pepper_previous: Option<Arc<Vec<u8>>>,
    /// Channel_ID → active Room_ID mapping. A Channel has at most one active Room.
    /// Lock ordering position 0: active_room_map (0) → rooms (1) → per-room (2) → peer_to_room (3).
    pub active_room_map: ActiveRoomMap,
    /// Pre-computed dummy Argon2id verifier for timing equalization on unknown recovery IDs.
    pub dummy_verifier: Arc<phrase::DummyVerifier>,
    /// HMAC pepper for pairing code hashing (separate from refresh token pepper).
    pub pairing_code_pepper: Arc<Vec<u8>>,
    /// Recovery-specific rate limiter (per-IP + per-recovery_id).
    pub recovery_rate_limiter: Arc<RecoveryRateLimiter>,
    /// Per-IP + per-user_id rate limiter for bug report submissions.
    pub bug_report_rate_limiter: Arc<BugReportRateLimiter>,
    /// GitHub API client for bug report issue creation + screenshot upload.
    pub github_client: Arc<dyn GitHubClient>,
    /// Target GitHub repository in `owner/repo` format for bug reports.
    pub github_bug_report_repo: String,
    /// LLM client for bug report analysis (server-side, developer-provided API key).
    pub llm_client: Arc<dyn LlmClient>,
    /// Argon2id configuration for phrase hashing.
    pub phrase_config: Arc<phrase::PhraseConfig>,
    /// AES-256-GCM encryption key for phrase_salt and phrase_verifier at-rest encryption.
    pub phrase_encryption_key: Arc<Vec<u8>>,
    /// Pairing row retention hours for background cleanup (default 24).
    pub pairing_retention_hours: u64,
    /// Consumed/revoked refresh token retention days for background cleanup (default 7).
    pub revoked_token_retention_days: u64,
    /// Bearer token for the test-metrics admin endpoint.
    /// Read once from `TEST_METRICS_TOKEN` at startup to avoid env var races in parallel tests.
    #[cfg(feature = "test-metrics")]
    pub metrics_token: String,
}

impl AppState {
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        sfu_room_manager: Arc<dyn SfuRoomManager>,
        sfu_signaling_proxy: Option<Arc<dyn SfuSignalingProxy>>,
        sfu_url: String,
        invite_store: Arc<InviteStore>,
        join_rate_limiter: Arc<JoinRateLimiter>,
        ip_config: IpConfig,
        jwt_secret: Arc<Vec<u8>>,
        jwt_secret_previous: Option<Arc<Vec<u8>>>,
        jwt_issuer: String,
        db_pool: sqlx::PgPool,
        auth_jwt_secret: Arc<Vec<u8>>,
        auth_jwt_secret_previous: Option<Arc<Vec<u8>>>,
        auth_rate_limiter: Arc<AuthRateLimiter>,
        refresh_token_ttl_days: u32,
        consumed_token_retention_hours: u64,
        refresh_token_pepper: Arc<Vec<u8>>,
        refresh_token_pepper_previous: Option<Arc<Vec<u8>>>,
        dummy_verifier: Arc<phrase::DummyVerifier>,
        pairing_code_pepper: Arc<Vec<u8>>,
        recovery_rate_limiter: Arc<RecoveryRateLimiter>,
        phrase_config: Arc<phrase::PhraseConfig>,
        phrase_encryption_key: Arc<Vec<u8>>,
        pairing_retention_hours: u64,
        revoked_token_retention_days: u64,
        github_client: Arc<dyn GitHubClient>,
        github_bug_report_repo: String,
        llm_client: Arc<dyn LlmClient>,
    ) -> Self {
        let require_invite_code = std::env::var("REQUIRE_INVITE_CODE")
            .map(|v| !v.eq_ignore_ascii_case("false") && v != "0")
            .unwrap_or(true);

        let require_tls = std::env::var("REQUIRE_TLS")
            .map(|v| v.eq_ignore_ascii_case("true") || v == "1")
            .unwrap_or_else(|_| !cfg!(debug_assertions));
        let max_connections_per_ip = std::env::var("MAX_CONNECTIONS_PER_IP")
            .ok()
            .and_then(|v| v.parse::<u32>().ok())
            .unwrap_or(10);

        // TURN config — Ok(None) if not configured, panic on bad config
        let turn_config = match TurnConfig::try_from_env() {
            Ok(Some(cfg)) => {
                tracing::info!(
                    "TURN credentials enabled (TTL={}s)",
                    cfg.credential_ttl_secs
                );
                Some(Arc::new(cfg))
            }
            Ok(None) => {
                tracing::info!("TURN_SHARED_SECRET not set — TURN credentials disabled");
                None
            }
            Err(e) => panic!("Invalid TURN configuration: {e}"),
        };

        let global_ws_per_sec = std::env::var("GLOBAL_WS_UPGRADES_PER_SEC")
            .ok()
            .and_then(|v| v.parse::<u32>().ok())
            .unwrap_or(100);
        let global_join_per_sec = std::env::var("GLOBAL_JOINS_PER_SEC")
            .ok()
            .and_then(|v| v.parse::<u32>().ok())
            .unwrap_or(50);

        Self {
            room_state: Arc::new(InMemoryRoomState::new()),
            connections: Arc::new(LiveConnections::new()),
            next_peer_id: Arc::new(AtomicU64::new(1)),
            sfu_health_status: Arc::new(RwLock::new(SfuHealth::Unavailable(
                "not yet checked".to_string(),
            ))),
            sfu_room_manager,
            sfu_signaling_proxy,
            sfu_url,
            ec2_controller: std::env::var("LIVEKIT_EC2_INSTANCE_ID")
                .ok()
                .map(|id| Arc::new(Ec2InstanceController::new(id))),
            pending_shutdown: Arc::new(AtomicBool::new(false)),
            invite_store,
            join_rate_limiter,
            ip_config,
            require_invite_code,
            abuse_metrics: Arc::new(AbuseMetrics::new()),
            temp_ban_list: Arc::new(TempBanList::new(TempBanConfig::from_env())),
            ip_connection_tracker: Arc::new(IpConnectionTracker::new(max_connections_per_ip)),
            ws_rate_limit_config: WsRateLimitConfig::from_env(),
            turn_config,
            global_ws_limiter: Arc::new(GlobalRateLimiter::new(global_ws_per_sec)),
            global_join_limiter: Arc::new(GlobalRateLimiter::new(global_join_per_sec)),
            jwt_secret,
            jwt_secret_previous,
            jwt_issuer,
            require_tls,
            ip_failed_join_tracker: Arc::new(IpFailedJoinTracker::default()),
            db_pool,
            auth_jwt_secret,
            auth_jwt_secret_previous,
            auth_rate_limiter,
            refresh_token_ttl_days,
            consumed_token_retention_hours,
            channel_rate_limiter: Arc::new(ChannelRateLimiter::new(ChannelRateLimiterConfig {
                max_per_user: std::env::var("CHANNEL_RATE_LIMIT_MAX")
                    .ok()
                    .and_then(|v| v.parse().ok())
                    .unwrap_or(30),
                window_secs: std::env::var("CHANNEL_RATE_LIMIT_WINDOW_SECS")
                    .ok()
                    .and_then(|v| v.parse().ok())
                    .unwrap_or(60),
            })),
            bug_report_rate_limiter: Arc::new(BugReportRateLimiter::new(
                BugReportRateLimiterConfig::from_env(),
            )),
            github_client,
            github_bug_report_repo,
            llm_client,
            refresh_token_pepper,
            refresh_token_pepper_previous,
            active_room_map: Arc::new(RwLock::new(HashMap::new())),
            dummy_verifier,
            pairing_code_pepper,
            recovery_rate_limiter,
            phrase_config,
            phrase_encryption_key,
            pairing_retention_hours,
            revoked_token_retention_days,
            #[cfg(feature = "test-metrics")]
            metrics_token: std::env::var("TEST_METRICS_TOKEN").unwrap_or_default(),
        }
    }

    // Generate a unique ID for each websocket connection session.
    // This ID is ephemeral (changes after reconnect).
    pub fn next_peer_id(&self) -> String {
        let id = self.next_peer_id.fetch_add(1, Ordering::Relaxed);
        format!("peer-{id}")
    }

    /// Returns true if the SFU is currently available for new multi-party joins.
    pub async fn is_sfu_available(&self) -> bool {
        is_join_allowed(&*self.sfu_health_status.read().await)
    }
}

/// Pure helper: returns true when the given health status allows new joins.
/// Extracted for easy property-based testing.
pub fn is_join_allowed(health: &SfuHealth) -> bool {
    matches!(health, SfuHealth::Available)
}

#[cfg(test)]
mod tests {
    use super::*;
    use proptest::prelude::*;

    // --- Property 5: SFU health state transitions ---
    // Feature: sfu-multi-party-voice, Property 5: SFU health state transitions
    // Validates: Requirements 1.4, 1.5

    /// Generate an arbitrary SfuHealth value.
    fn arb_health() -> impl Strategy<Value = SfuHealth> {
        prop_oneof![
            Just(SfuHealth::Available),
            "[a-z ]{1,20}".prop_map(SfuHealth::Unavailable),
            Just(SfuHealth::Starting {
                since: std::time::Instant::now(),
            }),
        ]
    }

    proptest! {
        #![proptest_config(ProptestConfig::with_cases(200))]

        /// For any sequence of health values, joins are accepted only when the
        /// latest status is Available; transitions preserve the invariant.
        #[test]
        fn prop_join_allowed_only_when_available(
            health_sequence in prop::collection::vec(arb_health(), 1..=20),
        ) {
            for health in &health_sequence {
                let allowed = is_join_allowed(health);
                match health {
                    SfuHealth::Available => {
                        prop_assert!(allowed, "Available must allow joins");
                    }
                    SfuHealth::Unavailable(_) => {
                        prop_assert!(!allowed, "Unavailable must reject joins");
                    }
                    SfuHealth::Starting { .. } => {
                        prop_assert!(!allowed, "Starting must reject joins");
                    }
                }
            }

            // The final state determines current join acceptance
            let last = health_sequence.last().unwrap();
            let final_allowed = is_join_allowed(last);
            match last {
                SfuHealth::Available => prop_assert!(final_allowed),
                SfuHealth::Unavailable(_) => prop_assert!(!final_allowed),
                SfuHealth::Starting { .. } => prop_assert!(!final_allowed),
            }
        }

        /// Transitions: Available → Unavailable rejects joins; Unavailable → Available resumes.
        #[test]
        fn prop_transitions_preserve_existing_rooms(
            reasons in prop::collection::vec("[a-z]{1,10}", 1..=10),
        ) {
            // Simulate a sequence of transitions and verify join acceptance at each step
            let mut current = SfuHealth::Available;
            prop_assert!(is_join_allowed(&current), "start Available allows joins");

            for reason in &reasons {
                // Transition to Unavailable
                current = SfuHealth::Unavailable(reason.clone());
                prop_assert!(!is_join_allowed(&current), "Unavailable rejects joins");

                // Transition back to Available
                current = SfuHealth::Available;
                prop_assert!(is_join_allowed(&current), "Available resumes joins");
            }
        }
    }
}
