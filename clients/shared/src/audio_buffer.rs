//! Owns the shared ring buffer for passing mono 48kHz f32 audio samples
//! between CPAL and WebRTC.
//!
//! This module does not own CPAL device management, per-peer volumes, or
//! stream lifecycle — those concerns live in `cpal_device`, `peer_volumes`,
//! and `cpal_audio` respectively.

use crate::peer_volumes::{perceptual_gain, DEFAULT_VOLUME};
use std::sync::atomic::{AtomicU8, Ordering};
use std::sync::{Arc, Mutex};

/// Default target fill level for ring buffers (ms).
pub const DEFAULT_TARGET_OCCUPANCY_MS: usize = 80;
/// Default margin above target before backlog drop triggers (ms).
pub const DEFAULT_MAX_MARGIN_MS: usize = 40;
/// Default total buffer capacity (ms). Reduced from 300 to bound latency.
pub const DEFAULT_BUFFER_DURATION_MS: usize = 120;
/// Playback needs materially more headroom than capture to absorb callback
/// cadence jitter and clock mismatch without frequent trimming artifacts.
pub const DEFAULT_PLAYBACK_BUFFER_DURATION_MS: usize = 600;
/// Playback target occupancy (ms).
pub const DEFAULT_PLAYBACK_TARGET_OCCUPANCY_MS: usize = 300;
/// Playback margin above target before trimming (ms).
pub const DEFAULT_PLAYBACK_MAX_MARGIN_MS: usize = 150;

/// Extended stats returned by `AudioBuffer::stats()`.
#[derive(Debug, Clone, Copy, Default)]
pub struct BufferStats {
    pub underruns: u64,
    pub overruns: u64,
    pub backlog_drops: u64,
    pub backlog_dropped_samples: u64,
}

/// Shared ring buffer for passing mono 48kHz f32 audio samples between
/// CPAL and WebRTC.
///
/// The capture side writes raw CPAL samples (potentially multi-channel,
/// any sample rate) and the buffer performs stereo→mono downmix inline.
/// The WebRTC side reads mono 48kHz f32 samples.
///
/// Uses an adaptive capacity: starts at a base size and tracks underrun/
/// overrun events so callers can monitor buffer health.
#[derive(Clone)]
pub struct AudioBuffer {
    inner: Arc<Mutex<AudioBufferInner>>,
    /// Playback volume 0–100. Atomic so the CPAL audio thread can read it
    /// without contending on the mutex.
    volume: Arc<AtomicU8>,
}

struct AudioBufferInner {
    /// Ring buffer of f32 samples (mono 48kHz)
    buf: Vec<f32>,
    /// Write position (monotonically increasing)
    write_pos: usize,
    /// Read position (monotonically increasing)
    read_pos: usize,
    /// Capacity in samples
    capacity: usize,
    /// Number of channels the writer is producing (set by CPAL config)
    write_channels: u16,
    /// Count of underrun events (reader wanted more than available)
    pub underruns: u64,
    /// Count of overrun events (writer overwrote unread data)
    pub overruns: u64,
    /// Target fill level in samples (default: 80ms * 48 = 3840)
    target_occupancy_samples: usize,
    /// Margin above target before backlog drop triggers (default: 40ms * 48 = 1920)
    max_margin_samples: usize,
    /// Count of backlog drop events
    backlog_drops: u64,
    /// Total samples dropped by backlog enforcement
    backlog_dropped_samples: u64,
}

impl AudioBufferInner {
    /// Preemptively drop oldest samples before writing a new batch so the
    /// buffer does not hit hard overrun on bursty writes.
    fn preempt_for_incoming(&mut self, incoming_samples: usize) {
        let fill = self.write_pos.saturating_sub(self.read_pos);
        let projected = fill.saturating_add(incoming_samples);
        let threshold = self.target_occupancy_samples + self.max_margin_samples;

        if projected > threshold {
            // Drop only what is necessary to keep projected fill at threshold.
            // This avoids large discontinuities that can sound metallic.
            let over = projected - threshold;
            let drop = over.min(fill);
            if drop > 0 {
                self.read_pos += drop;
                self.backlog_drops += 1;
                self.backlog_dropped_samples += drop as u64;
            }
        }
    }

