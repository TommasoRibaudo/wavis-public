/// OversizedPayloadScenario — Property 21: Oversized frame rejection
///                             Property 22: Deep JSON rejection
///                             Property 23: Malformed JSON resilience
///
/// Validates that:
///   P21) A WebSocket frame exceeding 64 KB is rejected and the connection is closed,
///        and `Abuse_Metrics.payload_size_violations` counter increases.
///   P22) A JSON message with nesting depth > 32 is rejected and the connection is closed.
///   P23) Malformed JSON strings produce parse errors without crashing the backend,
///        and the connection remains open (backend is resilient).
///
/// `config_preset`: `Default` — real production limits are exercised.
///
/// **Validates: Requirements 7.2, 7.3, 7.4**
use std::time::{Duration, Instant};

use async_trait::async_trait;

use crate::assertions::{assert_counter_delta, fetch_metrics};
use crate::client::StressClient;
use crate::config::{Capability, ConfigPreset, TestContext};
use crate::results::{InvariantViolation, LatencyTracker, ScenarioResult};
use crate::runner::{Scenario, Tier};

pub struct OversizedPayloadScenario;

/// Frame size that exceeds the 64 KB backend limit (65 537 bytes of 'A').
const OVERSIZED_FRAME_SIZE: usize = 65_537;

/// JSON nesting depth that exceeds the backend's max-depth limit of 32.
const DEEP_JSON_DEPTH: usize = 33;

/// How long to wait for the backend to close a connection after a bad frame.
const CLOSE_TIMEOUT: Duration = Duration::from_secs(5);

#[async_trait]
impl Scenario for OversizedPayloadScenario {
    fn name(&self) -> &str {
        "oversized-payload"
    }

    fn tier(&self) -> Tier {
        Tier::Tier2
    }

    fn requires(&self) -> Vec<Capability> {
        vec![]
    }

    fn config_preset(&self) -> ConfigPreset {
        ConfigPreset::Default
    }

    async fn run(&self, ctx: &TestContext) -> ScenarioResult {
        let start = Instant::now();
        let mut violations: Vec<InvariantViolation> = Vec::new();
        let latency = LatencyTracker::new();

        let metrics_token = std::env::var("TEST_METRICS_TOKEN").unwrap_or_default();

        // =====================================================================
        // Property 21: Oversized frame rejection
        // =====================================================================
        // Snapshot baseline before the oversized frame test.
        let baseline_p21 = fetch_metrics(&ctx.http_client, &ctx.metrics_url, &metrics_token)
            .await
            .unwrap_or(serde_json::Value::Null);

        let p21_result = run_p21_oversized_frame(ctx).await;
        match p21_result {
            Ok(()) => {}
            Err(v) => violations.push(v),
        }

        // Give the backend a moment to flush atomic counters.
        tokio::time::sleep(Duration::from_millis(300)).await;

        // Assert payload_size_violations counter increased.
        match fetch_metrics(&ctx.http_client, &ctx.metrics_url, &metrics_token).await {
            Ok(current) => {
                if let Some(v) =
                    assert_counter_delta(&baseline_p21, &current, "payload_size_violations", 1)
                {
                    violations.push(v);
                }
            }
            Err(e) => {
                violations.push(InvariantViolation {
                    invariant: "property_21: metrics_endpoint_reachable_after_oversized_frame"
                        .to_owned(),
                    expected: "metrics endpoint responds".to_owned(),
                    actual: format!("fetch failed: {e}"),
                });
            }
        }

        // =====================================================================
        // Property 22: Deep JSON rejection
        // =====================================================================
        let p22_result = run_p22_deep_json(ctx).await;
        match p22_result {
            Ok(()) => {}
            Err(v) => violations.push(v),
        }

        // =====================================================================
        // Property 23: Malformed JSON resilience
        // =====================================================================
        let p23_violations = run_p23_malformed_json(ctx).await;
        violations.extend(p23_violations);

        build_result(self.name(), start, violations, latency)
    }
}

