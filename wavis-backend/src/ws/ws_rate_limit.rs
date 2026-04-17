//! Per-connection WebSocket rate limiting: configuration, guard state, and
//! JSON depth checking.
//!
//! **Owns:** `WsRateLimitConfig` (env-driven thresholds loaded once at
//! startup), `WsRateLimiter` (per-connection counters for window, burst,
//! and action rate limits), and `check_json_depth` (abuse guard against
//! deeply nested JSON payloads).
//!
//! **Does not own:** the decision of *what happens* when a limit is hit
//! (closing the socket, sending an error frame, etc.) — that policy lives
//! in `handlers::ws`, which calls into this module.
//!
//! **Key invariant:** `WsRateLimitConfig` is read once via `AppState::new()`
//! and shared by reference with every connection. There is no per-connection
//! env-var reading.

use std::env;
use std::time::{Duration, Instant};

/// Configuration for per-connection WS message rate limiting.
/// Read from environment variables at connection start.
#[derive(Debug, Clone)]
pub struct WsRateLimitConfig {
    pub window: Duration,        // WS_RATE_LIMIT_WINDOW_SECS, default: 10
    pub max_messages: u32,       // WS_RATE_LIMIT_MAX_MESSAGES, default: 60
    pub burst_max: u32,          // WS_RATE_LIMIT_BURST, default: 15
    pub burst_window: Duration,  // always 1 second
    pub action_max: u32,         // ACTION_RATE_LIMIT_MAX, default: 5
    pub action_window: Duration, // ACTION_RATE_LIMIT_WINDOW_SECS, default: 60
    pub deafen_max: u32,         // DEAFEN_RATE_LIMIT_MAX, default: 20
    pub deafen_window: Duration, // DEAFEN_RATE_LIMIT_WINDOW_SECS, default: 60
    pub max_json_depth: u32,     // always 32
}

impl WsRateLimitConfig {
    pub fn from_env() -> Self {
        let window_secs = env::var("WS_RATE_LIMIT_WINDOW_SECS")
            .ok()
            .and_then(|v| v.parse::<u64>().ok())
            .unwrap_or(10);
        let max_messages = env::var("WS_RATE_LIMIT_MAX_MESSAGES")
            .ok()
            .and_then(|v| v.parse::<u32>().ok())
            .unwrap_or(60);
        let burst_max = env::var("WS_RATE_LIMIT_BURST")
            .ok()
            .and_then(|v| v.parse::<u32>().ok())
            .unwrap_or(15);
        let action_max = env::var("ACTION_RATE_LIMIT_MAX")
            .ok()
            .and_then(|v| v.parse::<u32>().ok())
            .unwrap_or(5);
        let action_window_secs = env::var("ACTION_RATE_LIMIT_WINDOW_SECS")
            .ok()
            .and_then(|v| v.parse::<u64>().ok())
            .unwrap_or(60);
        let deafen_max = env::var("DEAFEN_RATE_LIMIT_MAX")
            .ok()
            .and_then(|v| v.parse::<u32>().ok())
            .unwrap_or(20);
        let deafen_window_secs = env::var("DEAFEN_RATE_LIMIT_WINDOW_SECS")
            .ok()
            .and_then(|v| v.parse::<u64>().ok())
            .unwrap_or(60);
        Self {
            window: Duration::from_secs(window_secs),
            max_messages,
            burst_max,
            burst_window: Duration::from_secs(1),
            action_max,
            action_window: Duration::from_secs(action_window_secs),
            deafen_max,
            deafen_window: Duration::from_secs(deafen_window_secs),
            max_json_depth: 32,
        }
    }
}

pub(crate) struct WsRateLimiter {
    // Window-based rate limiting (existing, now configurable)
    window_start: Instant,
    message_count: u32,
    config_window: Duration,
    config_max: u32,
    // Burst detection (new)
    burst_window_start: Instant,
    burst_count: u32,
    config_burst_max: u32,
    // Action message throttling (new)
    action_window_start: Instant,
    action_count: u32,
    config_action_max: u32,
    config_action_window: Duration,
    // SelfDeafen/SelfUndeafen throttling — separate from action budget so that
    // rapid mute-toggling cannot exhaust the StartShare rate-limit allowance.
    deafen_window_start: Instant,
    deafen_count: u32,
    config_deafen_max: u32,
    config_deafen_window: Duration,
}