    /// If fill exceeds target + margin, advance read pointer to target occupancy.
    /// Never drops below one frame (960 samples).
    fn enforce_backlog_drop(&mut self) {
        let fill = self.write_pos.saturating_sub(self.read_pos);
        let threshold = self.target_occupancy_samples + self.max_margin_samples;
        if fill > threshold {
            let excess = fill - self.target_occupancy_samples;
            // Never drop below one frame (960 samples)
            let max_drop = fill.saturating_sub(960);
            let drop = excess.min(max_drop);
            if drop > 0 {
                self.read_pos += drop;
                self.backlog_drops += 1;
                self.backlog_dropped_samples += drop as u64;
            }
        }
    }
}

impl AudioBuffer {
    /// Create a buffer with default capacity (120ms) and default target/margin.
    pub fn new(duration_ms: usize) -> Self {
        Self::with_target(
            duration_ms,
            DEFAULT_TARGET_OCCUPANCY_MS,
            DEFAULT_MAX_MARGIN_MS,
        )
    }

    /// Create a buffer with explicit target occupancy and margin parameters.
    pub fn with_target(
        duration_ms: usize,
        target_occupancy_ms: usize,
        max_margin_ms: usize,
    ) -> Self {
        let capacity = 48 * duration_ms; // 48 samples per ms at 48kHz
        Self {
            inner: Arc::new(Mutex::new(AudioBufferInner {
                buf: vec![0.0; capacity],
                write_pos: 0,
                read_pos: 0,
                capacity,
                write_channels: 1,
                underruns: 0,
                overruns: 0,
                target_occupancy_samples: target_occupancy_ms * 48,
                max_margin_samples: max_margin_ms * 48,
                backlog_drops: 0,
                backlog_dropped_samples: 0,
            })),
            volume: Arc::new(AtomicU8::new(DEFAULT_VOLUME)),
        }
    }

    /// Set the number of channels the writer produces.
    /// Call this after querying the CPAL device config.
    pub fn set_write_channels(&self, channels: u16) {
        self.inner.lock().unwrap().write_channels = channels.max(1);
    }

    /// Write interleaved multi-channel samples from CPAL, downmixing to mono.
    /// For stereo input, averages L+R. For >2 channels, averages all.
    pub fn write(&self, samples: &[f32]) {
        let mut inner = self.inner.lock().unwrap();
        let ch = inner.write_channels as usize;
        if let Some(incoming_mono_samples) = samples.len().checked_div(ch) {
            inner.preempt_for_incoming(incoming_mono_samples);
        }

        // Process in frames of `ch` samples each
        let mut i = 0;
        while i + ch <= samples.len() {
            let mono = if ch == 1 {
                samples[i]
            } else {
                let sum: f32 = samples[i..i + ch].iter().sum();
                sum / ch as f32
            };

            let idx = inner.write_pos % inner.capacity;
            inner.buf[idx] = mono;
            inner.write_pos += 1;

            // Detect overrun: writer caught up to reader
            let available = inner.write_pos.saturating_sub(inner.read_pos);
            if available > inner.capacity {
                inner.read_pos = inner.write_pos - inner.capacity;
                inner.overruns += 1;
            }

            i += ch;
        }

        // Enforce backlog drop after the full write batch.
        inner.enforce_backlog_drop();
    }

    /// Write pre-mixed mono samples directly (used by the receive/playback path).
    pub fn write_mono(&self, samples: &[f32]) {
        let mut inner = self.inner.lock().unwrap();
        inner.preempt_for_incoming(samples.len());
        for &s in samples {
            let idx = inner.write_pos % inner.capacity;
            inner.buf[idx] = s;
            inner.write_pos += 1;

            let available = inner.write_pos.saturating_sub(inner.read_pos);
            if available > inner.capacity {
                inner.read_pos = inner.write_pos - inner.capacity;
                inner.overruns += 1;
            }
        }

        // Enforce backlog drop after the full write batch.
        inner.enforce_backlog_drop();
    }

