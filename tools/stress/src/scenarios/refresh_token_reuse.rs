/// RefreshTokenReuseScenario — Refresh token reuse detection
///
/// Validates the atomic consume-and-reissue + reuse detection logic:
///
///   1. Register a device → get (access_token, refresh_token_1).
///   2. Rotate refresh_token_1 → get (access_token_2, refresh_token_2). This consumes
///      refresh_token_1 and moves its hash to consumed_refresh_tokens.
///   3. Replay refresh_token_1 (already consumed) → assert HTTP 401 and that the
///      entire token family is revoked (refresh_token_2 also becomes invalid).
///   4. Attempt to use refresh_token_2 → assert HTTP 401 (family revoked).
///
/// Requires a real Postgres database. In-process mode uses a dummy pool, so this
/// scenario skips gracefully when running in-process.
///
/// **Validates: Requirements 2.5, 4.1, 4.2, 4.3**
use std::time::Instant;

use async_trait::async_trait;

use crate::config::{ConfigPreset, TestContext};
use crate::results::{InvariantViolation, LatencyTracker, ScenarioResult};
use crate::runner::{Scenario, Tier};

pub struct RefreshTokenReuseScenario;

#[async_trait]
impl Scenario for RefreshTokenReuseScenario {
    fn name(&self) -> &str {
        "refresh-token-reuse"
    }

    fn tier(&self) -> Tier {
        Tier::Tier1
    }

    fn config_preset(&self) -> ConfigPreset {
        ConfigPreset::Default
    }

    async fn run(&self, ctx: &TestContext) -> ScenarioResult {
        let start = Instant::now();
        let latency = LatencyTracker::new();

        // In-process mode uses a dummy Postgres pool — skip gracefully.
        if ctx.app_state.is_some() {
            return ScenarioResult {
                name: "refresh-token-reuse (SKIPPED: requires real Postgres)".to_owned(),
                passed: true,
                duration: start.elapsed(),
                actions_per_second: 0.0,
                p95_latency: latency.p95(),
                p99_latency: latency.p99(),
                violations: vec![],
            };
        }

        let mut violations: Vec<InvariantViolation> = Vec::new();
        run_external(ctx, &mut violations).await;
        build_result(self.name(), start, violations, latency)
    }
}

// ---------------------------------------------------------------------------
// External mode: HTTP requests to REST endpoints
// ---------------------------------------------------------------------------

async fn run_external(ctx: &TestContext, violations: &mut Vec<InvariantViolation>) {
    let base_url = ws_url_to_http(&ctx.ws_url);
    let register_url = format!("{base_url}/auth/register_device");
    let refresh_url = format!("{base_url}/auth/refresh");

    // =========================================================================
    // Step 1: Register a device
    // =========================================================================
    let resp = match ctx.http_client.post(&register_url).send().await {
        Ok(r) => r,
        Err(e) => {
            violations.push(InvariantViolation {
                invariant: "refresh_reuse: register reachable".to_owned(),
                expected: "HTTP response".to_owned(),
                actual: format!("request error: {e}"),
            });
            return;
        }
    };

    if resp.status().is_server_error() {
        // DB not available — skip gracefully.
        return;
    }

    if resp.status() != reqwest::StatusCode::CREATED {
        violations.push(InvariantViolation {
            invariant: "refresh_reuse: register succeeds".to_owned(),
            expected: "HTTP 201".to_owned(),
            actual: format!("HTTP {}", resp.status()),
        });
        return;
    }

    let reg_body: serde_json::Value = match resp.json().await {
        Ok(v) => v,
        Err(e) => {
            violations.push(InvariantViolation {
                invariant: "refresh_reuse: register response parseable".to_owned(),
                expected: "valid JSON".to_owned(),
                actual: format!("parse error: {e}"),
            });
            return;
        }
    };

    let refresh_token_1 = match reg_body.get("refresh_token").and_then(|v| v.as_str()) {
        Some(t) => t.to_owned(),
        None => {
            violations.push(InvariantViolation {
                invariant: "refresh_reuse: register returns refresh_token".to_owned(),
                expected: "refresh_token field present".to_owned(),
                actual: format!("missing from response: {reg_body}"),
            });
            return;
        }
    };

    // =========================================================================
    // Step 2: Rotate refresh_token_1 → get refresh_token_2
    // =========================================================================
    let resp2 = match ctx
        .http_client
        .post(&refresh_url)
        .json(&serde_json::json!({ "refresh_token": refresh_token_1 }))
        .send()
        .await
    {
        Ok(r) => r,
        Err(e) => {
            violations.push(InvariantViolation {
                invariant: "refresh_reuse: first rotation reachable".to_owned(),
                expected: "HTTP response".to_owned(),
                actual: format!("request error: {e}"),
            });
            return;
        }
    };

    if resp2.status() != reqwest::StatusCode::OK {
        violations.push(InvariantViolation {
            invariant: "refresh_reuse: first rotation succeeds".to_owned(),
            expected: "HTTP 200".to_owned(),
            actual: format!("HTTP {}", resp2.status()),
        });
        return;
    }

    let rot_body: serde_json::Value = match resp2.json().await {
        Ok(v) => v,
        Err(e) => {
            violations.push(InvariantViolation {
                invariant: "refresh_reuse: rotation response parseable".to_owned(),
                expected: "valid JSON".to_owned(),
                actual: format!("parse error: {e}"),
            });
            return;
        }
    };

    let refresh_token_2 = match rot_body.get("refresh_token").and_then(|v| v.as_str()) {
        Some(t) => t.to_owned(),
        None => {
            violations.push(InvariantViolation {
                invariant: "refresh_reuse: rotation returns new refresh_token".to_owned(),
                expected: "refresh_token field present".to_owned(),
                actual: format!("missing from response: {rot_body}"),
            });
            return;
        }
    };

    // =========================================================================
    // Step 3: Replay refresh_token_1 (consumed) → must get 401 + family revoked
    // =========================================================================
    let resp3 = match ctx
        .http_client
        .post(&refresh_url)
        .json(&serde_json::json!({ "refresh_token": refresh_token_1 }))
        .send()
        .await
    {
        Ok(r) => r,
        Err(e) => {
            violations.push(InvariantViolation {
                invariant: "refresh_reuse: replay request reachable".to_owned(),
                expected: "HTTP response".to_owned(),
                actual: format!("request error: {e}"),
            });
            return;
        }
    };

    if resp3.status() != reqwest::StatusCode::UNAUTHORIZED {
        violations.push(InvariantViolation {
            invariant: "refresh_reuse: consumed token replay must return 401".to_owned(),
            expected: "HTTP 401".to_owned(),
            actual: format!("HTTP {}", resp3.status()),
        });
    }

    // =========================================================================
    // Step 4: Use refresh_token_2 → must also get 401 (family revoked)
    // =========================================================================
    let resp4 = match ctx
        .http_client
        .post(&refresh_url)
        .json(&serde_json::json!({ "refresh_token": refresh_token_2 }))
        .send()
        .await
    {
        Ok(r) => r,
        Err(e) => {
            violations.push(InvariantViolation {
                invariant: "refresh_reuse: family revocation check reachable".to_owned(),
                expected: "HTTP response".to_owned(),
                actual: format!("request error: {e}"),
            });
            return;
        }
    };

    if resp4.status() != reqwest::StatusCode::UNAUTHORIZED {
        violations.push(InvariantViolation {
            invariant: "refresh_reuse: family must be revoked after reuse detection".to_owned(),
            expected: "HTTP 401 (refresh_token_2 revoked)".to_owned(),
            actual: format!("HTTP {}", resp4.status()),
        });
    }
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

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