impl WsRateLimiter {
    pub(crate) fn new(config: &WsRateLimitConfig) -> Self {
        let now = Instant::now();
        Self {
            window_start: now,
            message_count: 0,
            config_window: config.window,
            config_max: config.max_messages,
            burst_window_start: now,
            burst_count: 0,
            config_burst_max: config.burst_max,
            action_window_start: now,
            action_count: 0,
            config_action_max: config.action_max,
            config_action_window: config.action_window,
            deafen_window_start: now,
            deafen_count: 0,
            config_deafen_max: config.deafen_max,
            config_deafen_window: config.deafen_window,
        }
    }

    /// Check and record a general message. Returns false if rate limited.
    pub(crate) fn allow(&mut self) -> bool {
        let now = Instant::now();
        if now.duration_since(self.window_start) > self.config_window {
            self.window_start = now;
            self.message_count = 0;
        }
        self.message_count += 1;
        self.message_count <= self.config_max
    }

    /// Check and record a burst. Returns false if burst cap exceeded within 1-second sub-window.
    pub(crate) fn burst_allow(&mut self) -> bool {
        let now = Instant::now();
        if now.duration_since(self.burst_window_start) > Duration::from_secs(1) {
            self.burst_window_start = now;
            self.burst_count = 0;
        }
        self.burst_count += 1;
        self.burst_count <= self.config_burst_max
    }

    /// Refund one message from the window and burst counters.
    ///
    /// Called when a message is rejected by a domain-specific rate limiter
    /// (e.g. `ChatRateLimiter`) so that the non-fatal rejection does not
    /// consume global rate-limit budget and accidentally close the connection.
    pub(crate) fn refund(&mut self) {
        self.message_count = self.message_count.saturating_sub(1);
        self.burst_count = self.burst_count.saturating_sub(1);
    }

    /// Check and record an action message. Returns false if action throttled.
    pub(crate) fn action_allow(&mut self) -> bool {
        let now = Instant::now();
        if now.duration_since(self.action_window_start) > self.config_action_window {
            self.action_window_start = now;
            self.action_count = 0;
        }
        self.action_count += 1;
        self.action_count <= self.config_action_max
    }

    /// Check and record a SelfDeafen or SelfUndeafen message.
    ///
    /// Uses a **separate** counter from `action_allow()` so that rapid
    /// mute-toggling cannot exhaust the action budget and block `StartShare`.
    pub(crate) fn deafen_allow(&mut self) -> bool {
        let now = Instant::now();
        if now.duration_since(self.deafen_window_start) > self.config_deafen_window {
            self.deafen_window_start = now;
            self.deafen_count = 0;
        }
        self.deafen_count += 1;
        self.deafen_count <= self.config_deafen_max
    }
}

/// String-aware JSON depth checker. Counts `{`/`[` nesting only outside string literals.
/// Returns true if depth is within limit, false if it exceeds max_depth.
pub(crate) fn check_json_depth(text: &str, max_depth: u32) -> bool {
    let mut depth: u32 = 0;
    let mut in_string = false;
    let mut escaped = false;

    for byte in text.bytes() {
        if escaped {
            escaped = false;
            continue;
        }
        if in_string {
            match byte {
                b'\\' => escaped = true,
                b'"' => in_string = false,
                _ => {}
            }
            continue;
        }
        match byte {
            b'"' => in_string = true,
            b'{' | b'[' => {
                depth += 1;
                if depth > max_depth {
                    return false;
                }
            }
            b'}' | b']' => {
                depth = depth.saturating_sub(1);
            }
            _ => {}
        }
    }
    true
}

#[cfg(test)]
mod tests {
    use super::*;
    use proptest::prelude::*;