    /// Read up to `out.len()` mono samples for WebRTC or speaker output.
    /// Returns actual number of samples read. Tracks underruns.
    /// Applies the current volume scaling (0–100) to each sample.
    pub fn read(&self, out: &mut [f32]) -> usize {
        let vol = self.volume.load(Ordering::Relaxed);
        let gain = perceptual_gain(vol);
        let mut inner = self.inner.lock().unwrap();
        let available = inner.write_pos.saturating_sub(inner.read_pos);
        let to_read = out.len().min(available);

        if to_read < out.len() {
            inner.underruns += 1;
        }

        for (i, sample) in out.iter_mut().enumerate().take(to_read) {
            let idx = (inner.read_pos + i) % inner.capacity;
            *sample = inner.buf[idx] * gain;
        }
        inner.read_pos += to_read;
        to_read
    }

    /// Set playback volume (clamped to 0–100).
    pub fn set_volume(&self, vol: u8) {
        self.volume.store(vol.min(100), Ordering::Relaxed);
    }

    /// Get current playback volume (0–100).
    pub fn volume(&self) -> u8 {
        self.volume.load(Ordering::Relaxed)
    }

    /// Returns buffer health stats.
    pub fn stats(&self) -> BufferStats {
        let inner = self.inner.lock().unwrap();
        BufferStats {
            underruns: inner.underruns,
            overruns: inner.overruns,
            backlog_drops: inner.backlog_drops,
            backlog_dropped_samples: inner.backlog_dropped_samples,
        }
    }

    /// Returns the number of samples currently available to read.
    pub fn available(&self) -> usize {
        let inner = self.inner.lock().unwrap();
        inner.write_pos.saturating_sub(inner.read_pos)
    }

    /// Returns the current fill level in milliseconds.
    pub fn fill_ms(&self) -> usize {
        let inner = self.inner.lock().unwrap();
        inner.write_pos.saturating_sub(inner.read_pos) / 48
    }

    /// Returns total backlog-dropped duration in milliseconds.
    pub fn backlog_dropped_ms(&self) -> u64 {
        let inner = self.inner.lock().unwrap();
        inner.backlog_dropped_samples / 48
    }

    /// Advance the read pointer by `n * frame_samples` samples, discarding
    /// the oldest frames. This only adjusts the read pointer — it does NOT
    /// increment underrun, overrun, or backlog counters (those are separate
    /// concerns). Used by the sender to drop stale frames.
    pub fn skip_frames(&self, n: usize, frame_samples: usize) {
        let mut inner = self.inner.lock().unwrap();
        let skip = n * frame_samples;
        let available = inner.write_pos.saturating_sub(inner.read_pos);
        // Never skip more than what's available.
        let actual_skip = skip.min(available);
        inner.read_pos += actual_skip;
    }

    /// Peek at the most recent `out.len()` samples without advancing the
    /// read pointer. Used by the AEC reference path so it can observe what
    /// was recently played without stealing samples from the speaker output.
    ///
    /// Fills `out` with the most recent samples (or zeros if not enough data).
    /// Returns the number of real samples copied.
    pub fn peek_recent(&self, out: &mut [f32]) -> usize {
        let inner = self.inner.lock().unwrap();
        let available = inner.write_pos.saturating_sub(inner.read_pos);
        let to_peek = out.len().min(available);

        // Read from the most recent `to_peek` samples (near write_pos).
        let start = inner.write_pos.saturating_sub(to_peek);
        for (i, sample) in out.iter_mut().enumerate().take(to_peek) {
            let idx = (start + i) % inner.capacity;
            *sample = inner.buf[idx];
        }
        // Zero-fill the rest if not enough data.
        for sample in out[to_peek..].iter_mut() {
            *sample = 0.0;
        }
        to_peek
    }
}

// ===========================================================================
// Tests
// ===========================================================================

#[cfg(test)]
mod tests {
    use super::*;
    use proptest::prelude::*;

