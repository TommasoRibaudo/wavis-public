//! Owns APM (AEC/NS/AGC) and nnnoiseless denoise coordination for the
//! WebRTC send path.
//!
//! This module does not own Opus encoding, transport, or PeerConnection
//! lifecycle — those remain in `webrtc_backend` and `webrtc_loops`.
//! The [`ApmPipeline`] struct encapsulates the `RealAudioProcessor`,
//! `DenoiseFilter`, and the denoise ↔ APM NS transition state machine
//! so the send loop stays focused on frame timing and encoding.

use crate::audio_pipeline::{ApmMode, AudioProcess, APM_FRAME_SAMPLES, FRAME_SAMPLES};
use crate::audio_pipeline_real::RealAudioProcessor;
use crate::cpal_audio::AudioBuffer;
use crate::denoise_filter::DenoiseFilter;
use log::warn;
use std::sync::Arc;

/// Encapsulates APM processing and nnnoiseless denoise coordination.
///
/// Owns the `RealAudioProcessor`, a shared `DenoiseFilter` handle, and the
/// transition state that keeps APM NS and denoise mutually exclusive.
/// The send loop calls [`apply_denoise`] then [`process_apm`] each frame,
/// with metering points in between.
pub(crate) struct ApmPipeline {
    audio_processor: RealAudioProcessor,
    denoise: Arc<DenoiseFilter>,
    /// Tracks the previous custom suppressor state for transition detection.
    /// Sole owner of transition detection — detects changes and performs
    /// GRU state reset + APM NS toggle in one sequential block.
    prev_custom_suppressor_active: bool,
}

pub(crate) fn should_enable_apm_ns(custom_suppressor_active: bool) -> bool {
    !custom_suppressor_active
}

impl ApmPipeline {
    /// Create a new pipeline. Falls back to APM bypass if the APM library
    /// fails to initialize (graceful degradation per Req 2.5).
    ///
    /// If denoise starts enabled, APM NS is disabled up front so we don't
    /// double-suppress on the very first frame.
    pub(crate) fn new(denoise: Arc<DenoiseFilter>) -> Self {
        let mut audio_processor = RealAudioProcessor::new();
        let prev_custom_suppressor_active = denoise.is_custom_suppressor_active();

        if !should_enable_apm_ns(prev_custom_suppressor_active) {
            audio_processor.set_ns_enabled(false);
        }

        Self {
            audio_processor,
            denoise,
            prev_custom_suppressor_active,
        }
    }

    /// Returns the APM mode captured at processor creation time.
    pub(crate) fn apm_mode(&self) -> ApmMode {
        self.audio_processor.apm_mode()
    }

    /// Handle denoise ↔ APM NS transition coordination and apply denoise.
    ///
    /// Must be called once per frame **before** [`process_apm`]. Detects
    /// denoise toggle changes and keeps APM NS and denoise mutually
    /// exclusive, preferring a "neither" transient over a "both" transient
    /// to avoid double-suppression artifacts.
    pub(crate) fn apply_denoise(&mut self, pcm: &mut [f32]) {
        let current_custom_suppressor = self.denoise.is_custom_suppressor_active();
        if current_custom_suppressor != self.prev_custom_suppressor_active {
            if current_custom_suppressor {
                // Enabling denoise: disable APM NS first (prefer "neither" transient),
                // then reset GRU state for clean start.
                self.audio_processor.set_ns_enabled(false);
                self.denoise.reset_state();
            } else {
                // Disabling denoise: re-enable APM NS first (prefer "neither" transient).
                self.audio_processor.set_ns_enabled(true);
            }
            self.prev_custom_suppressor_active = current_custom_suppressor;
        }

        // Apply denoise to the full 960-sample frame before APM chunking.
        self.denoise.process(&mut pcm[..FRAME_SAMPLES]);
    }

    /// Process the 20ms frame through APM in 2×10ms chunks.
    ///
    /// Peeks AEC reference from the playback buffer (without consuming
    /// samples) and runs each 10ms chunk through `process_capture_frame`.
    /// Errors are logged and the frame continues with unprocessed audio
    /// (graceful degradation).
    pub(crate) fn process_apm(
        &mut self,
        pcm: &mut [f32],
        playback_buf: &AudioBuffer,
        ref_buf: &mut [f32],
    ) {
        for chunk_idx in 0..2 {
            let start = chunk_idx * APM_FRAME_SAMPLES;
            let end = start + APM_FRAME_SAMPLES;
            let capture_chunk = &mut pcm[start..end];

            // Peek AEC reference from playback buffer WITHOUT consuming
            // samples. Using peek_recent avoids draining samples the
            // speaker also needs — preventing playback underruns.
            playback_buf.peek_recent(ref_buf);

            if let Err(e) = self
                .audio_processor
                .process_capture_frame(capture_chunk, ref_buf)
            {
                warn!("APM processing error: {}", e);
                // Continue with unprocessed audio (graceful degradation).
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use proptest::prelude::*;

    #[test]
    fn rnnoise_active_disables_apm_ns() {
        assert!(!should_enable_apm_ns(true));
    }

    #[test]
    fn fake_custom_suppressor_active_disables_apm_ns() {
        assert!(!should_enable_apm_ns(true));
    }

    #[test]
    fn none_backend_allows_apm_ns() {
        assert!(should_enable_apm_ns(false));
    }

    proptest! {
        #[test]
        fn no_transition_allows_apm_ns_and_custom_suppressor_overlap(
            transitions in proptest::collection::vec(proptest::bool::ANY, 1..=50),
        ) {
            for custom_active in transitions {
                let apm_ns_enabled = if custom_active {
                    false
                } else {
                    should_enable_apm_ns(custom_active)
                };

                prop_assert!(
                    !(apm_ns_enabled && custom_active),
                    "APM NS and a custom suppressor overlapped"
                );
            }
        }
    }
}
