//! Mock implementations of audio pipeline traits for testing.
//!
//! These mocks are available without the `real-backends` feature flag,
//! enabling deterministic unit and property-based tests.

#![warn(missing_docs)]

use crate::audio_pipeline::{
    AudioPipelineError, AudioProcess, NetworkMonitoring, NetworkStats, OpusDecode, OpusEncode,
    FRAME_SAMPLES,
};

// ---------------------------------------------------------------------------
// MockOpusEncoder
// ---------------------------------------------------------------------------

/// A deterministic mock Opus encoder that produces fixed-size output.
///
/// The encoded output is a simple header (4 bytes: length as u32 LE)
/// followed by the first N sample bytes, truncated to `output_size`.
pub struct MockOpusEncoder {
    bitrate: u32,
    fec_enabled: bool,
    dtx_enabled: bool,
    packet_loss_pct: u8,
    /// Fixed output size in bytes for each encode call.
    output_size: usize,
}

impl MockOpusEncoder {
    /// Create a mock encoder with a fixed output size per frame.
    pub fn new(output_size: usize) -> Self {
        Self {
            bitrate: 32_000,
            fec_enabled: false,
            dtx_enabled: false,
            packet_loss_pct: 0,
            output_size,
        }
    }
}

impl Default for MockOpusEncoder {
    fn default() -> Self {
        Self::new(80) // ~32kbps at 20ms frames
    }
}

impl OpusEncode for MockOpusEncoder {
    fn encode_frame(
        &mut self,
        pcm: &[f32],
        output: &mut [u8],
    ) -> Result<usize, AudioPipelineError> {
        if pcm.len() != FRAME_SAMPLES {
            return Err(AudioPipelineError::EncodeError(format!(
                "expected {} samples, got {}",
                FRAME_SAMPLES,
                pcm.len()
            )));
        }
        if output.len() < self.output_size {
            return Err(AudioPipelineError::EncodeError(
                "output buffer too small".into(),
            ));
        }
        // Write deterministic output: length header + zero-padded payload.
        let size = self.output_size;
        let len_bytes = (pcm.len() as u32).to_le_bytes();
        for (i, byte) in output[..size].iter_mut().enumerate() {
            *byte = if i < 4 { len_bytes[i] } else { 0xAB };
        }
        Ok(size)
    }

    fn set_bitrate(&mut self, bps: u32) -> Result<(), AudioPipelineError> {
        self.bitrate = bps.clamp(16_000, 64_000);
        Ok(())
    }

    fn set_fec(&mut self, enabled: bool) -> Result<(), AudioPipelineError> {
        self.fec_enabled = enabled;
        Ok(())
    }

    fn set_dtx(&mut self, enabled: bool) -> Result<(), AudioPipelineError> {
        self.dtx_enabled = enabled;
        Ok(())
    }

    fn set_packet_loss_percentage(&mut self, pct: u8) -> Result<(), AudioPipelineError> {
        self.packet_loss_pct = pct;
        Ok(())
    }

    fn bitrate(&self) -> u32 {
        self.bitrate
    }
}

// ---------------------------------------------------------------------------
// MockOpusDecoder
// ---------------------------------------------------------------------------

/// A deterministic mock Opus decoder that produces fixed-size PCM output.
///
/// Decoding always writes `FRAME_SAMPLES` zero-valued samples.
/// PLC produces the same output (silence).
pub struct MockOpusDecoder;

impl MockOpusDecoder {
    /// Create a default mock decoder that produces silence on every decode and PLC call.
    pub fn new() -> Self {
        Self
    }
}

impl Default for MockOpusDecoder {
    fn default() -> Self {
        Self::new()
    }
}

impl OpusDecode for MockOpusDecoder {
    fn decode_frame(
        &mut self,
        _opus_data: &[u8],
        output: &mut [f32],
    ) -> Result<usize, AudioPipelineError> {
        if output.len() < FRAME_SAMPLES {
            return Err(AudioPipelineError::DecodeError(
                "output buffer too small".into(),
            ));
        }
        // Produce deterministic silence.
        for sample in output[..FRAME_SAMPLES].iter_mut() {
            *sample = 0.0;
        }
        Ok(FRAME_SAMPLES)
    }

    fn decode_plc(&mut self, output: &mut [f32]) -> Result<usize, AudioPipelineError> {
        if output.len() < FRAME_SAMPLES {
            return Err(AudioPipelineError::DecodeError(
                "output buffer too small for PLC".into(),
            ));
        }
        for sample in output[..FRAME_SAMPLES].iter_mut() {
            *sample = 0.0;
        }
        Ok(FRAME_SAMPLES)
    }
}

// ---------------------------------------------------------------------------
// MockAudioProcessor
// ---------------------------------------------------------------------------

/// A mock audio processor that can operate in bypass or processing mode.
///
/// In bypass mode (`bypass: true`), the capture frame is left unchanged.
/// In processing mode, a simple gain of 0.5 is applied (for testability).
pub struct MockAudioProcessor {
    /// When true, input passes through unchanged (bypass/disabled mode).
    pub bypass: bool,
}

impl MockAudioProcessor {
    /// Create a mock processor. If `bypass` is true, input is unchanged.
    pub fn new(bypass: bool) -> Self {
        Self { bypass }
    }
}

impl Default for MockAudioProcessor {
    fn default() -> Self {
        Self::new(false)
    }
}

