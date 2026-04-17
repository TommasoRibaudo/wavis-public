//! Real-time noise suppression filter wrapping nnnoiseless (RNNoise).
//!
//! Sits between CPAL capture and APM processing in the send loop.
//! Processes 960-sample (20 ms) pipeline frames by splitting into
//! two consecutive 480-sample nnnoiseless calls.
//!
//! ## Post-denoise gating
//!
//! After each 480-sample sub-frame is denoised, a two-layer gate decides
//! whether the output is speech or residual noise:
//!
//! 1. **VAD gate** — `process_frame()` returns a speech probability (0.0–1.0).
//!    Sub-frames below `VAD_PROB_THRESHOLD` are gated. Catches transient
//!    non-stationary noise (cat meows, chair scrapes) that RNNoise can't
//!    fully suppress but correctly classifies as non-speech.
//!
//! 2. **Energy gate** — post-denoise RMS below `VAD_GATE_RMS_THRESHOLD`
//!    triggers gating regardless of VAD. Catches residual hiss/hum that
//!    RNNoise attenuated but the VAD might flag as borderline.
//!
//! The gate applies a smooth gain ramp (attack/release) to avoid audible
//! clicks at gate transitions. See [`GateState`] and [`compute_gate_gain`].
//!
//! ## Native Linux/Hyprland speech-quality tuning
//!
//! Real-world validation on the native Tauri media path showed that "RNNoise
//! works" was not enough by itself: the surrounding post-denoise gate could
//! still produce clipped syllables or light crackle even while background
//! noise suppression was effective. The current gate tuning therefore bakes in
//! three pragmatic protections that came directly from that validation:
//!
//! 1. slower close than open, so brief speech dips do not get chopped
//! 2. a short close-hold, so one or two weak sub-frames do not cut word parts
//! 3. per-sub-frame gain ramps, so gain changes do not sound like crackle
//!
//! These comments are intentionally specific because this tuning should not be
//! "cleaned up" later without re-validating the native Linux speech path.

use nnnoiseless::DenoiseState;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Mutex;

use crate::audio_pipeline::FRAME_SAMPLES;

/// Number of samples per nnnoiseless processing unit (10 ms at 48 kHz).
pub const RNN_FRAME_SAMPLES: usize = 480;

/// Scale factor to convert between [-1.0, 1.0] float PCM and the
/// [-32768.0, 32767.0] range that nnnoiseless expects.
const I16_SCALE: f32 = i16::MAX as f32;

/// Post-denoise RMS threshold (in nnnoiseless i16-scale).
/// Sub-frames whose RMS falls below this after RNNoise are gated,
/// regardless of VAD probability. Catches residual hum/hiss that
/// RNNoise reduced but didn't fully eliminate.
///
/// Tuned empirically: ~0.5% of i16 range. Speech frames typically
/// sit well above this even at low volume.
pub const VAD_GATE_RMS_THRESHOLD: f32 = 150.0;

/// VAD probability threshold. Sub-frames whose RNNoise VAD probability
/// falls below this are gated. Catches transient non-stationary noise
/// (cat meows, chair scrapes, keyboard clicks) that has harmonic content
/// overlapping with speech — RNNoise can't fully suppress these, but its
/// VAD model correctly identifies them as non-speech during silence.
///
/// 0.0 = no speech, 1.0 = confident speech. Threshold of 0.5 is
/// conservative enough to avoid clipping speech onsets.
pub const VAD_PROB_THRESHOLD: f32 = 0.5;

/// Per-sub-frame gain step when the gate is closing (attack).
/// At 48 kHz / 480 samples per sub-frame = 10 ms per step.
/// 0.1 per step → full close in ~10 sub-frames (100 ms).
///
/// Keeping close slower than open reduces intermittent speech truncation when
/// RNNoise/VAD briefly dips on quiet syllables or word endings.
const GATE_ATTACK_STEP: f32 = 0.1;

/// Per-sub-frame gain step when the gate is opening (release).
/// 0.2 per step → full open in ~5 sub-frames (50 ms).
const GATE_RELEASE_STEP: f32 = 0.2;

/// Number of consecutive "close" decisions required before the gate starts
/// attenuating. This adds a small hangover so brief VAD/energy dips on speech
/// do not immediately clip consonants or word endings.
const GATE_CLOSE_HOLD_FRAMES: u32 = 2;

/// Minimum gain floor. Below this the gate snaps to 0.0 to avoid
/// multiplying by near-zero indefinitely.
const GATE_GAIN_FLOOR: f32 = 0.01;

// ---------------------------------------------------------------------------
// Pure gating functions (no RNNoise dependency — fully testable)
// ---------------------------------------------------------------------------

/// RMS of a buffer in nnnoiseless i16-scale.
#[inline]
pub fn rms_i16(buf: &[f32]) -> f32 {
    let sum_sq: f64 = buf.iter().map(|&s| (s as f64) * (s as f64)).sum();
    (sum_sq / buf.len() as f64).sqrt() as f32
}

