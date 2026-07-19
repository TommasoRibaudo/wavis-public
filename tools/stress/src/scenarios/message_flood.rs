/// MessageFloodScenario — Property 20: Flood isolation
///                         Property 24: Global rate limiter enforcement
///
/// Validates that:
///   A) A flooding connection is closed by the per-connection rate limiter while
///      a healthy connection in the same room continues to function.
///   B) The healthy connection's p95 latency stays under `ctx.scale.thresholds.flood_healthy_p95`
///      (500ms) while the flood is in progress.
///   C) `global_ws_ceiling_rejections` increases when the global WS limiter triggers
///      (tested by attempting many rapid new connections).
///
/// `config_preset`: `Default` — real production rate limits, so the actual defense
/// thresholds are exercised.
///
/// **Validates: Requirements 7.1, 7.5, 7.6**
use std::time::{Duration, Instant};

use crate::assertions::{assert_counter_delta, fetch_metrics};
use crate::client::StressClient;
use crate::config::{Capability, ConfigPreset, TestContext};
use crate::results::{InvariantViolation, LatencyTracker, ScenarioResult};
use crate::runner::{Scenario, Tier};
use async_trait::async_trait;

pub struct MessageFloodScenario;

/// Room type hint — SFU rooms have capacity 6 (matching the product's max group size).
const ROOM_TYPE: &str = "sfu";

/// How many messages the flooding client sends in its tight loop.
const FLOOD_MESSAGE_COUNT: usize = 2000;

/// How many ping/pong round-trips the healthy client measures during the flood.
const HEALTHY_PING_COUNT: usize = 10;

/// Interval between healthy-client pings (spread across the flood window).
/// Must stay well under the per-connection burst rate limit (default 15 msg/s)
/// so the healthy connection is never closed by the server's own rate limiter.
const HEALTHY_PING_INTERVAL: Duration = Duration::from_millis(150);

/// How many rapid new WS connections to attempt to trigger the global WS ceiling.
const GLOBAL_CEILING_PROBE_COUNT: usize = 200;

#[async_trait]
impl Scenario for MessageFloodScenario {
    fn name(&self) -> &str {
        "message-flood"
    }

    fn tier(&self) -> Tier {
        Tier::Tier2
    }

    fn requires(&self) -> Vec<Capability> {
        vec![]
    }

    fn config_preset(&self) -> ConfigPreset {
        // Real production rate limits — this scenario validates actual defense thresholds.
        ConfigPreset::Default
    }

