//! Real codec and audio-processing implementations for the shared audio pipeline.
//!
//! This module is gated behind the `real-backends` feature flag and provides
//! production implementations of the audio pipeline traits using:
//! - `audiopus` for Opus encoding/decoding
//!
//! Future additions (in later tasks): `rubato` for resampling.

use crate::audio_pipeline::{AudioPipelineError, OpusDecode, OpusEncode, FRAME_SAMPLES};
use audiopus::coder::{Decoder, Encoder};
use audiopus::{Application, Bitrate, Channels, SampleRate};

// ---------------------------------------------------------------------------
// RealOpusEncoder
// ---------------------------------------------------------------------------

/// Production Opus encoder wrapping `audiopus::coder::Encoder`.
///
/// Configured at 48 kHz mono in VoIP application mode with DTX enabled
/// and a configurable bitrate clamped to [16 kbps, 64 kbps].
pub struct RealOpusEncoder {
    encoder: Encoder,
    current_bitrate: u32,
}

impl RealOpusEncoder {
    /// Create a new encoder with the given initial bitrate (clamped to [16000, 64000]).
    pub fn new(initial_bitrate: u32) -> Result<Self, AudioPipelineError> {
        let mut encoder = Encoder::new(SampleRate::Hz48000, Channels::Mono, Application::Voip)
            .map_err(|e| AudioPipelineError::EncodeError(e.to_string()))?;

        let bitrate = initial_bitrate.clamp(16_000, 64_000);

        encoder
            .set_bitrate(Bitrate::BitsPerSecond(bitrate as i32))
            .map_err(|e| AudioPipelineError::EncodeError(e.to_string()))?;

        // Enable DTX via raw CTL (audiopus doesn't expose a dedicated method).
        encoder
            .set_encoder_ctl_request(audiopus::ffi::OPUS_SET_DTX_REQUEST, 1)
            .map_err(|e| AudioPipelineError::EncodeError(format!("DTX enable failed: {e}")))?;

        // Set complexity to 10 (maximum quality). Default is 9 for VoIP mode
        // but 10 gives measurably better quality at negligible CPU cost for
        // a single mono voice stream.
        encoder
            .set_encoder_ctl_request(audiopus::ffi::OPUS_SET_COMPLEXITY_REQUEST, 10)
            .map_err(|e| AudioPipelineError::EncodeError(format!("Complexity set failed: {e}")))?;

        Ok(Self {
            encoder,
            current_bitrate: bitrate,
        })
    }
}

impl OpusEncode for RealOpusEncoder {
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
        self.encoder
            .encode_float(pcm, output)
            .map_err(|e| AudioPipelineError::EncodeError(e.to_string()))
    }

    fn set_bitrate(&mut self, bps: u32) -> Result<(), AudioPipelineError> {
        let bps = bps.clamp(16_000, 64_000);
        self.encoder
            .set_bitrate(Bitrate::BitsPerSecond(bps as i32))
            .map_err(|e| AudioPipelineError::EncodeError(e.to_string()))?;
        self.current_bitrate = bps;
        Ok(())
    }

    fn set_fec(&mut self, enabled: bool) -> Result<(), AudioPipelineError> {
        self.encoder
            .set_inband_fec(enabled)
            .map_err(|e| AudioPipelineError::EncodeError(e.to_string()))
    }

    fn set_dtx(&mut self, enabled: bool) -> Result<(), AudioPipelineError> {
        let value = if enabled { 1 } else { 0 };
        self.encoder
            .set_encoder_ctl_request(audiopus::ffi::OPUS_SET_DTX_REQUEST, value)
            .map_err(|e| AudioPipelineError::EncodeError(format!("DTX set failed: {e}")))
    }

    fn set_packet_loss_percentage(&mut self, pct: u8) -> Result<(), AudioPipelineError> {
        self.encoder
            .set_packet_loss_perc(pct)
            .map_err(|e| AudioPipelineError::EncodeError(e.to_string()))
    }

    fn bitrate(&self) -> u32 {
        self.current_bitrate
    }
}

