//! Adaptive bitrate controller implementation and tests for the audio pipeline.

#![warn(missing_docs)]

use super::{BitrateConfig, BitrateControlling, BitrateDecision, NetworkStats};

/// Adaptive bitrate controller that adjusts encoder bitrate and FEC settings
/// based on observed network conditions.
///
/// Bitrate rules (from requirements):
/// - Loss > 5% -> reduce bitrate by 25%
/// - Loss < 1% for 5 consecutive seconds -> increase by 10% toward max
/// - RTT > 300ms -> drop to min bitrate
/// - Always clamped to [min_bitrate, max_bitrate]
///
/// FEC rules:
/// - Loss > 10% -> FEC depth 2
/// - Loss > 2% -> FEC depth 1
/// - Loss < 1% for 5 consecutive seconds -> disable FEC
pub struct AdaptiveBitrateController {
    config: BitrateConfig,
    current_bitrate: u32,
    /// Accumulated seconds of sustained low loss (< 1%).
    low_loss_duration_secs: f64,
    /// Interval between `on_stats` calls (seconds). Defaults to 1.0.
    stats_interval_secs: f64,
}

impl AdaptiveBitrateController {
    /// Create a new controller with the given configuration.
    pub fn new(config: BitrateConfig) -> Self {
        let initial = config
            .initial_bitrate
            .clamp(config.min_bitrate, config.max_bitrate);
        Self {
            config,
            current_bitrate: initial,
            low_loss_duration_secs: 0.0,
            stats_interval_secs: 1.0,
        }
    }

    /// Create a controller with default configuration.
    pub fn with_defaults() -> Self {
        Self::new(BitrateConfig::default())
    }

    /// Set the interval (in seconds) between successive `on_stats` calls.
    /// This is used to accumulate the low-loss duration timer.
    pub fn set_stats_interval(&mut self, secs: f64) {
        self.stats_interval_secs = secs;
    }

    /// Get the current bitrate in bps.
    pub fn current_bitrate(&self) -> u32 {
        self.current_bitrate
    }

    /// Get the accumulated low-loss duration in seconds.
    pub fn low_loss_duration(&self) -> f64 {
        self.low_loss_duration_secs
    }

    /// Compute the FEC decision based on packet loss and low-loss duration.
    fn compute_fec(&self, packet_loss: f64) -> (bool, u8) {
        if packet_loss > 0.10 {
            (true, 2)
        } else if packet_loss > 0.02 {
            (true, 1)
        } else if self.low_loss_duration_secs >= self.config.ramp_up_delay_secs {
            // Sustained low loss keeps FEC disabled to recover quality/bitrate.
            (false, 0)
        } else {
            (false, 0)
        }
    }
}

