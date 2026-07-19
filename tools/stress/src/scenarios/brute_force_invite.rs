/// BruteForceInviteScenario — Property 3: Rate limit counter accuracy
///
/// Spawns N concurrent clients each attempting to join with a random invalid invite code
/// (generated from a seeded RNG for reproducibility). Asserts that the rate limiter fires
/// and the `join_rate_limit_rejections` counter increases by at least 1.
///
/// All join attempts should be rejected — either `InviteInvalid` (code not found) or
/// `RateLimited` (rate limiter triggered). The key assertion is that the rate limiter
/// counter increases, proving the rate limiter is active under brute-force conditions.
///
/// **Validates: Requirements 2.1, 2.8**
use std::time::{Duration, Instant};

use async_trait::async_trait;
use rand::RngCore;
use tokio::task::JoinSet;

use crate::assertions::fetch_metrics;
use crate::client::StressClient;
use crate::config::{Capability, ConfigPreset, TestContext};
use crate::results::{InvariantViolation, LatencyTracker, ScenarioResult};
use crate::runner::{Scenario, Tier};

pub struct BruteForceInviteScenario;

/// Room type hint — SFU rooms have capacity 6 (matching the product's max group size).
const ROOM_TYPE: &str = "sfu";

#[async_trait]
impl Scenario for BruteForceInviteScenario {
    fn name(&self) -> &str {
        "brute-force-invite"
    }

    fn tier(&self) -> Tier {
        Tier::Tier1
    }

    fn requires(&self) -> Vec<Capability> {
        // No special capabilities required — P2P rooms are always available.
        vec![]
    }

    fn config_preset(&self) -> ConfigPreset {
        // Real rate limits — this scenario specifically tests rate limiter behaviour.
        ConfigPreset::BruteForce
    }