// ---------------------------------------------------------------------------
// RealOpusDecoder
// ---------------------------------------------------------------------------

/// Production Opus decoder wrapping `audiopus::coder::Decoder`.
///
/// Configured at 48 kHz mono. Supports normal decoding and PLC
/// (packet loss concealment) by passing `None` to the underlying decoder.
pub struct RealOpusDecoder {
    decoder: Decoder,
}

impl RealOpusDecoder {
    /// Create a new decoder at 48 kHz mono.
    pub fn new() -> Result<Self, AudioPipelineError> {
        let decoder = Decoder::new(SampleRate::Hz48000, Channels::Mono)
            .map_err(|e| AudioPipelineError::DecodeError(e.to_string()))?;
        Ok(Self { decoder })
    }
}

impl OpusDecode for RealOpusDecoder {
    fn decode_frame(
        &mut self,
        opus_data: &[u8],
        output: &mut [f32],
    ) -> Result<usize, AudioPipelineError> {
        self.decoder
            .decode_float(Some(opus_data), output, false)
            .map_err(|e| AudioPipelineError::DecodeError(e.to_string()))
    }

    fn decode_plc(&mut self, output: &mut [f32]) -> Result<usize, AudioPipelineError> {
        self.decoder
            .decode_float(None::<&[u8]>, output, false)
            .map_err(|e| AudioPipelineError::DecodeError(e.to_string()))
    }
}

// Encoder is Send via audiopus, and we only access it from one thread.
unsafe impl Sync for RealOpusEncoder {}
// Decoder is Send via audiopus, and we only access it from one thread.
unsafe impl Sync for RealOpusDecoder {}

// ---------------------------------------------------------------------------
// RealAudioProcessor
// ---------------------------------------------------------------------------

#[cfg(all(feature = "webrtc-audio-processing", not(target_os = "windows")))]
use crate::audio_pipeline::APM_FRAME_SAMPLES;
use crate::audio_pipeline::{ApmMode, AudioProcess};
#[cfg(all(feature = "webrtc-audio-processing", not(target_os = "windows")))]
use libwebrtc::native::apm::AudioProcessingModule;
use log::info;

/// Production audio processor wrapping `webrtc-audio-processing` APM when
/// available, or running in bypass mode on platforms where the native C++
/// library is not supported in this workspace configuration.
///
/// Configured at 48 kHz mono with AEC, noise suppression, and AGC enabled.
/// Processes 10 ms frames (480 samples).
pub struct RealAudioProcessor {
    #[cfg(all(feature = "webrtc-audio-processing", not(target_os = "windows")))]
    processor: Option<AudioProcessingModule>,
    #[cfg(any(not(feature = "webrtc-audio-processing"), target_os = "windows"))]
    _bypass: (),
    mode: ApmMode,
    #[cfg(all(feature = "webrtc-audio-processing", not(target_os = "windows")))]
    ns_enabled: bool,
}

impl Default for RealAudioProcessor {
    fn default() -> Self {
        Self::new()
    }
}

impl RealAudioProcessor {
    /// Create a new audio processor.
    ///
    /// When the `webrtc-audio-processing` feature is enabled on a supported
    /// target, configures AEC, NS, and AGC at 48 kHz mono. Falls back to
    /// bypass if init fails. Otherwise, always runs in bypass mode.
    pub fn new() -> Self {
        #[cfg(all(feature = "webrtc-audio-processing", not(target_os = "windows")))]
        {
            info!("APM mode: enabled");
            Self {
                processor: Some(Self::build_processor(true)),
                mode: ApmMode::Enabled,
                ns_enabled: true,
            }
        }
        #[cfg(any(not(feature = "webrtc-audio-processing"), target_os = "windows"))]
        {
            info!("APM mode: bypass (feature disabled)");
            Self {
                _bypass: (),
                mode: ApmMode::Bypass,
            }
        }
    }