impl BitrateControlling for AdaptiveBitrateController {
    fn on_stats(&mut self, stats: &NetworkStats) -> BitrateDecision {
        if stats.packet_loss < 0.01 {
            self.low_loss_duration_secs += self.stats_interval_secs;
        } else {
            self.low_loss_duration_secs = 0.0;
        }

        let mut new_bitrate = self.current_bitrate;

        if stats.rtt_ms > 300.0 {
            new_bitrate = self.config.min_bitrate;
        } else if stats.packet_loss > 0.05 {
            new_bitrate = (self.current_bitrate as f64 * 0.75) as u32;
        } else if self.low_loss_duration_secs >= self.config.ramp_up_delay_secs {
            let increase = (self.current_bitrate as f64 * 0.10) as u32;
            new_bitrate = self.current_bitrate.saturating_add(increase);
        }

        new_bitrate = new_bitrate.clamp(self.config.min_bitrate, self.config.max_bitrate);
        self.current_bitrate = new_bitrate;

        let (fec_enabled, fec_depth) = self.compute_fec(stats.packet_loss);

        BitrateDecision {
            bitrate_bps: new_bitrate,
            fec_enabled,
            fec_depth,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::audio_pipeline::audio_pipeline_config::test_support::arb_network_stats;
    use proptest::prelude::*;

    proptest! {
        #![proptest_config(ProptestConfig { cases: 100, .. ProptestConfig::default() })]

        /// Feature: audio-quality-overhaul, Property 12: Bitrate controller bounds invariant
        #[test]
        fn bitrate_bounds_invariant(
            stats_sequence in proptest::collection::vec(arb_network_stats(), 1..50),
        ) {
            let config = BitrateConfig::default();
            let min = config.min_bitrate;
            let max = config.max_bitrate;
            let mut ctrl = AdaptiveBitrateController::new(config);

            for stats in &stats_sequence {
                let decision = ctrl.on_stats(stats);
                prop_assert!(
                    decision.bitrate_bps >= min && decision.bitrate_bps <= max,
                    "bitrate {} out of bounds [{}, {}] for stats {:?}",
                    decision.bitrate_bps, min, max, stats
                );
            }
        }
    }

    proptest! {
        #![proptest_config(ProptestConfig { cases: 100, .. ProptestConfig::default() })]

        /// Feature: audio-quality-overhaul, Property 13: High packet loss reduces bitrate
        #[test]
        fn high_loss_reduces_bitrate(
            initial_bitrate in 16_000u32..=64_000,
            packet_loss in 0.051f64..1.0,
            rtt_ms in 0.0f64..300.0,
        ) {
            let config = BitrateConfig {
                min_bitrate: 16_000,
                max_bitrate: 64_000,
                initial_bitrate,
                ramp_up_delay_secs: 5.0,
            };
            let mut ctrl = AdaptiveBitrateController::new(config);
            let before = ctrl.current_bitrate();

            let stats = NetworkStats {
                packet_loss,
                rtt_ms,
                jitter_ms: 0.0,
                jitter_stddev_ms: 0.0,
                ..Default::default()
            };
            let decision = ctrl.on_stats(&stats);

            let expected_max = ((before as f64 * 0.75) as u32).max(16_000);
            prop_assert!(
                decision.bitrate_bps <= expected_max,
                "bitrate {} should be <= {} (75% of {} clamped to min) for loss {}",
                decision.bitrate_bps, expected_max, before, packet_loss
            );
        }
    }

    proptest! {
        #![proptest_config(ProptestConfig { cases: 100, .. ProptestConfig::default() })]

        /// Feature: audio-quality-overhaul, Property 14: High RTT drops to minimum bitrate
        #[test]
        fn high_rtt_drops_to_min(
            initial_bitrate in 16_000u32..=64_000,
            rtt_ms in 300.1f64..2000.0,
            packet_loss in 0.0f64..1.0,
        ) {
            let config = BitrateConfig {
                min_bitrate: 16_000,
                max_bitrate: 64_000,
                initial_bitrate,
                ramp_up_delay_secs: 5.0,
            };
            let mut ctrl = AdaptiveBitrateController::new(config);

            let stats = NetworkStats {
                packet_loss,
                rtt_ms,
                jitter_ms: 0.0,
                jitter_stddev_ms: 0.0,
                ..Default::default()
            };
            let decision = ctrl.on_stats(&stats);

            prop_assert_eq!(
                decision.bitrate_bps, 16_000,
                "RTT {} should force min bitrate, got {}",
                rtt_ms, decision.bitrate_bps
            );
        }
    }

    proptest! {
        #![proptest_config(ProptestConfig { cases: 100, .. ProptestConfig::default() })]

        /// Feature: audio-quality-overhaul, Property 15: Sustained low loss ramps up and disables FEC
        #[test]
        fn sustained_low_loss_ramps_up_disables_fec(
            initial_bitrate in 16_000u32..=58_000,
            low_loss in 0.0f64..0.01,
        ) {
            let config = BitrateConfig {
                min_bitrate: 16_000,
                max_bitrate: 64_000,
                initial_bitrate,
                ramp_up_delay_secs: 5.0,
            };
            let mut ctrl = AdaptiveBitrateController::new(config);

            let stats = NetworkStats {
                packet_loss: low_loss,
                rtt_ms: 50.0,
                jitter_ms: 5.0,
                jitter_stddev_ms: 2.0,
                ..Default::default()
            };

            for _ in 0..5 {
                ctrl.on_stats(&stats);
            }

            let before = ctrl.current_bitrate();
            let decision = ctrl.on_stats(&stats);

            if before < 64_000 {
                prop_assert!(
                    decision.bitrate_bps > before,
                    "Expected ramp-up from {} but got {}",
                    before, decision.bitrate_bps
                );
                let max_increase = (before as f64 * 0.10) as u32;
                let actual_increase = decision.bitrate_bps - before;
                prop_assert!(
                    actual_increase <= max_increase + 1,
                    "Increase {} exceeds 10% ({}) of {}",
                    actual_increase, max_increase, before
                );
            }

            prop_assert!(!decision.fec_enabled, "FEC should be disabled on sustained low loss");
            prop_assert_eq!(decision.fec_depth, 0, "FEC depth should be 0");
        }
    }

    proptest! {
        #![proptest_config(ProptestConfig { cases: 100, .. ProptestConfig::default() })]

        /// Feature: audio-quality-overhaul, Property 16: FEC depth scales with packet loss
        #[test]
        fn fec_depth_scales_with_loss(
            packet_loss in 0.0f64..1.0,
            rtt_ms in 0.0f64..300.0,
        ) {
            let config = BitrateConfig::default();
            let mut ctrl = AdaptiveBitrateController::new(config);

            let stats = NetworkStats {
                packet_loss,
                rtt_ms,
                jitter_ms: 10.0,
                jitter_stddev_ms: 5.0,
                ..Default::default()
            };
            let decision = ctrl.on_stats(&stats);

            if packet_loss > 0.10 {
                prop_assert_eq!(
                    decision.fec_depth,
                    2,
                    "loss {} > 10% should give depth 2",
                    packet_loss
                );
                prop_assert!(
                    decision.fec_enabled,
                    "FEC should be enabled at loss {}",
                    packet_loss
                );
            } else if packet_loss > 0.02 {
                prop_assert_eq!(
                    decision.fec_depth,
                    1,
                    "loss {} > 2% should give depth 1",
                    packet_loss
                );
                prop_assert!(
                    decision.fec_enabled,
                    "FEC should be enabled at loss {}",
                    packet_loss
                );
            } else {
                prop_assert_eq!(
                    decision.fec_depth,
                    0,
                    "low loss {} should give depth 0",
                    packet_loss
                );
                prop_assert!(
                    !decision.fec_enabled,
                    "FEC should be disabled at low loss {}",
                    packet_loss
                );
            }
        }
    }
}