    // -----------------------------------------------------------------------
    // Property 9: AudioBuffer write/read round-trip
    // **Validates: Requirements 4.1**
    //
    // For any sequence of mono f32 samples written to the AudioBuffer
    // (within capacity), reading the same number of samples returns the
    // exact same values in the same order.
    // -----------------------------------------------------------------------

    proptest! {
        #![proptest_config(ProptestConfig { cases: 100, .. ProptestConfig::default() })]

        /// Feature: audio-quality-overhaul, Property 9: AudioBuffer write/read round-trip
        #[test]
        fn audio_buffer_write_read_round_trip(
            samples in proptest::collection::vec(-1.0f32..1.0f32, 1..4800),
        ) {
            // Create a buffer large enough to hold all samples (100ms = 4800 samples).
            let buf = AudioBuffer::new(100);
            // Set volume to give unity gain so round-trip is lossless.
            buf.set_volume(69); // perceptual_gain(69) ≈ 1.0

            buf.write_mono(&samples);

            let mut output = vec![0.0f32; samples.len()];
            let read_count = buf.read(&mut output);

            prop_assert_eq!(read_count, samples.len());
            // Allow tiny floating-point tolerance from gain multiplication.
            let gain = perceptual_gain(69);
            for (i, (&got, &expected)) in output.iter().zip(samples.iter()).enumerate() {
                let diff = (got - expected * gain).abs();
                prop_assert!(
                    diff < 1e-5,
                    "sample[{}]: expected {}, got {}, diff={}",
                    i, expected * gain, got, diff
                );
            }
        }
    }

    // -----------------------------------------------------------------------
    // Property 11: Stereo-to-mono downmix averages channels
    // **Validates: Requirements 4.4**
    //
    // For any interleaved stereo frame [L0, R0, L1, R1, ...], the mono
    // output satisfies mono[i] == (L[i] + R[i]) / 2.0 for all i.
    // -----------------------------------------------------------------------

    proptest! {
        #![proptest_config(ProptestConfig { cases: 100, .. ProptestConfig::default() })]

        /// Feature: audio-quality-overhaul, Property 11: Stereo-to-mono downmix averages channels
        #[test]
        fn stereo_downmix_averages_channels(
            // Generate pairs of (left, right) samples.
            stereo_pairs in proptest::collection::vec(
                (-1.0f32..1.0f32, -1.0f32..1.0f32),
                1..2400,
            ),
        ) {
            let buf = AudioBuffer::new(100);
            buf.set_write_channels(2);
            // Set volume to give unity gain so downmix math is isolated.
            buf.set_volume(69); // perceptual_gain(69) ≈ 1.0

            // Build interleaved stereo data: [L0, R0, L1, R1, ...]
            let interleaved: Vec<f32> = stereo_pairs
                .iter()
                .flat_map(|&(l, r)| [l, r])
                .collect();

            buf.write(&interleaved);

            let frame_count = stereo_pairs.len();
            let mut output = vec![0.0f32; frame_count];
            let read_count = buf.read(&mut output);

            prop_assert_eq!(read_count, frame_count);

            for (i, &(l, r)) in stereo_pairs.iter().enumerate() {
                let expected = (l + r) / 2.0 * perceptual_gain(69);
                let diff = (output[i] - expected).abs();
                prop_assert!(
                    diff < 1e-6,
                    "Frame {}: expected ({} + {}) / 2 = {}, got {}, diff={}",
                    i, l, r, expected, output[i], diff
                );
            }
        }
    }

    // -----------------------------------------------------------------------
    // peek_recent: reads without consuming
    // -----------------------------------------------------------------------

    #[test]
    fn peek_recent_does_not_consume_samples() {
        let buf = AudioBuffer::new(100);
        let samples = vec![0.1f32, 0.2, 0.3, 0.4, 0.5];
        buf.write_mono(&samples);

        // Peek should return the most recent samples (unscaled by volume).
        let mut peeked = vec![0.0f32; 3];
        let n = buf.peek_recent(&mut peeked);
        assert_eq!(n, 3);
        assert_eq!(&peeked, &[0.3, 0.4, 0.5]);

        // Read should still return ALL samples (peek didn't consume).
        // Values are scaled by perceptual gain at DEFAULT_VOLUME.
        let mut output = vec![0.0f32; 5];
        let read = buf.read(&mut output);
        assert_eq!(read, 5);
        let gain = perceptual_gain(DEFAULT_VOLUME);
        for (i, &s) in samples.iter().enumerate() {
            let diff = (output[i] - s * gain).abs();
            assert!(
                diff < 1e-6,
                "sample[{i}]: expected {}, got {}",
                s * gain,
                output[i]
            );
        }
    }

