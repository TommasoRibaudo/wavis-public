use std::time::Instant;

/// Per-connection token bucket rate limiter for chat messages.
///
/// Each `ChatSend` consumes one token. Tokens refill at `refill_rate` per
/// second up to `max_tokens`. When the bucket is empty, `allow()` returns
/// `false` and the handler should send an error without relaying the message.
///
/// This is independent of the global `WsRateLimiter` — the global limiter
/// protects the connection; this limiter prevents chat spam.
pub struct ChatRateLimiter {
    tokens: f64,
    max_tokens: f64,
    refill_rate: f64, // tokens per second
    last_check: Instant,
}

impl ChatRateLimiter {
    /// Create a new limiter. `max_per_sec` sets both the bucket capacity and
    /// the refill rate (e.g. 5.0 → 5 tokens max, refilling at 5/sec).
    pub fn new(max_per_sec: f64) -> Self {
        Self {
            tokens: max_per_sec,
            max_tokens: max_per_sec,
            refill_rate: max_per_sec,
            last_check: Instant::now(),
        }
    }

    /// Try to consume one token. Returns `true` if allowed, `false` if the
    /// bucket is empty (rate limit exceeded).
    pub fn allow(&mut self) -> bool {
        let now = Instant::now();
        let elapsed = now.duration_since(self.last_check).as_secs_f64();
        self.last_check = now;

        // Refill tokens based on elapsed time, capped at max.
        self.tokens = (self.tokens + elapsed * self.refill_rate).min(self.max_tokens);

        if self.tokens >= 1.0 {
            self.tokens -= 1.0;
            true
        } else {
            false
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use proptest::prelude::*;
    use std::time::Duration;

    #[test]
    fn fresh_limiter_allows_up_to_max() {
        let mut limiter = ChatRateLimiter::new(5.0);
        for _ in 0..5 {
            assert!(limiter.allow());
        }
        assert!(!limiter.allow());
    }

    #[test]
    fn refill_after_time_elapses() {
        let mut limiter = ChatRateLimiter::new(5.0);
        // Drain all tokens.
        for _ in 0..5 {
            limiter.allow();
        }
        assert!(!limiter.allow());

        // Simulate 1 second passing → should refill 5 tokens.
        limiter.last_check -= Duration::from_secs(1);
        for _ in 0..5 {
            assert!(limiter.allow());
        }
        assert!(!limiter.allow());
    }

    #[test]
    fn partial_refill() {
        let mut limiter = ChatRateLimiter::new(5.0);
        // Drain all tokens.
        for _ in 0..5 {
            limiter.allow();
        }

        // Simulate 0.5 seconds → refills 2.5 tokens → 2 full tokens available.
        limiter.last_check -= Duration::from_millis(500);
        assert!(limiter.allow());
        assert!(limiter.allow());
        assert!(!limiter.allow());
    }

    #[test]
    fn tokens_do_not_exceed_max() {
        let mut limiter = ChatRateLimiter::new(5.0);
        // Simulate a long idle period.
        limiter.last_check -= Duration::from_secs(100);
        // Should still only allow max_tokens (5).
        for _ in 0..5 {
            assert!(limiter.allow());
        }
        assert!(!limiter.allow());
    }

    // Feature: ephemeral-room-chat, Property 13: Chat rate limiter enforces 5 messages per second
    // **Validates: Requirements 8.1, 8.2**
    proptest! {
        #![proptest_config(ProptestConfig::with_cases(100))]
        #[test]
        fn prop_burst_first_5_allowed_rest_denied(burst_size in 1u32..=20) {
            let mut limiter = ChatRateLimiter::new(5.0);
            for i in 0..burst_size {
                let result = limiter.allow();
                if i < 5 {
                    prop_assert!(result, "call {} should be allowed", i);
                } else {
                    prop_assert!(!result, "call {} should be denied", i);
                }
            }
        }
    }
}
