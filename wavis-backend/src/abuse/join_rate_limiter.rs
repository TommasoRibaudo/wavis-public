use std::collections::{HashMap, VecDeque};
use std::net::IpAddr;
use std::sync::RwLock;
use std::time::{Duration, Instant};

use shared::signaling::JoinRejectionReason;

// ---------------------------------------------------------------------------
// SlidingWindow
// ---------------------------------------------------------------------------

pub struct SlidingWindow {
    timestamps: VecDeque<Instant>,
    cooldown_until: Option<Instant>,
}

impl SlidingWindow {
    fn new() -> Self {
        Self {
            timestamps: VecDeque::new(),
            cooldown_until: None,
        }
    }

    /// Evict timestamps older than `window`.
    fn evict_old(&mut self, window: Duration, now: Instant) {
        let cutoff = now.checked_sub(window).unwrap_or(now);
        while let Some(&front) = self.timestamps.front() {
            if front < cutoff {
                self.timestamps.pop_front();
            } else {
                break;
            }
        }
    }

    /// Returns true if currently in cooldown.
    fn in_cooldown(&self, now: Instant) -> bool {
        self.cooldown_until.is_some_and(|until| now < until)
    }

    /// Check whether recording one more attempt would exceed `threshold`.
    /// Returns true if the attempt is allowed (count after recording <= threshold).
    fn check(&mut self, threshold: u32, window: Duration, now: Instant) -> bool {
        if self.in_cooldown(now) {
            return false;
        }
        self.evict_old(window, now);
        // current count + 1 (the attempt being checked) must be <= threshold
        (self.timestamps.len() as u32) < threshold
    }

    /// Record an attempt. If the count now exceeds `threshold`, set cooldown.
    fn record(&mut self, threshold: u32, window: Duration, cooldown: Duration, now: Instant) {
        self.evict_old(window, now);
        self.timestamps.push_back(now);
        if self.timestamps.len() as u32 > threshold {
            self.cooldown_until = Some(now + cooldown);
        }
    }

    /// True if this window has no timestamps and no active cooldown — safe to prune.
    fn is_stale(&self, now: Instant) -> bool {
        self.timestamps.is_empty() && !self.in_cooldown(now)
    }
}

// ---------------------------------------------------------------------------
// Config
// ---------------------------------------------------------------------------

#[derive(Clone)]
pub struct JoinRateLimiterConfig {
    pub ip_total_threshold: u32,
    pub ip_total_window: Duration,
    pub ip_failed_threshold: u32,
    pub ip_failed_window: Duration,
    pub code_threshold: u32,
    pub code_window: Duration,
    pub room_threshold: u32,
    pub room_window: Duration,
    pub connection_threshold: u32,
    pub connection_window: Duration,
    pub cooldown: Duration,
}

impl Default for JoinRateLimiterConfig {
    fn default() -> Self {
        Self {
            ip_total_threshold: 30,
            ip_total_window: Duration::from_secs(60),
            ip_failed_threshold: 10,
            ip_failed_window: Duration::from_secs(60),
            code_threshold: 10,
            code_window: Duration::from_secs(60),
            room_threshold: 12,
            room_window: Duration::from_secs(60),
            connection_threshold: 5,
            connection_window: Duration::from_secs(60),
            cooldown: Duration::from_secs(60),
        }
    }
}

// ---------------------------------------------------------------------------
// JoinRateLimiter
// ---------------------------------------------------------------------------

pub struct JoinRateLimiter {
    ip_total: RwLock<HashMap<IpAddr, SlidingWindow>>,
    ip_failed: RwLock<HashMap<IpAddr, SlidingWindow>>,
    code_attempts: RwLock<HashMap<String, SlidingWindow>>,
    room_attempts: RwLock<HashMap<String, SlidingWindow>>,
    connection_attempts: RwLock<HashMap<String, SlidingWindow>>,
    config: RwLock<JoinRateLimiterConfig>,
}

impl JoinRateLimiter {
    pub fn new(config: JoinRateLimiterConfig) -> Self {
        Self {
            ip_total: RwLock::new(HashMap::new()),
            ip_failed: RwLock::new(HashMap::new()),
            code_attempts: RwLock::new(HashMap::new()),
            room_attempts: RwLock::new(HashMap::new()),
            connection_attempts: RwLock::new(HashMap::new()),
            config: RwLock::new(config),
        }
    }