    // Test: both KickParticipant and MuteParticipant count toward action limit (Req 3.3)
    #[test]
    fn test_kick_and_mute_share_action_limit() {
        let config = WsRateLimitConfig {
            window: Duration::from_secs(60),
            max_messages: 10000,
            burst_max: 10000,
            burst_window: Duration::from_secs(1),
            action_max: 3,
            action_window: Duration::from_secs(60),
            deafen_max: 1000,
            deafen_window: Duration::from_secs(60),
            max_json_depth: 32,
        };
        let mut limiter = WsRateLimiter::new(&config);

        // Simulate: 1 kick + 1 mute + 1 kick = 3 actions (all allowed)
        assert!(
            limiter.action_allow(),
            "1st action (kick) should be allowed"
        );
        assert!(
            limiter.action_allow(),
            "2nd action (mute) should be allowed"
        );
        assert!(
            limiter.action_allow(),
            "3rd action (kick) should be allowed"
        );
        // 4th action must be rejected regardless of type
        assert!(
            !limiter.action_allow(),
            "4th action should be rejected (limit is 3)"
        );
    }

    // Test: burst window resets after 1 second (Req 1.4 edge case)
    #[test]
    fn test_burst_window_resets_after_expiry() {
        let config = WsRateLimitConfig {
            window: Duration::from_secs(60),
            max_messages: 10000,
            burst_max: 2,
            burst_window: Duration::from_secs(1),
            action_max: 1000,
            action_window: Duration::from_secs(60),
            deafen_max: 1000,
            deafen_window: Duration::from_secs(60),
            max_json_depth: 32,
        };
        let mut limiter = WsRateLimiter::new(&config);

        // Fill the burst window
        assert!(limiter.burst_allow(), "1st burst should be allowed");
        assert!(limiter.burst_allow(), "2nd burst should be allowed");
        assert!(!limiter.burst_allow(), "3rd burst should be rejected");

        // Manually reset the burst window (simulating time passing > 1 second)
        limiter.burst_window_start = Instant::now() - Duration::from_secs(2);
        limiter.burst_count = 0;

        // After window reset, burst should be allowed again
        assert!(
            limiter.burst_allow(),
            "burst should be allowed after window reset"
        );
        assert!(
            limiter.burst_allow(),
            "2nd burst after reset should be allowed"
        );
        assert!(
            !limiter.burst_allow(),
            "3rd burst after reset should be rejected again"
        );
    }

    // --- Property 1: WS rate limiter rejects after configured threshold ---
    // Feature: signaling-auth-and-abuse-controls
    // **Validates: Requirements 1.1, 1.2**

    proptest! {
        #![proptest_config(ProptestConfig::with_cases(200))]

        #[test]
        fn prop_ws_rate_limiter_rejects_after_threshold(
            max_messages in 1u32..=20u32,
            window_secs in 1u64..=60u64,
        ) {
            let config = WsRateLimitConfig {
                window: Duration::from_secs(window_secs),
                max_messages,
                burst_max: 1000, // high so burst doesn't interfere
                burst_window: Duration::from_secs(1),
                action_max: 1000,
                action_window: Duration::from_secs(60),
                deafen_max: 1000,
                deafen_window: Duration::from_secs(60),
                max_json_depth: 32,
            };
            let mut limiter = WsRateLimiter::new(&config);

            // First max_messages calls must all be accepted
            for i in 0..max_messages {
                prop_assert!(
                    limiter.allow(),
                    "Call {} of {} should be accepted", i + 1, max_messages
                );
            }

            // The (max_messages + 1)th call must be rejected
            prop_assert!(
                !limiter.allow(),
                "Call {} should be rejected (threshold is {})", max_messages + 1, max_messages
            );
        }
    }

    // --- Property 2: JSON depth checker accepts valid depth and rejects excessive depth ---
    // Feature: signaling-auth-and-abuse-controls
    // **Validates: Requirements 1.3**

