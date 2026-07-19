/// InviteExpiryRaceScenario — Property 6: Post-expiry join rejection
///
/// Creates an invite that expires in 2 seconds by using the `fake_now` trick:
/// pass `now = Instant::now() - (default_ttl - 2s)` so that
/// `expires_at = fake_now + default_ttl = real_now + 2s`.
///
/// Phase 1 — Pre-expiry joins (should succeed up to capacity):
///   Spawn 3 clients that join immediately — these should succeed.
///
/// Phase 2 — Post-expiry joins (should all fail with InviteExpired):
///   Sleep TTL + 500ms skew buffer (2500ms total), then spawn 10 clients.
///   Assert ALL of these fail with `InviteExpired` (or `InviteInvalid`).
///
/// External mode: skip gracefully (cannot control TTL on external backend).
///
/// **Property 6: Post-expiry join rejection**
/// **Validates: Requirements 3.4**
use std::time::{Duration, Instant};

use async_trait::async_trait;
use tokio::task::JoinSet;

use crate::assertions::fetch_metrics;
use crate::client::StressClient;
use crate::config::{Capability, ConfigPreset, TestContext};
use crate::results::{InvariantViolation, LatencyTracker, ScenarioResult};
use crate::runner::{Scenario, Tier};

/// TTL for the short-lived invite (2 seconds).
const INVITE_TTL: Duration = Duration::from_secs(2);
/// Default TTL used by the backend's InviteStore (24 hours).
const DEFAULT_TTL: Duration = Duration::from_secs(86400);
/// Skew buffer added on top of TTL before running phase 2.
const SKEW_BUFFER: Duration = Duration::from_millis(500);

/// Number of clients in phase 1 (pre-expiry, should succeed up to capacity).
const PHASE1_CLIENTS: usize = 3;
/// Number of clients in phase 2 (post-expiry, should all fail).
const PHASE2_CLIENTS: usize = 10;

pub struct InviteExpiryRaceScenario;

#[async_trait]
impl Scenario for InviteExpiryRaceScenario {
    fn name(&self) -> &str {
        "invite-expiry-race"
    }

    fn tier(&self) -> Tier {
        Tier::Tier1
    }

    fn requires(&self) -> Vec<Capability> {
        vec![]
    }

    fn config_preset(&self) -> ConfigPreset {
        ConfigPreset::JoinHeavy
    }