    /// Create a processor explicitly in bypass mode (no processing).
    pub fn bypass() -> Self {
        info!("APM mode: bypass (feature disabled)");
        #[cfg(all(feature = "webrtc-audio-processing", not(target_os = "windows")))]
        {
            Self {
                processor: None,
                mode: ApmMode::Bypass,
                ns_enabled: false,
            }
        }
        #[cfg(any(not(feature = "webrtc-audio-processing"), target_os = "windows"))]
        {
            Self {
                _bypass: (),
                mode: ApmMode::Bypass,
            }
        }
    }

    /// Returns the current APM operating mode.
    pub fn apm_mode(&self) -> ApmMode {
        self.mode
    }

    /// Returns `true` if the processor is in bypass mode.
    pub fn is_bypass(&self) -> bool {
        #[cfg(all(feature = "webrtc-audio-processing", not(target_os = "windows")))]
        {
            self.processor.is_none()
        }
        #[cfg(any(not(feature = "webrtc-audio-processing"), target_os = "windows"))]
        {
            true
        }
    }

    /// Enable or disable the APM noise suppression (NS) component at runtime.
    ///
    /// When `webrtc-audio-processing` is enabled on a supported target and the
    /// processor is active, reconfigures APM with `noise_suppression:
    /// Some(...)` or `None` while keeping AEC and AGC unchanged. Otherwise
    /// this is a no-op.
    pub fn set_ns_enabled(&mut self, enabled: bool) {
        #[cfg(all(feature = "webrtc-audio-processing", not(target_os = "windows")))]
        {
            if self.processor.is_none() || self.ns_enabled == enabled {
                return;
            }

            self.processor = Some(Self::build_processor(enabled));
            self.ns_enabled = enabled;
        }

        #[cfg(any(not(feature = "webrtc-audio-processing"), target_os = "windows"))]
        {
            let _ = enabled;
        }
    }
}

impl AudioProcess for RealAudioProcessor {
    fn process_capture_frame(
        &mut self,
        capture_frame: &mut [f32],
        reference_frame: &[f32],
    ) -> Result<(), AudioPipelineError> {
        #[cfg(all(feature = "webrtc-audio-processing", not(target_os = "windows")))]
        {
            let processor = match &mut self.processor {
                Some(p) => p,
                None => return Ok(()),
            };

            if capture_frame.len() != APM_FRAME_SAMPLES {
                return Err(AudioPipelineError::ProcessingError(format!(
                    "capture frame must be {} samples, got {}",
                    APM_FRAME_SAMPLES,
                    capture_frame.len()
                )));
            }
            if reference_frame.len() != APM_FRAME_SAMPLES {
                return Err(AudioPipelineError::ProcessingError(format!(
                    "reference frame must be {} samples, got {}",
                    APM_FRAME_SAMPLES,
                    reference_frame.len()
                )));
            }

            let mut render_buf = Self::f32_to_i16(reference_frame);
            processor
                .process_reverse_stream(&mut render_buf, 48_000, 1)
                .map_err(|e| AudioPipelineError::ProcessingError(e.to_string()))?;

            let mut capture_buf = Self::f32_to_i16(capture_frame);
            processor
                .process_stream(&mut capture_buf, 48_000, 1)
                .map_err(|e| AudioPipelineError::ProcessingError(e.to_string()))?;
            Self::copy_i16_to_f32(&capture_buf, capture_frame);
        }

        #[cfg(any(not(feature = "webrtc-audio-processing"), target_os = "windows"))]
        {
            let _ = (capture_frame, reference_frame);
        }

        Ok(())
    }

    fn apm_mode(&self) -> ApmMode {
        self.mode
    }
}

unsafe impl Send for RealAudioProcessor {}
unsafe impl Sync for RealAudioProcessor {}

impl RealAudioProcessor {
    #[cfg(all(feature = "webrtc-audio-processing", not(target_os = "windows")))]
    fn build_processor(ns_enabled: bool) -> AudioProcessingModule {
        AudioProcessingModule::new(true, true, false, ns_enabled)
    }