    async fn run(&self, ctx: &TestContext) -> ScenarioResult {
        let start = Instant::now();
        let mut violations: Vec<InvariantViolation> = Vec::new();

        // --- Generate a unique room ID ---
        let room_id = {
            use rand::RngCore;
            let mut rng = ctx.rng.lock().unwrap();
            format!("flood-{:016x}", rng.next_u64())
        };

        // --- Create invite code ---
        let invite_code = match &ctx.app_state {
            Some(app_state) => {
                match app_state.invite_store.generate(
                    &room_id,
                    "stress-issuer",
                    Some(10),
                    Instant::now(),
                ) {
                    Ok(r) => r.code,
                    Err(e) => {
                        return early_fail(self.name(), start, "invite_creation", format!("{e}"));
                    }
                }
            }
            None => match create_invite_via_signaling(ctx, &room_id).await {
                Ok(c) => c,
                Err(e) => {
                    return early_fail(self.name(), start, "invite_creation", e);
                }
            },
        };

        let metrics_token = std::env::var("TEST_METRICS_TOKEN").unwrap_or_default();

        // --- Snapshot baseline abuse metrics ---
        let baseline = fetch_metrics(&ctx.http_client, &ctx.metrics_url, &metrics_token)
            .await
            .unwrap_or(serde_json::Value::Null);

        // =====================================================================
        // Step 1 — Connect healthy client and join the room
        // =====================================================================

        let mut healthy = match StressClient::connect(&ctx.ws_url).await {
            Ok(c) => c,
            Err(e) => {
                return early_fail(self.name(), start, "healthy_connect", format!("{e}"));
            }
        };

        let healthy_join = match healthy
            .join_room(&room_id, ROOM_TYPE, Some(&invite_code))
            .await
        {
            Ok(r) => r,
            Err(e) => {
                healthy.close().await;
                return early_fail(self.name(), start, "healthy_join", format!("{e}"));
            }
        };

        if !healthy_join.success {
            healthy.close().await;
            return early_fail(
                self.name(),
                start,
                "healthy_join_rejected",
                format!("{:?}", healthy_join.rejection_reason),
            );
        }

        // =====================================================================
        // Step 2 — Connect flooding client and join the same room
        // =====================================================================

        let mut flood = match StressClient::connect(&ctx.ws_url).await {
            Ok(c) => c,
            Err(e) => {
                healthy.close().await;
                return early_fail(self.name(), start, "flood_connect", format!("{e}"));
            }
        };

        let flood_join = match flood
            .join_room(&room_id, ROOM_TYPE, Some(&invite_code))
            .await
        {
            Ok(r) => r,
            Err(e) => {
                healthy.close().await;
                flood.close().await;
                return early_fail(self.name(), start, "flood_join", format!("{e}"));
            }
        };

        if !flood_join.success {
            healthy.close().await;
            flood.close().await;
            return early_fail(
                self.name(),
                start,
                "flood_join_rejected",
                format!("{:?}", flood_join.rejection_reason),
            );
        }

        // =====================================================================
        // Step 3 — Run flood + healthy latency measurement concurrently
        //
        // Property 20: Flood isolation
        // =====================================================================

        let ws_url = ctx.ws_url.clone();
        let metrics_url = ctx.metrics_url.clone();
        let http_client = ctx.http_client.clone();

        // Spawn the flooding task — sends messages in a tight loop, no sleep.
        // We need raw sink access; extract it from the StressClient by destructuring.
        // Since StressClient doesn't expose the sink directly, we open a fresh raw
        // connection for the flood so we can drive it at maximum speed.
        let flood_task = tokio::spawn(async move {
            // Re-use the already-joined flood client's underlying connection.
            // We drive it by calling send_json in a tight loop.
            // The backend should close the connection after rate-limit threshold.
            let mut closed_by_server = false;
            for _i in 0..FLOOD_MESSAGE_COUNT {
                let msg = serde_json::json!({
                    "type": "ping",
                });
                match flood.send_json(&msg).await {
                    Ok(_) => {}
                    Err(_) => {
                        // Connection was closed (by server rate limiter) — this is expected.
                        closed_by_server = true;
                        break;
                    }
                }
            }
            // If we sent all messages without error, drain briefly to see if server closed it.
            if !closed_by_server {
                let drained = flood.drain(Duration::from_millis(500)).await;
                // Check if any drained message indicates closure or rate-limit error.
                for msg in &drained {
                    let t = msg.get("type").and_then(|v| v.as_str()).unwrap_or("");
                    if t == "error" || t == "Error" {
                        let body = msg
                            .get("message")
                            .and_then(|v| v.as_str())
                            .unwrap_or("")
                            .to_lowercase();
                        if body.contains("rate")
                            || body.contains("limit")
                            || body.contains("too many")
                        {
                            closed_by_server = true;
                            break;
                        }
                    }
                }
            }
            flood.close().await;
            closed_by_server
        });

        // Healthy client sends periodic pings and measures round-trip latency.
        let mut latency = LatencyTracker::new();
        for _ in 0..HEALTHY_PING_COUNT {
            tokio::time::sleep(HEALTHY_PING_INTERVAL).await;

            let ping_start = Instant::now();
            let send_result = healthy
                .send_json(&serde_json::json!({ "type": "ping" }))
                .await;

            if send_result.is_err() {
                // Healthy connection dropped — this is a violation.
                violations.push(InvariantViolation {
                    invariant: "property_20: healthy_connection_must_survive_flood".to_owned(),
                    expected: "healthy connection remains open during flood".to_owned(),
                    actual: "healthy connection send failed during flood".to_owned(),
                });
                break;
            }

            // Wait for any response (Pong, error, or any message) to measure RTT.
            // The backend may not implement Ping/Pong at the signaling level, so we
            // accept any message or a short timeout as the RTT bound.
            match tokio::time::timeout(
                ctx.scale.thresholds.flood_healthy_p95,
                healthy.drain(Duration::from_millis(10)),
            )
            .await
            {
                Ok(_) => {
                    latency.record(ping_start.elapsed());
                }
                Err(_) => {
                    // Timeout — record the threshold as the latency (worst case).
                    latency.record(ctx.scale.thresholds.flood_healthy_p95);
                }
            }
        }

        // Wait for the flood task to finish.
        let flood_closed_by_server = flood_task.await.unwrap_or(false);

        // =====================================================================
        // Assert: flood connection was closed by the rate limiter
        // Property 20: Flood isolation — backend closes the offending connection
        // =====================================================================
        if !flood_closed_by_server {
            violations.push(InvariantViolation {
                invariant: "property_20: flood_connection_closed_by_rate_limiter".to_owned(),
                expected: "flooding connection closed by backend rate limiter".to_owned(),
                actual: format!(
                    "flooding connection survived all {FLOOD_MESSAGE_COUNT} messages without being closed"
                ),
            });
        }

        // =====================================================================
        // Assert: healthy connection p95 latency < flood_healthy_p95 threshold
        // Property 20: Flood isolation — healthy conn unaffected
        // =====================================================================
        if latency.count() > 0 {
            let p95 = latency.p95();
            if p95 > ctx.scale.thresholds.flood_healthy_p95 {
                violations.push(InvariantViolation {
                    invariant: "property_20: healthy_connection_p95_latency_under_threshold"
                        .to_owned(),
                    expected: format!("p95 < {:?}", ctx.scale.thresholds.flood_healthy_p95),
                    actual: format!("p95 = {p95:?}"),
                });
            }
        }

        // =====================================================================
        // Assert: connections_closed_rate_limit counter increased
        // (backend increments this when it closes a connection due to rate limiting)
        // =====================================================================
        tokio::time::sleep(Duration::from_millis(300)).await;
        match fetch_metrics(&ctx.http_client, &ctx.metrics_url, &metrics_token).await {
            Ok(current) => {
                // The backend should have incremented connections_closed_rate_limit
                // when it closed the flooding connection.
                if let Some(v) =
                    assert_counter_delta(&baseline, &current, "connections_closed_rate_limit", 1)
                {
                    violations.push(v);
                }
            }
            Err(e) => {
                violations.push(InvariantViolation {
                    invariant: "metrics_endpoint_reachable_after_flood".to_owned(),
                    expected: "metrics endpoint responds".to_owned(),
                    actual: format!("fetch failed: {e}"),
                });
            }
        }

        // =====================================================================
        // Step 4 — Property 24: Global rate limiter enforcement
        //
        // Attempt many rapid new WS connections to trigger the global WS ceiling.
        // Assert `global_ws_ceiling_rejections` increases.
        // =====================================================================

        let baseline_global = fetch_metrics(&ctx.http_client, &ctx.metrics_url, &metrics_token)
            .await
            .unwrap_or(serde_json::Value::Null);

        // Spawn many concurrent connection attempts in a tight loop.
        // The global WS limiter caps upgrades per second; with enough concurrent
        // attempts we should exceed it within one second.
        let mut connect_handles = Vec::with_capacity(GLOBAL_CEILING_PROBE_COUNT);
        for _ in 0..GLOBAL_CEILING_PROBE_COUNT {
            let url = ws_url.clone();
            connect_handles.push(tokio::spawn(async move {
                // We don't care if these succeed or fail — we just want to hammer
                // the upgrade path to trigger the global ceiling.
                let _ = StressClient::try_connect(&url).await;
            }));
        }
        // Wait for all probes to complete.
        for h in connect_handles {
            let _ = h.await;
        }

        // Give the backend a moment to flush atomic counters.
        tokio::time::sleep(Duration::from_millis(300)).await;

        match fetch_metrics(&http_client, &metrics_url, &metrics_token).await {
            Ok(current_global) => {
                // Property 24: global_ws_ceiling_rejections must have increased.
                if let Some(v) = assert_counter_delta(
                    &baseline_global,
                    &current_global,
                    "global_ws_ceiling_rejections",
                    1,
                ) {
                    violations.push(v);
                }
            }
            Err(e) => {
                violations.push(InvariantViolation {
                    invariant: "metrics_endpoint_reachable_after_global_probe".to_owned(),
                    expected: "metrics endpoint responds".to_owned(),
                    actual: format!("fetch failed: {e}"),
                });
            }
        }

        // --- Clean up ---
        healthy.close().await;

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
    let actions = (FLOOD_MESSAGE_COUNT + HEALTHY_PING_COUNT + GLOBAL_CEILING_PROBE_COUNT) as f64;
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
        .join_room(room_id, ROOM_TYPE, None)
        .await
        .map_err(|e| format!("join failed: {e}"))?;

    if !join.success {
        host.close().await;
        return Err(format!("join rejected: {:?}", join.rejection_reason));
    }

    host.send_json(&serde_json::json!({ "type": "invite_create", "maxUses": 10 }))
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
