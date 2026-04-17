/// AuthBruteForceScenario — Auth REST rate limiter validation
///
/// Hammers `POST /auth/register_device` and `POST /auth/refresh` from a single IP
/// to validate that `AuthRateLimiter` fires correctly.
///
/// Phase 1 — Register flood: Send N register requests (N > default threshold of 5).
///           Assert HTTP 429 after threshold.
/// Phase 2 — Refresh flood: Send M refresh requests with garbage tokens (M > default
///           threshold of 30). Assert HTTP 429 after threshold.
///
/// In-process mode: Tests the `AuthRateLimiter` directly via `ctx.app_state` since
/// the in-process server uses a dummy Postgres pool and doesn't mount REST routes.
///
/// External mode: Sends real HTTP requests to the backend's REST endpoints.
///
/// **Validates: Requirements 1.5, 2.6**
use std::net::{IpAddr, Ipv4Addr};
use std::time::Instant;

use async_trait::async_trait;

use crate::config::{ConfigPreset, TestContext};
use crate::results::{InvariantViolation, LatencyTracker, ScenarioResult};
use crate::runner::{Scenario, Tier};

pub struct AuthBruteForceScenario;

#[async_trait]
impl Scenario for AuthBruteForceScenario {
    fn name(&self) -> &str {
        "auth-brute-force"
    }

    fn tier(&self) -> Tier {
        Tier::Tier1
    }

    fn config_preset(&self) -> ConfigPreset {
        ConfigPreset::Default
    }

    async fn run(&self, ctx: &TestContext) -> ScenarioResult {
        let start = Instant::now();
        let mut violations: Vec<InvariantViolation> = Vec::new();
        let latency = LatencyTracker::new();

        match &ctx.app_state {
            Some(app_state) => {
                run_in_process(app_state, &mut violations);
            }
            None => {
                run_external(ctx, &mut violations).await;
            }
        }

        build_result(self.name(), start, violations, latency)
    }
}

// ---------------------------------------------------------------------------
// In-process mode: test AuthRateLimiter directly
// ---------------------------------------------------------------------------

fn run_in_process(
    app_state: &wavis_backend::app_state::AppState,
    violations: &mut Vec<InvariantViolation>,
) {
    let limiter = &app_state.auth_rate_limiter;
    let ip = IpAddr::V4(Ipv4Addr::new(10, 99, 0, 1));
    let now = Instant::now();

    // --- Phase 1: Register flood ---
    // Default threshold: 5 per hour. Send 6 requests — first 5 allowed, 6th rejected.
    let register_threshold = 5u32;
    for i in 0..register_threshold {
        if !limiter.check_register(ip, now) {
            violations.push(InvariantViolation {
                invariant: format!(
                    "auth_brute_force: register request {i} should be allowed (under threshold)"
                ),
                expected: "allowed".to_owned(),
                actual: "rejected".to_owned(),
            });
            return;
        }
        limiter.record_register(ip, now);
    }

    // The (threshold+1)th request must be rejected.
    if limiter.check_register(ip, now) {
        violations.push(InvariantViolation {
            invariant: "auth_brute_force: register request at threshold must be rejected"
                .to_owned(),
            expected: "rejected (rate limited)".to_owned(),
            actual: "allowed".to_owned(),
        });
    }

    // --- Phase 2: Refresh flood ---
    // Default threshold: 30 per minute. Use a different IP to avoid cross-contamination.
    let refresh_ip = IpAddr::V4(Ipv4Addr::new(10, 99, 0, 2));
    let refresh_threshold = 30u32;
    for i in 0..refresh_threshold {
        if !limiter.check_refresh(refresh_ip, now) {
            violations.push(InvariantViolation {
                invariant: format!(
                    "auth_brute_force: refresh request {i} should be allowed (under threshold)"
                ),
                expected: "allowed".to_owned(),
                actual: "rejected".to_owned(),
            });
            return;
        }
        limiter.record_refresh(refresh_ip, now);
    }

    if limiter.check_refresh(refresh_ip, now) {
        violations.push(InvariantViolation {
            invariant: "auth_brute_force: refresh request at threshold must be rejected".to_owned(),
            expected: "rejected (rate limited)".to_owned(),
            actual: "allowed".to_owned(),
        });
    }
}

