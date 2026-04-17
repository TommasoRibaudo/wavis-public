//! Real network monitoring for the shared audio pipeline.
//!
//! This module owns the transport-stat ingestion types used by both the
//! WebRTC and LiveKit paths, plus the production `NetworkMonitoring`
//! implementation that derives packet loss, RTT, and jitter metrics.

use crate::audio_pipeline::{NetworkMonitoring, NetworkStats, RttSource};
use std::collections::VecDeque;
use std::sync::{Arc, Mutex};
use std::time::Instant;

/// Shared state that the RTP receive path writes to and the monitor reads from.
///
/// This is the bridge between the async receive handler and the periodic
/// `poll_stats` call. The receive path calls `record_packet` on the shared
/// handle; the monitor drains recorded arrivals on each poll.
#[derive(Debug, Default)]
pub struct NetworkMonitorInput {
    /// Packets recorded since the last poll.
    pending: Vec<(u16, Instant)>,
    /// Externally provided RTT measurement (e.g., from RTCP SR/RR).
    /// `None` means no RTT data available yet.
    rtt_ms: Option<f64>,
    /// Origin of the current RTT measurement.
    rtt_source: RttSource,
    /// Timestamp of the last accepted RTT update per source, used for
    /// 1-second rate limiting. Keyed by source: index 0 = Rtcp, index 1 = LiveKit.
    last_rtt_update: [Option<Instant>; 2],
}

/// Thread-safe handle for feeding data into the `RealNetworkMonitor`.
///
/// Clone this and hand it to the RTP receive path and any RTCP handler.
pub type NetworkMonitorHandle = Arc<Mutex<NetworkMonitorInput>>;

/// Create a new `(RealNetworkMonitor, NetworkMonitorHandle)` pair.
pub fn new_network_monitor() -> (RealNetworkMonitor, NetworkMonitorHandle) {
    let handle = Arc::new(Mutex::new(NetworkMonitorInput::default()));
    let monitor = RealNetworkMonitor::new(Arc::clone(&handle));
    (monitor, handle)
}

impl NetworkMonitorInput {
    /// Record a received packet. Called from the RTP receive path.
    pub fn record_packet(&mut self, seq: u16) {
        self.pending.push((seq, Instant::now()));
    }

    /// Set the latest RTT measurement with source tracking and rate limiting.
    pub fn set_rtt(&mut self, now: Instant, rtt_ms: f64, source: RttSource) {
        let idx = match source {
            RttSource::None => return,
            RttSource::Rtcp => 0,
            RttSource::LiveKit => 1,
        };

        if let Some(last) = self.last_rtt_update[idx] {
            if now.duration_since(last) < std::time::Duration::from_secs(1) {
                return;
            }
        }

        self.rtt_ms = Some(rtt_ms);
        self.rtt_source = source;
        self.last_rtt_update[idx] = Some(now);
    }
}

/// Production network monitor that computes packet loss, RTT, average jitter
/// (EMA), and jitter standard deviation from data fed by the RTP receive path.
pub struct RealNetworkMonitor {
    /// Shared input fed by the RTP receive path.
    input: NetworkMonitorHandle,
    /// Rolling window of the last 100 inter-arrival jitter values (ms).
    jitter_window: VecDeque<f64>,
    /// EMA of inter-arrival jitter (ms). Alpha = 1/16 per RFC 3550.
    ema_jitter_ms: f64,
    /// Highest sequence number seen so far.
    max_seq: Option<u16>,
    /// Total packets expected (based on sequence number range).
    total_expected: u64,
    /// Total packets actually received.
    total_received: u64,
    /// Timestamp of the previous packet arrival (for jitter computation).
    prev_arrival: Option<Instant>,
    /// Previous packet's sequence number (for jitter computation).
    prev_seq: Option<u16>,
    /// Whether we have received at least one packet.
    initialized: bool,
}

const JITTER_WINDOW_SIZE: usize = 100;
const JITTER_EMA_ALPHA: f64 = 1.0 / 16.0;

impl RealNetworkMonitor {
    /// Create a new monitor backed by the given shared input handle.
    pub fn new(input: NetworkMonitorHandle) -> Self {
        Self {
            input,
            jitter_window: VecDeque::with_capacity(JITTER_WINDOW_SIZE),
            ema_jitter_ms: 0.0,
            max_seq: None,
            total_expected: 0,
            total_received: 0,
            prev_arrival: None,
            prev_seq: None,
            initialized: false,
        }
    }