/// Decide whether a sub-frame should be gated based on VAD probability
/// and post-denoise energy. Returns `true` if the gate should close
/// (i.e. the sub-frame is non-speech / residual noise).
///
/// This is a pure function — no state, no side effects.
#[inline]
pub fn should_gate(vad_prob: f32, frame_rms: f32) -> bool {
    vad_prob < VAD_PROB_THRESHOLD || frame_rms < VAD_GATE_RMS_THRESHOLD
}

/// Smooth gate state. Tracks the current gain level and ramps it
/// toward the target (0.0 or 1.0) using attack/release steps to
/// avoid audible clicks at gate transitions.
#[derive(Debug, Clone)]
pub struct GateState {
    /// Current gain, 0.0 (fully closed) to 1.0 (fully open).
    pub gain: f32,
    /// Consecutive sub-frames requesting closure.
    pub close_hold_frames: u32,
}

impl Default for GateState {
    fn default() -> Self {
        Self::new()
    }
}

impl GateState {
    pub fn new() -> Self {
        Self {
            gain: 1.0,
            close_hold_frames: 0,
        }
    }

    /// Returns the number of sub-frames needed to fully open from
    /// fully closed (release time in sub-frame units).
    pub fn max_release_frames() -> u32 {
        (1.0 / GATE_RELEASE_STEP).ceil() as u32
    }

    /// Advance the smooth gate, including a short hold before closing.
    /// Returns `(start_gain, end_gain)` for applying a ramp to the sub-frame.
    pub fn advance(&mut self, gate_closed: bool) -> (f32, f32) {
        let start_gain = self.gain;
        let effective_closed = if gate_closed {
            self.close_hold_frames = self.close_hold_frames.saturating_add(1);
            // Native-path speech often dips for a single 10 ms RNNoise window.
            // Holding the close decision briefly avoids clipping consonants and
            // short word endings when suppression is otherwise working.
            self.close_hold_frames > GATE_CLOSE_HOLD_FRAMES
        } else {
            self.close_hold_frames = 0;
            false
        };
        self.gain = compute_gate_gain(self.gain, effective_closed);
        (start_gain, self.gain)
    }
}

/// Compute the next gate gain given the current gain and whether the
/// gate should be closed. Returns the new gain value.
///
/// Pure function — takes current gain + gate decision, returns new gain.
/// The caller applies the gain to the audio buffer.
#[inline]
pub fn compute_gate_gain(current_gain: f32, gate_closed: bool) -> f32 {
    if gate_closed {
        let g = current_gain - GATE_ATTACK_STEP;
        if g < GATE_GAIN_FLOOR {
            0.0
        } else {
            g
        }
    } else {
        let g = current_gain + GATE_RELEASE_STEP;
        if g > 1.0 {
            1.0
        } else {
            g
        }
    }
}

/// Apply a gain value to a buffer of samples in-place.
/// gain == 1.0 is a no-op (branch-free fast path).
/// gain == 0.0 zeros the buffer.
#[inline]
pub fn apply_gate_gain(buf: &mut [f32], gain: f32) {
    if gain == 1.0 {
        return;
    }
    if gain == 0.0 {
        buf.fill(0.0);
    } else {
        for s in buf.iter_mut() {
            *s *= gain;
        }
    }
}

/// Apply a linearly ramped gain across a buffer in-place.
///
/// This avoids hard step changes at sub-frame boundaries when the gate gain
/// changes between consecutive 10 ms RNNoise windows.
#[inline]
pub fn apply_gate_gain_ramped(buf: &mut [f32], start_gain: f32, end_gain: f32) {
    if start_gain == end_gain {
        apply_gate_gain(buf, end_gain);
        return;
    }

    let len = buf.len();
    if len == 0 {
        return;
    }

    let denom = (len - 1).max(1) as f32;
    for (i, s) in buf.iter_mut().enumerate() {
        let t = i as f32 / denom;
        let gain = start_gain + (end_gain - start_gain) * t;
        *s *= gain;
    }
}

/// Process a single 480-sample sub-frame through the denoise + gate pipeline
/// in-place.
///
/// This is the shared core used by both `DenoiseFilter::process()` and the
/// equivalence property test. Extracting it ensures the test can never drift
/// from the production gating logic.
///
/// Steps: scale f32→i16-range → `DenoiseState::process_frame` → VAD+energy
/// gate decision → `GateState::advance` → ramped gain → scale i16-range→f32
/// back into `subframe`.
///
/// The gain is applied as a ramp (`apply_gate_gain_ramped`) rather than a flat
/// step. This was the specific change that eliminated the residual "light
/// crackle" observed on the native Linux/Hyprland mic path after the initial
/// gate tuning — a hard gain step at the 10 ms sub-frame boundary was audible
/// even when the gate was working correctly. Do not replace with
/// `apply_gate_gain` without re-validating on that path.
#[inline]
fn process_subframe(
    state: &mut DenoiseState<'_>,
    gate: &mut GateState,
    subframe: &mut [f32],
    tmp: &mut [f32; RNN_FRAME_SAMPLES],
    out: &mut [f32; RNN_FRAME_SAMPLES],
) {
    for (i, s) in subframe.iter().enumerate() {
        tmp[i] = s * I16_SCALE;
    }
    let vad = state.process_frame(out, tmp);
    let closed = should_gate(vad, rms_i16(out));
    let (start_gain, end_gain) = gate.advance(closed);
    apply_gate_gain_ramped(out, start_gain, end_gain);
    for (i, s) in out.iter().enumerate() {
        subframe[i] = s / I16_SCALE;
    }
}

