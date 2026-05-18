use std::collections::VecDeque;
use std::time::{Duration, Instant};

pub(crate) struct SlidingWindow {
    timestamps: VecDeque<Instant>,
    window: Duration,
}

impl SlidingWindow {
    pub fn new(window: Duration) -> Self {
        Self {
            timestamps: VecDeque::new(),
            window,
        }
    }

    pub fn add(&mut self, now: Instant) {
        self.timestamps.push_back(now);
    }

    pub fn count(&mut self, now: Instant) -> usize {
        self.prune(now);
        self.timestamps.len()
    }

    pub fn prune(&mut self, now: Instant) {
        while self
            .timestamps
            .front()
            .is_some_and(|t| now.saturating_duration_since(*t) > self.window)
        {
            self.timestamps.pop_front();
        }
    }

    /// Returns seconds until the oldest in-window entry expires, iff count >= max.
    pub fn seconds_until_retry(&self, max: usize, now: Instant) -> Option<u64> {
        let mut count = 0usize;
        let mut oldest_in_window = None;

        for timestamp in &self.timestamps {
            if now.saturating_duration_since(*timestamp) <= self.window {
                count += 1;
                if oldest_in_window.is_none() {
                    oldest_in_window = Some(*timestamp);
                }
            }
        }

        if count < max {
            return None;
        }

        oldest_in_window.map(|oldest| {
            let elapsed = now.saturating_duration_since(oldest);
            self.window.saturating_sub(elapsed).as_secs().max(1)
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn count_prunes_expired_entries() {
        let now = Instant::now();
        let mut window = SlidingWindow::new(Duration::from_secs(60));

        window.add(now - Duration::from_secs(61));
        window.add(now - Duration::from_secs(30));

        assert_eq!(window.count(now), 1);
    }

    #[test]
    fn seconds_until_retry_uses_oldest_in_window_entry() {
        let now = Instant::now();
        let mut window = SlidingWindow::new(Duration::from_secs(60));

        window.add(now - Duration::from_secs(20));
        window.add(now - Duration::from_secs(10));

        assert_eq!(window.seconds_until_retry(2, now), Some(40));
    }

    #[test]
    fn seconds_until_retry_ignores_expired_entries_without_pruning() {
        let now = Instant::now();
        let mut window = SlidingWindow::new(Duration::from_secs(60));

        window.add(now - Duration::from_secs(61));
        window.add(now - Duration::from_secs(10));

        assert_eq!(window.seconds_until_retry(2, now), None);
    }
}
