/// TurnCredentialAbuseScenario — Properties 27–30 (TURN credential safety)
///
/// Validates that TURN credentials:
///   A) Embed the correct expiry timestamp (`now_unix + credential_ttl_secs`) — Req 8.1
///   B) Are considered expired once the embedded timestamp is in the past — Req 8.2
///   C) Are identity-bound: participant A's password does not verify against participant B's
///      username (cross-identity HMAC rejection) — Req 8.3
///   D) Can be generated at high churn without memory growth or panics — Req 8.4
///
/// `config_preset`: `Default`
///
/// For in-process mode, calls `wavis_backend::domain::turn_cred` functions directly.
/// For external mode, this scenario is skipped (TURN cred generation is internal to the backend).
///
/// **Validates: Requirements 8.1, 8.2, 8.3, 8.4**
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use async_trait::async_trait;
use base64::{Engine, engine::general_purpose::STANDARD as BASE64};
use hmac::{Hmac, Mac};
use sha1::Sha1;

use wavis_backend::domain::turn_cred::{TurnConfig, generate_turn_credentials};

use crate::config::{Capability, ConfigPreset, TestContext};
use crate::results::{InvariantViolation, ScenarioResult};
use crate::runner::{Scenario, Tier};

type HmacSha1 = Hmac<Sha1>;

/// Number of iterations for the high-churn stability loop (Req 8.4).
const CHURN_ITERATIONS: usize = 1000;

/// Maximum allowed RSS growth percentage during the churn loop.
const MAX_RSS_GROWTH_PCT: f64 = 10.0;

/// Tolerance window (seconds) for expiry timestamp comparison (Req 8.1).
const EXPIRY_TOLERANCE_SECS: u64 = 2;

pub struct TurnCredentialAbuseScenario;

#[async_trait]
impl Scenario for TurnCredentialAbuseScenario {
    fn name(&self) -> &str {
        "turn-credential-abuse"
    }

    fn tier(&self) -> Tier {
        Tier::Tier2
    }

    fn requires(&self) -> Vec<Capability> {
        // TURN credential generation is internal to the backend — only testable in-process.
        // We gate on P2P (always available in-process) and skip in external mode via the
        // `app_state` check inside `run`.
        vec![]
    }

    fn config_preset(&self) -> ConfigPreset {
        ConfigPreset::Default
    }