    proptest! {
        #![proptest_config(ProptestConfig::with_cases(200))]

        #[test]
        fn prop_json_depth_checker_accepts_valid_rejects_excessive(
            depth in 0u32..=64u32,
        ) {
            let max_depth = 32u32;

            // Build a JSON string with exactly `depth` levels of nesting
            let open: String = "{\"k\":".repeat(depth as usize);
            let close: String = "}".repeat(depth as usize);
            let json = if depth == 0 {
                "{}".to_string()
            } else {
                format!("{open}\"v\"{close}")
            };

            let result = check_json_depth(&json, max_depth);
            if depth <= max_depth {
                prop_assert!(result, "depth {} should be accepted (max {})", depth, max_depth);
            } else {
                prop_assert!(!result, "depth {} should be rejected (max {})", depth, max_depth);
            }
        }

        #[test]
        fn prop_json_depth_strings_not_counted(
            inner in "[^\"\\\\]{0,50}",
        ) {
            // Braces inside JSON strings must NOT count toward depth
            // e.g. {"key": "{{{{{"} has depth 1, not 6
            let json = format!("{{\"key\": \"{inner}\"}}");
            // Depth is always 1 regardless of braces inside the string value
            prop_assert!(
                check_json_depth(&json, 32),
                "Braces inside strings must not count toward depth"
            );
        }
    }

    // --- Property 3: Burst limiter rejects after burst cap within sub-window ---
    // Feature: signaling-auth-and-abuse-controls
    // **Validates: Requirements 1.4**

    proptest! {
        #![proptest_config(ProptestConfig::with_cases(200))]

        #[test]
        fn prop_burst_limiter_rejects_after_burst_cap(
            burst_max in 1u32..=20u32,
        ) {
            let config = WsRateLimitConfig {
                window: Duration::from_secs(60),
                max_messages: 10000, // high so window doesn't interfere
                burst_max,
                burst_window: Duration::from_secs(1),
                action_max: 1000,
                action_window: Duration::from_secs(60),
                deafen_max: 1000,
                deafen_window: Duration::from_secs(60),
                max_json_depth: 32,
            };
            let mut limiter = WsRateLimiter::new(&config);

            // First burst_max calls must all be accepted
            for i in 0..burst_max {
                prop_assert!(
                    limiter.burst_allow(),
                    "Burst call {} of {} should be accepted", i + 1, burst_max
                );
            }

            // The (burst_max + 1)th call within the same sub-window must be rejected
            prop_assert!(
                !limiter.burst_allow(),
                "Burst call {} should be rejected (cap is {})", burst_max + 1, burst_max
            );
        }
    }

    // Test: main allow() window resets after the configured time period elapses
    #[test]
    fn test_allow_window_resets_after_time_period() {
        let config = WsRateLimitConfig {
            window: Duration::from_secs(60),
            max_messages: 2,
            burst_max: 10000,
            burst_window: Duration::from_secs(1),
            action_max: 10000,
            action_window: Duration::from_secs(60),
            deafen_max: 1000,
            deafen_window: Duration::from_secs(60),
            max_json_depth: 32,
        };
        let mut limiter = WsRateLimiter::new(&config);

        assert!(limiter.allow(), "1st message should be allowed");
        assert!(limiter.allow(), "2nd message should be allowed");
        assert!(
            !limiter.allow(),
            "3rd message should be rejected (limit is 2)"
        );

        // Simulate the window expiring; allow() resets message_count internally.
        limiter.window_start = Instant::now() - Duration::from_secs(61);

        assert!(
            limiter.allow(),
            "1st message in new window should be allowed"
        );
        assert!(
            limiter.allow(),
            "2nd message in new window should be allowed"
        );
        assert!(
            !limiter.allow(),
            "3rd message in new window should be rejected"
        );
    }

    // Test: two WsRateLimiter instances have independent counters
    #[test]
    fn test_multiple_limiters_are_independent() {
        let config = WsRateLimitConfig {
            window: Duration::from_secs(60),
            max_messages: 3,
            burst_max: 10000,
            burst_window: Duration::from_secs(1),
            action_max: 10000,
            action_window: Duration::from_secs(60),
            deafen_max: 1000,
            deafen_window: Duration::from_secs(60),
            max_json_depth: 32,
        };
        let mut limiter_a = WsRateLimiter::new(&config);
        let mut limiter_b = WsRateLimiter::new(&config);

        // Exhaust limiter_a
        assert!(limiter_a.allow());
        assert!(limiter_a.allow());
        assert!(limiter_a.allow());
        assert!(!limiter_a.allow(), "limiter_a should be exhausted");

        // limiter_b must be unaffected
        assert!(
            limiter_b.allow(),
            "limiter_b should allow (independent counter)"
        );
        assert!(limiter_b.allow(), "limiter_b 2nd message should be allowed");
    }

