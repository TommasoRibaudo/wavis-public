use std::collections::HashMap;
use std::net::IpAddr;
use std::sync::Mutex;
use std::time::{Duration, Instant};
use uuid::Uuid;

// ---------------------------------------------------------------------------
// Config
// ---------------------------------------------------------------------------

pub struct BugReportRateLimiterConfig {
    pub max_requests: u32,
    pub window: Duration,
}

impl Default for BugReportRateLimiterConfig {
    fn default() -> Self {
        Self {
            max_requests: 5,
            window: Duration::from_secs(3600),
        }
    }
}

impl BugReportRateLimiterConfig {
    pub fn from_env() -> Self {
        let max_requests = std::env::var("BUG_REPORT_RATE_LIMIT_MAX")
            .ok()
            .and_then(|v| v.parse().ok())
            .unwrap_or(5);
        let window_secs: u64 = std::env::var("BUG_REPORT_RATE_LIMIT_WINDOW_SECS")
            .ok()
            .and_then(|v| v.parse().ok())
            .unwrap_or(3600);
        Self {
            max_requests,
            window: Duration::from_secs(window_secs),
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

    /// Returns the duration until the oldest entry expires, or `None` if under the limit.
    fn seconds_until_retry(&mut self, window: Duration, now: Instant, max: u32) -> Option<u64> {
        self.evict_old(window, now);
        if (self.timestamps.len() as u32) < max {
            return None;
        }
        // Find the oldest timestamp — that's the one that will expire first.
        self.timestamps.iter().min().map(|oldest| {
            let elapsed = now.duration_since(*oldest);
            if elapsed >= window {
                1 // Already expired on next tick
            } else {
                (window - elapsed).as_secs().max(1)
            }
        })
    }
}

// ---------------------------------------------------------------------------
// BugReportRateLimiter
// ---------------------------------------------------------------------------

pub struct BugReportRateLimiter {
    config: BugReportRateLimiterConfig,
    ip_windows: Mutex<HashMap<IpAddr, SlidingWindow>>,
    user_windows: Mutex<HashMap<Uuid, SlidingWindow>>,
}

impl BugReportRateLimiter {
    pub fn new(config: BugReportRateLimiterConfig) -> Self {
        Self {
            config,
            ip_windows: Mutex::new(HashMap::new()),
            user_windows: Mutex::new(HashMap::new()),
        }
    }

    /// Returns true if the IP has fewer than `max_requests` submissions in the window.
    pub fn check_ip(&self, ip: IpAddr, now: Instant) -> bool {
        let mut map = self.ip_windows.lock().unwrap();
        let window = map.entry(ip).or_insert_with(SlidingWindow::new);
        window.count(self.config.window, now) < self.config.max_requests
    }

    /// Record a bug report submission for the given IP.
    pub fn record_ip(&self, ip: IpAddr, now: Instant) {
        let mut map = self.ip_windows.lock().unwrap();
        let window = map.entry(ip).or_insert_with(SlidingWindow::new);
        window.record(now);
    }

    /// Returns true if the user has fewer than `max_requests` submissions in the window.
    pub fn check_user(&self, user_id: Uuid, now: Instant) -> bool {
        let mut map = self.user_windows.lock().unwrap();
        let window = map.entry(user_id).or_insert_with(SlidingWindow::new);
        window.count(self.config.window, now) < self.config.max_requests
    }

    /// Record a bug report submission for the given user.
    pub fn record_user(&self, user_id: Uuid, now: Instant) {
        let mut map = self.user_windows.lock().unwrap();
        let window = map.entry(user_id).or_insert_with(SlidingWindow::new);
        window.record(now);
    }

    /// Returns seconds until the IP can retry, or `None` if under the limit.
    pub fn seconds_until_retry_ip(&self, ip: IpAddr, now: Instant) -> Option<u64> {
        let mut map = self.ip_windows.lock().unwrap();
        let window = map.entry(ip).or_insert_with(SlidingWindow::new);
        window.seconds_until_retry(self.config.window, now, self.config.max_requests)
    }

    /// Returns seconds until the user can retry, or `None` if under the limit.
    pub fn seconds_until_retry_user(&self, user_id: Uuid, now: Instant) -> Option<u64> {
        let mut map = self.user_windows.lock().unwrap();
        let window = map.entry(user_id).or_insert_with(SlidingWindow::new);
        window.seconds_until_retry(self.config.window, now, self.config.max_requests)
    }

    /// Prune stale entries from both maps. Returns total entries removed.
    pub fn prune_stale(&self, now: Instant) -> usize {
        let mut count = 0;
        {
            let mut map = self.ip_windows.lock().unwrap();
            let before = map.len();
            map.retain(|_, w| !w.is_stale(self.config.window, now));
            count += before - map.len();
        }
        {
            let mut map = self.user_windows.lock().unwrap();
            let before = map.len();
            map.retain(|_, w| !w.is_stale(self.config.window, now));
            count += before - map.len();
        }
        count
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::net::Ipv4Addr;

    #[test]
    fn ip_allows_up_to_max_then_rejects() {
        let limiter = BugReportRateLimiter::new(BugReportRateLimiterConfig::default());
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
        let limiter = BugReportRateLimiter::new(BugReportRateLimiterConfig::default());
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
    fn user_allows_up_to_max_then_rejects() {
        let limiter = BugReportRateLimiter::new(BugReportRateLimiterConfig::default());
        let user_id = Uuid::new_v4();
        let now = Instant::now();

        for _ in 0..5 {
            assert!(limiter.check_user(user_id, now));
            limiter.record_user(user_id, now);
        }
        assert!(!limiter.check_user(user_id, now));
    }

    #[test]
    fn user_resets_after_window() {
        let limiter = BugReportRateLimiter::new(BugReportRateLimiterConfig::default());
        let user_id = Uuid::new_v4();
        let now = Instant::now();

        for _ in 0..5 {
            limiter.record_user(user_id, now);
        }
        assert!(!limiter.check_user(user_id, now));

        let after = now + Duration::from_secs(3601);
        assert!(limiter.check_user(user_id, after));
    }

    #[test]
    fn different_ips_tracked_independently() {
        let limiter = BugReportRateLimiter::new(BugReportRateLimiterConfig::default());
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
    fn different_users_tracked_independently() {
        let limiter = BugReportRateLimiter::new(BugReportRateLimiterConfig::default());
        let user1 = Uuid::new_v4();
        let user2 = Uuid::new_v4();
        let now = Instant::now();

        for _ in 0..5 {
            limiter.record_user(user1, now);
        }
        assert!(!limiter.check_user(user1, now));
        assert!(limiter.check_user(user2, now));
    }

    #[test]
    fn prune_stale_removes_expired_entries() {
        let limiter = BugReportRateLimiter::new(BugReportRateLimiterConfig::default());
        let ip = IpAddr::V4(Ipv4Addr::new(10, 0, 0, 1));
        let user_id = Uuid::new_v4();
        let now = Instant::now();

        limiter.record_ip(ip, now);
        limiter.record_user(user_id, now);

        let after = now + Duration::from_secs(3601);
        let pruned = limiter.prune_stale(after);
        assert_eq!(pruned, 2);
    }

    #[test]
    fn seconds_until_retry_ip_returns_none_when_under_limit() {
        let limiter = BugReportRateLimiter::new(BugReportRateLimiterConfig::default());
        let ip = IpAddr::V4(Ipv4Addr::new(10, 0, 0, 1));
        let now = Instant::now();

        limiter.record_ip(ip, now);
        assert!(limiter.seconds_until_retry_ip(ip, now).is_none());
    }

    #[test]
    fn seconds_until_retry_ip_returns_seconds_when_at_limit() {
        let limiter = BugReportRateLimiter::new(BugReportRateLimiterConfig::default());
        let ip = IpAddr::V4(Ipv4Addr::new(10, 0, 0, 1));
        let now = Instant::now();

        for _ in 0..5 {
            limiter.record_ip(ip, now);
        }
        let retry = limiter.seconds_until_retry_ip(ip, now);
        assert!(retry.is_some());
        assert!(retry.unwrap() <= 3600);
    }

    #[test]
    fn seconds_until_retry_user_returns_none_when_under_limit() {
        let limiter = BugReportRateLimiter::new(BugReportRateLimiterConfig::default());
        let user_id = Uuid::new_v4();
        let now = Instant::now();

        limiter.record_user(user_id, now);
        assert!(limiter.seconds_until_retry_user(user_id, now).is_none());
    }

    #[test]
    fn seconds_until_retry_user_returns_seconds_when_at_limit() {
        let limiter = BugReportRateLimiter::new(BugReportRateLimiterConfig::default());
        let user_id = Uuid::new_v4();
        let now = Instant::now();

        for _ in 0..5 {
            limiter.record_user(user_id, now);
        }
        let retry = limiter.seconds_until_retry_user(user_id, now);
        assert!(retry.is_some());
        assert!(retry.unwrap() <= 3600);
    }

    // -----------------------------------------------------------------------
    // Property-based tests
    // Feature: in-app-bug-report, Property 16: Rate limit enforcement
    // **Validates: Requirements 14.1, 14.2**
    // -----------------------------------------------------------------------

    use proptest::prelude::*;

    proptest! {
        #![proptest_config(ProptestConfig::with_cases(100))]

        /// For any number of submissions from the same IP, the first 5 are
        /// allowed and any beyond 5 are rejected. `seconds_until_retry_ip`
        /// returns `Some` when at the limit and `None` when under.
        #[test]
        fn prop_rate_limit_enforcement_ip(
            num_submissions in 1u32..20,
            ip_octets in prop::array::uniform4(0u8..),
        ) {
            let limiter = BugReportRateLimiter::new(BugReportRateLimiterConfig::default());
            let ip = IpAddr::V4(Ipv4Addr::new(
                ip_octets[0], ip_octets[1], ip_octets[2], ip_octets[3],
            ));
            let now = Instant::now();
            let max = limiter.config.max_requests; // 5

            for i in 0..num_submissions {
                if i < max {
                    // First max submissions must be allowed
                    prop_assert!(
                        limiter.check_ip(ip, now),
                        "Submission {} should be allowed (under limit {})",
                        i + 1,
                        max,
                    );
                    // Under the limit → no retry needed
                    prop_assert!(
                        limiter.seconds_until_retry_ip(ip, now).is_none(),
                        "seconds_until_retry_ip should be None when under limit (submission {})",
                        i + 1,
                    );
                } else {
                    // Beyond max → must be rejected
                    prop_assert!(
                        !limiter.check_ip(ip, now),
                        "Submission {} should be rejected (over limit {})",
                        i + 1,
                        max,
                    );
                    // At the limit → retry info must be present
                    let retry = limiter.seconds_until_retry_ip(ip, now);
                    prop_assert!(
                        retry.is_some(),
                        "seconds_until_retry_ip should be Some when at limit (submission {})",
                        i + 1,
                    );
                    prop_assert!(
                        retry.unwrap() <= 3600,
                        "retry seconds {} should be <= window (3600)",
                        retry.unwrap(),
                    );
                }
                limiter.record_ip(ip, now);
            }
        }

        /// For any number of submissions from the same user_id, the first 5
        /// are allowed and any beyond 5 are rejected. The per-user_id
        /// dimension is independent of the IP dimension.
        #[test]
        fn prop_rate_limit_enforcement_user(
            num_submissions in 1u32..20,
            user_id_bytes in prop::array::uniform16(0u8..),
        ) {
            let limiter = BugReportRateLimiter::new(BugReportRateLimiterConfig::default());
            let user_id = Uuid::from_bytes(user_id_bytes);
            let now = Instant::now();
            let max = limiter.config.max_requests; // 5

            for i in 0..num_submissions {
                if i < max {
                    prop_assert!(
                        limiter.check_user(user_id, now),
                        "User submission {} should be allowed (under limit {})",
                        i + 1,
                        max,
                    );
                    prop_assert!(
                        limiter.seconds_until_retry_user(user_id, now).is_none(),
                        "seconds_until_retry_user should be None when under limit (submission {})",
                        i + 1,
                    );
                } else {
                    prop_assert!(
                        !limiter.check_user(user_id, now),
                        "User submission {} should be rejected (over limit {})",
                        i + 1,
                        max,
                    );
                    let retry = limiter.seconds_until_retry_user(user_id, now);
                    prop_assert!(
                        retry.is_some(),
                        "seconds_until_retry_user should be Some when at limit (submission {})",
                        i + 1,
                    );
                    prop_assert!(
                        retry.unwrap() <= 3600,
                        "retry seconds {} should be <= window (3600)",
                        retry.unwrap(),
                    );
                }
                limiter.record_user(user_id, now);
            }
        }
    }
}