    async fn run(&self, ctx: &TestContext) -> ScenarioResult {
        let start = Instant::now();
        let mut violations: Vec<InvariantViolation> = Vec::new();

        // This scenario only makes sense in in-process mode where we can call the
        // domain functions directly. Skip gracefully in external mode.
        if ctx.app_state.is_none() {
            return ScenarioResult {
                name: format!("SKIPPED: {} (requires in-process mode)", self.name()),
                passed: true,
                duration: start.elapsed(),
                actions_per_second: 0.0,
                p95_latency: Duration::ZERO,
                p99_latency: Duration::ZERO,
                violations: vec![],
            };
        }

        // Build a TurnConfig with a known secret and TTL for deterministic assertions.
        // Secret must be ≥ 32 bytes.
        let secret: Vec<u8> = b"stress-test-turn-secret-32bytes!".to_vec();
        let ttl_secs: u64 = 3600;
        let short_ttl_secs: u64 = 1;

        let config = TurnConfig::new(
            secret.clone(),
            None,
            ttl_secs,
            vec!["stun:stun.example.com:3478".to_string()],
            vec!["turn:turn.example.com:3478".to_string()],
        );

        let now_unix = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs();

        // =====================================================================
        // Req 8.1 — Expiry timestamp correctness
        //
        // Generate credentials and verify the embedded expiry equals now + TTL (±2s).
        // =====================================================================
        {
            let creds = generate_turn_credentials("participant-a", &config, now_unix);

            // Parse username: "<expiry_unix_timestamp>:<participant_id>"
            let parts: Vec<&str> = creds.username.splitn(2, ':').collect();
            if parts.len() != 2 {
                violations.push(InvariantViolation {
                    invariant: "req_8.1: username_format".to_owned(),
                    expected: "<expiry>:<participant_id>".to_owned(),
                    actual: format!("malformed username: {}", creds.username),
                });
            } else {
                match parts[0].parse::<u64>() {
                    Err(e) => {
                        violations.push(InvariantViolation {
                            invariant: "req_8.1: expiry_parseable".to_owned(),
                            expected: "numeric expiry timestamp".to_owned(),
                            actual: format!("parse error: {e}"),
                        });
                    }
                    Ok(embedded_expiry) => {
                        let expected_expiry = now_unix + ttl_secs;
                        let diff = embedded_expiry.abs_diff(expected_expiry);
                        if diff > EXPIRY_TOLERANCE_SECS {
                            violations.push(InvariantViolation {
                                invariant: "req_8.1: expiry_timestamp_correctness".to_owned(),
                                expected: format!(
                                    "embedded_expiry ≈ now_unix + ttl = {expected_expiry} (±{EXPIRY_TOLERANCE_SECS}s)"
                                ),
                                actual: format!(
                                    "embedded_expiry = {embedded_expiry}, diff = {diff}s"
                                ),
                            });
                        }
                    }
                }
            }
        }

        // =====================================================================
        // Req 8.2 — Post-expiry rejection
        //
        // Generate credentials with a short TTL (1s), wait for them to expire,
        // then verify the embedded expiry is in the past.
        // =====================================================================
        {
            let short_config =
                TurnConfig::new(secret.clone(), None, short_ttl_secs, vec![], vec![]);

            let creds = generate_turn_credentials("participant-expiry", &short_config, now_unix);

            // Sleep past the TTL so the credentials expire.
            // Use 1200ms (TTL=1s + 200ms buffer) to avoid boundary conditions
            // where embedded_expiry == now_after due to second-level granularity.
            tokio::time::sleep(Duration::from_millis(1200)).await;

            let now_after = SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap_or_default()
                .as_secs();

            // Parse the embedded expiry from the username.
            let parts: Vec<&str> = creds.username.splitn(2, ':').collect();
            if parts.len() == 2
                && let Ok(embedded_expiry) = parts[0].parse::<u64>()
            {
                // The TURN server would reject credentials where expiry <= now.
                // At the exact boundary (expiry == now), the credential has expired.
                if embedded_expiry > now_after {
                    violations.push(InvariantViolation {
                        invariant: "req_8.2: post_expiry_rejection".to_owned(),
                        expected: format!(
                            "embedded_expiry ({embedded_expiry}) < now ({now_after}) after TTL elapsed"
                        ),
                        actual: format!(
                            "credentials still appear valid: expiry={embedded_expiry}, now={now_after}"
                        ),
                    });
                }
                // Confirm: expiry < now means the TURN server would reject them.
                // This is the correct outcome — no violation.
            }
        }

        // =====================================================================
        // Req 8.3 — Cross-identity HMAC rejection
        //
        // Generate credentials for participant A and B.
        // Verify HMAC(secret, username_b) != password_a.
        // This proves credentials are identity-bound.
        // =====================================================================
        {
            let creds_a = generate_turn_credentials("participant-a", &config, now_unix);
            let creds_b = generate_turn_credentials("participant-b", &config, now_unix);

            // Compute what the TURN server would compute for participant B's username.
            let mut mac =
                HmacSha1::new_from_slice(&secret).expect("HMAC-SHA1 accepts any key length");
            mac.update(creds_b.username.as_bytes());
            let expected_b_credential = BASE64.encode(mac.finalize().into_bytes());

            // Participant A's password must NOT equal the expected credential for B's username.
            if creds_a.credential == expected_b_credential {
                violations.push(InvariantViolation {
                    invariant: "req_8.3: cross_identity_hmac_rejection".to_owned(),
                    expected: "HMAC(secret, username_b) != password_a".to_owned(),
                    actual: format!(
                        "participant A's credential ({}) incorrectly verifies against participant B's username ({})",
                        creds_a.credential, creds_b.username
                    ),
                });
            }

            // Also verify that each participant's own credential is correct.
            let mut mac_a =
                HmacSha1::new_from_slice(&secret).expect("HMAC-SHA1 accepts any key length");
            mac_a.update(creds_a.username.as_bytes());
            let expected_a_credential = BASE64.encode(mac_a.finalize().into_bytes());

            if creds_a.credential != expected_a_credential {
                violations.push(InvariantViolation {
                    invariant: "req_8.3: participant_a_credential_valid".to_owned(),
                    expected: "credential_a == HMAC(secret, username_a)".to_owned(),
                    actual: format!(
                        "credential_a={} expected={}",
                        creds_a.credential, expected_a_credential
                    ),
                });
            }

            if creds_b.credential != expected_b_credential {
                violations.push(InvariantViolation {
                    invariant: "req_8.3: participant_b_credential_valid".to_owned(),
                    expected: "credential_b == HMAC(secret, username_b)".to_owned(),
                    actual: format!(
                        "credential_b={} expected={}",
                        creds_b.credential, expected_b_credential
                    ),
                });
            }
        }

        // =====================================================================
        // Req 8.4 — High-churn stability
        //
        // Run a tight loop generating credentials 1000 times.
        // Assert no panics or errors, and RSS growth stays under 10% (Linux only).
        // =====================================================================
        {
            // Record baseline RSS before the churn loop.
            let baseline_rss = sample_rss_kb();

            let mut churn_error: Option<String> = None;
            for i in 0..CHURN_ITERATIONS {
                let participant_id = format!("churn-participant-{i}");
                // Use a slightly varying now_unix to exercise different expiry values.
                let creds =
                    generate_turn_credentials(&participant_id, &config, now_unix + i as u64);

                // Basic sanity: credential must be non-empty and username must contain ':'
                if creds.credential.is_empty() || !creds.username.contains(':') {
                    churn_error = Some(format!(
                        "iteration {i}: malformed credentials — username={}, credential={}",
                        creds.username, creds.credential
                    ));
                    break;
                }
            }

            if let Some(err) = churn_error {
                violations.push(InvariantViolation {
                    invariant: "req_8.4: high_churn_no_errors".to_owned(),
                    expected: "all 1000 credential generations succeed without error".to_owned(),
                    actual: err,
                });
            }

            // Sample peak RSS after the churn loop.
            let peak_rss = sample_rss_kb();

            if let (Some(baseline), Some(peak)) = (baseline_rss, peak_rss)
                && baseline > 0
            {
                let growth_pct = (peak.saturating_sub(baseline) as f64) / baseline as f64 * 100.0;
                if growth_pct > MAX_RSS_GROWTH_PCT {
                    violations.push(InvariantViolation {
                        invariant: "req_8.4: high_churn_no_memory_growth".to_owned(),
                        expected: format!("RSS growth < {MAX_RSS_GROWTH_PCT}%"),
                        actual: format!(
                            "RSS grew {growth_pct:.1}% (baseline={baseline}KB, peak={peak}KB)"
                        ),
                    });
                }
            }
            // If RSS sampling is unavailable (non-Linux), we skip the memory assertion silently.
        }

        let duration = start.elapsed();
        // Total actions: 1 (req 8.1) + 1 (req 8.2) + 2 (req 8.3) + CHURN_ITERATIONS (req 8.4)
        let total_actions = 4 + CHURN_ITERATIONS;

        ScenarioResult {
            name: self.name().to_owned(),
            passed: violations.is_empty(),
            duration,
            actions_per_second: if duration.as_secs_f64() > 0.0 {
                total_actions as f64 / duration.as_secs_f64()
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
// Helpers
// ---------------------------------------------------------------------------

/// Sample current process RSS in KB (Linux only; returns None elsewhere).
fn sample_rss_kb() -> Option<u64> {
    #[cfg(target_os = "linux")]
    {
        let content = std::fs::read_to_string("/proc/self/status").ok()?;
        for line in content.lines() {
            if line.starts_with("VmRSS:") {
                return line.split_whitespace().nth(1)?.parse().ok();
            }
        }
        None
    }
    #[cfg(not(target_os = "linux"))]
    {
        None
    }
}