// ---------------------------------------------------------------------------
// P21 — Oversized frame
// ---------------------------------------------------------------------------

/// Connect a fresh client, send a >64 KB text frame, assert the connection is closed.
async fn run_p21_oversized_frame(ctx: &TestContext) -> Result<(), InvariantViolation> {
    // Retry connection with backoff — earlier stress scenarios may have exhausted
    // the global WS rate limiter for the current second.
    let mut client = None;
    for attempt in 0..5u32 {
        match StressClient::connect(&ctx.ws_url).await {
            Ok(c) => {
                client = Some(c);
                break;
            }
            Err(_) if attempt < 4 => {
                tokio::time::sleep(Duration::from_millis(500 * (attempt as u64 + 1))).await;
            }
            Err(e) => {
                return Err(InvariantViolation {
                    invariant: "property_21: connect_for_oversized_frame_test".to_owned(),
                    expected: "connection succeeds".to_owned(),
                    actual: format!("connect failed after retries: {e}"),
                });
            }
        }
    }
    let mut client = client.unwrap();

    // Build a text frame that is just over 64 KB.
    let oversized_payload = "A".repeat(OVERSIZED_FRAME_SIZE);

    // Send the oversized frame. The send itself may succeed (the frame is buffered
    // locally) or fail immediately if the backend closes the connection first.
    let _ = client.send_raw(&oversized_payload).await;

    // The backend should close the connection. Drain with a timeout to detect closure.
    let closed = wait_for_close(client, CLOSE_TIMEOUT).await;

    if closed {
        Ok(())
    } else {
        Err(InvariantViolation {
            invariant: "property_21: oversized_frame_closes_connection".to_owned(),
            expected: "backend closes connection after >64 KB frame".to_owned(),
            actual: format!(
                "connection remained open after sending {OVERSIZED_FRAME_SIZE}-byte frame"
            ),
        })
    }
}

// ---------------------------------------------------------------------------
// P22 — Deep JSON rejection
// ---------------------------------------------------------------------------

/// Connect a fresh client, send a deeply nested JSON object (depth > 32), assert
/// the connection is closed or an error is returned.
async fn run_p22_deep_json(ctx: &TestContext) -> Result<(), InvariantViolation> {
    // Retry connection with backoff — earlier stress scenarios may have exhausted
    // the global WS rate limiter for the current second.
    let mut client = None;
    for attempt in 0..5u32 {
        match StressClient::connect(&ctx.ws_url).await {
            Ok(c) => {
                client = Some(c);
                break;
            }
            Err(_) if attempt < 4 => {
                tokio::time::sleep(Duration::from_millis(500 * (attempt as u64 + 1))).await;
            }
            Err(e) => {
                return Err(InvariantViolation {
                    invariant: "property_22: connect_for_deep_json_test".to_owned(),
                    expected: "connection succeeds".to_owned(),
                    actual: format!("connect failed after retries: {e}"),
                });
            }
        }
    }
    let mut client = client.unwrap();

    // Build a deeply nested JSON string: {"a":{"a":{"a":...}}} at DEEP_JSON_DEPTH levels.
    let deep_json = build_deep_json(DEEP_JSON_DEPTH);

    // Send the deeply nested JSON.
    let send_result = client.send_raw(&deep_json).await;

    if send_result.is_err() {
        // Connection already closed by the backend — this is acceptable.
        return Ok(());
    }

    // Wait for the backend to either close the connection or return an error message.
    let closed = wait_for_close_or_error(client, CLOSE_TIMEOUT).await;

    if closed {
        Ok(())
    } else {
        Err(InvariantViolation {
            invariant: "property_22: deep_json_closes_connection_or_returns_error".to_owned(),
            expected: format!(
                "backend closes connection or returns error for JSON depth > 32 (sent depth {DEEP_JSON_DEPTH})"
            ),
            actual: "connection remained open with no error response".to_owned(),
        })
    }
}

