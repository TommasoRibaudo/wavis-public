use async_trait::async_trait;

use crate::assertions::fetch_metrics;
use crate::config::{Capability, ConfigPreset, TestContext};
use crate::results::{InvariantViolation, ScenarioResult};

/// Probe the backend to determine which capabilities are available.
/// Returns a `Vec<Capability>` of available capabilities.
///
/// Strategy:
/// - Query `GET /test/metrics` with bearer token — if it responds, backend has test-metrics.
/// - Check if any room snapshot contains an `active_shares` field to infer
///   `Capability::Sfu` (SFU-specific field).
/// - `Capability::P2P` is always assumed available.
/// - `Capability::TokenRevocation` and `Capability::ScreenShare` are inferred from SFU
///   availability (they require SFU room support).
/// - If `--enable-sfu-tests` is set, all SFU capabilities are added regardless of probe result.
pub async fn probe_capabilities(
    metrics_url: &str,
    metrics_token: &str,
    enable_sfu_tests: bool,
) -> Vec<Capability> {
    let mut caps = vec![Capability::P2P];

    // Attempt to reach the test metrics endpoint.
    let client = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(5))
        .build()
        .unwrap_or_default();

    let response = client
        .get(metrics_url)
        .header("Authorization", format!("Bearer {metrics_token}"))
        .send()
        .await;

    let sfu_detected = match response {
        Ok(resp) if resp.status().is_success() => {
            // Parse the JSON to check for SFU availability.
            match resp.json::<serde_json::Value>().await {
                Ok(json) => {
                    // Primary: check the explicit sfu_available field.
                    json.get("sfu_available")
                        .and_then(|v| v.as_bool())
                        .unwrap_or_else(|| {
                            // Fallback: if any room snapshot has an `active_shares` key
                            // the backend was compiled with SFU support.
                            json.get("rooms")
                                .and_then(|r| r.as_object())
                                .map(|rooms| {
                                    rooms
                                        .values()
                                        .any(|room| room.get("active_shares").is_some())
                                })
                                .unwrap_or(false)
                        })
                }
                Err(_) => false,
            }
        }
        _ => false,
    };

    if sfu_detected || enable_sfu_tests {
        caps.push(Capability::Sfu);
        caps.push(Capability::ScreenShare);
        caps.push(Capability::TokenRevocation);
    }

    caps
}

/// Test tier — Tier 1 scenarios are correctness tests (zero tolerance, run R times);
/// Tier 2 scenarios are resilience tests (run once with a 10% tolerance margin).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Tier {
    Tier1,
    Tier2,
}

/// A self-contained stress test scenario.
///
/// Implement this trait for each scenario struct. The runner calls `run` (possibly
/// multiple times for Tier 1 race scenarios) and collects `ScenarioResult`s.
#[async_trait]
pub trait Scenario: Send + Sync {
    /// Human-readable name used in progress output and result reporting.
    fn name(&self) -> &str;

    /// Tier classification — affects repetition count and tolerance.
    fn tier(&self) -> Tier;

    /// Backend capabilities required by this scenario.
    /// If any required capability is absent from `ctx.capabilities`, the scenario is skipped.
    fn requires(&self) -> Vec<Capability> {
        vec![]
    }

    /// Backend config preset to apply before running this scenario.
    fn config_preset(&self) -> ConfigPreset {
        ConfigPreset::Default
    }

    /// Execute the scenario and return a result.
    async fn run(&self, ctx: &TestContext) -> ScenarioResult;
}

/// Apply a config preset to the in-process backend's AppState.
/// No-op when `app_state` is `None` (external backend mode).
///
/// For `Default` and `BruteForce`, the production `JoinRateLimiterConfig::default()` is used.
/// For `JoinHeavy`, relaxed join rate limits are applied so that capacity/atomicity scenarios
/// are not interfered with by the rate limiter.
fn apply_preset(ctx: &mut TestContext, preset: ConfigPreset) {
    use wavis_backend::domain::join_rate_limiter::JoinRateLimiterConfig;

    let Some(ref mut app_state) = ctx.app_state else {
        // External backend mode — presets are ignored.
        return;
    };

    let config = match preset {
        ConfigPreset::Default | ConfigPreset::BruteForce | ConfigPreset::Slowloris => {
            // Real production rate limits.
            JoinRateLimiterConfig::default()
        }
        ConfigPreset::JoinHeavy => {
            // Relaxed join rate limits: much higher per-IP and per-code windows so the
            // rate limiter does not interfere with capacity and atomicity tests.
            JoinRateLimiterConfig {
                ip_total_threshold: 10_000,
                ip_total_window: std::time::Duration::from_secs(60),
                ip_failed_threshold: 10_000,
                ip_failed_window: std::time::Duration::from_secs(60),
                code_threshold: 10_000,
                code_window: std::time::Duration::from_secs(60),
                room_threshold: 10_000,
                room_window: std::time::Duration::from_secs(60),
                connection_threshold: 10_000,
                connection_window: std::time::Duration::from_secs(60),
                cooldown: std::time::Duration::from_secs(1),
            }
        }
    };

    // Reconfigure the existing Arc in-place so the running server sees the change.
    app_state.join_rate_limiter.reconfigure(config);

    // Slowloris preset: lower the per-IP connection cap to the production default (10)
    // so the per-IP cap test can verify rejection without opening 200 connections.
    // All other presets: restore the high cap used by the stress harness.
    // Uses set_max_per_ip() on the shared Arc so the server thread sees the change.
    match preset {
        ConfigPreset::Slowloris => {
            app_state.ip_connection_tracker.set_max_per_ip(10);
        }
        _ => {
            let harness_cap = std::env::var("MAX_CONNECTIONS_PER_IP")
                .ok()
                .and_then(|v| v.parse::<u32>().ok())
                .unwrap_or(200);
            app_state.ip_connection_tracker.set_max_per_ip(harness_cap);
        }
    }
}

