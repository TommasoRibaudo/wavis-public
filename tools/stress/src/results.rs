use std::time::Duration;

/// Describes a single invariant violation found during a scenario run.
pub struct InvariantViolation {
    pub invariant: String,
    pub expected: String,
    pub actual: String,
}

/// Summary result returned by each scenario after it completes.
pub struct ScenarioResult {
    pub name: String,
    pub passed: bool,
    pub duration: Duration,
    pub actions_per_second: f64,
    pub p95_latency: Duration,
    pub p99_latency: Duration,
    pub violations: Vec<InvariantViolation>,
}

/// Collects latency samples and computes percentile statistics.
///
/// Requirements: 11.3, 11.4, 11.6
pub struct LatencyTracker {
    samples: Vec<Duration>,
}

impl Default for LatencyTracker {
    fn default() -> Self {
        Self::new()
    }
}

impl LatencyTracker {
    pub fn new() -> Self {
        Self {
            samples: Vec::new(),
        }
    }

    /// Record a single latency sample.
    pub fn record(&mut self, d: Duration) {
        self.samples.push(d);
    }

    /// Return the 95th-percentile latency, or `Duration::ZERO` if no samples.
    pub fn p95(&self) -> Duration {
        self.percentile(95)
    }

    /// Return the 99th-percentile latency, or `Duration::ZERO` if no samples.
    pub fn p99(&self) -> Duration {
        self.percentile(99)
    }

    /// Return the arithmetic mean of all samples, or `Duration::ZERO` if no samples.
    pub fn mean(&self) -> Duration {
        if self.samples.is_empty() {
            return Duration::ZERO;
        }
        let total_nanos: u128 = self.samples.iter().map(|d| d.as_nanos()).sum();
        Duration::from_nanos((total_nanos / self.samples.len() as u128) as u64)
    }

    /// Return the number of recorded samples.
    pub fn count(&self) -> usize {
        self.samples.len()
    }

    fn percentile(&self, pct: usize) -> Duration {
        if self.samples.is_empty() {
            return Duration::ZERO;
        }
        let mut sorted = self.samples.clone();
        sorted.sort_unstable();
        // Index: floor((pct / 100) * (n - 1)) — nearest-rank style
        let idx = (pct * (sorted.len() - 1)) / 100;
        sorted[idx]
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    #[test]
    fn empty_tracker_returns_zero() {
        let t = LatencyTracker::new();
        assert_eq!(t.p95(), Duration::ZERO);
        assert_eq!(t.p99(), Duration::ZERO);
        assert_eq!(t.mean(), Duration::ZERO);
        assert_eq!(t.count(), 0);
    }

    #[test]
    fn single_sample_all_percentiles_equal_sample() {
        let mut t = LatencyTracker::new();
        let d = Duration::from_millis(42);
        t.record(d);
        assert_eq!(t.p95(), d);
        assert_eq!(t.p99(), d);
        assert_eq!(t.mean(), d);
        assert_eq!(t.count(), 1);
    }

    #[test]
    fn p95_and_p99_are_ordered() {
        let mut t = LatencyTracker::new();
        for ms in 1u64..=100 {
            t.record(Duration::from_millis(ms));
        }
        assert!(t.p95() <= t.p99(), "p95 should be <= p99");
        assert!(t.p99() <= Duration::from_millis(100));
    }

    #[test]
    fn mean_is_correct_for_uniform_samples() {
        let mut t = LatencyTracker::new();
        t.record(Duration::from_millis(10));
        t.record(Duration::from_millis(20));
        t.record(Duration::from_millis(30));
        assert_eq!(t.mean(), Duration::from_millis(20));
    }

    #[test]
    fn p95_picks_correct_index() {
        // 20 samples: 1ms..=20ms. p95 index = (95 * 19) / 100 = 18 → 19ms
        let mut t = LatencyTracker::new();
        for ms in 1u64..=20 {
            t.record(Duration::from_millis(ms));
        }
        assert_eq!(t.p95(), Duration::from_millis(19));
    }
}