    async fn run(&self, ctx: &TestContext) -> ScenarioResult {
        let start = Instant::now();
        let mut violations: Vec<InvariantViolation> = Vec::new();
        let mut latency = LatencyTracker::new();

        // External mode: we cannot control the backend's invite TTL, so skip gracefully.
        let app_state = match &ctx.app_state {
            Some(s) => s,
            None => {
                return ScenarioResult {
                    name: self.name().to_owned(),
                    passed: true,
                    duration: start.elapsed(),
                    actions_per_second: 0.0,
                    p95_latency: Duration::ZERO,
                    p99_latency: Duration::ZERO,
                    violations: vec![],
                };
            }
        };

        // --- Generate a unique room ID ---
        let room_id = {
            use rand::RngCore;
            let mut rng = ctx.rng.lock().unwrap();
            let hi = rng.next_u64();
            let lo = rng.next_u64();
            format!("expiry-{hi:016x}-{lo:016x}")
        };

        // --- Create invite with effective TTL of 2 seconds ---
        //
        // InviteStore::generate sets: expires_at = now + config.default_ttl (24h).
        // We want:                    expires_at = real_now + 2s
        //
        // So we pass:  fake_now = real_now - (default_ttl - invite_ttl)
        //              expires_at = fake_now + default_ttl
        //                        = real_now - (default_ttl - invite_ttl) + default_ttl
        //                        = real_now + invite_ttl  ✓
        //
        // On Windows, Instant is based on system uptime which may be < 86398s,
        // causing an underflow panic. Use checked_sub and skip gracefully if it fails.
        let fake_now = match Instant::now().checked_sub(DEFAULT_TTL - INVITE_TTL) {
            Some(t) => t,
            None => {
                // System uptime is too short for the Instant subtraction trick.
                // Skip this scenario gracefully — cannot test invite expiry with
                // the fake_now approach when uptime < 24h.
                return ScenarioResult {
                    name: self.name().to_owned(),
                    passed: true,
                    duration: start.elapsed(),
                    actions_per_second: 0.0,
                    p95_latency: Duration::ZERO,
                    p99_latency: Duration::ZERO,
                    violations: vec![],
                };
            }
        };

        let invite_code =
            match app_state
                .invite_store
                .generate(&room_id, "stress-issuer", Some(100), fake_now)
            {
                Ok(record) => record.code,
                Err(e) => {
                    return ScenarioResult {
                        name: self.name().to_owned(),
                        passed: false,
                        duration: start.elapsed(),
                        actions_per_second: 0.0,
                        p95_latency: Duration::ZERO,
                        p99_latency: Duration::ZERO,
                        violations: vec![InvariantViolation {
                            invariant: "invite_creation".to_owned(),
                            expected: "invite created with 2s TTL".to_owned(),
                            actual: format!("InviteStore::generate failed: {e}"),
                        }],
                    };
                }
            };

        // =========================================================
        // Phase 1: Pre-expiry joins — should succeed (up to capacity)
        // =========================================================
        let mut phase1_set: JoinSet<ClientOutcome> = JoinSet::new();

        for _ in 0..PHASE1_CLIENTS {
            let ws_url = ctx.ws_url.clone();
            let room_id_clone = room_id.clone();
            let code_clone = invite_code.clone();

            phase1_set.spawn(async move {
                let t0 = Instant::now();
                let mut client = match StressClient::connect(&ws_url).await {
                    Ok(c) => c,
                    Err(e) => {
                        return ClientOutcome {
                            success: false,
                            rejection_reason: Some(format!("connect_error: {e}")),
                            latency: t0.elapsed(),
                        };
                    }
                };

                let result = client
                    .join_room(&room_id_clone, "p2p", Some(&code_clone))
                    .await;

                let elapsed = t0.elapsed();
                client.close().await;

                match result {
                    Ok(r) => ClientOutcome {
                        success: r.success,
                        rejection_reason: r.rejection_reason,
                        latency: elapsed,
                    },
                    Err(e) => ClientOutcome {
                        success: false,
                        rejection_reason: Some(format!("join_error: {e}")),
                        latency: elapsed,
                    },
                }
            });
        }

        let mut phase1_successes = 0usize;
        while let Some(res) = phase1_set.join_next().await {
            match res {
                Ok(o) => {
                    latency.record(o.latency);
                    if o.success {
                        phase1_successes += 1;
                    }
                }
                Err(e) => {
                    violations.push(InvariantViolation {
                        invariant: "phase1_task_panic".to_owned(),
                        expected: "no panics".to_owned(),
                        actual: format!("task panicked: {e}"),
                    });
                }
            }
        }

        // Phase 1 sanity: at least 1 pre-expiry join should succeed (invite is still valid).
        if phase1_successes == 0 {
            violations.push(InvariantViolation {
                invariant: "phase1_pre_expiry_join: at least 1 join must succeed before expiry"
                    .to_owned(),
                expected: ">= 1 successful join".to_owned(),
                actual: "0 successful joins in phase 1 (invite may have expired too fast)"
                    .to_owned(),
            });
        }

        // =========================================================
        // Sleep past TTL + skew buffer so the invite is definitely expired.
        // =========================================================
        tokio::time::sleep(INVITE_TTL + SKEW_BUFFER).await;

        // =========================================================
        // Phase 2: Post-expiry joins — ALL must fail with InviteExpired
        // =========================================================
        let mut phase2_set: JoinSet<ClientOutcome> = JoinSet::new();

        for _ in 0..PHASE2_CLIENTS {
            let ws_url = ctx.ws_url.clone();
            let room_id_clone = room_id.clone();
            let code_clone = invite_code.clone();

            phase2_set.spawn(async move {
                let t0 = Instant::now();
                let mut client = match StressClient::connect(&ws_url).await {
                    Ok(c) => c,
                    Err(e) => {
                        return ClientOutcome {
                            success: false,
                            rejection_reason: Some(format!("connect_error: {e}")),
                            latency: t0.elapsed(),
                        };
                    }
                };

                let result = client
                    .join_room(&room_id_clone, "p2p", Some(&code_clone))
                    .await;

                let elapsed = t0.elapsed();
                client.close().await;

                match result {
                    Ok(r) => ClientOutcome {
                        success: r.success,
                        rejection_reason: r.rejection_reason,
                        latency: elapsed,
                    },
                    Err(e) => ClientOutcome {
                        success: false,
                        rejection_reason: Some(format!("join_error: {e}")),
                        latency: elapsed,
                    },
                }
            });
        }

        let mut phase2_successes = 0usize;
        let mut phase2_expired_count = 0usize;
        let mut phase2_unexpected: Vec<String> = Vec::new();

        while let Some(res) = phase2_set.join_next().await {
            match res {
                Ok(o) => {
                    latency.record(o.latency);
                    if o.success {
                        phase2_successes += 1;
                    } else {
                        match o.rejection_reason.as_deref() {
                            // Both InviteExpired and InviteInvalid are acceptable —
                            // some backends return InviteInvalid for expired invites
                            // (e.g. after a sweep removes the record entirely).
                            Some("invite_expired") | Some("invite_invalid") => {
                                phase2_expired_count += 1;
                            }
                            other => {
                                phase2_unexpected.push(other.unwrap_or("<no reason>").to_owned());
                            }
                        }
                    }
                }
                Err(e) => {
                    violations.push(InvariantViolation {
                        invariant: "phase2_task_panic".to_owned(),
                        expected: "no panics".to_owned(),
                        actual: format!("task panicked: {e}"),
                    });
                }
            }
        }

        // --- Property 6: No post-expiry join may succeed ---
        if phase2_successes > 0 {
            violations.push(InvariantViolation {
                invariant: "post_expiry_join_rejection: no joins succeed after expiry".to_owned(),
                expected: "0 post-expiry successes".to_owned(),
                actual: format!("{phase2_successes} client(s) succeeded after invite expiry"),
            });
        }

        // --- Property 6: All phase-2 rejections must be InviteExpired / InviteInvalid ---
        if !phase2_unexpected.is_empty() {
            violations.push(InvariantViolation {
                invariant: "post_expiry_rejection_reason: rejections must be InviteExpired or InviteInvalid".to_owned(),
                expected: "all rejections: invite_expired or invite_invalid".to_owned(),
                actual: format!(
                    "{} unexpected rejection reason(s): {:?}",
                    phase2_unexpected.len(),
                    phase2_unexpected
                ),
            });
        }

        // --- Sanity: at least some phase-2 clients must have been rejected with expiry ---
        if phase2_expired_count == 0 && phase2_successes == 0 {
            violations.push(InvariantViolation {
                invariant: "phase2_expiry_observed: at least 1 client must observe InviteExpired"
                    .to_owned(),
                expected: ">= 1 invite_expired or invite_invalid rejection".to_owned(),
                actual: format!(
                    "0 expiry rejections (unexpected reasons: {:?})",
                    phase2_unexpected
                ),
            });
        }

        // --- Query metrics endpoint ---
        let metrics_token = std::env::var("TEST_METRICS_TOKEN").unwrap_or_default();
        match fetch_metrics(&ctx.http_client, &ctx.metrics_url, &metrics_token).await {
            Ok(_) => {}
            Err(e) => {
                violations.push(InvariantViolation {
                    invariant: "metrics_endpoint_reachable".to_owned(),
                    expected: "metrics endpoint responds".to_owned(),
                    actual: format!("fetch failed: {e}"),
                });
            }
        }

        let total_actions = PHASE1_CLIENTS + PHASE2_CLIENTS;
        let duration = start.elapsed();
        let actions_per_second = if duration.as_secs_f64() > 0.0 {
            total_actions as f64 / duration.as_secs_f64()
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

/// Outcome of a single client's join attempt.
struct ClientOutcome {
    success: bool,
    rejection_reason: Option<String>,
    latency: Duration,
}