    // --- Test: SelfDeafen/SelfUndeafen budget is independent of StartShare budget ---
    // Regression: rapid mute-toggling exhausted the shared action counter and blocked
    // screen sharing. deafen_allow() must use a separate counter from action_allow().

    #[test]
    fn test_deafen_does_not_consume_action_budget() {
        let config = WsRateLimitConfig {
            window: Duration::from_secs(60),
            max_messages: 10000,
            burst_max: 10000,
            burst_window: Duration::from_secs(1),
            action_max: 5,
            action_window: Duration::from_secs(60),
            deafen_max: 20,
            deafen_window: Duration::from_secs(60),
            max_json_depth: 32,
        };
        let mut limiter = WsRateLimiter::new(&config);

        // Exhaust the deafen budget (5 rapid toggles, same as the bug report)
        for i in 0..5 {
            assert!(
                limiter.deafen_allow(),
                "deafen toggle {} should be allowed",
                i + 1
            );
        }

        // StartShare must not be blocked by prior deafen toggles
        assert!(
            limiter.action_allow(),
            "start_share must not be blocked by prior SelfDeafen/SelfUndeafen toggles"
        );
    }

    #[test]
    fn test_action_exhaustion_does_not_block_deafen() {
        let config = WsRateLimitConfig {
            window: Duration::from_secs(60),
            max_messages: 10000,
            burst_max: 10000,
            burst_window: Duration::from_secs(1),
            action_max: 3,
            action_window: Duration::from_secs(60),
            deafen_max: 20,
            deafen_window: Duration::from_secs(60),
            max_json_depth: 32,
        };
        let mut limiter = WsRateLimiter::new(&config);

        // Exhaust the action budget
        for _ in 0..3 {
            limiter.action_allow();
        }
        assert!(!limiter.action_allow(), "action budget should be exhausted");

        // SelfDeafen/SelfUndeafen must still work
        assert!(
            limiter.deafen_allow(),
            "SelfDeafen must not be blocked by action budget exhaustion"
        );
    }

    #[test]
    fn test_deafen_limiter_enforces_its_own_cap() {
        let config = WsRateLimitConfig {
            window: Duration::from_secs(60),
            max_messages: 10000,
            burst_max: 10000,
            burst_window: Duration::from_secs(1),
            action_max: 1000,
            action_window: Duration::from_secs(60),
            deafen_max: 4,
            deafen_window: Duration::from_secs(60),
            max_json_depth: 32,
        };
        let mut limiter = WsRateLimiter::new(&config);

        for i in 0..4 {
            assert!(limiter.deafen_allow(), "deafen {} should be allowed", i + 1);
        }
        assert!(
            !limiter.deafen_allow(),
            "5th deafen should be rejected (own cap of 4)"
        );
    }

    // --- Property 5: Action rate limiter rejects after action threshold ---
    // Feature: signaling-auth-and-abuse-controls
    // **Validates: Requirements 3.1, 3.3**

    proptest! {
        #![proptest_config(ProptestConfig::with_cases(200))]

        #[test]
        fn prop_action_rate_limiter_rejects_after_threshold(
            action_max in 1u32..=20u32,
            action_window_secs in 1u64..=60u64,
        ) {
            let config = WsRateLimitConfig {
                window: Duration::from_secs(60),
                max_messages: 10000,
                burst_max: 10000,
                burst_window: Duration::from_secs(1),
                action_max,
                action_window: Duration::from_secs(action_window_secs),
                deafen_max: 1000,
                deafen_window: Duration::from_secs(60),
                max_json_depth: 32,
            };
            let mut limiter = WsRateLimiter::new(&config);

            // First action_max calls must all be accepted
            for i in 0..action_max {
                prop_assert!(
                    limiter.action_allow(),
                    "Action call {} of {} should be accepted", i + 1, action_max
                );
            }

            // The (action_max + 1)th call must be rejected
            prop_assert!(
                !limiter.action_allow(),
                "Action call {} should be rejected (threshold is {})", action_max + 1, action_max
            );
        }
    }
}
