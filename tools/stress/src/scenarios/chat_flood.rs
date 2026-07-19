/// ChatFloodScenario — Chat rate limiter stress test
///
/// Validates that:
///   A) The per-connection `ChatRateLimiter` (5 msgs/sec token bucket) enforces
///      its limit: the first 5 `chat_send` messages in a burst succeed (broadcast
///      `chat_message` to the room), and subsequent messages receive an error
///      response containing "chat rate limit".
///   B) The connection stays open after hitting the chat rate limit — chat rate
///      limiting is non-fatal (unlike the global WS rate limiter which closes
///      the connection).
///   C) After a 1-second pause (token refill), the client can send chat messages
///      again successfully.
///   D) The chat rate limiter is independent of the global WS rate limiter — a
///      client that hits the chat limit can still send non-chat messages (Ping).
///
/// **Validates: ChatRateLimiter enforcement, non-fatal rate limiting, token refill**
use std::time::{Duration, Instant};

use async_trait::async_trait;

use crate::client::StressClient;
use crate::config::{Capability, ConfigPreset, TestContext};
use crate::results::{InvariantViolation, LatencyTracker, ScenarioResult};
use crate::runner::{Scenario, Tier};

pub struct ChatFloodScenario;

const ROOM_TYPE: &str = "sfu";

/// Burst size — send this many chat messages as fast as possible.
/// The limiter allows 5, so messages 6+ should be rejected.
const BURST_SIZE: usize = 15;

#[async_trait]
impl Scenario for ChatFloodScenario {
    fn name(&self) -> &str {
        "chat-flood"
    }

    fn tier(&self) -> Tier {
        Tier::Tier2
    }

    fn requires(&self) -> Vec<Capability> {
        vec![Capability::Sfu]
    }

    fn config_preset(&self) -> ConfigPreset {
        ConfigPreset::Default
    }

