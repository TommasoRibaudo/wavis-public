use std::collections::HashMap;
use std::sync::Mutex;
use std::time::{Duration, Instant};
use uuid::Uuid;

// ---------------------------------------------------------------------------
// Config
// ---------------------------------------------------------------------------

pub struct ChannelRateLimiterConfig {
    /// Max channel-mutating requests per user per window.
    pub max_per_user: u32,
    /// Sliding window duration in seconds.
    pub window_secs: u64,
}

impl Default for ChannelRateLimiterConfig {
    fn default() -> Self {
        Self {
            max_per_user: 30,
            window_secs: 60,
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
// ChannelRateLimiter
// ---------------------------------------------------------------------------

pub struct ChannelRateLimiter {
    config: ChannelRateLimiterConfig,
    windows: Mutex<HashMap<Uuid, SlidingWindow>>,
}

impl ChannelRateLimiter {
    pub fn new(config: ChannelRateLimiterConfig) -> Self {
        Self {
            config,
            windows: Mutex::new(HashMap::new()),
        }
    }

    /// Returns true if the request is allowed (under limit).
    pub fn check(&self, user_id: Uuid, now: Instant) -> bool {
        let mut map = self.windows.lock().unwrap();
        let window = map.entry(user_id).or_insert_with(SlidingWindow::new);
        let dur = Duration::from_secs(self.config.window_secs);
        window.count(dur, now) < self.config.max_per_user
    }

    /// Record a request.
    pub fn record(&self, user_id: Uuid, now: Instant) {
        let mut map = self.windows.lock().unwrap();
        let window = map.entry(user_id).or_insert_with(SlidingWindow::new);
        window.record(now);
    }

    /// Clear all tracked state. Used by test harnesses so rate-limit
    /// counters from a previous scenario don't carry over.
    pub fn clear(&self) {
        self.windows.lock().unwrap().clear();
    }

    /// Prune stale entries. Returns number of entries removed.
    pub fn prune_stale(&self, now: Instant) -> usize {
        let dur = Duration::from_secs(self.config.window_secs);
        let mut map = self.windows.lock().unwrap();
        let before = map.len();
        map.retain(|_, w| !w.is_stale(dur, now));
        before - map.len()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use proptest::prelude::*;

    fn arb_uuid() -> impl Strategy<Value = Uuid> {
        any::<[u8; 16]>().prop_map(Uuid::from_bytes)
    }

    proptest! {
        #![proptest_config(ProptestConfig::with_cases(100))]
        #[test]
        fn prop_rate_limiter_ceiling(
            threshold in 1u32..=30,
            window_secs in 1u64..=60,
            user_id in arb_uuid(),
        ) {
            let config = ChannelRateLimiterConfig {
                max_per_user: threshold,
                window_secs,
            };
            let limiter = ChannelRateLimiter::new(config);
            let now = Instant::now();

            for i in 0..threshold {
                prop_assert!(
                    limiter.check(user_id, now),
                    "request {} should be allowed", i
                );
                limiter.record(user_id, now);
            }

            prop_assert!(
                !limiter.check(user_id, now),
                "request {} should be rejected", threshold
            );

            let after_window = now + Duration::from_secs(window_secs + 1);
            prop_assert!(
                limiter.check(user_id, after_window),
                "should be allowed after window expires"
            );
        }
    }
}