    /// Replace the rate limiter configuration and clear all sliding windows.
    /// Used by the stress harness to swap presets at runtime through the shared `Arc`.
    pub fn reconfigure(&self, new_config: JoinRateLimiterConfig) {
        // Clear all windows first so stale cooldowns from the old config don't linger.
        self.ip_total.write().unwrap().clear();
        self.ip_failed.write().unwrap().clear();
        self.code_attempts.write().unwrap().clear();
        self.room_attempts.write().unwrap().clear();
        self.connection_attempts.write().unwrap().clear();
        *self.config.write().unwrap() = new_config;
    }

    /// Check all rate limit dimensions. Returns Ok(()) or Err(RateLimited).
    /// Does NOT record the attempt — call `record_attempt` after the outcome is known.
    pub fn check_join(
        &self,
        ip: IpAddr,
        code: Option<&str>,
        room_id: &str,
        connection_id: &str,
        now: Instant,
    ) -> Result<(), JoinRejectionReason> {
        let cfg = self.config.read().unwrap();
        // ip_total
        {
            let mut map = self.ip_total.write().unwrap();
            let w = map.entry(ip).or_insert_with(SlidingWindow::new);
            if !w.check(cfg.ip_total_threshold, cfg.ip_total_window, now) {
                return Err(JoinRejectionReason::RateLimited);
            }
        }
        // ip_failed
        {
            let mut map = self.ip_failed.write().unwrap();
            let w = map.entry(ip).or_insert_with(SlidingWindow::new);
            if !w.check(cfg.ip_failed_threshold, cfg.ip_failed_window, now) {
                return Err(JoinRejectionReason::RateLimited);
            }
        }
        // per_code
        if let Some(code) = code {
            let mut map = self.code_attempts.write().unwrap();
            let w = map
                .entry(code.to_string())
                .or_insert_with(SlidingWindow::new);
            if !w.check(cfg.code_threshold, cfg.code_window, now) {
                return Err(JoinRejectionReason::RateLimited);
            }
        }
        // per_room
        {
            let mut map = self.room_attempts.write().unwrap();
            let w = map
                .entry(room_id.to_string())
                .or_insert_with(SlidingWindow::new);
            if !w.check(cfg.room_threshold, cfg.room_window, now) {
                return Err(JoinRejectionReason::RateLimited);
            }
        }
        // per_connection
        {
            let mut map = self.connection_attempts.write().unwrap();
            let w = map
                .entry(connection_id.to_string())
                .or_insert_with(SlidingWindow::new);
            if !w.check(cfg.connection_threshold, cfg.connection_window, now) {
                return Err(JoinRejectionReason::RateLimited);
            }
        }
        Ok(())
    }

    /// Record a join attempt across all applicable dimensions.
    /// `failed` indicates whether the join was rejected (records in ip_failed if true).
    pub fn record_attempt(
        &self,
        ip: IpAddr,
        code: Option<&str>,
        room_id: &str,
        connection_id: &str,
        failed: bool,
        now: Instant,
    ) {
        let cfg = self.config.read().unwrap();

        {
            let mut map = self.ip_total.write().unwrap();
            let w = map.entry(ip).or_insert_with(SlidingWindow::new);
            w.record(
                cfg.ip_total_threshold,
                cfg.ip_total_window,
                cfg.cooldown,
                now,
            );
        }
        if failed {
            let mut map = self.ip_failed.write().unwrap();
            let w = map.entry(ip).or_insert_with(SlidingWindow::new);
            w.record(
                cfg.ip_failed_threshold,
                cfg.ip_failed_window,
                cfg.cooldown,
                now,
            );
        }
        if let Some(code) = code {
            let mut map = self.code_attempts.write().unwrap();
            let w = map
                .entry(code.to_string())
                .or_insert_with(SlidingWindow::new);
            w.record(cfg.code_threshold, cfg.code_window, cfg.cooldown, now);
        }
        {
            let mut map = self.room_attempts.write().unwrap();
            let w = map
                .entry(room_id.to_string())
                .or_insert_with(SlidingWindow::new);
            w.record(cfg.room_threshold, cfg.room_window, cfg.cooldown, now);
        }
        {
            let mut map = self.connection_attempts.write().unwrap();
            let w = map
                .entry(connection_id.to_string())
                .or_insert_with(SlidingWindow::new);
            w.record(
                cfg.connection_threshold,
                cfg.connection_window,
                cfg.cooldown,
                now,
            );
        }

        self.prune_stale_entries(now);
    }