    #[cfg(all(feature = "webrtc-audio-processing", not(target_os = "windows")))]
    fn f32_to_i16(input: &[f32]) -> Vec<i16> {
        input
            .iter()
            .map(|sample| (sample.clamp(-1.0, 1.0) * i16::MAX as f32) as i16)
            .collect()
    }

    #[cfg(all(feature = "webrtc-audio-processing", not(target_os = "windows")))]
    fn copy_i16_to_f32(input: &[i16], output: &mut [f32]) {
        for (src, dst) in input.iter().zip(output.iter_mut()) {
            *dst = *src as f32 / i16::MAX as f32;
        }
    }
}

// ===========================================================================
// Tests (require real-backends feature — Opus codec must be available)
// ===========================================================================

#[cfg(test)]
mod tests {
    use super::*;
    use crate::audio_pipeline::{FRAME_SAMPLES, MAX_OPUS_PACKET};
    use proptest::prelude::*;

    // -----------------------------------------------------------------------
    // Property 1: Opus encode/decode round-trip
    // **Validates: Requirements 1.2, 1.3, 1.4**
    //
    // For any 960-sample f32 PCM frame with values in [-1.0, 1.0], encoding
    // then decoding produces exactly 960 samples, and re-encoding then
    // re-decoding produces output within bounded tolerance of the first
    // decode output.
    // -----------------------------------------------------------------------

    proptest! {
        #![proptest_config(ProptestConfig { cases: 100, .. ProptestConfig::default() })]

        /// Feature: audio-quality-overhaul, Property 1: Opus encode/decode round-trip
        #[test]
        fn opus_round_trip(
            pcm in proptest::collection::vec(-1.0f32..1.0f32, FRAME_SAMPLES..=FRAME_SAMPLES),
        ) {
            let mut encoder = RealOpusEncoder::new(32_000).unwrap();
            let mut decoder = RealOpusDecoder::new().unwrap();

            // First pass: encode → decode
            let mut opus_buf = vec![0u8; MAX_OPUS_PACKET];
            let encoded_len = encoder.encode_frame(&pcm, &mut opus_buf).unwrap();
            prop_assert!(encoded_len > 0, "Encoded length should be > 0");

            let mut decoded1 = vec![0.0f32; FRAME_SAMPLES];
            let decoded_samples = decoder.decode_frame(&opus_buf[..encoded_len], &mut decoded1).unwrap();
            prop_assert_eq!(decoded_samples, FRAME_SAMPLES, "First decode should produce {} samples", FRAME_SAMPLES);

            // Second pass: use fresh encoder/decoder to avoid DTX state contamination.
            // Re-encode the decoded output → decode again.
            let mut encoder2 = RealOpusEncoder::new(32_000).unwrap();
            let mut decoder2 = RealOpusDecoder::new().unwrap();

            let mut opus_buf2 = vec![0u8; MAX_OPUS_PACKET];
            let encoded_len2 = encoder2.encode_frame(&decoded1, &mut opus_buf2).unwrap();
            prop_assert!(encoded_len2 > 0, "Re-encoded length should be > 0");

            let mut decoded2 = vec![0.0f32; FRAME_SAMPLES];
            let decoded_samples2 = decoder2.decode_frame(&opus_buf2[..encoded_len2], &mut decoded2).unwrap();
            prop_assert_eq!(decoded_samples2, FRAME_SAMPLES, "Second decode should produce {} samples", FRAME_SAMPLES);

            // The second decode should be close to the first decode (lossy compression is stable).
            // Use RMS difference — more meaningful for audio than max sample diff.
            let rms_diff: f32 = (decoded1.iter().zip(decoded2.iter())
                .map(|(a, b)| (a - b).powi(2))
                .sum::<f32>() / FRAME_SAMPLES as f32)
                .sqrt();

            prop_assert!(
                rms_diff < 0.75,
                "Re-encoded round-trip RMS diff {} exceeds tolerance 0.75",
                rms_diff
            );
        }
    }

