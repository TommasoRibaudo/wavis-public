/// LogLeakScenario — Property 26: Log leak absence
///
/// Validates that:
///   P26) For any error scenario triggered during stress testing (bad invites, bad tokens,
///        malformed signaling, oversized payloads), the captured Backend log output SHALL
///        not contain any sensitive patterns: raw invite codes, raw JWT strings, full SDP
///        content, or full ICE candidate strings.
///
/// `config_preset`: `Default`
///
/// **Validates: Requirements 10.1, 10.2, 10.3, 10.4, 10.5, 10.6**
use std::time::{Duration, Instant};

use async_trait::async_trait;

use crate::client::StressClient;
use crate::config::{Capability, ConfigPreset, TestContext};
use crate::results::{InvariantViolation, LatencyTracker, ScenarioResult};
use crate::runner::{Scenario, Tier};

pub struct LogLeakScenario;

#[async_trait]
impl Scenario for LogLeakScenario {
    fn name(&self) -> &str {
        "log-leak"
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

        // Log capture requires in-process mode. Skip gracefully for external backends.
        let log_capture = match &ctx.log_capture {
            Some(lc) => lc.clone(),
            None => {
                // External mode: we cannot capture logs without explicit log-file setup.
                // Report as a skipped-but-passing result.
                return ScenarioResult {
                    name: "log-leak (SKIPPED: requires in-process mode for log capture)".to_owned(),
                    passed: true,
                    duration: start.elapsed(),
                    actions_per_second: 0.0,
                    p95_latency: latency.p95(),
                    p99_latency: latency.p99(),
                    violations: vec![],
                };
            }
        };

        // Clear any log lines accumulated before this scenario starts.
        log_capture.clear();

        // ------------------------------------------------------------------
        // Trigger error paths to generate log output
        // ------------------------------------------------------------------

        // 1. Join with a random invalid invite code → invite validation error
        trigger_bad_invite(ctx).await;

        // 2. Join with an expired/invalid JWT token → token validation error
        trigger_bad_token(ctx).await;

        // 3. Send malformed JSON → parse error
        trigger_malformed_json(ctx).await;

        // 4. Send an oversized payload (>64 KB) → payload size violation
        trigger_oversized_payload(ctx).await;

        // 5. Send a message with wrong room ID → cross-room rejection
        trigger_wrong_room(ctx).await;

        // 6. Send a bad access token via WS Auth → auth validation error
        trigger_bad_auth_token(ctx).await;

        // 7. Send a bad refresh token via REST → refresh validation error
        trigger_bad_refresh_token(ctx).await;

        // Give the backend a moment to flush all log events.
        tokio::time::sleep(Duration::from_millis(300)).await;

        // ------------------------------------------------------------------
        // Grep captured logs for sensitive patterns
        // ------------------------------------------------------------------
        let sensitive_matches = log_capture.grep_sensitive();

        for m in sensitive_matches {
            violations.push(InvariantViolation {
                invariant: format!(
                    "property_26: no_sensitive_pattern_in_logs (pattern={:?})",
                    m.pattern
                ),
                expected: "log line does not contain sensitive pattern".to_owned(),
                actual: format!("found {:?} in log line: {:?}", m.pattern, m.line),
            });
        }

        let duration = start.elapsed();
        let actions = 7.0_f64;
        ScenarioResult {
            name: self.name().to_owned(),
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
}

// ---------------------------------------------------------------------------
// Error-path triggers
// ---------------------------------------------------------------------------

/// Send a Join with a random invalid invite code to trigger invite validation error logs.
async fn trigger_bad_invite(ctx: &TestContext) {
    let bad_code = {
        use rand::RngCore;
        let mut rng = ctx.rng.lock().unwrap();
        format!("INVALID-{:016x}", rng.next_u64())
    };

    if let Ok(mut client) = StressClient::connect(&ctx.ws_url).await {
        let _ = client
            .join_room("log-leak-room", "p2p", Some(&bad_code))
            .await;
        client.close().await;
    }
}

/// Send a Join with a syntactically valid but semantically invalid JWT token
/// to trigger token validation error logs.
async fn trigger_bad_token(ctx: &TestContext) {
    // A well-formed JWT structure (header.payload.signature) with garbage content.
    // The backend will attempt to validate it and log an error — without logging the token value.
    let fake_jwt = "eyJhbGciOiJIUzI1NiIsInR5cCI6IkpXVCJ9\
                    .eyJzdWIiOiJiYWQtdG9rZW4iLCJleHAiOjF9\
                    .AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA";

    if let Ok(mut client) = StressClient::connect(&ctx.ws_url).await {
        // Send a Join with the fake token in the mediaToken field.
        let msg = serde_json::json!({
            "type": "join",
            "roomId": "log-leak-token-room",
            "roomType": "p2p",
            "inviteCode": null,
            "mediaToken": fake_jwt,
        });
        let _ = client.send_json(&msg).await;
        // Drain briefly to let the backend process and log the error.
        let _ = client.drain(Duration::from_millis(200)).await;
        client.close().await;
    }
}

/// Send malformed JSON to trigger parse error logs.
async fn trigger_malformed_json(ctx: &TestContext) {
    if let Ok(mut client) = StressClient::connect(&ctx.ws_url).await {
        // Truncated JSON — will cause a parse error in the backend.
        let _ = client.send_raw("{\"type\":\"Join\",\"roomId\":").await;
        tokio::time::sleep(Duration::from_millis(100)).await;
        // Also send completely invalid JSON.
        let _ = client.send_raw("not-json-at-all!!!").await;
        tokio::time::sleep(Duration::from_millis(100)).await;
        client.close().await;
    }
}

/// Send an oversized payload (>64 KB) to trigger payload size violation logs.
async fn trigger_oversized_payload(ctx: &TestContext) {
    if let Ok(mut client) = StressClient::connect(&ctx.ws_url).await {
        let oversized = "X".repeat(65_537);
        let _ = client.send_raw(&oversized).await;
        // Wait briefly for the backend to process and close the connection.
        tokio::time::sleep(Duration::from_millis(200)).await;
        client.close().await;
    }
}

/// Join a room successfully, then send a message referencing a different room ID
/// to trigger cross-room rejection logs.
async fn trigger_wrong_room(ctx: &TestContext) {
    // Create an invite for a room so we can join it.
    let room_id = {
        use rand::RngCore;
        let mut rng = ctx.rng.lock().unwrap();
        format!("log-leak-wr-{:016x}", rng.next_u64())
    };

    let invite_code_opt: Option<String> = match &ctx.app_state {
        Some(app_state) => app_state
            .invite_store
            .generate(&room_id, "stress-issuer", Some(2), Instant::now())
            .ok()
            .map(|r| r.code),
        None => {
            // External mode: try to get an invite via signaling.
            create_invite_via_signaling(ctx, &room_id).await.ok()
        }
    };

    let Some(invite_code) = invite_code_opt else {
        return;
    };

    if let Ok(mut client) = StressClient::connect(&ctx.ws_url).await {
        let join = client.join_room(&room_id, "p2p", Some(&invite_code)).await;
        if join.map(|r| r.success).unwrap_or(false) {
            // Send a signaling message referencing a completely different room.
            let msg = serde_json::json!({
                "type": "offer",
                "roomId": "completely-different-room-id",
                "targetPeerId": "nonexistent-peer",
                "sdp": "v=0",
            });
            let _ = client.send_json(&msg).await;
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
        client.close().await;
    }
}

/// External-mode helper: create an invite via signaling (host joins, requests invite, leaves).
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

    host.send_json(&serde_json::json!({ "type": "invite_create", "maxUses": 2 }))
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

    host.close().await;
    Ok(code)
}

/// Send a bad access token via WS Auth message to trigger auth validation error logs.
/// The backend should log the failure without leaking the raw token value.
async fn trigger_bad_auth_token(ctx: &TestContext) {
    // A well-formed JWT with garbage signature — the backend will attempt to validate
    // and log an auth failure.
    let fake_jwt = "eyJhbGciOiJIUzI1NiIsInR5cCI6IkpXVCJ9\
                    .eyJzdWIiOiJiYWQtdXNlciIsImV4cCI6OTk5OTk5OTk5OSwiYXVkIjoid2F2aXMiLCJpc3MiOiJ3YXZpcy1iYWNrZW5kIiwiaWF0IjoxfQ\
                    .BBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBB";

    if let Ok(mut client) = StressClient::connect(&ctx.ws_url).await {
        let msg = serde_json::json!({
            "type": "auth",
            "accessToken": fake_jwt,
        });
        let _ = client.send_json(&msg).await;
        // Drain briefly to let the backend process and log the error.
        let _ = client.drain(Duration::from_millis(300)).await;
        client.close().await;
    }
}

/// Send a bad refresh token via REST to trigger refresh validation error logs.
/// The backend should log the failure without leaking the raw refresh token value.
async fn trigger_bad_refresh_token(ctx: &TestContext) {
    let base_url = ctx
        .ws_url
        .replace("wss://", "https://")
        .replace("ws://", "http://")
        .trim_end_matches("/ws")
        .to_owned();

    let refresh_url = format!("{base_url}/auth/refresh");
    let fake_refresh = "AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA";

    // Best-effort — if the endpoint is unreachable (in-process mode with dummy DB),
    // we still triggered the WS auth path above.
    let _ = ctx
        .http_client
        .post(&refresh_url)
        .json(&serde_json::json!({ "refresh_token": fake_refresh }))
        .send()
        .await;

    // Brief pause for log flush.
    tokio::time::sleep(Duration::from_millis(100)).await;
}

#[cfg(test)]
mod tests {
    use crate::log_capture::{LogCapture, SENSITIVE_PATTERNS};

    /// Property 26: Log leak absence — allowlist exemptions are respected.
    ///
    /// Verifies that lines containing only allowlisted substrings are NOT flagged,
    /// while lines containing the raw sensitive pattern ARE flagged.
    ///
    /// **Validates: Requirements 10.1, 10.2, 10.3, 10.4**
    #[test]
    fn allowlisted_lines_are_not_flagged() {
        for rule in SENSITIVE_PATTERNS {
            for allow in rule.allowlist {
                // A line that contains the allowlist term (but not the bare pattern alone)
                // should not be flagged.
                let safe_line = format!("INFO backend: {allow}=42");
                let cap = make_capture(&[&safe_line]);
                let matches = cap.grep_sensitive();
                assert!(
                    matches.is_empty(),
                    "allowlisted line should not be flagged — pattern={:?}, allow={:?}, line={:?}",
                    rule.pattern,
                    allow,
                    safe_line
                );
            }
        }
    }

    /// Property 26: Log leak absence — bare sensitive patterns ARE flagged.
    ///
    /// **Validates: Requirements 10.1, 10.2, 10.3, 10.4**
    #[test]
    fn bare_sensitive_patterns_are_flagged() {
        for rule in SENSITIVE_PATTERNS {
            // Build a line that contains the pattern but none of the allowlist terms.
            let sensitive_line = format!("DEBUG handler: {} some_secret_value", rule.pattern);
            let cap = make_capture(&[&sensitive_line]);
            let matches = cap.grep_sensitive();
            assert_eq!(
                matches.len(),
                1,
                "bare sensitive pattern should be flagged — pattern={:?}, line={:?}",
                rule.pattern,
                sensitive_line
            );
            assert_eq!(matches[0].pattern, rule.pattern);
        }
    }

    /// Property 26: Log leak absence — clean log output produces zero matches.
    ///
    /// **Validates: Requirements 10.5**
    #[test]
    fn clean_log_output_produces_zero_matches() {
        let clean_lines = [
            "INFO wavis_backend::domain::invite: invite validated invite_code_count=3",
            "DEBUG wavis_backend::handlers::ws: join attempt room_id=\"room-abc\"",
            "WARN wavis_backend::domain::join_rate_limiter: rate limit exceeded ip=127.0.0.1",
            "ERROR wavis_backend::handlers::ws: payload too large payload_size=70000",
            "INFO wavis_backend::domain::turn_cred: credential issued token_ttl=600",
            "DEBUG wavis_backend: sdp_length=1024 sdp_type=offer",
            "DEBUG wavis_backend: candidate_count=3 candidate_type=srflx",
        ];
        let cap = make_capture(&clean_lines);
        let matches = cap.grep_sensitive();
        assert!(
            matches.is_empty(),
            "clean log lines should produce zero sensitive matches, got: {matches:?}",
            matches = matches.iter().map(|m| &m.line).collect::<Vec<_>>()
        );
    }

    /// Property 26: Log leak absence — clear() resets the buffer between scenarios.
    ///
    /// **Validates: Requirements 10.5**
    #[test]
    fn clear_resets_buffer_between_scenarios() {
        let cap = LogCapture::new();
        cap.push("DEBUG: token = eyJhbGciOiJIUzI1NiJ9...".to_owned());
        assert_eq!(
            cap.grep_sensitive().len(),
            1,
            "should have one match before clear"
        );
        cap.clear();
        assert!(
            cap.grep_sensitive().is_empty(),
            "should have zero matches after clear"
        );
    }

    fn make_capture(lines: &[&str]) -> LogCapture {
        let cap = LogCapture::new();
        for line in lines {
            cap.push(line.to_string());
        }
        cap
    }
}
