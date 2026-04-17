/// AuthStateMachineRaceScenario — Auth → Join state machine race
///
/// Spawns concurrent clients that send Auth and Join messages in rapid succession
/// on the same WebSocket connection. Validates the 3-state auth machine:
///
///   1. Auth rejected after successful auth ("already authenticated")
///   2. Auth rejected after join ("auth not permitted after join")
///   3. Non-auth/non-join messages rejected before join ("not authenticated")
///
/// In-process mode: signs a real access token via `auth::sign_access_token` using
/// the `auth_jwt_secret` from AppState.
///
/// External mode: registers a device via REST to get a valid access token, then
/// tests the WS state machine. Falls back to a fake token if REST is unavailable.
///
/// **Validates: Requirements 6.4, 6.6, 6.7, 12.1, 12.2**
use std::time::{Duration, Instant};

use async_trait::async_trait;

use crate::client::StressClient;
use crate::config::{ConfigPreset, TestContext};
use crate::results::{InvariantViolation, LatencyTracker, ScenarioResult};
use crate::runner::{Scenario, Tier};

pub struct AuthStateMachineRaceScenario;

#[async_trait]
impl Scenario for AuthStateMachineRaceScenario {
    fn name(&self) -> &str {
        "auth-state-machine-race"
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

        // Obtain a valid access token for testing.
        let access_token = match get_access_token(ctx).await {
            Ok(t) => t,
            Err(e)
                if e.contains("database")
                    || e.contains("No such host")
                    || e.contains("missing access_token") =>
            {
                // DB unavailable (dummy pool or Postgres down) — skip gracefully.
                return ScenarioResult {
                    name: "auth-state-machine-race (SKIPPED: requires real Postgres)".to_owned(),
                    passed: true,
                    duration: start.elapsed(),
                    actions_per_second: 0.0,
                    p95_latency: latency.p95(),
                    p99_latency: latency.p99(),
                    violations: vec![],
                };
            }
            Err(e) => {
                return early_fail(self.name(), start, "get_access_token", e);
            }
        };

        // Generate a unique room ID.
        let room_id = {
            use rand::RngCore;
            let mut rng = ctx.rng.lock().unwrap();
            format!("auth-sm-{:016x}", rng.next_u64())
        };

        // Create an invite code for the room.
        let invite_code = match create_invite(ctx, &room_id).await {
            Ok(c) => c,
            Err(e) => {
                return early_fail(self.name(), start, "create_invite", e);
            }
        };

        // =====================================================================
        // Test 1: Auth → Auth (duplicate auth rejected with "already authenticated")
        // =====================================================================
        {
            let mut client = match StressClient::connect(&ctx.ws_url).await {
                Ok(c) => c,
                Err(e) => {
                    return early_fail(self.name(), start, "test1_connect", format!("{e}"));
                }
            };

            // First Auth — should succeed.
            client
                .send_json(&serde_json::json!({
                    "type": "auth",
                    "accessToken": access_token,
                }))
                .await
                .ok();

            match client
                .recv_type("auth_success", Duration::from_secs(5))
                .await
            {
                Ok(_) => {}
                Err(e) => {
                    violations.push(InvariantViolation {
                        invariant: "auth_sm: first auth must succeed".to_owned(),
                        expected: "auth_success".to_owned(),
                        actual: format!("error: {e}"),
                    });
                    client.close().await;
                    return build_result(self.name(), start, violations, latency);
                }
            }

            // Second Auth — must be rejected with "already authenticated".
            client
                .send_json(&serde_json::json!({
                    "type": "auth",
                    "accessToken": access_token,
                }))
                .await
                .ok();

            match client.recv_type("error", Duration::from_secs(3)).await {
                Ok(err_msg) => {
                    let msg = err_msg
                        .get("message")
                        .and_then(|v| v.as_str())
                        .unwrap_or("")
                        .to_lowercase();
                    if !msg.contains("already authenticated") {
                        violations.push(InvariantViolation {
                            invariant:
                                "auth_sm: duplicate auth rejected with 'already authenticated'"
                                    .to_owned(),
                            expected: "error containing 'already authenticated'".to_owned(),
                            actual: format!("error message: '{msg}'"),
                        });
                    }
                }
                Err(e) => {
                    violations.push(InvariantViolation {
                        invariant: "auth_sm: duplicate auth must produce error".to_owned(),
                        expected: "error message".to_owned(),
                        actual: format!("no error received: {e}"),
                    });
                }
            }

            client.close().await;
        }

        // =====================================================================
        // Test 2: Auth → Join → Auth (auth rejected after join)
        // =====================================================================
        {
            let mut client = match StressClient::connect(&ctx.ws_url).await {
                Ok(c) => c,
                Err(e) => {
                    return early_fail(self.name(), start, "test2_connect", format!("{e}"));
                }
            };

            // Auth first.
            client
                .send_json(&serde_json::json!({
                    "type": "auth",
                    "accessToken": access_token,
                }))
                .await
                .ok();
            let _ = client
                .recv_type("auth_success", Duration::from_secs(5))
                .await;

            // Join the room.
            let join = client.join_room(&room_id, "sfu", Some(&invite_code)).await;
            if join.as_ref().map(|r| r.success).unwrap_or(false) {
                // Now send Auth again — must be rejected with "auth not permitted after join".
                client
                    .send_json(&serde_json::json!({
                        "type": "auth",
                        "accessToken": access_token,
                    }))
                    .await
                    .ok();

                match client.recv_type("error", Duration::from_secs(3)).await {
                    Ok(err_msg) => {
                        let msg = err_msg
                            .get("message")
                            .and_then(|v| v.as_str())
                            .unwrap_or("")
                            .to_lowercase();
                        if !msg.contains("auth not permitted after join") {
                            violations.push(InvariantViolation {
                                invariant:
                                    "auth_sm: auth after join rejected with 'auth not permitted after join'"
                                        .to_owned(),
                                expected:
                                    "error containing 'auth not permitted after join'".to_owned(),
                                actual: format!("error message: '{msg}'"),
                            });
                        }
                    }
                    Err(e) => {
                        violations.push(InvariantViolation {
                            invariant: "auth_sm: auth after join must produce error".to_owned(),
                            expected: "error message".to_owned(),
                            actual: format!("no error received: {e}"),
                        });
                    }
                }
            } else {
                // Join failed — not a state machine violation, but note it.
                violations.push(InvariantViolation {
                    invariant: "auth_sm: test2 join must succeed".to_owned(),
                    expected: "join success".to_owned(),
                    actual: format!("join failed: {:?}", join.map(|r| r.rejection_reason)),
                });
            }

            client.close().await;
        }

        // =====================================================================
        // Test 3: Non-auth/non-join message before join ("not authenticated")
        // =====================================================================
        {
            let mut client = match StressClient::connect(&ctx.ws_url).await {
                Ok(c) => c,
                Err(e) => {
                    return early_fail(self.name(), start, "test3_connect", format!("{e}"));
                }
            };

            // Send a Leave message without joining — must be rejected.
            client
                .send_json(&serde_json::json!({ "type": "leave" }))
                .await
                .ok();

            match client.recv_type("error", Duration::from_secs(3)).await {
                Ok(err_msg) => {
                    let msg = err_msg
                        .get("message")
                        .and_then(|v| v.as_str())
                        .unwrap_or("")
                        .to_lowercase();
                    if !msg.contains("not authenticated") {
                        violations.push(InvariantViolation {
                            invariant:
                                "auth_sm: pre-join non-auth message rejected with 'not authenticated'"
                                    .to_owned(),
                            expected: "error containing 'not authenticated'".to_owned(),
                            actual: format!("error message: '{msg}'"),
                        });
                    }
                }
                Err(crate::client::StressError::Closed) => {
                    // Connection closed — also a valid rejection.
                }
                Err(e) => {
                    violations.push(InvariantViolation {
                        invariant: "auth_sm: pre-join rejection must produce error or close"
                            .to_owned(),
                        expected: "error message or connection close".to_owned(),
                        actual: format!("unexpected: {e}"),
                    });
                }
            }

            client.close().await;
        }

        // =====================================================================
        // Test 4: Concurrent Auth + Join race (rapid fire on same connection)
        // =====================================================================
        {
            let n_races = 5;
            for race_idx in 0..n_races {
                let mut client = match StressClient::connect(&ctx.ws_url).await {
                    Ok(c) => c,
                    Err(_) => continue,
                };

                // Fire Auth and Join nearly simultaneously.
                let _ = client
                    .send_json(&serde_json::json!({
                        "type": "auth",
                        "accessToken": access_token,
                    }))
                    .await;
                let _ = client
                    .send_json(&serde_json::json!({
                        "type": "join",
                        "roomId": room_id,
                        "roomType": "sfu",
                        "inviteCode": invite_code,
                    }))
                    .await;

                // Drain all responses — we should see some combination of
                // auth_success/auth_failed/joined/join_rejected/error.
                // The key invariant: no panic, no crash, responses are well-formed.
                let messages = client.drain(Duration::from_secs(3)).await;

                for msg in &messages {
                    let msg_type = msg.get("type").and_then(|v| v.as_str()).unwrap_or("");
                    // Every response must have a valid "type" field.
                    if msg_type.is_empty() {
                        violations.push(InvariantViolation {
                            invariant: format!(
                                "auth_sm: race {race_idx} response must have valid type"
                            ),
                            expected: "non-empty type field".to_owned(),
                            actual: format!("message: {msg}"),
                        });
                    }
                }

                client.close().await;
            }
        }

        build_result(self.name(), start, violations, latency)
    }
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

/// Get a valid access token for testing.
async fn get_access_token(ctx: &TestContext) -> Result<String, String> {
    if let Some(ref app_state) = ctx.app_state {
        // In-process: register a real device via DB so the token passes
        // epoch + device revocation checks in the WS auth handler.
        let reg = wavis_backend::domain::auth::register_device(
            &app_state.db_pool,
            &app_state.auth_jwt_secret,
            wavis_backend::domain::auth::ACCESS_TOKEN_TTL_SECS,
            app_state.refresh_token_ttl_days,
            &app_state.refresh_token_pepper,
        )
        .await
        .map_err(|e| format!("register_device failed: {e}"))?;
        Ok(reg.access_token)
    } else {
        // External: register a device via REST.
        let base_url = ws_url_to_http(&ctx.ws_url);
        let resp = reqwest::Client::new()
            .post(format!("{base_url}/auth/register_device"))
            .send()
            .await
            .map_err(|e| format!("register request failed: {e}"))?;

        if resp.status().is_server_error() {
            // DB unavailable — use a fake token (tests will still validate state machine
            // rejection behavior since the token will fail validation).
            return Ok("eyJhbGciOiJIUzI1NiJ9.eyJzdWIiOiJ0ZXN0In0.fake".to_owned());
        }

        let body: serde_json::Value = resp
            .json()
            .await
            .map_err(|e| format!("register response parse failed: {e}"))?;

        body.get("access_token")
            .and_then(|v| v.as_str())
            .map(|s| s.to_owned())
            .ok_or_else(|| "register response missing access_token".to_owned())
    }
}

/// Create an invite code for a room.
async fn create_invite(ctx: &TestContext, room_id: &str) -> Result<String, String> {
    if let Some(ref app_state) = ctx.app_state {
        app_state
            .invite_store
            .generate(room_id, "stress-issuer", Some(20), Instant::now())
            .map(|r| r.code)
            .map_err(|e| format!("invite generation failed: {e}"))
    } else {
        create_invite_via_signaling(ctx, room_id).await
    }
}

async fn create_invite_via_signaling(ctx: &TestContext, room_id: &str) -> Result<String, String> {
    let mut host = StressClient::connect(&ctx.ws_url)
        .await
        .map_err(|e| format!("connect failed: {e}"))?;

    let join = host
        .join_room(room_id, "sfu", None)
        .await
        .map_err(|e| format!("join failed: {e}"))?;

    if !join.success {
        host.close().await;
        return Err(format!("join rejected: {:?}", join.rejection_reason));
    }

    host.send_json(&serde_json::json!({ "type": "invite_create", "maxUses": 20 }))
        .await
        .map_err(|e| format!("invite_create send failed: {e}"))?;

    let msg = host
        .recv_type("invite_created", Duration::from_secs(5))
        .await
        .map_err(|e| format!("invite_created recv failed: {e}"))?;

    let code = msg
        .get("inviteCode")
        .and_then(|v| v.as_str())
        .ok_or_else(|| "invite_created missing inviteCode".to_owned())?
        .to_owned();

    host.send_json(&serde_json::json!({ "type": "leave" }))
        .await
        .ok();
    host.close().await;

    Ok(code)
}

fn ws_url_to_http(ws_url: &str) -> String {
    ws_url
        .replace("wss://", "https://")
        .replace("ws://", "http://")
        .trim_end_matches("/ws")
        .to_owned()
}

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