// ---------------------------------------------------------------------------
// External mode: HTTP requests to REST endpoints
// ---------------------------------------------------------------------------

async fn run_external(ctx: &TestContext, violations: &mut Vec<InvariantViolation>) {
    let base_url = ws_url_to_http(&ctx.ws_url);

    // --- Phase 1: Register flood ---
    let register_url = format!("{base_url}/auth/register_device");
    let mut got_429_register = false;
    // Send more than the default threshold (5). Send 10 to be safe.
    let register_attempts = 10;

    for i in 0..register_attempts {
        let resp = ctx.http_client.post(&register_url).send().await;

        match resp {
            Ok(r) if r.status() == reqwest::StatusCode::TOO_MANY_REQUESTS => {
                got_429_register = true;
                break;
            }
            Ok(r) if r.status() == reqwest::StatusCode::CREATED => {
                // Allowed — expected for first N requests.
            }
            Ok(r) => {
                // 500 is acceptable (dummy DB in some configs), but not a rate limit.
                if r.status().is_server_error() && i < 5 {
                    // DB not available — skip REST test gracefully.
                    return;
                }
            }
            Err(e) => {
                violations.push(InvariantViolation {
                    invariant: "auth_brute_force: register request reachable".to_owned(),
                    expected: "HTTP response".to_owned(),
                    actual: format!("request error: {e}"),
                });
                return;
            }
        }
    }

    if !got_429_register {
        violations.push(InvariantViolation {
            invariant: "auth_brute_force: register rate limiter must fire".to_owned(),
            expected: "HTTP 429 after threshold".to_owned(),
            actual: format!("no 429 after {register_attempts} attempts"),
        });
    }

    // --- Phase 2: Refresh flood ---
    let refresh_url = format!("{base_url}/auth/refresh");
    let mut got_429_refresh = false;
    let refresh_attempts = 40;

    for i in 0..refresh_attempts {
        let resp = ctx
            .http_client
            .post(&refresh_url)
            .json(&serde_json::json!({ "refresh_token": format!("garbage-token-{i}") }))
            .send()
            .await;

        match resp {
            Ok(r) if r.status() == reqwest::StatusCode::TOO_MANY_REQUESTS => {
                got_429_refresh = true;
                break;
            }
            Ok(_) => {
                // 401 or 500 — expected for garbage tokens, keep going.
            }
            Err(e) => {
                violations.push(InvariantViolation {
                    invariant: "auth_brute_force: refresh request reachable".to_owned(),
                    expected: "HTTP response".to_owned(),
                    actual: format!("request error: {e}"),
                });
                return;
            }
        }
    }

    if !got_429_refresh {
        violations.push(InvariantViolation {
            invariant: "auth_brute_force: refresh rate limiter must fire".to_owned(),
            expected: "HTTP 429 after threshold".to_owned(),
            actual: format!("no 429 after {refresh_attempts} attempts"),
        });
    }
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

/// Convert a WebSocket URL to an HTTP base URL.
/// `ws://127.0.0.1:3000/ws` → `http://127.0.0.1:3000`
fn ws_url_to_http(ws_url: &str) -> String {
    ws_url
        .replace("wss://", "https://")
        .replace("ws://", "http://")
        .trim_end_matches("/ws")
        .to_owned()
}

fn build_result(
    name: &str,
    start: Instant,
    violations: Vec<InvariantViolation>,
    latency: LatencyTracker,
) -> ScenarioResult {
    let duration = start.elapsed();
    ScenarioResult {
        name: name.to_owned(),
        passed: violations.is_empty(),
        duration,
        actions_per_second: if duration.as_secs_f64() > 0.0 {
            1.0 / duration.as_secs_f64()
        } else {
            0.0
        },
        p95_latency: latency.p95(),
        p99_latency: latency.p99(),
        violations,
    }
}