    #[test]
    fn peek_recent_zero_fills_when_insufficient_data() {
        let buf = AudioBuffer::new(100);
        buf.write_mono(&[0.5, 0.6]);

        let mut peeked = vec![0.0f32; 5];
        let n = buf.peek_recent(&mut peeked);
        assert_eq!(n, 2);
        // First 2 slots filled, rest zero.
        assert_eq!(&peeked, &[0.5, 0.6, 0.0, 0.0, 0.0]);
    }

    // -----------------------------------------------------------------------
    // Property 2: Received audio reaches playback buffer
    // **Validates: Requirements 3.1**
    //
    // For any non-empty array of f32 samples written via `write_mono`
    // (simulating what the `connect` event loop does after
    // `convert_audio_frame`), `available()` SHALL equal the number of
    // samples written.
    //
    // Feature: livekit-audio-fix, Property 2: Received audio reaches playback buffer
    // -----------------------------------------------------------------------

    proptest! {
        #![proptest_config(ProptestConfig::with_cases(100))]

        #[test]
        fn prop_received_audio_reaches_playback_buffer(
            // Generate between 1 and 4800 samples (≤ 100ms buffer capacity).
            samples in proptest::collection::vec(
                proptest::num::f32::NORMAL,
                1..=4800usize,
            ),
        ) {
            // Buffer sized to hold all samples without overrun (100ms = 4800 samples).
            let buf = AudioBuffer::new(100);

            buf.write_mono(&samples);

            prop_assert_eq!(
                buf.available(),
                samples.len(),
                "available() should equal the number of samples written via write_mono"
            );
        }
    }

    // -----------------------------------------------------------------------
    // Feature: audio-transport-hardening, Property 7: Backlog drop enforces target occupancy
    // **Validates: Requirements 3.3**
    //
    // For any AudioBuffer with target_occupancy_ms = T and max_margin_ms = M,
    // and for any write that causes fill to exceed (T + M) * 48 samples,
    // the buffer SHALL advance the read pointer so that fill equals T * 48.
    // The resulting fill SHALL never be less than one frame (960 samples).
    // The backlog_drops counter SHALL increment by 1 and backlog_dropped_samples
    // SHALL increase by the number of samples dropped.
    // -----------------------------------------------------------------------

    proptest! {
        #![proptest_config(ProptestConfig::with_cases(100))]

        #[test]
        fn prop_backlog_drop_enforces_target_occupancy(
            // Target occupancy: 20..=200 ms (must be at least one frame = 20ms)
            target_ms in 20usize..=200,
            // Margin: 10..=100 ms
            margin_ms in 10usize..=100,
            // Write size: enough to potentially exceed threshold
            write_samples in 960usize..=20000,
        ) {
            // Buffer capacity must hold at least target + margin + write
            let capacity_ms = (target_ms + margin_ms + write_samples / 48).max(target_ms + margin_ms + 50);
            let buf = AudioBuffer::with_target(capacity_ms, target_ms, margin_ms);

            let samples = vec![0.5f32; write_samples];
            buf.write_mono(&samples);

            let stats = buf.stats();
            let fill = buf.available();
            let target_samples = target_ms * 48;
            let threshold = target_samples + margin_ms * 48;

            if write_samples > threshold {
                // Drop should have triggered
                prop_assert!(stats.backlog_drops >= 1, "backlog_drops should be >= 1 when fill exceeded threshold");
                // Fill should equal target (or 960 if target < 960)
                let expected_fill = target_samples.max(960);
                prop_assert_eq!(fill, expected_fill,
                    "fill should equal target_samples ({}) after drop, got {}",
                    expected_fill, fill);
                // Dropped samples should match
                let expected_dropped = write_samples - expected_fill;
                prop_assert_eq!(stats.backlog_dropped_samples, expected_dropped as u64,
                    "backlog_dropped_samples should be {}, got {}",
                    expected_dropped, stats.backlog_dropped_samples);
            }

            // Fill must never be below 960 samples (one frame)
            if fill > 0 {
                prop_assert!(fill >= 960.min(write_samples),
                    "fill ({}) should never be below min(960, write_samples({}))",
                    fill, write_samples);
            }
        }
    }

