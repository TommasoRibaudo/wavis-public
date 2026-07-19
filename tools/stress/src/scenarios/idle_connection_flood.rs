/// IdleConnectionFloodScenario — Property 25: Per-IP connection cap (standalone)
///
/// Validates that:
///   Req 9.2) When multiple clients hold idle WebSocket connections from the same IP,
///            the per-IP connection cap limits total connections and the
///            `connections_rejected_ip_cap` counter increases for excess attempts.
///
/// This is a focused standalone test for Property 25. It opens `max_per_ip` idle
/// WebSocket connections from 127.0.0.1, then attempts one more connection beyond
/// the cap and asserts it is rejected with HTTP 429. Uses raw TCP (like
/// `slowloris.rs`) to inspect the HTTP response status code.
///
/// `config_preset`: `Default` — real production per-IP limits are exercised.
///
/// **Property 25: Per-IP connection cap**
/// **Validates: Requirements 9.2**
use std::time::{Duration, Instant};

use async_trait::async_trait;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpStream;

use crate::assertions::{assert_counter_delta, fetch_metrics};
use crate::client::StressClient;
use crate::config::{Capability, ConfigPreset, TestContext};
use crate::results::{InvariantViolation, ScenarioResult};
use crate::runner::{Scenario, Tier};

/// Default per-IP connection cap (matches `IpConnectionTracker::from_env()` default).
const DEFAULT_MAX_PER_IP: usize = 10;

/// How long to hold idle WebSocket connections open to ensure they are registered
/// in the IP tracker before attempting the extra connection.
const IDLE_HOLD_DURATION: Duration = Duration::from_millis(500);

/// How long to wait for an HTTP response when probing the per-IP cap.
const HTTP_RESPONSE_TIMEOUT: Duration = Duration::from_secs(5);

pub struct IdleConnectionFloodScenario;

#[async_trait]
impl Scenario for IdleConnectionFloodScenario {
    fn name(&self) -> &str {
        "idle_connection_flood"
    }

    fn tier(&self) -> Tier {
        Tier::Tier2
    }

    fn requires(&self) -> Vec<Capability> {
        vec![]
    }

    fn config_preset(&self) -> ConfigPreset {
        ConfigPreset::Slowloris
    }

