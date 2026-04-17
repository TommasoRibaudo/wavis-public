/// Auth Flow Scenarios
///
/// Tests the REST endpoints the GUI auth layer depends on:
/// - POST /auth/register_device  — success, response shape
/// - POST /auth/refresh           — valid refresh, invalid token
/// - Authenticated request with valid token succeeds
/// - Authenticated request without token returns 401
use std::time::Instant;

use async_trait::async_trait;
use serde::Deserialize;

use crate::harness_context::TestContext;
use crate::results::{AssertionFailure, ScenarioResult};
use crate::runner::Scenario;

pub struct AuthFlowScenario;

#[derive(Deserialize)]
struct RegisterResponse {
    #[allow(dead_code)]
    device_id: String,
    #[allow(dead_code)]
    user_id: String,
    access_token: String,
    refresh_token: String,
}

#[async_trait]
impl Scenario for AuthFlowScenario {
    fn name(&self) -> &str {
        "auth-flow"
    }

    async fn run(&self, ctx: &TestContext) -> ScenarioResult {
        let start = Instant::now();
        let mut failures: Vec<AssertionFailure> = Vec::new();

        // ── Register device — success + response shape ──
        let reg = check_register_success(ctx, &mut failures).await;

        if let Some(reg) = &reg {
            // ── Authenticated request with valid token ──
            check_authenticated_request(ctx, &reg.access_token, &mut failures).await;

            // ── Refresh token — success ──
            check_refresh_success(ctx, &reg.refresh_token, &mut failures).await;
        }

        // ── Request without token — 401 ──
        check_unauthenticated_request(ctx, &mut failures).await;

        // ── Refresh with invalid token — failure ──
        check_refresh_invalid(ctx, &mut failures).await;

        ScenarioResult {
            name: self.name().to_owned(),
            passed: failures.is_empty(),
            duration: start.elapsed(),
            failures,
        }
    }
}

async fn check_register_success(
    ctx: &TestContext,
    failures: &mut Vec<AssertionFailure>,
) -> Option<RegisterResponse> {
    let url = format!("{}/auth/register_device", ctx.base_url);
    let resp = match ctx.http_client.post(&url).send().await {
        Ok(r) => r,
        Err(e) => {
            failures.push(AssertionFailure {
                check: "POST /auth/register_device request".into(),
                expected: "response".into(),
                actual: format!("error: {e}"),
            });
            return None;
        }
    };

    if resp.status() != reqwest::StatusCode::CREATED {
        failures.push(AssertionFailure {
            check: "POST /auth/register_device status".into(),
            expected: "201".into(),
            actual: format!("{}", resp.status()),
        });
        return None;
    }

    let body: serde_json::Value = match resp.json().await {
        Ok(v) => v,
        Err(e) => {
            failures.push(AssertionFailure {
                check: "register response parse".into(),
                expected: "valid JSON".into(),
                actual: format!("{e}"),
            });
            return None;
        }
    };

    // Verify response shape matches what GUI expects
    for field in ["device_id", "user_id", "access_token", "refresh_token"] {
        if body.get(field).is_none() {
            failures.push(AssertionFailure {
                check: format!("register response has {field}"),
                expected: format!("{field} field present"),
                actual: "missing".into(),
            });
        }
    }

    // Deserialize for use in subsequent checks
    serde_json::from_value(body).ok()
}

async fn check_authenticated_request(
    ctx: &TestContext,
    token: &str,
    failures: &mut Vec<AssertionFailure>,
) {
    // GET /channels with valid token should return 200 (empty list)
    let resp = match ctx
        .http_client
        .get(format!("{}/channels", ctx.base_url))
        .header("Authorization", format!("Bearer {token}"))
        .send()
        .await
    {
        Ok(r) => r,
        Err(e) => {
            failures.push(AssertionFailure {
                check: "GET /channels with token".into(),
                expected: "response".into(),
                actual: format!("error: {e}"),
            });
            return;
        }
    };

    if resp.status() != reqwest::StatusCode::OK {
        failures.push(AssertionFailure {
            check: "GET /channels with valid token".into(),
            expected: "200".into(),
            actual: format!("{}", resp.status()),
        });
    }
}

async fn check_unauthenticated_request(ctx: &TestContext, failures: &mut Vec<AssertionFailure>) {
    let resp = match ctx
        .http_client
        .get(format!("{}/channels", ctx.base_url))
        .send()
        .await
    {
        Ok(r) => r,
        Err(e) => {
            failures.push(AssertionFailure {
                check: "GET /channels without token".into(),
                expected: "response".into(),
                actual: format!("error: {e}"),
            });
            return;
        }
    };

    if resp.status() != reqwest::StatusCode::UNAUTHORIZED {
        failures.push(AssertionFailure {
            check: "GET /channels without token returns 401".into(),
            expected: "401".into(),
            actual: format!("{}", resp.status()),
        });
    }
}

async fn check_refresh_success(
    ctx: &TestContext,
    refresh_token: &str,
    failures: &mut Vec<AssertionFailure>,
) {
    let url = format!("{}/auth/refresh", ctx.base_url);
    let resp = match ctx
        .http_client
        .post(&url)
        .header("Content-Type", "application/json")
        .json(&serde_json::json!({ "refresh_token": refresh_token }))
        .send()
        .await
    {
        Ok(r) => r,
        Err(e) => {
            failures.push(AssertionFailure {
                check: "POST /auth/refresh request".into(),
                expected: "response".into(),
                actual: format!("error: {e}"),
            });
            return;
        }
    };

    if resp.status() != reqwest::StatusCode::OK {
        failures.push(AssertionFailure {
            check: "POST /auth/refresh success".into(),
            expected: "200".into(),
            actual: format!("{}", resp.status()),
        });
        return;
    }

    let body: serde_json::Value = match resp.json().await {
        Ok(v) => v,
        Err(e) => {
            failures.push(AssertionFailure {
                check: "refresh response parse".into(),
                expected: "valid JSON".into(),
                actual: format!("{e}"),
            });
            return;
        }
    };

    // Verify refreshed response has new tokens
    for field in ["user_id", "access_token", "refresh_token"] {
        if body.get(field).is_none() {
            failures.push(AssertionFailure {
                check: format!("refresh response has {field}"),
                expected: format!("{field} field present"),
                actual: "missing".into(),
            });
        }
    }
}

async fn check_refresh_invalid(ctx: &TestContext, failures: &mut Vec<AssertionFailure>) {
    let url = format!("{}/auth/refresh", ctx.base_url);
    let resp = match ctx
        .http_client
        .post(&url)
        .header("Content-Type", "application/json")
        .json(&serde_json::json!({ "refresh_token": "invalid-token-garbage" }))
        .send()
        .await
    {
        Ok(r) => r,
        Err(e) => {
            failures.push(AssertionFailure {
                check: "POST /auth/refresh invalid token".into(),
                expected: "response".into(),
                actual: format!("error: {e}"),
            });
            return;
        }
    };

    // Should be 401 or 400 — not 200
    if resp.status().is_success() {
        failures.push(AssertionFailure {
            check: "POST /auth/refresh with invalid token rejects".into(),
            expected: "4xx".into(),
            actual: format!("{}", resp.status()),
        });
    }
}
