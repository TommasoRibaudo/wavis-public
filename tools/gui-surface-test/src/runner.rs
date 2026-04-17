use async_trait::async_trait;

use crate::harness_context::TestContext;
use crate::results::ScenarioResult;

/// A self-contained surface test scenario.
///
/// Each scenario exercises one or more REST endpoints that the GUI client depends on.
#[async_trait]
pub trait Scenario: Send + Sync {
    /// Human-readable name used in progress output and result reporting.
    fn name(&self) -> &str;

    /// Execute the scenario and return a result.
    async fn run(&self, ctx: &TestContext) -> ScenarioResult;
}

/// Orchestrates a list of scenarios sequentially and collects results.
pub struct ScenarioRunner {
    scenarios: Vec<Box<dyn Scenario>>,
}

impl ScenarioRunner {
    pub fn new(scenarios: Vec<Box<dyn Scenario>>) -> Self {
        Self { scenarios }
    }

    /// Reset backend rate limiters between scenarios so per-IP registration
    /// limits don't cascade across independent test runs.
    async fn reset_rate_limits(ctx: &TestContext) {
        let url = format!("{}/test/reset_rate_limits", ctx.base_url);
        let resp = ctx
            .http_client
            .post(&url)
            .header("authorization", format!("Bearer {}", ctx.metrics_token))
            .send()
            .await;
        match resp {
            Ok(r) if r.status().is_success() => {}
            Ok(r) => {
                tracing::warn!(
                    "reset_rate_limits returned {}: is the backend built with test-metrics feature and TEST_METRICS_TOKEN set?",
                    r.status()
                );
            }
            Err(e) => {
                tracing::warn!("reset_rate_limits failed: {e} — rate limits may not be cleared");
            }
        }
    }

    /// Run all registered scenarios sequentially against `ctx`.
    pub async fn run_all(&self, ctx: &TestContext) -> Vec<ScenarioResult> {
        let total = self.scenarios.len();
        let mut results = Vec::with_capacity(total);

        for (idx, scenario) in self.scenarios.iter().enumerate() {
            Self::reset_rate_limits(ctx).await;
            println!("[{}/{}] Running: {}...", idx + 1, total, scenario.name());

            let result = scenario.run(ctx).await;

            let status = if result.passed { "PASS" } else { "FAIL" };
            println!(
                "[{status}] {} ({:.3}s)",
                result.name,
                result.duration.as_secs_f64()
            );

            for f in &result.failures {
                println!("       ✗ {}", f.check);
                println!("           expected: {}", f.expected);
                println!("           actual:   {}", f.actual);
            }

            results.push(result);
        }

        results
    }
}