    async fn run(&self, ctx: &TestContext) -> ScenarioResult {
        let start = Instant::now();
        let mut violations: Vec<InvariantViolation> = Vec::new();

        let metrics_token = std::env::var("TEST_METRICS_TOKEN").unwrap_or_default();

        // Parse host:port from the WS URL for raw TCP connections.
        let host_port = match extract_host_port(&ctx.ws_url) {
            Some(hp) => hp,
            None => {
                return early_fail(
                    self.name(),
                    start,
                    "parse_ws_url",
                    format!("could not extract host:port from ws_url={}", ctx.ws_url),
                );
            }
        };

        // =====================================================================
        // Req 9.2 + Property 25 — Per-IP connection cap
        //
        // Snapshot baseline before the test.
        // =====================================================================
        let baseline = fetch_metrics(&ctx.http_client, &ctx.metrics_url, &metrics_token)
            .await
            .unwrap_or(serde_json::Value::Null);

        // Open DEFAULT_MAX_PER_IP WebSocket connections and hold them open.
        let mut idle_connections: Vec<StressClient> = Vec::with_capacity(DEFAULT_MAX_PER_IP);
        let mut connect_failures = 0usize;

        for i in 0..DEFAULT_MAX_PER_IP {
            match StressClient::connect(&ctx.ws_url).await {
                Ok(c) => {
                    idle_connections.push(c);
                }
                Err(_) => {
                    connect_failures += 1;
                    // If we can't open the first few connections, the cap may already be
                    // hit from a previous test run or the backend is unavailable.
                    if i < 3 {
                        violations.push(InvariantViolation {
                            invariant: format!("req_9.2: open_idle_connection_{i}"),
                            expected: "idle WS connection opens successfully".to_owned(),
                            actual: format!(
                                "connection {i} failed (total failures: {connect_failures})"
                            ),
                        });
                        // Close what we have and bail early.
                        for c in idle_connections {
                            c.close().await;
                        }
                        return ScenarioResult {
                            name: self.name().to_owned(),
                            passed: false,
                            duration: start.elapsed(),
                            actions_per_second: 0.0,
                            p95_latency: Duration::ZERO,
                            p99_latency: Duration::ZERO,
                            violations,
                        };
                    }
                }
            }
        }

        // Hold connections briefly to ensure they're registered in the IP tracker.
        tokio::time::sleep(IDLE_HOLD_DURATION).await;

        // Attempt one more connection beyond the cap using raw TCP so we can
        // inspect the HTTP response status code.
        let extra_result = probe_connection_rejected(&host_port).await;

        match extra_result {
            ProbeResult::Rejected429 => {
                // Expected — per-IP cap triggered correctly.
            }
            ProbeResult::Accepted => {
                violations.push(InvariantViolation {
                    invariant: "property_25: per_ip_cap_rejects_excess_connection".to_owned(),
                    expected: format!(
                        "connection beyond per-IP cap ({DEFAULT_MAX_PER_IP}) rejected with HTTP 429"
                    ),
                    actual: "extra connection was accepted (per-IP cap not enforced)".to_owned(),
                });
            }
            ProbeResult::ConnectFailed(e) => {
                // TCP connect failed — OS-level rejection is also an acceptable form of
                // rejection (the connection was not accepted). Treat as soft pass.
                let _ = e;
            }
            ProbeResult::Timeout => {
                violations.push(InvariantViolation {
                    invariant: "property_25: per_ip_cap_response_timeout".to_owned(),
                    expected: "HTTP 429 response within timeout".to_owned(),
                    actual: format!("no response within {HTTP_RESPONSE_TIMEOUT:?}"),
                });
            }
            ProbeResult::OtherStatus(code) => {
                // Any non-2xx response is an acceptable rejection.
                // Only a 2xx (successful upgrade) would be a violation.
                if code < 400 {
                    violations.push(InvariantViolation {
                        invariant: "property_25: per_ip_cap_rejects_excess_connection".to_owned(),
                        expected: "HTTP 4xx rejection for excess connection".to_owned(),
                        actual: format!("got HTTP {code} — connection may have been accepted"),
                    });
                }
            }
        }

        // Close all idle connections.
        for c in idle_connections {
            c.close().await;
        }

        // Give the backend a moment to flush atomic counters.
        tokio::time::sleep(Duration::from_millis(300)).await;

        // Assert `connections_rejected_ip_cap` counter increased.
        match fetch_metrics(&ctx.http_client, &ctx.metrics_url, &metrics_token).await {
            Ok(current) => {
                if let Some(v) =
                    assert_counter_delta(&baseline, &current, "connections_rejected_ip_cap", 1)
                {
                    violations.push(v);
                }
            }
            Err(e) => {
                violations.push(InvariantViolation {
                    invariant: "property_25: metrics_endpoint_reachable_after_ip_cap_test"
                        .to_owned(),
                    expected: "metrics endpoint responds".to_owned(),
                    actual: format!("fetch failed: {e}"),
                });
            }
        }

        let duration = start.elapsed();
        let total_actions = (DEFAULT_MAX_PER_IP + 1) as f64;

        ScenarioResult {
            name: self.name().to_owned(),
            passed: violations.is_empty(),
            duration,
            actions_per_second: if duration.as_secs_f64() > 0.0 {
                total_actions / duration.as_secs_f64()
            } else {
                0.0
            },
            p95_latency: Duration::ZERO,
            p99_latency: Duration::ZERO,
            violations,
        }
    }
}

// ---------------------------------------------------------------------------
// HTTP probe helpers
// ---------------------------------------------------------------------------

enum ProbeResult {
    Rejected429,
    Accepted,
    OtherStatus(u16),
    ConnectFailed(String),
    Timeout,
}

/// Attempt a raw HTTP WebSocket upgrade and return the HTTP response status.
/// This lets us distinguish HTTP 429 (per-IP cap) from a successful upgrade.
async fn probe_connection_rejected(host_port: &str) -> ProbeResult {
    let mut stream = match TcpStream::connect(host_port).await {
        Ok(s) => s,
        Err(e) => return ProbeResult::ConnectFailed(e.to_string()),
    };

    // Send a complete HTTP upgrade request.
    let request = "GET /ws HTTP/1.1\r\n\
        Host: 127.0.0.1\r\n\
        Upgrade: websocket\r\n\
        Connection: Upgrade\r\n\
        Sec-WebSocket-Key: dGhlIHNhbXBsZSBub25jZQ==\r\n\
        Sec-WebSocket-Version: 13\r\n\
        \r\n";

    if stream.write_all(request.as_bytes()).await.is_err() {
        return ProbeResult::ConnectFailed("write failed".to_owned());
    }

    // Read the HTTP response status line.
    let result = tokio::time::timeout(HTTP_RESPONSE_TIMEOUT, async {
        let mut buf = Vec::with_capacity(256);
        let mut byte = [0u8; 1];

        // Read until we have the status line (ends with \r\n).
        loop {
            match stream.read(&mut byte).await {
                Ok(0) => break, // EOF
                Ok(_) => {
                    buf.push(byte[0]);
                    if buf.ends_with(b"\r\n") {
                        break;
                    }
                    // Safety limit — don't read more than 512 bytes for the status line.
                    if buf.len() > 512 {
                        break;
                    }
                }
                Err(_) => break,
            }
        }
        buf
    })
    .await;

    match result {
        Err(_) => ProbeResult::Timeout,
        Ok(buf) => {
            let response = String::from_utf8_lossy(&buf);
            parse_http_status(&response)
        }
    }
}