/// Orchestrates a list of scenarios: gates by capability, handles Tier 1 repetitions,
/// prints progress, and collects results.
pub struct ScenarioRunner {
    scenarios: Vec<Box<dyn Scenario>>,
}

impl ScenarioRunner {
    pub fn new(scenarios: Vec<Box<dyn Scenario>>) -> Self {
        Self { scenarios }
    }

    /// Run all registered scenarios sequentially against `ctx`.
    ///
    /// - Scenarios whose `requires()` capabilities are not in `ctx.capabilities` are skipped.
    /// - Tier 1 scenarios are run `ctx.scale.repetitions` times; any failing repetition
    ///   causes the overall result to be marked as failed.
    /// - Tier 2 scenarios are run once.
    /// - Before each scenario, the config preset is applied via `apply_preset`; after the
    ///   scenario completes (pass or fail), the preset is reset to `Default`.
    /// - Each scenario's `run()` call is wrapped in `tokio::task::spawn` so that a panic
    ///   inside the scenario is caught as a `JoinError` rather than crashing the harness.
    /// - After each scenario, the backend metrics endpoint is queried; if it is unreachable,
    ///   a critical `backend_panic_or_crash` violation is recorded.
    pub async fn run_all(
        &self,
        ctx: &mut TestContext,
        filter: Option<&str>,
    ) -> Vec<ScenarioResult> {
        let scenarios: Vec<_> = match filter {
            Some(name) => self
                .scenarios
                .iter()
                .filter(|s| s.name().eq_ignore_ascii_case(name))
                .collect(),
            None => self.scenarios.iter().collect(),
        };

        if let Some(name) = filter
            && scenarios.is_empty()
        {
            eprintln!("No scenario matched filter '{name}'");
            eprintln!("Available scenarios:");
            for s in &self.scenarios {
                eprintln!("  - {}", s.name());
            }
            return vec![];
        }

        let total = scenarios.len();
        let mut results = Vec::with_capacity(total);

        let metrics_token = std::env::var("TEST_METRICS_TOKEN").unwrap_or_default();

        for (idx, scenario) in scenarios.iter().enumerate() {
            println!("[{}/{}] Running: {}...", idx + 1, total, scenario.name());

            // --- capability gate ---
            let missing = scenario
                .requires()
                .into_iter()
                .find(|cap| !ctx.capabilities.contains(cap));

            if let Some(missing_cap) = missing {
                let skip_name =
                    format!("SKIPPED: {} (requires {:?})", scenario.name(), missing_cap);
                results.push(ScenarioResult {
                    name: skip_name,
                    passed: true,
                    duration: std::time::Duration::ZERO,
                    actions_per_second: 0.0,
                    p95_latency: std::time::Duration::ZERO,
                    p99_latency: std::time::Duration::ZERO,
                    violations: vec![],
                });
                continue;
            }

            // --- apply config preset before running ---
            let preset = scenario.config_preset();
            apply_preset(ctx, preset);

            // --- repetition logic for Tier 1 ---
            let repetitions = if scenario.tier() == Tier::Tier1 {
                ctx.scale.repetitions
            } else {
                1
            };

            // Per-scenario timeout prevents any single scenario from hanging
            // the entire harness (e.g. half-open TCP sockets on Windows).
            let scenario_timeout = std::time::Duration::from_secs(60);

            let mut final_result = match tokio::time::timeout(
                scenario_timeout,
                run_scenario_with_panic_detection(scenario.as_ref(), ctx),
            )
            .await
            {
                Ok(r) => r,
                Err(_elapsed) => {
                    eprintln!(
                        "[TIMEOUT] Scenario '{}' exceeded {}s — marking as failed",
                        scenario.name(),
                        scenario_timeout.as_secs()
                    );
                    ScenarioResult {
                        name: scenario.name().to_owned(),
                        passed: false,
                        duration: scenario_timeout,
                        actions_per_second: 0.0,
                        p95_latency: std::time::Duration::ZERO,
                        p99_latency: std::time::Duration::ZERO,
                        violations: vec![InvariantViolation {
                            invariant: "scenario_timeout".to_string(),
                            expected: format!(
                                "scenario completes within {}s",
                                scenario_timeout.as_secs()
                            ),
                            actual: "scenario timed out".to_string(),
                        }],
                    }
                }
            };

            for rep in 1..repetitions {
                if !final_result.passed {
                    // Already failed — no need to run more repetitions.
                    break;
                }
                // Reset global rate limiters between repetitions so token buckets
                // don't carry over depletion from the previous rep.
                if let Some(ref app_state) = ctx.app_state {
                    app_state.global_ws_limiter.reconfigure();
                    app_state.global_join_limiter.reconfigure();
                    app_state.auth_rate_limiter.clear();
                }
                let rep_result = match tokio::time::timeout(
                    scenario_timeout,
                    run_scenario_with_panic_detection(scenario.as_ref(), ctx),
                )
                .await
                {
                    Ok(r) => r,
                    Err(_elapsed) => {
                        eprintln!(
                            "[TIMEOUT] Scenario '{}' rep {} exceeded {}s — marking as failed",
                            scenario.name(),
                            rep + 1,
                            scenario_timeout.as_secs()
                        );
                        ScenarioResult {
                            name: format!("{} (rep {})", scenario.name(), rep + 1),
                            passed: false,
                            duration: scenario_timeout,
                            actions_per_second: 0.0,
                            p95_latency: std::time::Duration::ZERO,
                            p99_latency: std::time::Duration::ZERO,
                            violations: vec![InvariantViolation {
                                invariant: "scenario_timeout".to_string(),
                                expected: format!(
                                    "scenario completes within {}s",
                                    scenario_timeout.as_secs()
                                ),
                                actual: "scenario timed out".to_string(),
                            }],
                        }
                    }
                };
                if !rep_result.passed {
                    // Propagate the failure; keep the violations from the failing run.
                    final_result = ScenarioResult {
                        name: format!("{} (rep {})", rep_result.name, rep + 1),
                        ..rep_result
                    };
                }
            }

            // --- reset to Default preset after scenario ---
            apply_preset(ctx, ConfigPreset::Default);

            // --- backend reachability check (Requirement 11.5) ---
            // If the metrics endpoint is unreachable after the scenario, the backend may have
            // panicked or crashed. Report this as a critical failure.
            if let Err(e) = fetch_metrics(&ctx.http_client, &ctx.metrics_url, &metrics_token).await
            {
                eprintln!(
                    "[CRITICAL] Backend metrics endpoint unreachable after scenario '{}': {e}",
                    scenario.name()
                );
                final_result.passed = false;
                final_result.violations.push(InvariantViolation {
                    invariant: "backend_panic_or_crash".to_string(),
                    expected: "metrics endpoint reachable".to_string(),
                    actual: format!("metrics endpoint unreachable after scenario: {e}"),
                });
            }

            results.push(final_result);
        }

        results
    }
}