    /// Prune entries that have no timestamps and no active cooldown.
    fn prune_stale_entries(&self, now: Instant) -> usize {
        macro_rules! prune {
            ($field:expr) => {{
                let before = $field.read().unwrap().len();
                $field.write().unwrap().retain(|_, w| !w.is_stale(now));
                let after = $field.read().unwrap().len();
                before.saturating_sub(after)
            }};
        }
        prune!(self.ip_total)
            + prune!(self.ip_failed)
            + prune!(self.code_attempts)
            + prune!(self.room_attempts)
            + prune!(self.connection_attempts)
    }

    /// Full sweep for background task. Returns the number of stale entries pruned.
    pub fn prune_all(&self, now: Instant) -> usize {
        self.prune_stale_entries(now)
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use proptest::prelude::*;

    // -----------------------------------------------------------------------
    // Helpers
    // -----------------------------------------------------------------------

    fn ip(n: u8) -> IpAddr {
        IpAddr::V4(std::net::Ipv4Addr::new(10, 0, 0, n))
    }

    /// Build a config where only one dimension has a low threshold; all others
    /// are set very high so they never interfere.
    fn isolated_config(
        threshold: u32,
        window: Duration,
        cooldown: Duration,
    ) -> JoinRateLimiterConfig {
        JoinRateLimiterConfig {
            ip_total_threshold: threshold,
            ip_total_window: window,
            ip_failed_threshold: threshold,
            ip_failed_window: window,
            code_threshold: threshold,
            code_window: window,
            room_threshold: threshold,
            room_window: window,
            connection_threshold: threshold,
            connection_window: window,
            cooldown,
        }
    }

    // -----------------------------------------------------------------------
    // Property 11: Rate limiter rejects after threshold (per dimension)
    // Feature: invite-code-hardening, Property 11: Rate limiter rejects after threshold
    // -----------------------------------------------------------------------

    proptest! {
        #![proptest_config(ProptestConfig::with_cases(200))]

        // --- ip_total dimension ---
        // Feature: invite-code-hardening, Property 11: Rate limiter rejects after threshold
        // Validates: Requirements 6.1, 6.3
        #[test]
        fn prop_ip_total_rejects_after_threshold(threshold in 1u32..=20u32) {
            let cfg = JoinRateLimiterConfig {
                ip_total_threshold: threshold,
                ip_total_window: Duration::from_secs(60),
                ip_failed_threshold: 9999,
                ip_failed_window: Duration::from_secs(60),
                code_threshold: 9999,
                code_window: Duration::from_secs(60),
                room_threshold: 9999,
                room_window: Duration::from_secs(60),
                connection_threshold: 9999,
                connection_window: Duration::from_secs(60),
                cooldown: Duration::from_secs(60),
            };
            let rl = JoinRateLimiter::new(cfg);
            let test_ip = ip(1);
            let now = Instant::now();

            // Record exactly `threshold` attempts — each check before recording should pass
            for i in 0..threshold {
                let result = rl.check_join(test_ip, Some("code"), "room1", "conn1", now);
                prop_assert!(result.is_ok(), "attempt {} of {} should be allowed", i + 1, threshold);
                rl.record_attempt(test_ip, Some("code"), "room1", "conn1", false, now);
            }

            // The (threshold+1)th attempt must be rejected
            let result = rl.check_join(test_ip, Some("code"), "room1", "conn1", now);
            prop_assert_eq!(result, Err(JoinRejectionReason::RateLimited));
        }

        // --- ip_failed dimension ---
        // Feature: invite-code-hardening, Property 11: Rate limiter rejects after threshold
        // Validates: Requirements 6.2, 6.3
        #[test]
        fn prop_ip_failed_rejects_after_threshold(threshold in 1u32..=20u32) {
            let cfg = JoinRateLimiterConfig {
                ip_total_threshold: 9999,
                ip_total_window: Duration::from_secs(60),
                ip_failed_threshold: threshold,
                ip_failed_window: Duration::from_secs(60),
                code_threshold: 9999,
                code_window: Duration::from_secs(60),
                room_threshold: 9999,
                room_window: Duration::from_secs(60),
                connection_threshold: 9999,
                connection_window: Duration::from_secs(60),
                cooldown: Duration::from_secs(60),
            };
            let rl = JoinRateLimiter::new(cfg);
            let test_ip = ip(2);
            let now = Instant::now();

            // Record exactly `threshold` failed attempts
            for i in 0..threshold {
                let result = rl.check_join(test_ip, Some("code"), "room1", "conn1", now);
                prop_assert!(result.is_ok(), "failed attempt {} of {} should be allowed", i + 1, threshold);
                rl.record_attempt(test_ip, Some("code"), "room1", "conn1", true, now);
            }

            // Next check must be rejected (ip_failed cooldown active)
            let result = rl.check_join(test_ip, Some("code"), "room1", "conn1", now);
            prop_assert_eq!(result, Err(JoinRejectionReason::RateLimited));
        }

        // --- per_code dimension ---
        // Feature: invite-code-hardening, Property 11: Rate limiter rejects after threshold
        // Validates: Requirements 6.4, 6.5
        #[test]
        fn prop_per_code_rejects_after_threshold(threshold in 1u32..=20u32) {
            let cfg = JoinRateLimiterConfig {
                ip_total_threshold: 9999,
                ip_total_window: Duration::from_secs(60),
                ip_failed_threshold: 9999,
                ip_failed_window: Duration::from_secs(60),
                code_threshold: threshold,
                code_window: Duration::from_secs(60),
                room_threshold: 9999,
                room_window: Duration::from_secs(60),
                connection_threshold: 9999,
                connection_window: Duration::from_secs(60),
                cooldown: Duration::from_secs(60),
            };
            let rl = JoinRateLimiter::new(cfg);
            let now = Instant::now();

            // Use distinct IPs so ip_total/ip_failed never trigger
            for i in 0..threshold {
                let test_ip = ip((i % 200 + 1) as u8);
                let result = rl.check_join(test_ip, Some("shared-code"), "room1", &format!("conn{i}"), now);
                prop_assert!(result.is_ok(), "code attempt {} of {} should be allowed", i + 1, threshold);
                rl.record_attempt(test_ip, Some("shared-code"), "room1", &format!("conn{i}"), false, now);
            }

            let extra_ip = ip(255);
            let result = rl.check_join(extra_ip, Some("shared-code"), "room1", "conn-extra", now);
            prop_assert_eq!(result, Err(JoinRejectionReason::RateLimited));
        }

        // --- per_room dimension ---
        // Feature: invite-code-hardening, Property 11: Rate limiter rejects after threshold
        // Validates: Requirements 6.6, 6.7
        #[test]
        fn prop_per_room_rejects_after_threshold(threshold in 1u32..=20u32) {
            let cfg = JoinRateLimiterConfig {
                ip_total_threshold: 9999,
                ip_total_window: Duration::from_secs(60),
                ip_failed_threshold: 9999,
                ip_failed_window: Duration::from_secs(60),
                code_threshold: 9999,
                code_window: Duration::from_secs(60),
                room_threshold: threshold,
                room_window: Duration::from_secs(60),
                connection_threshold: 9999,
                connection_window: Duration::from_secs(60),
                cooldown: Duration::from_secs(60),
            };
            let rl = JoinRateLimiter::new(cfg);
            let now = Instant::now();

            for i in 0..threshold {
                let test_ip = ip((i % 200 + 1) as u8);
                let result = rl.check_join(test_ip, None, "hot-room", &format!("conn{i}"), now);
                prop_assert!(result.is_ok(), "room attempt {} of {} should be allowed", i + 1, threshold);
                rl.record_attempt(test_ip, None, "hot-room", &format!("conn{i}"), false, now);
            }

            let extra_ip = ip(255);
            let result = rl.check_join(extra_ip, None, "hot-room", "conn-extra", now);
            prop_assert_eq!(result, Err(JoinRejectionReason::RateLimited));
        }

        // --- per_connection dimension ---
        // Feature: invite-code-hardening, Property 11: Rate limiter rejects after threshold
        // Validates: Requirements 6.8
        #[test]
        fn prop_per_connection_rejects_after_threshold(threshold in 1u32..=20u32) {
            let cfg = JoinRateLimiterConfig {
                ip_total_threshold: 9999,
                ip_total_window: Duration::from_secs(60),
                ip_failed_threshold: 9999,
                ip_failed_window: Duration::from_secs(60),
                code_threshold: 9999,
                code_window: Duration::from_secs(60),
                room_threshold: 9999,
                room_window: Duration::from_secs(60),
                connection_threshold: threshold,
                connection_window: Duration::from_secs(60),
                cooldown: Duration::from_secs(60),
            };
            let rl = JoinRateLimiter::new(cfg);
            let now = Instant::now();

            for i in 0..threshold {
                // Different rooms to avoid room threshold; different IPs to avoid IP threshold
                let test_ip = ip((i % 200 + 1) as u8);
                let result = rl.check_join(test_ip, None, &format!("room{i}"), "sticky-conn", now);
                prop_assert!(result.is_ok(), "connection attempt {} of {} should be allowed", i + 1, threshold);
                rl.record_attempt(test_ip, None, &format!("room{i}"), "sticky-conn", false, now);
            }

            let extra_ip = ip(255);
            let result = rl.check_join(extra_ip, None, "room-extra", "sticky-conn", now);
            prop_assert_eq!(result, Err(JoinRejectionReason::RateLimited));
        }

        // --- cooldown expiration restores access ---
        // Feature: invite-code-hardening, Property 11: Rate limiter rejects after threshold
        // Validates: Requirements 6.3, 6.5, 6.7, 6.8
        #[test]
        fn prop_cooldown_expiration_restores_access(threshold in 1u32..=10u32) {
            // Use a very short window and cooldown so we can simulate expiry by advancing `now`.
            // Both must be short so that at t_after: timestamps are evicted (window expired)
            // AND cooldown has passed.
            let window = Duration::from_millis(50);
            let cooldown = Duration::from_millis(50);
            let cfg = isolated_config(threshold, window, cooldown);
            let rl = JoinRateLimiter::new(cfg);
            let test_ip = ip(1);
            let t0 = Instant::now();

            // Exhaust the threshold on all dimensions simultaneously
            for _ in 0..threshold {
                rl.record_attempt(test_ip, Some("code"), "room1", "conn1", true, t0);
            }

            // Immediately after exhaustion, check should be rejected
            let blocked = rl.check_join(test_ip, Some("code"), "room1", "conn1", t0);
            prop_assert_eq!(blocked, Err(JoinRejectionReason::RateLimited));

            // Advance time past both the window and the cooldown
            let t_after = t0 + window + cooldown + Duration::from_millis(10);

            // Access should be restored: timestamps evicted (window expired) and cooldown passed
            let restored = rl.check_join(test_ip, Some("code"), "room1", "conn1", t_after);
            prop_assert!(
                restored.is_ok(),
                "access should be restored after window+cooldown expires, got {:?}",
                restored
            );
        }
    }

    // -----------------------------------------------------------------------
    // Unit tests for specific edge cases
    // -----------------------------------------------------------------------

    #[test]
    fn different_ips_are_tracked_independently() {
        let cfg = JoinRateLimiterConfig {
            ip_total_threshold: 2,
            ip_total_window: Duration::from_secs(60),
            ..isolated_config(9999, Duration::from_secs(60), Duration::from_secs(60))
        };
        let rl = JoinRateLimiter::new(cfg);
        let now = Instant::now();

        // Exhaust ip1
        rl.record_attempt(ip(1), None, "room", "c1", false, now);
        rl.record_attempt(ip(1), None, "room", "c1", false, now);
        assert_eq!(
            rl.check_join(ip(1), None, "room", "c1", now),
            Err(JoinRejectionReason::RateLimited)
        );

        // ip2 should still be allowed (different key)
        assert!(rl.check_join(ip(2), None, "room", "c2", now).is_ok());
    }

    #[test]
    fn window_expiry_clears_old_timestamps() {
        let window = Duration::from_millis(100);
        let cfg = JoinRateLimiterConfig {
            ip_total_threshold: 2,
            ip_total_window: window,
            ip_failed_threshold: 9999,
            ip_failed_window: window,
            code_threshold: 9999,
            code_window: window,
            room_threshold: 9999,
            room_window: window,
            connection_threshold: 9999,
            connection_window: window,
            cooldown: Duration::from_millis(10),
        };
        let rl = JoinRateLimiter::new(cfg);
        let t0 = Instant::now();

        // Fill up the window at t0
        rl.record_attempt(ip(1), None, "room", "conn", false, t0);
        rl.record_attempt(ip(1), None, "room", "conn", false, t0);

        // At t0 the 3rd attempt is blocked
        assert_eq!(
            rl.check_join(ip(1), None, "room", "conn", t0),
            Err(JoinRejectionReason::RateLimited)
        );

        // After window + cooldown elapses, old timestamps are evicted and cooldown expired
        let t1 = t0 + window + Duration::from_millis(20);
        assert!(rl.check_join(ip(1), None, "room", "conn", t1).is_ok());
    }
}