// ---------------------------------------------------------------------------
// DenoiseFilter — wraps nnnoiseless + gating state
// ---------------------------------------------------------------------------

pub struct DenoiseFilter {
    state: Mutex<Box<DenoiseState<'static>>>,
    gate: Mutex<GateState>,
    enabled: AtomicBool,
}

impl DenoiseFilter {
    /// Create a new `DenoiseFilter` with the given initial enabled state.
    ///
    /// Allocates a `DenoiseState` configured for 48 kHz (the default).
    pub fn new(enabled: bool) -> Self {
        Self {
            state: Mutex::new(DenoiseState::new()),
            gate: Mutex::new(GateState::new()),
            enabled: AtomicBool::new(enabled),
        }
    }

    /// Process a 960-sample pipeline frame in-place through nnnoiseless.
    ///
    /// Splits the frame into two consecutive 480-sample slices and runs
    /// `DenoiseState::process_frame()` on each in order. After each
    /// sub-frame, a two-layer gate (VAD + energy) with smooth attack/release
    /// is applied. When disabled, the frame passes through unmodified.
    ///
    /// The pipeline uses f32 samples in [-1.0, 1.0] range, but nnnoiseless
    /// expects the i16 range [-32768.0, 32767.0]. This method handles the
    /// scaling transparently.
    ///
    /// # Internal locking
    ///
    /// This method acquires internal `Mutex`es to access the `DenoiseState`
    /// and `GateState`. In practice only the send loop calls `process()`,
    /// so the locks are uncontended. Do **not** call from multiple threads
    /// concurrently — the `Mutex`es exist solely to satisfy `Sync` for the
    /// `Arc<DenoiseFilter>` sharing pattern (IPC toggle path only touches
    /// the `AtomicBool`).
    pub fn process(&self, frame: &mut [f32]) {
        debug_assert!(
            frame.len() == FRAME_SAMPLES,
            "DenoiseFilter::process expects exactly {} samples, got {}",
            FRAME_SAMPLES,
            frame.len()
        );

        if !self.enabled.load(Ordering::Acquire) {
            return;
        }

        let mut state = self.state.lock().unwrap();
        let mut gate = self.gate.lock().unwrap();
        let mut tmp = [0.0f32; RNN_FRAME_SAMPLES];
        let mut out = [0.0f32; RNN_FRAME_SAMPLES];

        // First half: frame[..480]
        process_subframe(
            &mut state,
            &mut gate,
            &mut frame[..RNN_FRAME_SAMPLES],
            &mut tmp,
            &mut out,
        );

        // Second half: frame[480..960]
        process_subframe(
            &mut state,
            &mut gate,
            &mut frame[RNN_FRAME_SAMPLES..],
            &mut tmp,
            &mut out,
        );
    }

    /// Drop and reconstruct the internal `DenoiseState`, clearing stale
    /// GRU recurrent state. Called by the send loop on a disabled→enabled
    /// transition to avoid artifacts from stale state accumulated while
    /// the filter was bypassed.
    ///
    /// Also resets the gate to fully open so the first frame after
    /// re-enable isn't attenuated by stale gate state.
    pub fn reset_state(&self) {
        let mut state = self.state.lock().unwrap();
        *state = DenoiseState::new();
        let mut gate = self.gate.lock().unwrap();
        *gate = GateState::new();
    }

    /// Set the enabled flag. Uses `Ordering::Release` so that a subsequent
    /// `Acquire` load in `process()` or `is_enabled()` sees the new value.
    pub fn set_enabled(&self, enabled: bool) {
        self.enabled.store(enabled, Ordering::Release);
    }

