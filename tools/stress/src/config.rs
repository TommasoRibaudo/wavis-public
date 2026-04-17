use std::time::Duration;

use rand_chacha::ChaChaRng;
use reqwest::Client;
use wavis_backend::app_state::AppState;

use crate::log_capture::LogCapture;
use crate::process_stats::ProcessStatsCollector;

/// Latency thresholds for pass/fail assertions.
pub struct LatencyThresholds {
    /// p95 join response latency (Requirement 2.9: must be < 200ms)
    pub join_p95: Duration,
    /// p99 join response latency
    pub join_p99: Duration,
    /// p95 message round-trip latency
    pub message_p95: Duration,
    /// p99 message round-trip latency
    pub message_p99: Duration,
    /// p95 latency for healthy connections while a flood is in progress (Requirement 7.5: < 500ms)
    pub flood_healthy_p95: Duration,
}

/// Scale and threshold configuration for a test run.
pub struct ScaleConfig {
    /// Number of concurrent WebSocket clients to spawn.
    pub concurrent_clients: usize,
    /// Number of actions each client performs.
    pub actions_per_client: usize,
    /// Total actions across all clients.
    pub total_actions: usize,
    /// Maximum allowed RSS growth percentage (default: 10.0).
    pub rss_growth_threshold_pct: f64,
    /// CPU spike threshold as a multiplier over idle baseline (default: 2.0).
    pub cpu_spike_threshold_x: f64,
    /// Maximum duration (seconds) a CPU spike may last before failing (default: 30).
    pub cpu_spike_max_duration_secs: u64,
    /// Latency thresholds for assertions.
    pub thresholds: LatencyThresholds,
    /// Number of repetitions for Tier 1 race scenarios (CI: 3, Local: 20).
    pub repetitions: usize,
}

impl ScaleConfig {
    /// CI mode: reduced scale for fast pipeline execution.
    /// 100 concurrent clients, 50 actions each, 5 000 total, 3 repetitions.
    ///
    /// Repetitions were reduced from 5 to 3 after the scenario count grew from
    /// 13 to ~16 runnable scenarios (25 registered, 9 skipped for missing
    /// capabilities). 3 reps still surface race conditions while keeping the
    /// total wall-clock time under the CI timeout.
    pub fn ci() -> Self {
        Self {
            concurrent_clients: 100,
            actions_per_client: 50,
            total_actions: 5000,
            rss_growth_threshold_pct: 10.0,
            cpu_spike_threshold_x: 2.0,
            cpu_spike_max_duration_secs: 30,
            thresholds: LatencyThresholds {
                join_p95: Duration::from_millis(200),
                join_p99: Duration::from_millis(500),
                message_p95: Duration::from_millis(500),
                message_p99: Duration::from_millis(1000),
                flood_healthy_p95: Duration::from_millis(500),
            },
            repetitions: 3,
        }
    }

    /// Local mode: larger scale for thorough local validation.
    /// 1 000 concurrent clients, 200 actions each, 20 000 total, 20 repetitions.
    pub fn local() -> Self {
        Self {
            concurrent_clients: 1000,
            actions_per_client: 200,
            total_actions: 20000,
            repetitions: 20,
            ..Self::ci()
        }
    }
}

/// Backend capabilities that may or may not be available depending on build configuration.
/// Scenarios declare which capabilities they require; missing ones cause the scenario to be
/// skipped rather than failed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Capability {
    Sfu,
    ScreenShare,
    TokenRevocation,
    P2P,
}

/// Per-scenario backend configuration preset.
///
/// - `Default`: real production rate limits (Tier 2 scenarios that validate actual defenses).
/// - `JoinHeavy`: relaxed join rate limits only (Tier 1 capacity/atomicity scenarios).
/// - `BruteForce`: real rate limits (brute-force scenarios that test rate limiter behaviour).
/// - `Slowloris`: real rate limits + lowered per-IP connection cap (10) for per-IP cap testing.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ConfigPreset {
    Default,
    JoinHeavy,
    BruteForce,
    Slowloris,
}

/// Shared context passed to every scenario.
pub struct TestContext {
    /// WebSocket URL of the backend, e.g. `"ws://127.0.0.1:3000/ws"`.
    pub ws_url: String,
    /// Test metrics endpoint URL, e.g. `"http://127.0.0.1:3001/test/metrics"`.
    pub metrics_url: String,
    /// Shared HTTP client for querying the metrics endpoint.
    pub http_client: Client,
    /// Scale and threshold configuration for this run.
    pub scale: ScaleConfig,
    /// In-process backend state when running in CI in-process mode; `None` for external mode.
    pub app_state: Option<AppState>,
    /// RNG seed for deterministic reproducibility (printed at run start).
    pub rng_seed: u64,
    /// Seeded ChaCha RNG for deterministic random data in scenarios.
    pub rng: std::sync::Mutex<ChaChaRng>,
    /// Backend capabilities probed at startup; used to gate scenario execution.
    pub capabilities: Vec<Capability>,
    /// Backend process stats collector for RSS/CPU assertions (Linux only).
    pub process_stats: Option<ProcessStatsCollector>,
    /// Log capture handle for in-process mode log-leak detection.
    /// `None` when running against an external backend.
    pub log_capture: Option<LogCapture>,
}
