use std::collections::HashMap;
use std::net::IpAddr;
use std::sync::Mutex;
use std::time::{Duration, Instant};

use crate::abuse::SlidingWindow;

// ---------------------------------------------------------------------------
// Config
// ---------------------------------------------------------------------------

pub struct AuthRateLimiterConfig {
    pub register_max_per_ip: u32,
    pub register_window_secs: u64,
    pub refresh_max_per_ip: u32,
    pub refresh_window_secs: u64,
}

impl Default for AuthRateLimiterConfig {
    fn default() -> Self {
        Self {
            register_max_per_ip: 5,
            register_window_secs: 3600,
            refresh_max_per_ip: 30,
            refresh_window_secs: 60,
        }
    }
}

// ---------------------------------------------------------------------------
// AuthRateLimiter
// ---------------------------------------------------------------------------

pub struct AuthRateLimiter {
    config: AuthRateLimiterConfig,
    register_windows: Mutex<HashMap<IpAddr, SlidingWindow>>,
    refresh_windows: Mutex<HashMap<IpAddr, SlidingWindow>>,
}

impl AuthRateLimiter {
    pub fn new(config: AuthRateLimiterConfig) -> Self {
        Self {
            config,
            register_windows: Mutex::new(HashMap::new()),
            refresh_windows: Mutex::new(HashMap::new()),
        }
    }

    /// Returns true if the register request is allowed (under limit).
    pub fn check_register(&self, ip: IpAddr, now: Instant) -> bool {
        let mut map = self.register_windows.lock().unwrap();
        let dur = Duration::from_secs(self.config.register_window_secs);
        let window = map.entry(ip).or_insert_with(|| SlidingWindow::new(dur));
        window.count(now) < self.config.register_max_per_ip as usize
    }

    /// Returns true if the refresh request is allowed (under limit).
    pub fn check_refresh(&self, ip: IpAddr, now: Instant) -> bool {
        let mut map = self.refresh_windows.lock().unwrap();
        let dur = Duration::from_secs(self.config.refresh_window_secs);
        let window = map.entry(ip).or_insert_with(|| SlidingWindow::new(dur));
        window.count(now) < self.config.refresh_max_per_ip as usize
    }

    /// Returns seconds until register can be retried, or `None` if under the limit.
    pub fn seconds_until_register(&self, ip: IpAddr, now: Instant) -> Option<u64> {
        let mut map = self.register_windows.lock().unwrap();
        let dur = Duration::from_secs(self.config.register_window_secs);
        let window = map.entry(ip).or_insert_with(|| SlidingWindow::new(dur));
        window.seconds_until_retry(self.config.register_max_per_ip as usize, now)
    }

    /// Returns seconds until refresh can be retried, or `None` if under the limit.
    pub fn seconds_until_refresh(&self, ip: IpAddr, now: Instant) -> Option<u64> {
        let mut map = self.refresh_windows.lock().unwrap();
        let dur = Duration::from_secs(self.config.refresh_window_secs);
        let window = map.entry(ip).or_insert_with(|| SlidingWindow::new(dur));
        window.seconds_until_retry(self.config.refresh_max_per_ip as usize, now)
    }

    /// Record a register request.
    pub fn record_register(&self, ip: IpAddr, now: Instant) {
        let mut map = self.register_windows.lock().unwrap();
        let dur = Duration::from_secs(self.config.register_window_secs);
        let window = map.entry(ip).or_insert_with(|| SlidingWindow::new(dur));
        window.add(now);
    }

    /// Record a refresh request.
    pub fn record_refresh(&self, ip: IpAddr, now: Instant) {
        let mut map = self.refresh_windows.lock().unwrap();
        let dur = Duration::from_secs(self.config.refresh_window_secs);
        let window = map.entry(ip).or_insert_with(|| SlidingWindow::new(dur));
        window.add(now);
    }

    /// Clear all tracked state. Used between stress-test repetitions so
    /// rate-limit counters from a previous run don't carry over.
    pub fn clear(&self) {
        self.register_windows.lock().unwrap().clear();
        self.refresh_windows.lock().unwrap().clear();
    }

    /// Prune stale entries from both maps. Returns total entries removed.
    pub fn prune_stale(&self, now: Instant) -> usize {
        let mut count = 0;
        {
            let mut map = self.register_windows.lock().unwrap();
            let before = map.len();
            map.retain(|_, w| w.count(now) > 0);
            count += before - map.len();
        }
        {
            let mut map = self.refresh_windows.lock().unwrap();
            let before = map.len();
            map.retain(|_, w| w.count(now) > 0);
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
    fn seconds_until_register_returns_some_at_limit() {
        let limiter = AuthRateLimiter::new(AuthRateLimiterConfig::default());
        let ip = IpAddr::V4(Ipv4Addr::new(10, 0, 0, 1));
        let now = Instant::now();

        for _ in 0..5 {
            limiter.record_register(ip, now);
        }

        assert!(limiter.seconds_until_register(ip, now).is_some());
    }

    #[test]
    fn seconds_until_refresh_returns_none_under_limit() {
        let limiter = AuthRateLimiter::new(AuthRateLimiterConfig::default());
        let ip = IpAddr::V4(Ipv4Addr::new(10, 0, 0, 1));
        let now = Instant::now();

        limiter.record_refresh(ip, now);

        assert!(limiter.seconds_until_refresh(ip, now).is_none());
    }

    // Feature: device-auth, Property 10: Auth rate limiter ceiling
    // **Validates: Requirements 1.5, 2.6**
    proptest! {
        #![proptest_config(ProptestConfig::with_cases(100))]
        #[test]
        fn prop_rate_limiter_ceiling(
            threshold in 1u32..=20,
            window_secs in 1u64..=60,
        ) {
            let config = AuthRateLimiterConfig {
                register_max_per_ip: threshold,
                register_window_secs: window_secs,
                refresh_max_per_ip: threshold,
                refresh_window_secs: window_secs,
            };
            let limiter = AuthRateLimiter::new(config);
            let ip = IpAddr::V4(Ipv4Addr::new(10, 0, 0, 1));
            let now = Instant::now();

            // First N requests should be allowed
            for i in 0..threshold {
                prop_assert!(
                    limiter.check_register(ip, now),
                    "register request {} should be allowed", i
                );
                limiter.record_register(ip, now);
            }

            // (N+1)th should be rejected
            prop_assert!(
                !limiter.check_register(ip, now),
                "register request {} should be rejected", threshold
            );

            // After window expires, should be allowed again
            let after_window = now + Duration::from_secs(window_secs + 1);
            prop_assert!(
                limiter.check_register(ip, after_window),
                "register should be allowed after window expires"
            );

            // Same property for refresh
            let config2 = AuthRateLimiterConfig {
                register_max_per_ip: threshold,
                register_window_secs: window_secs,
                refresh_max_per_ip: threshold,
                refresh_window_secs: window_secs,
            };
            let limiter2 = AuthRateLimiter::new(config2);

            for i in 0..threshold {
                prop_assert!(
                    limiter2.check_refresh(ip, now),
                    "refresh request {} should be allowed", i
                );
                limiter2.record_refresh(ip, now);
            }

            prop_assert!(
                !limiter2.check_refresh(ip, now),
                "refresh request {} should be rejected", threshold
            );

            prop_assert!(
                limiter2.check_refresh(ip, after_window),
                "refresh should be allowed after window expires"
            );
        }
    }
}