    /// Read the current enabled flag with `Ordering::Acquire`.
    pub fn is_enabled(&self) -> bool {
        self.enabled.load(Ordering::Acquire)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use proptest::prelude::*;

    /// Bitwise comparison for f32 slices. Uses `to_bits()` so that NaN and
    /// -0.0 are compared by their bit patterns, matching the design's
    /// "bitwise equality" requirement.
    fn bitwise_eq(a: &[f32], b: &[f32]) -> bool {
        a.len() == b.len()
            && a.iter()
                .zip(b.iter())
                .all(|(x, y)| x.to_bits() == y.to_bits())
    }

    /// Compute RMS of a slice of f32 samples ([-1,1] scale).
    fn rms(samples: &[f32]) -> f64 {
        let sum_sq: f64 = samples.iter().map(|&s| (s as f64) * (s as f64)).sum();
        (sum_sq / samples.len() as f64).sqrt()
    }

    // ===================================================================
    // Option C: Post-RNNoise Energy Gate — pure function tests
    // ===================================================================

    // -------------------------------------------------------------------
    // C.1: Silent passthrough
    // For any frame that is all zeros, the gate should output all zeros
    // regardless of threshold setting.
    // -------------------------------------------------------------------
    #[test]
    fn energy_gate_silent_passthrough() {
        let buf = [0.0f32; RNN_FRAME_SAMPLES];
        assert_eq!(rms_i16(&buf), 0.0);
        assert!(
            should_gate(1.0, rms_i16(&buf)),
            "zero-energy frame should be gated"
        );

        // Apply gate gain of 0.0 to a zero buffer — still zero.
        let mut out = buf;
        apply_gate_gain(&mut out, 0.0);
        assert!(out.iter().all(|&s| s == 0.0));
    }

    #[test]
    fn ramped_gate_gain_interpolates_between_endpoints() {
        let mut buf = [1.0f32; 5];
        apply_gate_gain_ramped(&mut buf, 1.0, 0.0);
        assert_eq!(buf[0], 1.0);
        assert_eq!(buf[4], 0.0);
        assert!(buf[1] < buf[0]);
        assert!(buf[2] < buf[1]);
        assert!(buf[3] < buf[2]);
    }

    #[test]
    fn gate_close_hold_delays_attenuation() {
        let mut gate = GateState::new();

        for _ in 0..GATE_CLOSE_HOLD_FRAMES {
            let (start_gain, end_gain) = gate.advance(true);
            assert_eq!(start_gain, 1.0);
            assert_eq!(end_gain, 1.0);
        }

        let (_, end_gain) = gate.advance(true);
        assert!(end_gain < 1.0);
    }

    // -------------------------------------------------------------------
    // C.2: Above-threshold passthrough (property)
    // For any frame whose RMS exceeds the threshold and VAD is high,
    // the gate decision is "open" and gain 1.0 leaves the frame intact.
    // -------------------------------------------------------------------
    proptest! {
        #![proptest_config(ProptestConfig { cases: 200, .. ProptestConfig::default() })]

        #[test]
        fn prop_energy_above_threshold_passthrough(
            amplitude in (VAD_GATE_RMS_THRESHOLD as f64 * 1.5)..30000.0_f64,
        ) {
            // Constant-amplitude frame well above threshold.
            let val = amplitude as f32;
            let buf = [val; RNN_FRAME_SAMPLES];
            let frame_rms = rms_i16(&buf);
            prop_assert!(frame_rms >= VAD_GATE_RMS_THRESHOLD);
            // With high VAD, gate should be open.
            prop_assert!(!should_gate(1.0, frame_rms));
            // Gain 1.0 leaves frame untouched.
            let mut out = buf;
            apply_gate_gain(&mut out, 1.0);
            prop_assert!(bitwise_eq(&out, &buf));
        }
    }

    // -------------------------------------------------------------------
    // C.3: Below-threshold attenuation (property)
    // For any frame whose RMS is well below the threshold (<50%),
    // after the gate ramps down, output RMS < input RMS.
    // -------------------------------------------------------------------
    proptest! {
        #![proptest_config(ProptestConfig { cases: 200, .. ProptestConfig::default() })]

        #[test]
        fn prop_energy_below_threshold_attenuation(
            amplitude in 1.0_f64..(VAD_GATE_RMS_THRESHOLD as f64 * 0.5),
        ) {
            let val = amplitude as f32;
            let buf = [val; RNN_FRAME_SAMPLES];
            let frame_rms = rms_i16(&buf);
            prop_assert!(frame_rms < VAD_GATE_RMS_THRESHOLD);
            // Gate should close.
            prop_assert!(should_gate(1.0, frame_rms));
            // After one attack step from gain=1.0, gain < 1.0.
            let new_gain = compute_gate_gain(1.0, true);
            prop_assert!(new_gain < 1.0);
            let mut out = buf;
            apply_gate_gain(&mut out, new_gain);
            let out_rms = rms_i16(&out);
            prop_assert!(
                out_rms < frame_rms,
                "output RMS ({}) should be less than input RMS ({})",
                out_rms, frame_rms
            );
        }
    }

    // -------------------------------------------------------------------
    // C.4: Monotonicity of attenuation (property)
    // Given two frames at different amplitudes (both below threshold),
    // the quieter frame should be attenuated more aggressively after
    // the same number of gate steps.
    // -------------------------------------------------------------------
    proptest! {
        #![proptest_config(ProptestConfig { cases: 200, .. ProptestConfig::default() })]

        #[test]
        fn prop_energy_gate_monotonicity(
            amp_high in (VAD_GATE_RMS_THRESHOLD as f64 * 0.3)..(VAD_GATE_RMS_THRESHOLD as f64 * 0.5),
            amp_low in 1.0_f64..(VAD_GATE_RMS_THRESHOLD as f64 * 0.3),
        ) {
            // Both below threshold, amp_high > amp_low.
            let buf_high = [amp_high as f32; RNN_FRAME_SAMPLES];
            let buf_low = [amp_low as f32; RNN_FRAME_SAMPLES];

            // Both get gated — run 3 attack steps from gain=1.0.
            let mut gain = 1.0f32;
            for _ in 0..3 {
                gain = compute_gate_gain(gain, true);
            }

            let mut out_high = buf_high;
            let mut out_low = buf_low;
            apply_gate_gain(&mut out_high, gain);
            apply_gate_gain(&mut out_low, gain);

            // Same gain applied to both, so the quieter input produces
            // quieter output — monotonicity of the gain curve.
            let rms_high = rms_i16(&out_high);
            let rms_low = rms_i16(&out_low);
            prop_assert!(
                rms_low <= rms_high,
                "quieter frame ({}) should have <= RMS than louder frame ({})",
                rms_low, rms_high
            );
        }
    }

    // -------------------------------------------------------------------
    // C.5: Smooth transition (no clicks)
    // Generate a sequence of sub-frames where the gate transitions from
    // open to closed. Verify the gain changes smoothly — no jump larger
    // than GATE_ATTACK_STEP between consecutive sub-frames.
    // -------------------------------------------------------------------
    #[test]
    fn energy_gate_smooth_transition_no_clicks() {
        let mut gate = GateState::new();
        let mut gains: Vec<f32> = Vec::new();

        // 5 sub-frames open, then 5 sub-frames closed.
        for i in 0..10 {
            let closed = i >= 5;
            gate.gain = compute_gate_gain(gate.gain, closed);
            gains.push(gate.gain);
        }

        // The gain should never jump more than the step size between
        // consecutive values — this is what prevents audible clicks.
        let max_step = GATE_ATTACK_STEP.max(GATE_RELEASE_STEP);
        for w in gains.windows(2) {
            let delta = (w[1] - w[0]).abs();
            assert!(
                delta <= max_step + f32::EPSILON,
                "gain jumped {} between sub-frames (max allowed {}) — gate is clicking. gains: {:?}",
                delta, max_step, gains
            );
        }

        // Verify the gate is trending closed without abrupt jumps.
        assert!(
            *gains.last().unwrap() < 1.0,
            "gate should attenuate after repeated closed decisions"
        );
    }

    // -------------------------------------------------------------------
    // C.6: Gain recovery
    // After frames below threshold (gate closed), send frames above
    // threshold. The gate should fully open within a bounded number
    // of frames.
    // -------------------------------------------------------------------
    #[test]
    fn energy_gate_gain_recovery() {
        let mut gate = GateState::new();

        // Close the gate fully: run enough attack steps.
        for _ in 0..10 {
            gate.gain = compute_gate_gain(gate.gain, true);
        }
        assert_eq!(gate.gain, 0.0, "gate should be fully closed");

        // Now open: send above-threshold decisions.
        let max_release = GateState::max_release_frames();
        for _ in 0..max_release {
            gate.gain = compute_gate_gain(gate.gain, false);
        }
        assert_eq!(
            gate.gain, 1.0,
            "gate should be fully open after {} release steps",
            max_release
        );
    }

    // ===================================================================
    // Deterministic unit tests for energy gate
    // ===================================================================

    // Exactly at threshold — should be gated (< not <=).
    #[test]
    fn energy_gate_at_exact_threshold() {
        // RMS exactly at threshold: should_gate uses `<`, so exactly at
        // threshold is NOT gated.
        assert!(!should_gate(1.0, VAD_GATE_RMS_THRESHOLD));
        // Just below: gated.
        assert!(should_gate(1.0, VAD_GATE_RMS_THRESHOLD - 0.01));
    }

    // Constant DC offset at low amplitude — verify RMS-based gating.
    #[test]
    fn energy_gate_dc_offset_low_amplitude() {
        let dc = [10.0f32; RNN_FRAME_SAMPLES]; // low constant DC
        let frame_rms = rms_i16(&dc);
        assert!(frame_rms < VAD_GATE_RMS_THRESHOLD);
        assert!(should_gate(1.0, frame_rms), "low DC should be gated");
    }

    // One loud spike but low RMS — verify gate uses RMS not peak.
    #[test]
    fn energy_gate_spike_low_rms() {
        let mut buf = [1.0f32; RNN_FRAME_SAMPLES]; // very low baseline
        buf[0] = 20000.0; // one loud spike
        let frame_rms = rms_i16(&buf);
        // RMS of one spike in 480 samples: sqrt(20000^2 / 480) ≈ 913
        // That's above threshold, so the spike pulls RMS up.
        // This test documents that we use RMS (not peak).
        if frame_rms >= VAD_GATE_RMS_THRESHOLD {
            assert!(
                !should_gate(1.0, frame_rms),
                "high-RMS frame should not be gated"
            );
        } else {
            assert!(should_gate(1.0, frame_rms), "low-RMS frame should be gated");
        }
    }

    // ===================================================================
    // Option A: VAD Probability Gate — model-dependent tests
    // ===================================================================

    // -------------------------------------------------------------------
    // A.1: Zero-input VAD
    // Feed silence for 50+ frames. After convergence, VAD probability
    // should be below the gating threshold for all frames.
    // -------------------------------------------------------------------
    #[test]
    fn vad_gate_silence_produces_low_vad() {
        let mut state = DenoiseState::new();
        let silence = [0.0f32; RNN_FRAME_SAMPLES];
        let mut out = [0.0f32; RNN_FRAME_SAMPLES];

        let convergence_frames = 5;
        let total_frames = 60;
        let mut low_vad_count = 0u32;

        for i in 0..total_frames {
            let vad = state.process_frame(&mut out, &silence);
            if i >= convergence_frames && vad < VAD_PROB_THRESHOLD {
                low_vad_count += 1;
            }
        }

        let tested = (total_frames - convergence_frames) as u32;
        assert_eq!(
            low_vad_count, tested,
            "silence should produce low VAD for all post-convergence frames, got {}/{} low",
            low_vad_count, tested
        );
    }

    // -------------------------------------------------------------------
    // A.2: High-energy speech-like input
    // Feed a synthetic sine wave at speech-like frequency (200Hz).
    // Over a window of frames, VAD probability should be above the
    // gating threshold for most frames.
    // -------------------------------------------------------------------
    #[test]
    fn vad_gate_speech_like_sine_high_vad() {
        let mut state = DenoiseState::new();
        let mut out = [0.0f32; RNN_FRAME_SAMPLES];

        let freq = 200.0f32;
        let sample_rate = 48000.0f32;
        let amplitude = 10000.0f32; // i16-scale

        let total_frames = 60;
        let convergence_frames = 5;
        let mut high_vad_count = 0u32;

        for frame_idx in 0..total_frames {
            let mut buf = [0.0f32; RNN_FRAME_SAMPLES];
            for (i, s) in buf.iter_mut().enumerate() {
                let t = (frame_idx * RNN_FRAME_SAMPLES + i) as f32 / sample_rate;
                *s = amplitude * (2.0 * std::f32::consts::PI * freq * t).sin();
            }
            let vad = state.process_frame(&mut out, &buf);
            if frame_idx >= convergence_frames && vad >= VAD_PROB_THRESHOLD {
                high_vad_count += 1;
            }
        }

        let tested = total_frames - convergence_frames;
        // "Most frames" — at least 50%. RNNoise's VAD on synthetic tones
        // is less reliable than on real speech, so we use a relaxed threshold.
        let threshold = (tested as f64 * 0.5) as u32;
        assert!(
            high_vad_count >= threshold,
            "speech-like sine should produce high VAD for many frames, got {}/{} high (need {})",
            high_vad_count,
            tested,
            threshold
        );
    }

    // -------------------------------------------------------------------
    // A.3: Gate-closed zeroes frame
    // When should_gate returns true, applying gain 0.0 zeroes the frame.
    // -------------------------------------------------------------------
    #[test]
    fn vad_gate_closed_zeroes_frame() {
        // VAD below threshold, energy above — VAD gate triggers.
        assert!(should_gate(0.1, VAD_GATE_RMS_THRESHOLD * 2.0));

        let mut buf = [500.0f32; RNN_FRAME_SAMPLES];
        apply_gate_gain(&mut buf, 0.0);
        assert!(buf.iter().all(|&s| s == 0.0));
    }

    // -------------------------------------------------------------------
    // A.4: Gate-open passthrough
    // When should_gate returns false, gain 1.0 leaves frame untouched.
    // -------------------------------------------------------------------
    #[test]
    fn vad_gate_open_passthrough() {
        // VAD above threshold, energy above threshold — gate open.
        assert!(!should_gate(0.9, VAD_GATE_RMS_THRESHOLD * 2.0));

        let original = [1234.0f32; RNN_FRAME_SAMPLES];
        let mut buf = original;
        apply_gate_gain(&mut buf, 1.0);
        assert!(bitwise_eq(&buf, &original));
    }

    // -------------------------------------------------------------------
    // A.5: VAD probability monotonicity across SNR
    // Noise at fixed level, mix in tone at increasing amplitudes.
    // Average VAD should increase monotonically with SNR.
    // -------------------------------------------------------------------
    #[test]
    fn vad_probability_monotonicity_across_snr() {
        let noise_amplitude = 500.0f32; // i16-scale
                                        // Narrower range — RNNoise VAD can saturate/behave non-linearly
                                        // at very high amplitudes, so we test the useful range.
        let tone_amplitudes = [0.0f32, 500.0, 1500.0, 4000.0, 8000.0];
        let freq = 200.0f32;
        let sample_rate = 48000.0f32;
        let frames_per_level = 40usize;
        let convergence = 5;

        let mut avg_vads: Vec<f64> = Vec::new();

        // Use a simple deterministic noise source.
        for &tone_amp in &tone_amplitudes {
            let mut state = DenoiseState::new();
            let mut out = [0.0f32; RNN_FRAME_SAMPLES];
            let mut vad_sum = 0.0f64;
            let mut rng: u64 = 42;

            for frame_idx in 0..frames_per_level {
                let mut buf = [0.0f32; RNN_FRAME_SAMPLES];
                for (i, s) in buf.iter_mut().enumerate() {
                    // LCG noise
                    rng = rng.wrapping_mul(6364136223846793005).wrapping_add(1);
                    let noise =
                        ((rng >> 33) as f32 / u32::MAX as f32 - 0.5) * 2.0 * noise_amplitude;
                    // Tone
                    let t = (frame_idx * RNN_FRAME_SAMPLES + i) as f32 / sample_rate;
                    let tone = tone_amp * (2.0 * std::f32::consts::PI * freq * t).sin();
                    *s = noise + tone;
                }
                let vad = state.process_frame(&mut out, &buf);
                if frame_idx >= convergence {
                    vad_sum += vad as f64;
                }
            }
            avg_vads.push(vad_sum / (frames_per_level - convergence) as f64);
        }

        // Check weak monotonicity: the overall trend should be increasing.
        // We compare first vs last rather than strict pairwise, since the
        // RNNoise model can have local non-monotonicity.
        assert!(
            avg_vads.last().unwrap() > avg_vads.first().unwrap(),
            "VAD should increase from pure noise to high-SNR tone: first={:.3}, last={:.3}, all={:?}",
            avg_vads.first().unwrap(), avg_vads.last().unwrap(), avg_vads
        );
    }

    // -------------------------------------------------------------------
    // A: VAD threshold decision unit test
    // -------------------------------------------------------------------
    #[test]
    fn vad_gate_threshold_decision() {
        // Below VAD threshold — gated regardless of energy.
        assert!(should_gate(0.0, 10000.0));
        assert!(should_gate(0.49, 10000.0));
        // At VAD threshold — not gated (if energy is also above).
        assert!(!should_gate(0.5, VAD_GATE_RMS_THRESHOLD));
        assert!(!should_gate(1.0, VAD_GATE_RMS_THRESHOLD));
        // Above VAD but below energy — gated.
        assert!(should_gate(0.9, VAD_GATE_RMS_THRESHOLD - 1.0));
    }

    // -------------------------------------------------------------------
    // A: Hysteresis — gate doesn't toggle every frame when VAD hovers
    // near threshold. The smooth gain ramp provides implicit hysteresis.
    // -------------------------------------------------------------------
    #[test]
    fn vad_gate_implicit_hysteresis() {
        let mut gate = GateState::new();
        // Alternate above/below threshold every sub-frame.
        let decisions = [false, true, false, true, false, true, false, true];
        let mut gains: Vec<f32> = Vec::new();

        for &closed in &decisions {
            gate.gain = compute_gate_gain(gate.gain, closed);
            gains.push(gate.gain);
        }

        // The gain should never jump more than GATE_ATTACK_STEP or
        // GATE_RELEASE_STEP between consecutive values.
        let max_step = GATE_ATTACK_STEP.max(GATE_RELEASE_STEP);
        for w in gains.windows(2) {
            let delta = (w[1] - w[0]).abs();
            assert!(
                delta <= max_step + f32::EPSILON,
                "gain jumped {} between frames (max allowed {})",
                delta,
                max_step
            );
        }
    }

    // ===================================================================
    // DenoiseFilter integration tests (existing properties)
    // ===================================================================

    // -------------------------------------------------------------------
    // Property: Bypass Passthrough
    // -------------------------------------------------------------------
    proptest! {
        #![proptest_config(ProptestConfig { cases: 100, .. ProptestConfig::default() })]

        #[test]
        fn prop_bypass_passthrough(
            frame in proptest::collection::vec(proptest::num::f32::ANY, FRAME_SAMPLES..=FRAME_SAMPLES),
        ) {
            let filter = DenoiseFilter::new(false);
            let original = frame.clone();
            let mut buf = frame;
            filter.process(&mut buf);
            prop_assert!(bitwise_eq(&buf, &original), "disabled filter modified the frame");
        }
    }

    // -------------------------------------------------------------------
    // Property: Frame Split Equivalence (with smooth gate)
    // -------------------------------------------------------------------
    proptest! {
        #![proptest_config(ProptestConfig { cases: 100, .. ProptestConfig::default() })]

        #[test]
        fn prop_frame_split_equivalence(
            frame in proptest::collection::vec(
                proptest::num::f32::NORMAL.prop_map(|v| v.clamp(-1.0, 1.0)),
                FRAME_SAMPLES..=FRAME_SAMPLES,
            ),
        ) {
            // Path A: through DenoiseFilter
            let filter = DenoiseFilter::new(true);
            let mut via_filter = frame.clone();
            filter.process(&mut via_filter);

            // Path B: manual 2×480 split with same gating logic.
            let mut state = DenoiseState::new();
            let mut gate = GateState::new();
            let mut via_manual = frame.clone();

            let mut tmp = [0.0f32; RNN_FRAME_SAMPLES];
            let mut out = [0.0f32; RNN_FRAME_SAMPLES];

            // First half [..480]
            process_subframe(&mut state, &mut gate, &mut via_manual[..RNN_FRAME_SAMPLES], &mut tmp, &mut out);

            // Second half [480..960]
            process_subframe(&mut state, &mut gate, &mut via_manual[RNN_FRAME_SAMPLES..], &mut tmp, &mut out);

            prop_assert!(
                bitwise_eq(&via_filter, &via_manual),
                "DenoiseFilter output differs from manual 2×480 split"
            );
        }
    }

    // -------------------------------------------------------------------
    // Property: Toggle Idempotence
    // -------------------------------------------------------------------
    proptest! {
        #![proptest_config(ProptestConfig { cases: 100, .. ProptestConfig::default() })]

        #[test]
        fn prop_toggle_idempotence(
            frame in proptest::collection::vec(
                proptest::num::f32::NORMAL.prop_map(|v| v.clamp(-1.0, 1.0)),
                FRAME_SAMPLES..=FRAME_SAMPLES,
            ),
            enabled in proptest::bool::ANY,
        ) {
            let filter1 = DenoiseFilter::new(enabled);
            filter1.set_enabled(enabled);
            filter1.set_enabled(enabled);
            let mut f1 = frame.clone();
            filter1.process(&mut f1);

            let filter2 = DenoiseFilter::new(enabled);
            filter2.set_enabled(enabled);
            let mut f2 = frame.clone();
            filter2.process(&mut f2);

            prop_assert!(
                bitwise_eq(&f1, &f2),
                "redundant set_enabled({}) produced different output than single set",
                enabled,
            );
        }
    }

    // -------------------------------------------------------------------
    // Property: Noise Reduction Observable
    // -------------------------------------------------------------------
    proptest! {
        #![proptest_config(ProptestConfig { cases: 15, .. ProptestConfig::default() })]

        #[test]
        fn prop_noise_reduction_observable(
            seed in proptest::num::u64::ANY,
            num_frames in 50u32..=80,
        ) {
            use std::collections::hash_map::DefaultHasher;
            use std::hash::{Hash, Hasher};

            let mut rng_state: u64 = {
                let mut h = DefaultHasher::new();
                seed.hash(&mut h);
                h.finish()
            };

            let mut noise_frames: Vec<Vec<f32>> = Vec::with_capacity(num_frames as usize);
            for _ in 0..num_frames {
                let mut frame = vec![0.0f32; FRAME_SAMPLES];
                for sample in frame.iter_mut() {
                    rng_state = rng_state.wrapping_mul(6364136223846793005).wrapping_add(1442695040888963407);
                    let normalized = ((rng_state >> 33) as f64) / (u32::MAX as f64);
                    *sample = ((normalized * 0.6) - 0.3) as f32;
                }
                noise_frames.push(frame);
            }

            let avg_input_rms: f64 = noise_frames.iter()
                .map(|f| rms(f))
                .sum::<f64>() / num_frames as f64;

            let filter = DenoiseFilter::new(true);
            let mut output_frames = noise_frames.clone();
            for frame in output_frames.iter_mut() {
                filter.process(frame);
            }

            let avg_output_rms: f64 = output_frames.iter()
                .map(|f| rms(f))
                .sum::<f64>() / num_frames as f64;

            prop_assert!(
                avg_output_rms < avg_input_rms,
                "Noise reduction not observable: avg_output_rms ({:.6}) >= avg_input_rms ({:.6})",
                avg_output_rms,
                avg_input_rms,
            );
        }
    }

    // -------------------------------------------------------------------
    // Property: APM NS Mutual Exclusion Invariant
    // -------------------------------------------------------------------
    proptest! {
        #![proptest_config(ProptestConfig { cases: 200, .. ProptestConfig::default() })]

        #[test]
        fn prop_apm_ns_mutual_exclusion(
            toggles in proptest::collection::vec(proptest::bool::ANY, 1..=50),
        ) {
            let denoise = DenoiseFilter::new(false);
            let mut apm_ns_enabled = true;
            let mut prev_denoise_enabled = denoise.is_enabled();

            for &toggle_value in &toggles {
                denoise.set_enabled(toggle_value);

                let current_denoise = denoise.is_enabled();
                if current_denoise != prev_denoise_enabled {
                    if current_denoise {
                        apm_ns_enabled = false;
                        denoise.reset_state();
                    } else {
                        apm_ns_enabled = true;
                    }
                    prev_denoise_enabled = current_denoise;
                }

                let denoise_on = denoise.is_enabled();
                prop_assert!(
                    denoise_on ^ apm_ns_enabled,
                    "XOR violated after toggle to {}: denoise={}, apm_ns={}",
                    toggle_value,
                    denoise_on,
                    apm_ns_enabled,
                );
            }
        }
    }
}