/// Run a single scenario, catching any panic via `futures::FutureExt::catch_unwind`.
///
/// If the future panics, a `ScenarioResult` with `passed: false` and a descriptive
/// `InvariantViolation` is returned instead of propagating the panic to the harness.
async fn run_scenario_with_panic_detection(
    scenario: &dyn Scenario,
    ctx: &TestContext,
) -> ScenarioResult {
    use futures_util::FutureExt;
    use std::panic::AssertUnwindSafe;

    let scenario_name = scenario.name().to_owned();

    let result = AssertUnwindSafe(scenario.run(ctx)).catch_unwind().await;

    match result {
        Ok(scenario_result) => scenario_result,
        Err(panic_payload) => {
            // The scenario future panicked — report it as a critical failure.
            let panic_msg = if let Some(s) = panic_payload.downcast_ref::<&str>() {
                format!("scenario panicked: {s}")
            } else if let Some(s) = panic_payload.downcast_ref::<String>() {
                format!("scenario panicked: {s}")
            } else {
                "scenario panicked with non-string payload".to_string()
            };

            eprintln!("[CRITICAL] Scenario '{scenario_name}' panicked: {panic_msg}");

            ScenarioResult {
                name: scenario_name,
                passed: false,
                duration: std::time::Duration::ZERO,
                actions_per_second: 0.0,
                p95_latency: std::time::Duration::ZERO,
                p99_latency: std::time::Duration::ZERO,
                violations: vec![InvariantViolation {
                    invariant: "scenario_panic".to_string(),
                    expected: "scenario completes without panic".to_string(),
                    actual: panic_msg,
                }],
            }
        }
    }
}