impl AudioProcess for MockAudioProcessor {
    fn process_capture_frame(
        &mut self,
        capture_frame: &mut [f32],
        _reference_frame: &[f32],
    ) -> Result<(), AudioPipelineError> {
        if !self.bypass {
            // Simple deterministic processing: halve the amplitude.
            for sample in capture_frame.iter_mut() {
                *sample *= 0.5;
            }
        }
        // Bypass: leave capture_frame untouched.
        Ok(())
    }
}

// ---------------------------------------------------------------------------
// MockNetworkMonitor
// ---------------------------------------------------------------------------

/// A mock network monitor that returns configurable `NetworkStats`.
///
/// Each call to `poll_stats` returns the currently configured stats,
/// which can be updated via `set_stats`.
pub struct MockNetworkMonitor {
    stats: NetworkStats,
}

impl MockNetworkMonitor {
    /// Create a monitor that returns the given stats on every poll.
    pub fn new(stats: NetworkStats) -> Self {
        Self { stats }
    }

    /// Update the stats that will be returned by subsequent `poll_stats` calls.
    pub fn set_stats(&mut self, stats: NetworkStats) {
        self.stats = stats;
    }
}

impl Default for MockNetworkMonitor {
    fn default() -> Self {
        Self::new(NetworkStats::default())
    }
}

impl NetworkMonitoring for MockNetworkMonitor {
    fn poll_stats(&mut self) -> NetworkStats {
        self.stats.clone()
    }
}

// ===========================================================================
// Tests
// ===========================================================================

#[cfg(test)]
mod tests {
    use super::*;
    use crate::audio_pipeline::{RttSource, MAX_OPUS_PACKET};
    use proptest::prelude::*;

    // -----------------------------------------------------------------------
    // Property 3: Audio processor bypass preserves input
    // **Validates: Requirements 2.5**
    //
    // For any 480-sample f32 audio frame, when the AudioProcessor is in
    // bypass mode, the output frame is identical to the input (bit-exact).
    // -----------------------------------------------------------------------

    proptest! {
        #![proptest_config(ProptestConfig { cases: 100, .. ProptestConfig::default() })]

        /// Feature: audio-quality-overhaul, Property 3: Audio processor bypass preserves input
        #[test]
        fn audio_processor_bypass_preserves_input(
            frame in proptest::collection::vec(-1.0f32..1.0f32, 480),
            reference in proptest::collection::vec(-1.0f32..1.0f32, 480),
        ) {
            let mut proc = MockAudioProcessor::new(true); // bypass mode
            let original = frame.clone();
            let mut capture = frame;
            proc.process_capture_frame(&mut capture, &reference).unwrap();
            prop_assert_eq!(capture, original, "Bypass mode must not modify the frame");
        }
    }

    #[test]
    fn mock_encoder_produces_deterministic_output() {
        let mut enc = MockOpusEncoder::default();
        let pcm = vec![0.0f32; FRAME_SAMPLES];
        let mut out = vec![0u8; MAX_OPUS_PACKET];
        let n = enc.encode_frame(&pcm, &mut out).unwrap();
        assert_eq!(n, 80);
        // First 4 bytes are the sample count as u32 LE.
        let len = u32::from_le_bytes([out[0], out[1], out[2], out[3]]);
        assert_eq!(len, FRAME_SAMPLES as u32);
    }

    #[test]
    fn mock_decoder_produces_silence() {
        let mut dec = MockOpusDecoder::new();
        let mut out = vec![1.0f32; FRAME_SAMPLES];
        let n = dec.decode_frame(&[0xAB; 80], &mut out).unwrap();
        assert_eq!(n, FRAME_SAMPLES);
        assert!(out.iter().all(|&s| s == 0.0));
    }

    #[test]
    fn mock_processor_bypass_preserves_input() {
        let mut proc = MockAudioProcessor::new(true);
        let original = vec![0.5f32; 480];
        let mut frame = original.clone();
        let reference = vec![0.0f32; 480];
        proc.process_capture_frame(&mut frame, &reference).unwrap();
        assert_eq!(frame, original);
    }

    #[test]
    fn mock_processor_active_modifies_input() {
        let mut proc = MockAudioProcessor::new(false);
        let mut frame = vec![1.0f32; 480];
        let reference = vec![0.0f32; 480];
        proc.process_capture_frame(&mut frame, &reference).unwrap();
        assert!(frame.iter().all(|&s| (s - 0.5).abs() < f32::EPSILON));
    }

    #[test]
    fn mock_network_monitor_returns_configured_stats() {
        let stats = NetworkStats {
            packet_loss: 0.05,
            rtt_ms: 150.0,
            jitter_ms: 20.0,
            jitter_stddev_ms: 5.0,
            rtt_source: RttSource::None,
        };
        let mut mon = MockNetworkMonitor::new(stats.clone());
        let polled = mon.poll_stats();
        assert_eq!(polled.packet_loss, stats.packet_loss);
        assert_eq!(polled.rtt_ms, stats.rtt_ms);
    }

    #[test]
    fn mock_network_monitor_default_returns_zeros() {
        let mut mon = MockNetworkMonitor::default();
        let polled = mon.poll_stats();
        assert_eq!(polled.packet_loss, 0.0);
        assert_eq!(polled.rtt_ms, 0.0);
        assert_eq!(polled.jitter_ms, 0.0);
        assert_eq!(polled.jitter_stddev_ms, 0.0);
    }
}
