/// CrossSecretTokenConfusionScenario — Cross-secret token confusion
///
/// Signs an auth access token with the SFU JWT secret (wrong secret), then sends
/// it via the WebSocket `Auth` message. Asserts `AuthFailed` is returned.
///
/// Tests that the two separate JWT secret namespaces (SFU vs device-auth) don't
/// cross-validate. The backend must use `auth_jwt_secret` for Auth messages and
/// `jwt_secret` for MediaTokens — mixing them must fail.
///
/// In-process mode: signs tokens directly using both secrets from AppState.
/// External mode: constructs a plausible JWT signed with a test secret and sends
/// it via WS Auth. Since the external backend's SFU secret is unknown, we test
/// with a garbage-signed token to verify AuthFailed is returned.
///
/// **Validates: Requirements 3.3, 3.6, 8.4**
use std::time::{Duration, Instant};

use async_trait::async_trait;

use crate::client::StressClient;
use crate::config::{ConfigPreset, TestContext};
use crate::results::{InvariantViolation, LatencyTracker, ScenarioResult};
use crate::runner::{Scenario, Tier};

pub struct CrossSecretTokenConfusionScenario;

#[async_trait]
impl Scenario for CrossSecretTokenConfusionScenario {
    fn name(&self) -> &str {
        "cross-secret-token-confusion"
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

        // =====================================================================
        // Build a token signed with the WRONG secret
        // =====================================================================
        let wrong_secret_token = match &ctx.app_state {
            Some(app_state) => {
                // In-process: sign an auth-format token with the SFU secret.
                let user_id = uuid::Uuid::new_v4();
                match wavis_backend::domain::auth::sign_access_token(
                    &user_id,
                    &uuid::Uuid::nil(),
                    &app_state.jwt_secret, // SFU secret, NOT auth secret
                    wavis_backend::domain::auth::ACCESS_TOKEN_TTL_SECS,
                    0,
                ) {
                    Ok(t) => t,
                    Err(e) => {
                        return early_fail(
                            self.name(),
                            start,
                            "sign_with_sfu_secret",
                            format!("{e}"),
                        );
                    }
                }
            }
            None => {
                // External: sign a token with a known test secret that differs
                // from the backend's auth secret.
                let fake_secret = b"this-is-not-the-auth-secret-32b!";
                let user_id = uuid::Uuid::new_v4();
                match wavis_backend::domain::auth::sign_access_token(
                    &user_id,
                    &uuid::Uuid::nil(),
                    fake_secret,
                    wavis_backend::domain::auth::ACCESS_TOKEN_TTL_SECS,
                    0,
                ) {
                    Ok(t) => t,
                    Err(e) => {
                        return early_fail(
                            self.name(),
                            start,
                            "sign_with_fake_secret",
                            format!("{e}"),
                        );
                    }
                }
            }
        };

        // =====================================================================
        // Test A: Send wrong-secret token via WS Auth → must get AuthFailed
        // =====================================================================
        {
            let mut client = match StressClient::connect(&ctx.ws_url).await {
                Ok(c) => c,
                Err(e) => {
                    return early_fail(self.name(), start, "connect", format!("{e}"));
                }
            };

            client
                .send_json(&serde_json::json!({
                    "type": "auth",
                    "accessToken": wrong_secret_token,
                }))
                .await
                .ok();

            match client
                .recv_type_any_of(&["auth_failed", "error"], Duration::from_secs(5))
                .await
            {
                Ok(msg) => {
                    let msg_type = msg.get("type").and_then(|v| v.as_str()).unwrap_or("");
                    if msg_type == "auth_success" {
                        violations.push(InvariantViolation {
                            invariant:
                                "cross_secret: wrong-secret token must not produce auth_success"
                                    .to_owned(),
                            expected: "auth_failed".to_owned(),
                            actual: "auth_success — secrets cross-validated".to_owned(),
                        });
                    }
                    // auth_failed or error are both acceptable rejections.
                }
                Err(crate::client::StressError::Closed) => {
                    // Connection closed — also a valid rejection.
                }
                Err(e) => {
                    violations.push(InvariantViolation {
                        invariant: "cross_secret: must receive auth_failed or error".to_owned(),
                        expected: "auth_failed or error response".to_owned(),
                        actual: format!("unexpected: {e}"),
                    });
                }
            }

            client.close().await;
        }

        // =====================================================================
        // Test B (in-process only): Verify the correct auth secret DOES work
        // Skipped when using a dummy Postgres pool (in-process without real DB).
        // =====================================================================
        if let Some(ref app_state) = ctx.app_state {
            // Register a real device so the token passes epoch + revocation checks.
            let correct_token = match wavis_backend::domain::auth::register_device(
                &app_state.db_pool,
                &app_state.auth_jwt_secret,
                wavis_backend::domain::auth::ACCESS_TOKEN_TTL_SECS,
                app_state.refresh_token_ttl_days,
                &app_state.refresh_token_pepper,
            )
            .await
            {
                Ok(reg) => reg.access_token,
                Err(_) => {
                    // DB unavailable (dummy pool in in-process mode) — skip Test B.
                    return build_result(self.name(), start, violations, latency);
                }
            };

            let mut client = match StressClient::connect(&ctx.ws_url).await {
                Ok(c) => c,
                Err(e) => {
                    return early_fail(self.name(), start, "connect_b", format!("{e}"));
                }
            };

            client
                .send_json(&serde_json::json!({
                    "type": "auth",
                    "accessToken": correct_token,
                }))
                .await
                .ok();

            match client
                .recv_type("auth_success", Duration::from_secs(5))
                .await
            {
                Ok(_) => {
                    // Correct secret accepted — good.
                }
                Err(e) => {
                    violations.push(InvariantViolation {
                        invariant: "cross_secret: correct auth secret must produce auth_success"
                            .to_owned(),
                        expected: "auth_success".to_owned(),
                        actual: format!("error: {e}"),
                    });
                }
            }

            client.close().await;
        }

        // =====================================================================
        // Test C: Completely garbage token → must get AuthFailed
        // =====================================================================
        {
            let mut client = match StressClient::connect(&ctx.ws_url).await {
                Ok(c) => c,
                Err(e) => {
                    return early_fail(self.name(), start, "connect_c", format!("{e}"));
                }
            };

            client
                .send_json(&serde_json::json!({
                    "type": "auth",
                    "accessToken": "not.a.jwt",
                }))
                .await
                .ok();

            match client
                .recv_type_any_of(&["auth_failed", "error"], Duration::from_secs(5))
                .await
            {
                Ok(_) => {
                    // Rejected — correct.
                }
                Err(crate::client::StressError::Closed) => {
                    // Also acceptable.
                }
                Err(e) => {
                    violations.push(InvariantViolation {
                        invariant: "cross_secret: garbage token must be rejected".to_owned(),
                        expected: "auth_failed or error".to_owned(),
                        actual: format!("unexpected: {e}"),
                    });
                }
            }

            client.close().await;
        }

        build_result(self.name(), start, violations, latency)
    }
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

fn early_fail(
    name: &str,
    start: Instant,
    invariant: impl Into<String>,
    actual: impl std::fmt::Display,
) -> ScenarioResult {
    ScenarioResult {
        name: name.to_owned(),
        passed: false,
        duration: start.elapsed(),
        actions_per_second: 0.0,
        p95_latency: Duration::ZERO,
        p99_latency: Duration::ZERO,
        violations: vec![InvariantViolation {
            invariant: invariant.into(),
            expected: "success".to_owned(),
            actual: actual.to_string(),
        }],
    }
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