    /// Process a batch of newly arrived packets, updating jitter and loss counters.
    fn ingest(&mut self, arrivals: Vec<(u16, Instant)>) {
        let mut arrivals = arrivals;
        arrivals.sort_by_key(|&(seq, _)| seq);

        for (seq, arrived_at) in arrivals {
            self.total_received += 1;

            match self.max_seq {
                Some(prev_max) => {
                    let diff = seq.wrapping_sub(prev_max);
                    if diff > 0 && diff <= 0x7FFF {
                        self.total_expected += diff as u64;
                        self.max_seq = Some(seq);
                    }
                }
                None => {
                    self.max_seq = Some(seq);
                    self.total_expected = 1;
                    self.initialized = true;
                }
            }

            if let (Some(prev_at), Some(_prev_s)) = (self.prev_arrival, self.prev_seq) {
                let transit_diff_ms = arrived_at.duration_since(prev_at).as_secs_f64() * 1000.0;
                let jitter_sample = (transit_diff_ms - 20.0).abs();

                self.ema_jitter_ms += JITTER_EMA_ALPHA * (jitter_sample - self.ema_jitter_ms);

                if self.jitter_window.len() >= JITTER_WINDOW_SIZE {
                    self.jitter_window.pop_front();
                }
                self.jitter_window.push_back(jitter_sample);
            }

            self.prev_arrival = Some(arrived_at);
            self.prev_seq = Some(seq);
        }
    }

    fn jitter_stddev(&self) -> f64 {
        if self.jitter_window.len() < 2 {
            return 0.0;
        }

        let n = self.jitter_window.len() as f64;
        let mean: f64 = self.jitter_window.iter().sum::<f64>() / n;
        let variance: f64 = self
            .jitter_window
            .iter()
            .map(|&j| (j - mean).powi(2))
            .sum::<f64>()
            / n;
        variance.sqrt()
    }

    fn packet_loss_ratio(&self) -> f64 {
        if self.total_expected == 0 {
            return 0.0;
        }

        let lost = self.total_expected.saturating_sub(self.total_received);
        lost as f64 / self.total_expected as f64
    }
}