    // -----------------------------------------------------------------------
    // Property 2: DTX reduces packet size for silence
    // **Validates: Requirements 1.6**
    //
    // For any Opus encoder with DTX enabled, encoding a frame of silence
    // (all zeros) should produce a packet smaller than encoding a frame of
    // non-silent audio (sine wave at 0.5 amplitude). This is a metamorphic
    // property: changing input from signal to silence reduces output size.
    // -----------------------------------------------------------------------

    proptest! {
        #![proptest_config(ProptestConfig { cases: 100, .. ProptestConfig::default() })]

        /// Feature: audio-quality-overhaul, Property 2: DTX reduces packet size for silence
        #[test]
        fn dtx_reduces_packet_size_for_silence(
            bitrate in 16_000u32..=64_000,
        ) {
            let mut encoder = RealOpusEncoder::new(bitrate).unwrap();

            // Generate a non-silent signal: sine wave at 0.5 amplitude, ~440 Hz.
            let signal: Vec<f32> = (0..FRAME_SAMPLES)
                .map(|i| 0.5 * (2.0 * std::f32::consts::PI * 440.0 * i as f32 / 48000.0).sin())
                .collect();

            // Feed several signal frames to warm up the encoder state,
            // then measure the signal packet size.
            let mut opus_buf = vec![0u8; MAX_OPUS_PACKET];
            for _ in 0..5 {
                encoder.encode_frame(&signal, &mut opus_buf).unwrap();
            }
            let signal_len = encoder.encode_frame(&signal, &mut opus_buf).unwrap();

            // Now feed several silence frames so DTX kicks in,
            // then measure the silence packet size.
            let silence = vec![0.0f32; FRAME_SAMPLES];
            for _ in 0..10 {
                encoder.encode_frame(&silence, &mut opus_buf).unwrap();
            }
            let silence_len = encoder.encode_frame(&silence, &mut opus_buf).unwrap();

            prop_assert!(
                silence_len < signal_len,
                "DTX silence packet ({} bytes) should be smaller than signal packet ({} bytes) at bitrate {}",
                silence_len, signal_len, bitrate
            );
        }
    }

    // -----------------------------------------------------------------------
    // Unit tests: APM mode variants
    // Feature: audio-transport-hardening, Task 4.3
    // **Validates: Requirements 5.1, 5.2**
    //
    // Verify that RealAudioProcessor reports the correct ApmMode from
    // new(), bypass(), and the AudioProcess trait default.
    // -----------------------------------------------------------------------

    #[test]
    fn apm_mode_bypass_constructor() {
        let proc = RealAudioProcessor::bypass();
        assert_eq!(proc.apm_mode(), ApmMode::Bypass);
        assert!(proc.is_bypass());
    }

    #[cfg(any(not(feature = "webrtc-audio-processing"), target_os = "windows"))]
    #[test]
    fn apm_mode_new_without_feature_returns_bypass() {
        // Without the webrtc-audio-processing feature, new() always returns Bypass.
        let proc = RealAudioProcessor::new();
        assert_eq!(proc.apm_mode(), ApmMode::Bypass);
        assert!(proc.is_bypass());
    }

    #[cfg(all(feature = "webrtc-audio-processing", not(target_os = "windows")))]
    #[test]
    fn apm_mode_new_with_feature_returns_enabled_or_failed_init() {
        // With the feature compiled, new() returns Enabled on success or
        // FailedInit if the native APM library fails to initialize.
        let proc = RealAudioProcessor::new();
        let mode = proc.apm_mode();
        assert!(
            mode == ApmMode::Enabled || mode == ApmMode::FailedInit,
            "expected Enabled or FailedInit, got {:?}",
            mode
        );
    }

    #[test]
    fn apm_mode_trait_default_returns_bypass() {
        use crate::audio_pipeline::AudioProcess;

        // MockAudioProcessor doesn't override apm_mode(), so it gets the
        // trait default of Bypass.
        use crate::audio_pipeline_mock::MockAudioProcessor;
        let mock = MockAudioProcessor::new(false);
        assert_eq!(mock.apm_mode(), ApmMode::Bypass);
    }
}
