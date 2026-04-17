use std::collections::HashMap;
use std::net::IpAddr;
use std::sync::Mutex;
use std::time::{Duration, Instant};

// ---------------------------------------------------------------------------
// Config
// ---------------------------------------------------------------------------

pub struct RecoveryRateLimiterConfig {
    pub per_ip_max: u32,
    pub per_ip_window_secs: u64,
    pub per_rid_max: u32,
    pub per_rid_window_secs: u64,
}

impl Default for RecoveryRateLimiterConfig {
    fn default() -> Self {
        Self {
            per_ip_max: 5,
            per_ip_window_secs: 3600,
            per_rid_max: 3,
            per_rid_window_secs: 3600,
        }
    }
}

// ---------------------------------------------------------------------------
// SlidingWindow (private)
// ---------------------------------------------------------------------------

struct SlidingWindow {
    timestamps: Vec<Instant>,
}

impl SlidingWindow {
    fn new() -> Self {
        Self {
            timestamps: Vec::new(),
        }
    }

    fn evict_old(&mut self, window: Duration, now: Instant) {
        self.timestamps.retain(|t| now.duration_since(*t) < window);
    }

    fn count(&mut self, window: Duration, now: Instant) -> u32 {
        self.evict_old(window, now);
        self.timestamps.len() as u32
    }

    fn record(&mut self, now: Instant) {
        self.timestamps.push(now);
    }

    fn is_stale(&self, max_window: Duration, now: Instant) -> bool {
        self.timestamps
            .iter()
            .all(|t| now.duration_since(*t) >= max_window)
    }
}

// ---------------------------------------------------------------------------
// RecoveryRateLimiter
// ---------------------------------------------------------------------------

pub struct RecoveryRateLimiter {
    config: RecoveryRateLimiterConfig,
    ip_windows: Mutex<HashMap<IpAddr, SlidingWindow>>,
    rid_windows: Mutex<HashMap<String, SlidingWindow>>,
}

impl RecoveryRateLimiter {
    pub fn new(config: RecoveryRateLimiterConfig) -> Self {
        Self {
            config,
            ip_windows: Mutex::new(HashMap::new()),
            rid_windows: Mutex::new(HashMap::new()),
        }
    }

    /// Returns true if the IP has fewer than `per_ip_max` attempts in the window.
    pub fn check_ip(&self, ip: IpAddr, now: Instant) -> bool {
        let mut map = self.ip_windows.lock().unwrap();
        let window = map.entry(ip).or_insert_with(SlidingWindow::new);
        let dur = Duration::from_secs(self.config.per_ip_window_secs);
        window.count(dur, now) < self.config.per_ip_max
    }

    /// Record a recovery attempt for the given IP.
    pub fn record_ip(&self, ip: IpAddr, now: Instant) {
        let mut map = self.ip_windows.lock().unwrap();
        let window = map.entry(ip).or_insert_with(SlidingWindow::new);
        window.record(now);
    }

    /// Returns true if the recovery_id has fewer than `per_rid_max` attempts in the window.
    pub fn check_recovery_id(&self, rid: &str, now: Instant) -> bool {
        let mut map = self.rid_windows.lock().unwrap();
        let window = map
            .entry(rid.to_string())
            .or_insert_with(SlidingWindow::new);
        let dur = Duration::from_secs(self.config.per_rid_window_secs);
        window.count(dur, now) < self.config.per_rid_max
    }

    /// Record a recovery attempt for the given recovery_id.
    pub fn record_recovery_id(&self, rid: &str, now: Instant) {
        let mut map = self.rid_windows.lock().unwrap();
        let window = map
            .entry(rid.to_string())
            .or_insert_with(SlidingWindow::new);
        window.record(now);
    }

    /// Clear all tracked state. Used by test-metrics reset endpoint.
    pub fn clear(&self) {
        self.ip_windows.lock().unwrap().clear();
        self.rid_windows.lock().unwrap().clear();
    }

