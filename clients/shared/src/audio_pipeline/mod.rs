//! Audio pipeline facade traits and re-exports.
//!
//! This module owns the public trait surface for the shared audio pipeline and
//! re-exports the concrete config, jitter, and bitrate modules. Concrete
//! implementations live in sibling files so this facade stays focused on the
//! API boundary used by real and mock backends.

#![warn(missing_docs)]

use std::time::Instant;

pub mod audio_pipeline_config;
pub mod bitrate_controller;
pub mod jitter_buffer;

pub use audio_pipeline_config::{
    ApmMode, AudioPipelineConfig, AudioPipelineError, BitrateConfig, BitrateDecision,
    JitterBufferStats, JitterPacket, JitterResult, NetworkStats, PipelineTelemetry, RttSource,
    APM_FRAME_SAMPLES, CHANNELS, FRAME_DURATION, FRAME_SAMPLES, MAX_BUFFERED_PACKETS,
    MAX_DELAY_SHRINK_PER_SEC_MS, MAX_JITTER_DELAY_MS, MAX_OPUS_PACKET, MAX_PACKET_SIZE,
    MIN_JITTER_DELAY_MS, MIN_PREFETCH_MS, SAMPLE_RATE,
};
pub use bitrate_controller::AdaptiveBitrateController;
pub use jitter_buffer::AdaptiveJitterBuffer;

/// Trait for Opus encoding. Real impl wraps `audiopus::coder::Encoder`.
pub trait OpusEncode: Send + Sync {
    /// Encode a 20 ms frame of 48 kHz mono f32 samples into an Opus packet.
    /// Returns the number of bytes written to `output`.
    fn encode_frame(&mut self, pcm: &[f32], output: &mut [u8])
        -> Result<usize, AudioPipelineError>;

    /// Set the encoder bitrate in bits per second (16 000 ..= 64 000).
    fn set_bitrate(&mut self, bps: u32) -> Result<(), AudioPipelineError>;

    /// Enable or disable in-band FEC.
    fn set_fec(&mut self, enabled: bool) -> Result<(), AudioPipelineError>;

    /// Enable or disable DTX (discontinuous transmission).
    fn set_dtx(&mut self, enabled: bool) -> Result<(), AudioPipelineError>;

    /// Hint the expected packet loss percentage to the encoder (0-100).
    fn set_packet_loss_percentage(&mut self, pct: u8) -> Result<(), AudioPipelineError>;

    /// Get the current bitrate in bps.
    fn bitrate(&self) -> u32;
}

/// Trait for Opus decoding. Real impl wraps `audiopus::coder::Decoder`.
pub trait OpusDecode: Send + Sync {
    /// Decode an Opus packet into 48 kHz mono f32 samples.
    /// Returns the number of samples written.
    fn decode_frame(
        &mut self,
        opus_data: &[u8],
        output: &mut [f32],
    ) -> Result<usize, AudioPipelineError>;

    /// Generate a PLC (packet loss concealment) frame.
    /// Called when a packet is missing.
    fn decode_plc(&mut self, output: &mut [f32]) -> Result<usize, AudioPipelineError>;
}

/// Trait for the audio processing chain (AEC + NS + AGC).
pub trait AudioProcess: Send + Sync {
    /// Process a 10 ms frame of 48 kHz mono f32 samples in-place.
    ///
    /// `capture_frame` is the mic input (modified in-place).
    /// `reference_frame` is the speaker output used for echo cancellation.
    fn process_capture_frame(
        &mut self,
        capture_frame: &mut [f32],
        reference_frame: &[f32],
    ) -> Result<(), AudioPipelineError>;

    /// Returns the current APM operating mode.
    ///
    /// Default returns `Bypass` so mock processors work without change.
    fn apm_mode(&self) -> ApmMode {
        ApmMode::Bypass
    }
}

/// Trait for the adaptive jitter buffer.
pub trait JitterBuffering: Send + Sync {
    /// Insert a received packet into the buffer.
    fn push(&mut self, seq: u16, data: Vec<u8>);

    /// Request the next packet for playback.
    /// `now` is the current monotonic Instant, used for playout gating.
    /// Returns `Packet`, `Missing` (invoke PLC), or `NotReady` (wait).
    fn pop(&mut self, now: Instant) -> JitterResult;

    /// Update jitter statistics from the network monitor.
    /// Recomputes target delay as:
    /// `max(20 ms, min(200 ms, avg_jitter + 2 * jitter_stddev))`.
    fn update_stats(&mut self, avg_jitter_ms: f64, jitter_stddev_ms: f64);

    /// Get the current target delay in milliseconds.
    fn target_delay_ms(&self) -> f64;

    /// Return a snapshot of jitter buffer health metrics.
    fn stats(&self) -> JitterBufferStats;
}

/// Trait for monitoring network conditions.
pub trait NetworkMonitoring: Send + Sync {
    /// Poll the latest network statistics.
    /// Called at intervals <= 1 second.
    /// Returns default (zero) stats if transport stats are unavailable.
    fn poll_stats(&mut self) -> NetworkStats;
}

/// Trait for adaptive bitrate control.
pub trait BitrateControlling: Send + Sync {
    /// Called each time new network stats arrive.
    /// Returns the new target bitrate and FEC settings.
    fn on_stats(&mut self, stats: &NetworkStats) -> BitrateDecision;
}

#[cfg(test)]
pub(crate) mod tests {
    #[allow(unused_imports)]
    pub use super::jitter_buffer::test_support::FakeClock;
}