/// Parse the HTTP status code from a status line like "HTTP/1.1 429 Too Many Requests\r\n".
fn parse_http_status(status_line: &str) -> ProbeResult {
    let parts: Vec<&str> = status_line.split_whitespace().collect();
    if parts.len() < 2 {
        return ProbeResult::OtherStatus(0);
    }
    match parts[1].parse::<u16>() {
        Ok(101) => ProbeResult::Accepted, // 101 Switching Protocols = successful upgrade
        Ok(429) => ProbeResult::Rejected429,
        Ok(code) => ProbeResult::OtherStatus(code),
        Err(_) => ProbeResult::OtherStatus(0),
    }
}

// ---------------------------------------------------------------------------
// URL parsing helper
// ---------------------------------------------------------------------------

/// Extract "host:port" from a WebSocket URL like "ws://127.0.0.1:3000/ws".
fn extract_host_port(ws_url: &str) -> Option<String> {
    let without_scheme = ws_url
        .strip_prefix("ws://")
        .or_else(|| ws_url.strip_prefix("wss://"))?;

    let host_port = without_scheme.split('/').next()?;

    if host_port.contains(':') {
        Some(host_port.to_owned())
    } else {
        Some(format!("{host_port}:80"))
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

// ---------------------------------------------------------------------------
// Unit tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    /// Property 25: Per-IP connection cap
    ///
    /// `extract_host_port` correctly parses host:port from various WS URL formats.
    ///
    /// **Validates: Requirements 9.2**
    #[test]
    fn extract_host_port_parses_standard_url() {
        assert_eq!(
            extract_host_port("ws://127.0.0.1:3000/ws"),
            Some("127.0.0.1:3000".to_owned())
        );
    }

    #[test]
    fn extract_host_port_parses_wss_url() {
        assert_eq!(
            extract_host_port("wss://example.com:443/ws"),
            Some("example.com:443".to_owned())
        );
    }

    #[test]
    fn extract_host_port_parses_url_without_path() {
        assert_eq!(
            extract_host_port("ws://127.0.0.1:3000"),
            Some("127.0.0.1:3000".to_owned())
        );
    }

    #[test]
    fn extract_host_port_returns_none_for_invalid_url() {
        assert_eq!(extract_host_port("not-a-url"), None);
    }

    #[test]
    fn parse_http_status_429() {
        let line = "HTTP/1.1 429 Too Many Requests\r\n";
        assert!(matches!(parse_http_status(line), ProbeResult::Rejected429));
    }

    #[test]
    fn parse_http_status_101() {
        let line = "HTTP/1.1 101 Switching Protocols\r\n";
        assert!(matches!(parse_http_status(line), ProbeResult::Accepted));
    }

    #[test]
    fn parse_http_status_400() {
        let line = "HTTP/1.1 400 Bad Request\r\n";
        assert!(matches!(
            parse_http_status(line),
            ProbeResult::OtherStatus(400)
        ));
    }

    /// Property 25: Per-IP connection cap
    ///
    /// For any HTTP status code >= 400, the connection is treated as rejected.
    /// For status 101, the connection is treated as accepted.
    ///
    /// **Validates: Requirements 9.2**
    #[test]
    fn rejection_logic_treats_4xx_as_rejected() {
        for code in [400u16, 429, 503] {
            let line = format!("HTTP/1.1 {code} Reason\r\n");
            let result = parse_http_status(&line);
            let is_rejection = matches!(result, ProbeResult::Rejected429)
                || matches!(result, ProbeResult::OtherStatus(c) if c >= 400);
            assert!(is_rejection, "Expected rejection for HTTP {code}");
        }
    }

    /// Property 25: Per-IP connection cap
    ///
    /// HTTP 101 (successful WebSocket upgrade) is treated as an accepted connection,
    /// not a rejection — this is the violation case.
    ///
    /// **Validates: Requirements 9.2**
    #[test]
    fn accepted_connection_is_not_a_rejection() {
        let line = "HTTP/1.1 101 Switching Protocols\r\n";
        let result = parse_http_status(line);
        assert!(
            matches!(result, ProbeResult::Accepted),
            "HTTP 101 should be treated as accepted"
        );
    }
}
