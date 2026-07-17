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

#[cfg(test)]
mod tests {
    use std::sync::{Arc, Mutex};
    use std::time::Duration;

    use super::*;
    use crate::results::AssertionFailure;

    /// Records its own name into a shared log when run, so tests can assert
    /// both "did every scenario execute" and "in what order".
    struct RecordingScenario {
        name: String,
        invocations: Arc<Mutex<Vec<String>>>,
    }

    #[async_trait]
    impl Scenario for RecordingScenario {
        fn name(&self) -> &str {
            &self.name
        }

        async fn run(&self, _ctx: &TestContext) -> ScenarioResult {
            self.invocations.lock().unwrap().push(self.name.clone());
            ScenarioResult {
                name: self.name.clone(),
                passed: true,
                duration: Duration::from_millis(0),
                failures: Vec::<AssertionFailure>::new(),
            }
        }
    }

    /// A TestContext whose base_url nothing listens on, so reset_rate_limits'
    /// internal HTTP call always hits a connection error — reset_rate_limits
    /// swallows that (it returns `()`, not a `Result`), so this exercises the
    /// real failure path without needing a mock server or a production seam.
    fn unreachable_ctx() -> TestContext {
        TestContext {
            base_url: "http://127.0.0.1:1".to_string(),
            metrics_token: "unused".to_string(),
            http_client: reqwest::Client::new(),
        }
    }

    #[tokio::test]
    async fn run_all_executes_every_scenario_in_order_even_when_reset_rate_limits_fails() {
        let invocations = Arc::new(Mutex::new(Vec::new()));
        let scenarios: Vec<Box<dyn Scenario>> = vec![
            Box::new(RecordingScenario {
                name: "scenario-a".to_string(),
                invocations: invocations.clone(),
            }),
            Box::new(RecordingScenario {
                name: "scenario-b".to_string(),
                invocations: invocations.clone(),
            }),
        ];
        let runner = ScenarioRunner::new(scenarios);
        let ctx = unreachable_ctx();

        let results = runner.run_all(&ctx).await;

        assert_eq!(results.len(), 2, "both scenarios must produce a result");
        assert_eq!(results[0].name, "scenario-a");
        assert_eq!(results[1].name, "scenario-b");
        assert_eq!(
            *invocations.lock().unwrap(),
            vec!["scenario-a".to_string(), "scenario-b".to_string()],
            "both scenarios must actually run, in registration order, despite \
             every reset_rate_limits call failing"
        );
    }

    #[tokio::test]
    async fn run_all_returns_empty_for_no_scenarios_without_calling_reset() {
        let runner = ScenarioRunner::new(Vec::new());
        let ctx = unreachable_ctx();

        let results = runner.run_all(&ctx).await;

        assert!(results.is_empty());
    }
}