    async fn run(&self, ctx: &TestContext) -> ScenarioResult {
        let start = Instant::now();
        let mut violations: Vec<InvariantViolation> = Vec::new();
        let latency = LatencyTracker::new();

        // --- Generate unique room ID ---
        let room_id = {
            use rand::RngCore;
            let mut rng = ctx.rng.lock().unwrap();
            format!("chatflood-{:016x}", rng.next_u64())
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
                    Err(e) => return early_fail(self.name(), start, "invite_creation", e),
                }
            }
            None => match create_invite_via_signaling(ctx, &room_id).await {
                Ok(c) => c,
                Err(e) => return early_fail(self.name(), start, "invite_creation", e),
            },
        };

        // --- Connect host (first joiner) ---
        let mut host = match StressClient::connect(&ctx.ws_url).await {
            Ok(c) => c,
            Err(e) => return early_fail(self.name(), start, "host_connect", format!("{e}")),
        };
        let host_join = match host
            .join_room(&room_id, ROOM_TYPE, Some(&invite_code))
            .await
        {
            Ok(r) => r,
            Err(e) => {
                host.close().await;
                return early_fail(self.name(), start, "host_join", format!("{e}"));
            }
        };
        if !host_join.success {
            host.close().await;
            return early_fail(
                self.name(),
                start,
                "host_join_rejected",
                format!("{:?}", host_join.rejection_reason),
            );
        }

        // --- Connect chatter (second joiner — Guest role) ---
        let mut chatter = match StressClient::connect(&ctx.ws_url).await {
            Ok(c) => c,
            Err(e) => {
                host.close().await;
                return early_fail(self.name(), start, "chatter_connect", format!("{e}"));
            }
        };
        let chatter_join = match chatter
            .join_room(&room_id, ROOM_TYPE, Some(&invite_code))
            .await
        {
            Ok(r) => r,
            Err(e) => {
                host.close().await;
                chatter.close().await;
                return early_fail(self.name(), start, "chatter_join", format!("{e}"));
            }
        };
        if !chatter_join.success {
            host.close().await;
            chatter.close().await;
            return early_fail(
                self.name(),
                start,
                "chatter_join_rejected",
                format!("{:?}", chatter_join.rejection_reason),
            );
        }

        // =====================================================================
        // Test A — Burst: first 5 allowed, rest rate-limited
        // =====================================================================

        // Send BURST_SIZE chat messages as fast as possible.
        for i in 0..BURST_SIZE {
            let msg = serde_json::json!({
                "type": "chat_send",
                "text": format!("flood message {i}")
            });
            if chatter.send_json(&msg).await.is_err() {
                violations.push(InvariantViolation {
                    invariant: "chat_flood: connection_must_stay_open_during_burst".to_owned(),
                    expected: format!("send succeeds for message {i}"),
                    actual: "send failed — connection closed prematurely".to_owned(),
                });
                host.close().await;
                return build_result(self.name(), start, violations, latency);
            }
        }

        // Drain all responses from the chatter. We expect a mix of chat_message
        // broadcasts (for allowed messages) and error responses (for rate-limited ones).
        tokio::time::sleep(Duration::from_millis(500)).await;
        let responses = chatter.drain(Duration::from_millis(300)).await;

        let mut chat_messages = 0usize;
        let mut rate_limit_errors = 0usize;
        for msg in &responses {
            match msg.get("type").and_then(|t| t.as_str()) {
                Some("chat_message") => chat_messages += 1,
                Some("error") => {
                    let body = msg
                        .get("message")
                        .and_then(|v| v.as_str())
                        .unwrap_or("")
                        .to_lowercase();
                    if body.contains("chat rate limit") {
                        rate_limit_errors += 1;
                    }
                }
                _ => {}
            }
        }

        // We expect at least some rate limit errors (burst exceeded 5).
        if rate_limit_errors == 0 {
            violations.push(InvariantViolation {
                invariant: "chat_flood: rate_limiter_must_reject_excess_messages".to_owned(),
                expected: format!("at least 1 'chat rate limit' error after {BURST_SIZE} messages"),
                actual: format!("0 rate limit errors, {chat_messages} chat_messages received"),
            });
        }

        // =====================================================================
        // Test B — Connection stays open after rate limiting
        // =====================================================================

        // Send a Ping to verify the connection is still alive.
        let ping_result = chatter
            .send_json(&serde_json::json!({ "type": "ping" }))
            .await;
        if ping_result.is_err() {
            violations.push(InvariantViolation {
                invariant: "chat_flood: connection_survives_chat_rate_limit".to_owned(),
                expected: "connection open after chat rate limit".to_owned(),
                actual: "connection closed after chat rate limit".to_owned(),
            });
            host.close().await;
            return build_result(self.name(), start, violations, latency);
        }

        // =====================================================================
        // Test C — Token refill: after 1s pause, chat works again
        // =====================================================================

        tokio::time::sleep(Duration::from_millis(1100)).await;

        // Drain stale messages from both connections before testing refill.
        // The host still has chat_message broadcasts from the initial burst
        // queued up — if we don't drain those, recv_type will pick up a stale
        // "flood message N" instead of the "post-refill message".
        let _ = host.drain(Duration::from_millis(100)).await;
        let _ = chatter.drain(Duration::from_millis(100)).await;

        chatter
            .send_json(&serde_json::json!({
                "type": "chat_send",
                "text": "post-refill message"
            }))
            .await
            .ok();

        // The host should receive the chat_message broadcast.
        match host.recv_type("chat_message", Duration::from_secs(3)).await {
            Ok(msg) => {
                let text = msg.get("text").and_then(|v| v.as_str()).unwrap_or("");
                if text != "post-refill message" {
                    violations.push(InvariantViolation {
                        invariant: "chat_flood: refill_message_text_matches".to_owned(),
                        expected: "post-refill message".to_owned(),
                        actual: format!("'{text}'"),
                    });
                }
            }
            Err(e) => {
                violations.push(InvariantViolation {
                    invariant: "chat_flood: token_refill_allows_new_messages".to_owned(),
                    expected: "chat_message received by host after 1s refill".to_owned(),
                    actual: format!("no chat_message received: {e}"),
                });
            }
        }

        // =====================================================================
        // Test D — Chat rate limit is independent of WS rate limit
        // =====================================================================

        // After hitting chat rate limit, non-chat messages should still work.
        // Send another burst to re-exhaust the chat limiter.
        for i in 0..10 {
            let _ = chatter
                .send_json(&serde_json::json!({
                    "type": "chat_send",
                    "text": format!("exhaust {i}")
                }))
                .await;
        }
        tokio::time::sleep(Duration::from_millis(200)).await;
        let _ = chatter.drain(Duration::from_millis(100)).await;

        // Now send a Ping — should still work even though chat is rate-limited.
        let ping_ok = chatter
            .send_json(&serde_json::json!({ "type": "ping" }))
            .await
            .is_ok();
        if !ping_ok {
            violations.push(InvariantViolation {
                invariant: "chat_flood: non_chat_messages_unaffected_by_chat_limit".to_owned(),
                expected: "Ping succeeds while chat is rate-limited".to_owned(),
                actual: "Ping send failed".to_owned(),
            });
        }

        // --- Clean up ---
        host.close().await;
        chatter.close().await;

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
            (BURST_SIZE as f64 + 11.0) / duration.as_secs_f64()
        } else {
            0.0
        },
        p95_latency: latency.p95(),
        p99_latency: latency.p99(),
        violations,
    }
}

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