    /// Prune stale entries from both maps. Returns total entries removed.
    pub fn prune_stale(&self, now: Instant) -> usize {
        let ip_dur = Duration::from_secs(self.config.per_ip_window_secs);
        let rid_dur = Duration::from_secs(self.config.per_rid_window_secs);

        let mut count = 0;
        {
            let mut map = self.ip_windows.lock().unwrap();
            let before = map.len();
            map.retain(|_, w| !w.is_stale(ip_dur, now));
            count += before - map.len();
        }
        {
            let mut map = self.rid_windows.lock().unwrap();
            let before = map.len();
            map.retain(|_, w| !w.is_stale(rid_dur, now));
            count += before - map.len();
        }
        count
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use proptest::prelude::*;
    use std::net::{IpAddr, Ipv4Addr};

    #[test]
    fn ip_allows_up_to_max_then_rejects() {
        let limiter = RecoveryRateLimiter::new(RecoveryRateLimiterConfig::default());
        let ip = IpAddr::V4(Ipv4Addr::new(10, 0, 0, 1));
        let now = Instant::now();

        for _ in 0..5 {
            assert!(limiter.check_ip(ip, now));
            limiter.record_ip(ip, now);
        }
        assert!(!limiter.check_ip(ip, now));
    }

    #[test]
    fn ip_resets_after_window() {
        let limiter = RecoveryRateLimiter::new(RecoveryRateLimiterConfig::default());
        let ip = IpAddr::V4(Ipv4Addr::new(10, 0, 0, 1));
        let now = Instant::now();

        for _ in 0..5 {
            limiter.record_ip(ip, now);
        }
        assert!(!limiter.check_ip(ip, now));

        let after = now + Duration::from_secs(3601);
        assert!(limiter.check_ip(ip, after));
    }

    #[test]
    fn rid_allows_up_to_max_then_rejects() {
        let limiter = RecoveryRateLimiter::new(RecoveryRateLimiterConfig::default());
        let now = Instant::now();

        for _ in 0..3 {
            assert!(limiter.check_recovery_id("wvs-ABCD-EFGH", now));
            limiter.record_recovery_id("wvs-ABCD-EFGH", now);
        }
        assert!(!limiter.check_recovery_id("wvs-ABCD-EFGH", now));
    }

    #[test]
    fn rid_resets_after_window() {
        let limiter = RecoveryRateLimiter::new(RecoveryRateLimiterConfig::default());
        let now = Instant::now();

        for _ in 0..3 {
            limiter.record_recovery_id("wvs-ABCD-EFGH", now);
        }
        assert!(!limiter.check_recovery_id("wvs-ABCD-EFGH", now));

        let after = now + Duration::from_secs(3601);
        assert!(limiter.check_recovery_id("wvs-ABCD-EFGH", after));
    }

    #[test]
    fn different_ips_tracked_independently() {
        let limiter = RecoveryRateLimiter::new(RecoveryRateLimiterConfig::default());
        let ip1 = IpAddr::V4(Ipv4Addr::new(10, 0, 0, 1));
        let ip2 = IpAddr::V4(Ipv4Addr::new(10, 0, 0, 2));
        let now = Instant::now();

        for _ in 0..5 {
            limiter.record_ip(ip1, now);
        }
        assert!(!limiter.check_ip(ip1, now));
        assert!(limiter.check_ip(ip2, now));
    }

    #[test]
    fn different_rids_tracked_independently() {
        let limiter = RecoveryRateLimiter::new(RecoveryRateLimiterConfig::default());
        let now = Instant::now();

        for _ in 0..3 {
            limiter.record_recovery_id("wvs-AAAA-BBBB", now);
        }
        assert!(!limiter.check_recovery_id("wvs-AAAA-BBBB", now));
        assert!(limiter.check_recovery_id("wvs-CCCC-DDDD", now));
    }

    #[test]
    fn prune_stale_removes_expired_entries() {
        let limiter = RecoveryRateLimiter::new(RecoveryRateLimiterConfig::default());
        let ip = IpAddr::V4(Ipv4Addr::new(10, 0, 0, 1));
        let now = Instant::now();

        limiter.record_ip(ip, now);
        limiter.record_recovery_id("wvs-AAAA-BBBB", now);

        let after = now + Duration::from_secs(3601);
        let pruned = limiter.prune_stale(after);
        assert_eq!(pruned, 2);
    }

    // Feature: user-identity-recovery, Property 10: Recovery rate limiter ceiling
    // **Validates: Requirements 6.1, 6.2**
    proptest! {
        #[test]
        fn prop_ip_ceiling_enforced_and_window_resets(
            per_ip_max in 1u32..20,
            per_ip_window_secs in 60u64..7200,
            a in 0u8..=255,
            b in 0u8..=255,
            c in 0u8..=255,
            d in 0u8..=255,
        ) {
            let config = RecoveryRateLimiterConfig {
                per_ip_max,
                per_ip_window_secs,
                per_rid_max: 3,
                per_rid_window_secs: 3600,
            };
            let limiter = RecoveryRateLimiter::new(config);
            let ip = IpAddr::V4(Ipv4Addr::new(a, b, c, d));
            let now = Instant::now();

            // Record exactly per_ip_max attempts — all should be allowed
            for _ in 0..per_ip_max {
                prop_assert!(limiter.check_ip(ip, now));
                limiter.record_ip(ip, now);
            }

            // The next check must be rejected (ceiling enforced)
            prop_assert!(!limiter.check_ip(ip, now));

            // After the window expires, checks should be allowed again
            let after_window = now + Duration::from_secs(per_ip_window_secs + 1);
            prop_assert!(limiter.check_ip(ip, after_window));
        }

        #[test]
        fn prop_rid_ceiling_enforced_and_window_resets(
            per_rid_max in 1u32..10,
            per_rid_window_secs in 60u64..7200,
            rid_suffix in "[A-Z0-9]{4}-[A-Z0-9]{4}",
        ) {
            let config = RecoveryRateLimiterConfig {
                per_ip_max: 5,
                per_ip_window_secs: 3600,
                per_rid_max,
                per_rid_window_secs,
            };
            let limiter = RecoveryRateLimiter::new(config);
            let rid = format!("wvs-{rid_suffix}");
            let now = Instant::now();

            // Record exactly per_rid_max attempts — all should be allowed
            for _ in 0..per_rid_max {
                prop_assert!(limiter.check_recovery_id(&rid, now));
                limiter.record_recovery_id(&rid, now);
            }

            // The next check must be rejected (ceiling enforced)
            prop_assert!(!limiter.check_recovery_id(&rid, now));

            // After the window expires, checks should be allowed again
            let after_window = now + Duration::from_secs(per_rid_window_secs + 1);
            prop_assert!(limiter.check_recovery_id(&rid, after_window));
        }
    }
}