    // -----------------------------------------------------------------------
    // Unit tests for AudioBuffer edge cases (Task 2.7)
    // Requirements: 3.1, 3.2, 3.3
    // -----------------------------------------------------------------------

    #[test]
    fn default_capacity_is_120ms() {
        // DEFAULT_BUFFER_DURATION_MS = 120, so capacity = 120 * 48 = 5760
        let buf = AudioBuffer::new(DEFAULT_BUFFER_DURATION_MS);
        // Write exactly 5760 samples — should all fit
        let samples = vec![1.0f32; 5760];
        buf.write_mono(&samples);
        assert_eq!(buf.available(), 5760);
        // Write one more — preemptive backlog trim should avoid hard overrun.
        buf.write_mono(&[1.0]);
        let stats = buf.stats();
        assert_eq!(stats.overruns, 0);
        assert!(stats.backlog_drops >= 1);
    }

    #[test]
    fn write_exactly_at_threshold_no_drop() {
        // target=80ms (3840 samples), margin=40ms (1920 samples)
        // threshold = 3840 + 1920 = 5760 samples
        let buf = AudioBuffer::with_target(200, 80, 40);
        let samples = vec![0.5f32; 5760]; // exactly at threshold
        buf.write_mono(&samples);

        let stats = buf.stats();
        assert_eq!(stats.backlog_drops, 0, "no drop at exact threshold");
        assert_eq!(stats.backlog_dropped_samples, 0);
        assert_eq!(buf.available(), 5760);
    }

    #[test]
    fn write_one_over_threshold_triggers_drop() {
        // target=80ms (3840 samples), margin=40ms (1920 samples)
        // threshold = 5760, writing 5761 should trigger drop
        let buf = AudioBuffer::with_target(200, 80, 40);
        let samples = vec![0.5f32; 5761]; // one over threshold
        buf.write_mono(&samples);

        let stats = buf.stats();
        assert_eq!(stats.backlog_drops, 1, "drop should trigger at threshold+1");
        // excess = 5761 - 3840 = 1921, max_drop = 5761 - 960 = 4801
        // drop = min(1921, 4801) = 1921
        assert_eq!(stats.backlog_dropped_samples, 1921);
        assert_eq!(buf.available(), 3840); // fill == target
    }

    #[test]
    fn fill_ms_accuracy() {
        let buf = AudioBuffer::with_target(200, 80, 40);
        // Write exactly 960 samples = 20ms
        buf.write_mono(&vec![0.5f32; 960]);
        assert_eq!(buf.fill_ms(), 20);

        // Write 3840 more = 80ms total = 4800 samples
        buf.write_mono(&vec![0.5f32; 3840]);
        assert_eq!(buf.fill_ms(), 100); // 4800 / 48 = 100
    }

    #[test]
    fn backlog_drop_never_below_960_samples() {
        // Use a very small target (20ms = 960 samples) and small margin (10ms = 480)
        // threshold = 960 + 480 = 1440
        // Write 2000 samples: fill=2000, excess=2000-960=1040, max_drop=2000-960=1040
        // drop = min(1040, 1040) = 1040, remaining = 960
        let buf = AudioBuffer::with_target(100, 20, 10);
        buf.write_mono(&vec![0.5f32; 2000]);

        assert_eq!(buf.available(), 960, "fill should not drop below 960");
        let stats = buf.stats();
        assert_eq!(stats.backlog_drops, 1);
        assert_eq!(stats.backlog_dropped_samples, 1040);
    }
}