    async fn run(&self, ctx: &TestContext) -> ScenarioResult {
        let start = Instant::now();
        let mut violations: Vec<InvariantViolation> = Vec::new();
        let mut latency = LatencyTracker::new();

        // --- Generate a unique room ID for this run ---
        let room_id = {
            let mut rng = ctx.rng.lock().unwrap();
            format!("brute-room-{:016x}", rng.next_u64())
        };

        // --- Optionally create a valid room+invite (in-process mode only) ---
        // We create a real room so the backend has a valid room to rate-limit against.
        // In external mode we skip this and just use the room_id directly.
        if let Some(ref app_state) = ctx.app_state {
            let _ =
                app_state
                    .invite_store
                    .generate(&room_id, "stress-issuer", None, Instant::now());
        }

        // --- Pre-generate random invalid invite codes from the seeded RNG ---
        let n_clients = ctx.scale.concurrent_clients.min(50);
        let invite_codes: Vec<String> = {
            let mut rng = ctx.rng.lock().unwrap();
            (0..n_clients)
                .map(|_| {
                    let mut bytes = [0u8; 16];
                    rng.fill_bytes(&mut bytes);
                    // Encode as lowercase hex — no base64 dependency needed.
                    bytes.iter().map(|b| format!("{b:02x}")).collect::<String>()
                })
                .collect()
        };

        // --- Snapshot baseline abuse metrics before the flood ---
        let metrics_token = std::env::var("TEST_METRICS_TOKEN").unwrap_or_default();
        let baseline_metrics = fetch_metrics(&ctx.http_client, &ctx.metrics_url, &metrics_token)
            .await
            .unwrap_or(serde_json::Value::Null);

        // --- Spawn N concurrent clients, each using a different random invalid code ---
        let mut join_set: JoinSet<ClientOutcome> = JoinSet::new();

        for code in invite_codes {
            let ws_url = ctx.ws_url.clone();
            let room_id = room_id.clone();

            join_set.spawn(async move {
                let t0 = Instant::now();

                let mut client = match StressClient::connect(&ws_url).await {
                    Ok(c) => c,
                    Err(e) => {
                        return ClientOutcome {
                            rejection_reason: Some(format!("connect_error: {e}")),
                            latency: t0.elapsed(),
                        };
                    }
                };

                let result = client.join_room(&room_id, ROOM_TYPE, Some(&code)).await;
                let elapsed = t0.elapsed();
                client.close().await;

                match result {
                    Ok(join_result) => ClientOutcome {
                        rejection_reason: join_result.rejection_reason,
                        latency: elapsed,
                    },
                    Err(e) => ClientOutcome {
                        rejection_reason: Some(format!("join_error: {e}")),
                        latency: elapsed,
                    },
                }
            });
        }

        // --- Collect results ---
        let mut rate_limited_count: usize = 0;
        let mut unexpected_success_count: usize = 0;

        while let Some(outcome) = join_set.join_next().await {
            match outcome {
                Ok(o) => {
                    latency.record(o.latency);
                    match o.rejection_reason.as_deref() {
                        // Expected: rate limiter fired
                        Some("rate_limited") | Some("RateLimited") => rate_limited_count += 1,
                        // Unexpected: join succeeded with a random invalid code
                        None => unexpected_success_count += 1,
                        // invite_invalid, connect errors, etc. — all expected rejections
                        _ => {}
                    }
                }
                Err(e) => {
                    violations.push(InvariantViolation {
                        invariant: "task_panic".to_owned(),
                        expected: "no panics".to_owned(),
                        actual: format!("task panicked: {e}"),
                    });
                }
            }
        }

        // --- No join should have succeeded with a random invalid code ---
        if unexpected_success_count > 0 {
            violations.push(InvariantViolation {
                invariant: "brute_force: no join succeeds with random invalid code".to_owned(),
                expected: "0 successes".to_owned(),
                actual: format!("{unexpected_success_count} unexpected successes"),
            });
        }

        // --- Give the backend a moment to flush counters ---
        tokio::time::sleep(Duration::from_millis(200)).await;

        // --- Property 3: Rate limit counter accuracy ---
        // Assert that join_rate_limit_rejections increased by at least 1.
        match fetch_metrics(&ctx.http_client, &ctx.metrics_url, &metrics_token).await {
            Ok(current_metrics) => {
                let baseline_rl = baseline_metrics
                    .get("abuse_metrics")
                    .and_then(|m| m.get("join_rate_limit_rejections"))
                    .and_then(|v| v.as_u64())
                    .unwrap_or(0);
                let current_rl = current_metrics
                    .get("abuse_metrics")
                    .and_then(|m| m.get("join_rate_limit_rejections"))
                    .and_then(|v| v.as_u64())
                    .unwrap_or(0);
                let rl_delta = current_rl.saturating_sub(baseline_rl);

                // The rate limiter must have fired at least once during the brute-force flood.
                if rl_delta < 1 {
                    violations.push(InvariantViolation {
                        invariant:
                            "rate_limit_counter_accuracy: join_rate_limit_rejections must increase"
                                .to_owned(),
                        expected: ">= 1".to_owned(),
                        actual: rl_delta.to_string(),
                    });
                }

                // Also assert that the counter delta matches the number of rate-limited
                // responses observed by clients (Property 3 exact accuracy).
                if rate_limited_count > 0 && rl_delta < rate_limited_count as u64 {
                    violations.push(InvariantViolation {
                        invariant: "rate_limit_counter_accuracy: counter delta >= client-observed rate-limited count".to_owned(),
                        expected: format!(">= {rate_limited_count}"),
                        actual: rl_delta.to_string(),
                    });
                }
            }
            Err(e) => {
                violations.push(InvariantViolation {
                    invariant: "metrics_endpoint_reachable".to_owned(),
                    expected: "metrics endpoint responds".to_owned(),
                    actual: format!("fetch failed: {e}"),
                });
            }
        }

        let duration = start.elapsed();
        let actions_per_second = if duration.as_secs_f64() > 0.0 {
            n_clients as f64 / duration.as_secs_f64()
        } else {
            0.0
        };

        ScenarioResult {
            name: self.name().to_owned(),
            passed: violations.is_empty(),
            duration,
            actions_per_second,
            p95_latency: latency.p95(),
            p99_latency: latency.p99(),
            violations,
        }
    }
}

/// Outcome of a single concurrent client's join attempt.
struct ClientOutcome {
    rejection_reason: Option<String>,
    latency: Duration,
}