impl NetworkMonitoring for RealNetworkMonitor {
    fn poll_stats(&mut self) -> NetworkStats {
        let (arrivals, rtt, rtt_source) = {
            let mut input = self.input.lock().unwrap();
            let arrivals = std::mem::take(&mut input.pending);
            let rtt = input.rtt_ms;
            let rtt_source = input.rtt_source;
            (arrivals, rtt, rtt_source)
        };

        if arrivals.is_empty() && !self.initialized {
            return NetworkStats::default();
        }

        self.ingest(arrivals);

        NetworkStats {
            packet_loss: self.packet_loss_ratio(),
            rtt_ms: rtt.unwrap_or(0.0),
            jitter_ms: self.ema_jitter_ms,
            jitter_stddev_ms: self.jitter_stddev(),
            rtt_source,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::audio_pipeline::tests::FakeClock;
    use proptest::prelude::*;

    fn arb_rtt_source() -> impl Strategy<Value = RttSource> {
        prop_oneof![Just(RttSource::Rtcp), Just(RttSource::LiveKit),]
    }

    #[test]
    fn network_monitor_returns_zeros_when_stats_unavailable() {
        let (mut monitor, _handle) = new_network_monitor();

        let stats = monitor.poll_stats();
        assert_eq!(stats.packet_loss, 0.0);
        assert_eq!(stats.rtt_ms, 0.0);
        assert_eq!(stats.jitter_ms, 0.0);
        assert_eq!(stats.jitter_stddev_ms, 0.0);

        let stats2 = monitor.poll_stats();
        assert_eq!(stats2.packet_loss, 0.0);
        assert_eq!(stats2.rtt_ms, 0.0);
        assert_eq!(stats2.jitter_ms, 0.0);
        assert_eq!(stats2.jitter_stddev_ms, 0.0);
    }

    proptest! {
        #![proptest_config(ProptestConfig { cases: 100, .. ProptestConfig::default() })]

        #[test]
        fn rtt_round_trip_through_monitor(
            rtt_val in 0.0f64..2000.0,
            source in arb_rtt_source(),
        ) {
            let (mut monitor, handle) = new_network_monitor();

            {
                let mut input = handle.lock().unwrap();
                input.record_packet(0);
            }
            let default_stats = monitor.poll_stats();
            prop_assert!((default_stats.rtt_ms - 0.0).abs() < f64::EPSILON);
            prop_assert_eq!(default_stats.rtt_source, RttSource::None);

            {
                let mut input = handle.lock().unwrap();
                input.set_rtt(Instant::now(), rtt_val, source);
                input.record_packet(1);
            }
            let stats = monitor.poll_stats();
            prop_assert!((stats.rtt_ms - rtt_val).abs() < f64::EPSILON);
            prop_assert_eq!(stats.rtt_source, source);
        }
    }

    proptest! {
        #![proptest_config(ProptestConfig { cases: 100, .. ProptestConfig::default() })]

        #[test]
        fn rtt_rate_limiting(
            updates in proptest::collection::vec(
                (0.0f64..2000.0, arb_rtt_source(), 0u64..5000),
                2..20
            ),
        ) {
            let clock = FakeClock::new();
            let mut input = NetworkMonitorInput::default();
            let mut last_accepted: [Option<u64>; 2] = [None, None];

            let mut sorted_updates = updates.clone();
            sorted_updates.sort_by_key(|&(_, _, ts)| ts);

            for (rtt_val, source, ts_ms) in &sorted_updates {
                let idx = match source {
                    RttSource::Rtcp => 0,
                    RttSource::LiveKit => 1,
                    RttSource::None => continue,
                };

                let now = clock.now() + std::time::Duration::from_millis(*ts_ms);

                let rtt_before = input.rtt_ms;
                let source_before = input.rtt_source;
                input.set_rtt(now, *rtt_val, *source);

                let should_accept = match last_accepted[idx] {
                    None => true,
                    Some(last_ts) => ts_ms.saturating_sub(last_ts) >= 1000,
                };

                if should_accept {
                    prop_assert_eq!(input.rtt_ms, Some(*rtt_val));
                    prop_assert_eq!(input.rtt_source, *source);
                    last_accepted[idx] = Some(*ts_ms);
                } else {
                    prop_assert_eq!(input.rtt_ms, rtt_before);
                    prop_assert_eq!(input.rtt_source, source_before);
                }
            }
        }
    }

    #[test]
    fn rtt_source_switching() {
        let clock = FakeClock::new();
        let (mut monitor, handle) = new_network_monitor();

        {
            let mut input = handle.lock().unwrap();
            input.set_rtt(clock.now(), 50.0, RttSource::Rtcp);
            input.record_packet(0);
        }
        let stats = monitor.poll_stats();
        assert_eq!(stats.rtt_source, RttSource::Rtcp);
        assert!((stats.rtt_ms - 50.0).abs() < f64::EPSILON);

        {
            let mut input = handle.lock().unwrap();
            input.set_rtt(clock.now(), 75.0, RttSource::LiveKit);
            input.record_packet(1);
        }
        let stats = monitor.poll_stats();
        assert_eq!(stats.rtt_source, RttSource::LiveKit);
        assert!((stats.rtt_ms - 75.0).abs() < f64::EPSILON);

        {
            let mut input = handle.lock().unwrap();
            input.set_rtt(clock.now(), 100.0, RttSource::Rtcp);
            input.record_packet(2);
        }
        let stats = monitor.poll_stats();
        assert_eq!(stats.rtt_source, RttSource::LiveKit);
        assert!((stats.rtt_ms - 75.0).abs() < f64::EPSILON);

        clock.advance(1001);
        {
            let mut input = handle.lock().unwrap();
            input.set_rtt(clock.now(), 100.0, RttSource::Rtcp);
            input.record_packet(3);
        }
        let stats = monitor.poll_stats();
        assert_eq!(stats.rtt_source, RttSource::Rtcp);
        assert!((stats.rtt_ms - 100.0).abs() < f64::EPSILON);
    }

    #[test]
    fn rtt_rate_limiting_boundary_exactly_1s() {
        let clock = FakeClock::new();
        let mut input = NetworkMonitorInput::default();

        input.set_rtt(clock.now(), 10.0, RttSource::Rtcp);
        assert_eq!(input.rtt_ms, Some(10.0));
        assert_eq!(input.rtt_source, RttSource::Rtcp);

        clock.advance(999);
        input.set_rtt(clock.now(), 20.0, RttSource::Rtcp);
        assert_eq!(input.rtt_ms, Some(10.0));

        clock.advance(1);
        input.set_rtt(clock.now(), 30.0, RttSource::Rtcp);
        assert_eq!(input.rtt_ms, Some(30.0));
        assert_eq!(input.rtt_source, RttSource::Rtcp);
    }

    #[test]
    fn rtt_none_source_is_noop() {
        let mut input = NetworkMonitorInput::default();
        input.set_rtt(Instant::now(), 42.0, RttSource::None);
        assert_eq!(input.rtt_ms, None);
    }
}
