/// Error & Edge Case Scenarios
///
/// Tests error responses the GUI must handle correctly:
/// - 403 for nonexistent channel ID (backend hides 404)
/// - 401 for requests without Authorization header
/// - 401 for requests with an invalid/expired token
use std::time::Instant;

use async_trait::async_trait;

use crate::client::AuthenticatedClient;
use crate::harness_context::TestContext;
use crate::results::{AssertionFailure, ScenarioResult};
use crate::runner::Scenario;

pub struct ErrorEdgesScenario;

#[async_trait]
impl Scenario for ErrorEdgesScenario {
    fn name(&self) -> &str {
        "error-edges"
    }

    async fn run(&self, ctx: &TestContext) -> ScenarioResult {
        let start = Instant::now();
        let mut failures: Vec<AssertionFailure> = Vec::new();

        let client = match AuthenticatedClient::register(&ctx.base_url, &ctx.http_client).await {
            Ok(c) => c,
            Err(e) => return err_result(self.name(), start, &format!("register: {e}")),
        };

        // ── Nonexistent channel → 403 ──
        check_nonexistent_channel(&client, &mut failures).await;

        // ── No auth header → 401 ──
        check_no_auth_header(ctx, &mut failures).await;

        // ── Invalid token → 401 ──
        check_invalid_token(ctx, &mut failures).await;

        ScenarioResult {
            name: self.name().to_owned(),
            passed: failures.is_empty(),
            duration: start.elapsed(),
            failures,
        }
    }
}

async fn check_nonexistent_channel(
    client: &AuthenticatedClient,
    failures: &mut Vec<AssertionFailure>,
) {
    let fake_id = uuid::Uuid::new_v4();
    let resp = match client.get(&format!("/channels/{fake_id}")).await {
        Ok(r) => r,
        Err(e) => {
            failures.push(AssertionFailure {
                check: "GET nonexistent channel".into(),
                expected: "response".into(),
                actual: format!("error: {e}"),
            });
            return;
        }
    };

    // Backend returns 403 to avoid leaking channel existence
    if resp.status() != reqwest::StatusCode::FORBIDDEN {
        failures.push(AssertionFailure {
            check: "nonexistent channel returns 403".into(),
            expected: "403".into(),
            actual: format!("{}", resp.status()),
        });
    }
}

async fn check_no_auth_header(ctx: &TestContext, failures: &mut Vec<AssertionFailure>) {
    let endpoints = [("GET", "/channels"), ("POST", "/channels")];

    for (method, path) in &endpoints {
        let url = format!("{}{path}", ctx.base_url);
        let resp = match *method {
            "GET" => ctx.http_client.get(&url).send().await,
            "POST" => {
                ctx.http_client
                    .post(&url)
                    .header("Content-Type", "application/json")
                    .body("{}")
                    .send()
                    .await
            }
            _ => unreachable!(),
        };

        match resp {
            Ok(r) => {
                if r.status() != reqwest::StatusCode::UNAUTHORIZED {
                    failures.push(AssertionFailure {
                        check: format!("{method} {path} no auth → 401"),
                        expected: "401".into(),
                        actual: format!("{}", r.status()),
                    });
                }
            }
            Err(e) => {
                failures.push(AssertionFailure {
                    check: format!("{method} {path} no auth request"),
                    expected: "response".into(),
                    actual: format!("error: {e}"),
                });
            }
        }
    }
}

async fn check_invalid_token(ctx: &TestContext, failures: &mut Vec<AssertionFailure>) {
    let url = format!("{}/channels", ctx.base_url);
    let resp = match ctx
        .http_client
        .get(&url)
        .header("Authorization", "Bearer invalid-garbage-token")
        .send()
        .await
    {
        Ok(r) => r,
        Err(e) => {
            failures.push(AssertionFailure {
                check: "GET /channels invalid token".into(),
                expected: "response".into(),
                actual: format!("error: {e}"),
            });
            return;
        }
    };

    if resp.status() != reqwest::StatusCode::UNAUTHORIZED {
        failures.push(AssertionFailure {
            check: "GET /channels invalid token → 401".into(),
            expected: "401".into(),
            actual: format!("{}", resp.status()),
        });
    }
}

fn err_result(name: &str, start: Instant, msg: &str) -> ScenarioResult {
    ScenarioResult {
        name: name.to_owned(),
        passed: false,
        duration: start.elapsed(),
        failures: vec![AssertionFailure {
            check: "setup".into(),
            expected: "success".into(),
            actual: msg.to_owned(),
        }],
    }
}