// ---------------------------------------------------------------------------
// P23 — Malformed JSON resilience
// ---------------------------------------------------------------------------

/// Connect a fresh client, join the room, send various malformed JSON strings,
/// assert the backend returns parse errors without crashing, and the connection
/// remains open throughout.
async fn run_p23_malformed_json(ctx: &TestContext) -> Vec<InvariantViolation> {
    let mut violations = Vec::new();

    // Generate a unique room ID for this sub-test.
    let room_id = {
        use rand::RngCore;
        let mut rng = ctx.rng.lock().unwrap();
        format!("malformed-{:016x}", rng.next_u64())
    };

    // Create an invite code.
    let invite_code = match &ctx.app_state {
        Some(app_state) => {
            match app_state.invite_store.generate(
                &room_id,
                "stress-issuer",
                Some(5),
                Instant::now(),
            ) {
                Ok(r) => r.code,
                Err(e) => {
                    violations.push(InvariantViolation {
                        invariant: "property_23: invite_creation".to_owned(),
                        expected: "invite created".to_owned(),
                        actual: format!("{e}"),
                    });
                    return violations;
                }
            }
        }
        None => match create_invite_via_signaling(ctx, &room_id).await {
            Ok(c) => c,
            Err(e) => {
                violations.push(InvariantViolation {
                    invariant: "property_23: invite_creation_external".to_owned(),
                    expected: "invite created".to_owned(),
                    actual: e,
                });
                return violations;
            }
        },
    };

    let connect_result = {
        // Retry connection with backoff — earlier stress scenarios may have exhausted
        // the global WS rate limiter for the current second.
        let mut result = Err(String::new());
        for attempt in 0..5u32 {
            match StressClient::connect(&ctx.ws_url).await {
                Ok(c) => {
                    result = Ok(c);
                    break;
                }
                Err(e) if attempt < 4 => {
                    tokio::time::sleep(Duration::from_millis(500 * (attempt as u64 + 1))).await;
                    result = Err(format!("{e}"));
                }
                Err(e) => {
                    result = Err(format!("{e}"));
                }
            }
        }
        result
    };
    let mut client = match connect_result {
        Ok(c) => c,
        Err(e) => {
            violations.push(InvariantViolation {
                invariant: "property_23: connect_for_malformed_json_test".to_owned(),
                expected: "connection succeeds".to_owned(),
                actual: format!("connect failed after retries: {e}"),
            });
            return violations;
        }
    };

    // Join the room so we have an authenticated session.
    let join = match client.join_room(&room_id, "p2p", Some(&invite_code)).await {
        Ok(r) => r,
        Err(e) => {
            client.close().await;
            violations.push(InvariantViolation {
                invariant: "property_23: join_for_malformed_json_test".to_owned(),
                expected: "join succeeds".to_owned(),
                actual: format!("join failed: {e}"),
            });
            return violations;
        }
    };

    if !join.success {
        client.close().await;
        violations.push(InvariantViolation {
            invariant: "property_23: join_for_malformed_json_test".to_owned(),
            expected: "join succeeds".to_owned(),
            actual: format!("join rejected: {:?}", join.rejection_reason),
        });
        return violations;
    }

    // Malformed JSON inputs to test.
    let malformed_inputs: &[&str] = &[
        "{",            // truncated object
        "null",         // valid JSON but not an object — not a valid SignalingMessage
        "\"string\"",   // valid JSON string — not a valid SignalingMessage
        "[]",           // valid JSON array — not a valid SignalingMessage
        "{invalid}",    // invalid JSON
        "\x00\x01\x02", // binary garbage as text
        "}{",           // reversed braces
        "{\"type\":}",  // missing value
    ];

    for input in malformed_inputs {
        let send_result = client.send_raw(input).await;

        if send_result.is_err() {
            // Connection was closed — this is a violation for P23 (backend should be resilient).
            violations.push(InvariantViolation {
                invariant: "property_23: connection_remains_open_after_malformed_json".to_owned(),
                expected: "connection stays open after malformed JSON".to_owned(),
                actual: format!("connection closed after sending: {input:?}"),
            });
            // Can't continue testing — connection is gone.
            return violations;
        }

        // Give the backend a brief moment to respond.
        tokio::time::sleep(Duration::from_millis(50)).await;

        // Drain any pending messages (error responses are fine, closure is not).
        let drained = client.drain(Duration::from_millis(100)).await;

        // Check if the backend sent a close frame (would be detected as empty drain after error).
        // We check by attempting another send — if it fails, the connection was closed.
        let probe = client
            .send_json(&serde_json::json!({ "type": "ping" }))
            .await;

        if probe.is_err() {
            violations.push(InvariantViolation {
                invariant: "property_23: connection_remains_open_after_malformed_json".to_owned(),
                expected: "connection stays open after malformed JSON".to_owned(),
                actual: format!(
                    "connection closed after sending malformed input: {input:?} (drained {} messages before close)",
                    drained.len()
                ),
            });
            // Connection is gone — stop testing.
            return violations;
        }
    }

    // Final check: backend is still reachable after all malformed inputs.
    let metrics_token = std::env::var("TEST_METRICS_TOKEN").unwrap_or_default();
    match fetch_metrics(&ctx.http_client, &ctx.metrics_url, &metrics_token).await {
        Ok(_) => {}
        Err(e) => {
            violations.push(InvariantViolation {
                invariant: "property_23: backend_reachable_after_malformed_json".to_owned(),
                expected: "metrics endpoint responds after malformed JSON test".to_owned(),
                actual: format!("fetch failed: {e}"),
            });
        }
    }

    client.close().await;
    violations
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

/// Build a deeply nested JSON string of the form `{"a":{"a":...{"a":{}}...}}`.
fn build_deep_json(depth: usize) -> String {
    let mut s = String::with_capacity(depth * 6 + 2);
    for _ in 0..depth {
        s.push_str("{\"a\":");
    }
    s.push_str("{}");
    for _ in 0..depth {
        s.push('}');
    }
    s
}

/// Drain the client until the connection is closed or the timeout expires.
/// Returns `true` if the connection was closed (by the backend or due to an error).
async fn wait_for_close(mut client: StressClient, timeout: Duration) -> bool {
    tokio::time::timeout(timeout, async {
        loop {
            match client.drain(Duration::from_millis(100)).await {
                msgs if msgs.is_empty() => {
                    // No messages — try a probe send to check if connection is alive.
                    let probe = client
                        .send_json(&serde_json::json!({ "type": "ping" }))
                        .await;
                    if probe.is_err() {
                        return true; // connection closed
                    }
                    tokio::time::sleep(Duration::from_millis(100)).await;
                }
                _ => {
                    // Got messages — keep draining.
                }
            }
        }
    })
    .await
    .unwrap_or_default()
}

/// Wait for the connection to be closed OR for an error message to be received.
/// Returns `true` if either condition is met within the timeout.
async fn wait_for_close_or_error(mut client: StressClient, timeout: Duration) -> bool {
    tokio::time::timeout(timeout, async {
        loop {
            // Try a probe send first.
            let probe = client
                .send_json(&serde_json::json!({ "type": "ping" }))
                .await;
            if probe.is_err() {
                return true; // connection closed
            }

            // Drain and check for error messages.
            let msgs = client.drain(Duration::from_millis(200)).await;
            if msgs.is_empty() {
                // No response — wait a bit and retry.
                tokio::time::sleep(Duration::from_millis(100)).await;
                continue;
            }

            for msg in &msgs {
                let t = msg.get("type").and_then(|v| v.as_str()).unwrap_or("");
                if t == "error" || t == "Error" {
                    return true; // got an error response
                }
            }

            // Check if connection is still alive after drain.
            let probe2 = client
                .send_json(&serde_json::json!({ "type": "ping" }))
                .await;
            if probe2.is_err() {
                return true; // connection closed
            }

            tokio::time::sleep(Duration::from_millis(100)).await;
        }
    })
    .await
    .unwrap_or_default()
}

fn build_result(
    name: &str,
    start: Instant,
    violations: Vec<InvariantViolation>,
    latency: LatencyTracker,
) -> ScenarioResult {
    let duration = start.elapsed();
    // Approximate action count: 1 oversized frame + 1 deep JSON + 8 malformed inputs.
    let actions = 10.0_f64;
    ScenarioResult {
        name: name.to_owned(),
        passed: violations.is_empty(),
        duration,
        actions_per_second: if duration.as_secs_f64() > 0.0 {
            actions / duration.as_secs_f64()
        } else {
            0.0
        },
        p95_latency: latency.p95(),
        p99_latency: latency.p99(),
        violations,
    }
}

/// External-mode: connect a client, join the room as first joiner (host),
/// request an invite code, then leave.
async fn create_invite_via_signaling(ctx: &TestContext, room_id: &str) -> Result<String, String> {
    let mut host = StressClient::connect(&ctx.ws_url)
        .await
        .map_err(|e| format!("connect failed: {e}"))?;

    let join = host
        .join_room(room_id, "p2p", None)
        .await
        .map_err(|e| format!("join failed: {e}"))?;

    if !join.success {
        host.close().await;
        return Err(format!("join rejected: {:?}", join.rejection_reason));
    }

    host.send_json(&serde_json::json!({ "type": "invite_create", "maxUses": 5 }))
        .await
        .map_err(|e| format!("InviteCreate send failed: {e}"))?;

    let msg = host
        .recv_type("invite_created", Duration::from_secs(5))
        .await
        .map_err(|e| format!("InviteCreated recv failed: {e}"))?;

    let code = msg
        .get("inviteCode")
        .and_then(|v| v.as_str())
        .ok_or_else(|| "InviteCreated missing inviteCode".to_owned())?
        .to_owned();

    host.send_json(&serde_json::json!({ "type": "leave" }))
        .await
        .ok();
    host.close().await;

    Ok(code)
}

#[cfg(test)]
mod tests {
    use super::build_deep_json;

    /// Property 22: Deep JSON rejection
    ///
    /// For any depth > 0, `build_deep_json` produces a string that:
    /// - starts with `{` and ends with `}`
    /// - has exactly `depth` opening `{"a":` sequences
    ///
    /// **Validates: Requirements 7.3**
    #[test]
    fn deep_json_structure_is_correct() {
        for depth in [1, 10, 32, 33, 50] {
            let s = build_deep_json(depth);
            // Should start with `{"a":` and end with `}`
            assert!(
                s.starts_with("{\"a\":"),
                "depth={depth}: should start with {{\"a\":"
            );
            assert!(s.ends_with('}'), "depth={depth}: should end with }}");
            // Count the number of `{"a":` occurrences — should equal depth.
            let count = s.matches("{\"a\":").count();
            assert_eq!(
                count, depth,
                "depth={depth}: expected {depth} nesting levels, got {count}"
            );
        }
    }

    /// Property 21: Oversized frame size constant is above the 64 KB limit.
    ///
    /// **Validates: Requirements 7.2**
    #[test]
    fn oversized_frame_exceeds_64kb() {
        const {
            assert!(super::OVERSIZED_FRAME_SIZE > 64 * 1024);
        }
    }

    /// Property 22: Deep JSON depth constant exceeds the backend's max depth of 32.
    ///
    /// **Validates: Requirements 7.3**
    #[test]
    fn deep_json_depth_exceeds_limit() {
        const {
            assert!(super::DEEP_JSON_DEPTH > 32);
        }
    }
}
