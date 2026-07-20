//! Dynamic de-esser for the mic capture path (#286).
//!
//! Consumer mics often reproduce sibilant consonants ("S"/"T") and other
//! high-pitched transients well hotter than the rest of the voice signal,
//! which is uncomfortable to listen to even at a normal playback volume.
//! This applies a high-shelf cut above [`SHELF_HZ`] whose depth tracks how
//! much energy is currently detected in that band: normal speech passes
//! through close to untouched, sibilant bursts get pulled down. Coefficients
//! are retuned in place each frame (not rebuilt), so the filter state
//! carries over and gain changes don't click.

use crate::biquad::Biquad;

const SAMPLE_RATE_HZ: f32 = 48_000.0;
const DETECT_HZ: f32 = 5_000.0;
const SHELF_HZ: f32 = 5_000.0;
const SHELF_Q: f32 = 0.707;
/// Detector RMS (post high-pass) above which the shelf starts cutting.
const ENGAGE_RMS: f32 = 0.05;
/// Deepest cut the shelf will apply, regardless of how hot the sibilance is.
const MAX_CUT_DB: f32 = -14.0;
/// How many dB of cut are added per unit of RMS above `ENGAGE_RMS`.
const CUT_DB_PER_RMS: f32 = 60.0;
/// One-pole smoothing applied to the gain each 20ms frame (~50-60ms time
/// constant) so the shelf eases in/out instead of snapping.
const GAIN_SMOOTHING: f32 = 0.35;

pub struct DeesserFilter {
    detector: Biquad,
    shelf: Biquad,
    gain_db: f32,
}

impl DeesserFilter {
    pub fn new() -> Self {
        Self {
            detector: Biquad::high_pass(SAMPLE_RATE_HZ, DETECT_HZ, SHELF_Q),
            shelf: Biquad::high_shelf(SAMPLE_RATE_HZ, SHELF_HZ, 0.0, SHELF_Q),
            gain_db: 0.0,
        }
    }

    /// Process a frame in place. Called once per 20ms (960-sample) capture
    /// frame in production, but any non-empty length works.
    pub fn process(&mut self, frame: &mut [f32]) {
        if frame.is_empty() {
            return;
        }

        let mut sum_sq = 0.0f32;
        for &sample in frame.iter() {
            let sample = if sample.is_finite() { sample } else { 0.0 };
            let detected = self.detector.process(sample);
            sum_sq += detected * detected;
        }
        let detect_rms = (sum_sq / frame.len() as f32).sqrt();

        let target_db = if detect_rms > ENGAGE_RMS {
            (-(detect_rms - ENGAGE_RMS) * CUT_DB_PER_RMS).max(MAX_CUT_DB)
        } else {
            0.0
        };
        self.gain_db += (target_db - self.gain_db) * GAIN_SMOOTHING;
        self.shelf
            .retune_high_shelf(SAMPLE_RATE_HZ, SHELF_HZ, self.gain_db, SHELF_Q);

        for sample in frame.iter_mut() {
            let input = if sample.is_finite() { *sample } else { 0.0 };
            let output = self.shelf.process(input);
            *sample = if output.is_finite() { output } else { 0.0 };
        }
    }
}

impl Default for DeesserFilter {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::f32::consts::PI;

    const FRAME_SAMPLES: usize = 960; // 20ms @ 48kHz, matches production frames

    fn sine(freq: f32, amplitude: f32, samples: usize) -> Vec<f32> {
        (0..samples)
            .map(|i| (2.0 * PI * freq * i as f32 / SAMPLE_RATE_HZ).sin() * amplitude)
            .collect()
    }

    fn rms(samples: &[f32]) -> f32 {
        (samples.iter().map(|s| s * s).sum::<f32>() / samples.len() as f32).sqrt()
    }

    /// Feeds a signal through the filter for several 20ms frames so the
    /// smoothed gain has time to converge, returning the last frame's output.
    fn settle(
        filter: &mut DeesserFilter,
        freq: f32,
        amplitude: f32,
        frame_count: usize,
    ) -> Vec<f32> {
        let mut last = Vec::new();
        for _ in 0..frame_count {
            let mut frame = sine(freq, amplitude, FRAME_SAMPLES);
            filter.process(&mut frame);
            last = frame;
        }
        last
    }

    #[test]
    fn loud_sibilant_hiss_is_attenuated() {
        // 7.5kHz stands in for "S"/"T" sibilance energy.
        let mut filter = DeesserFilter::new();
        let input_rms = rms(&sine(7_500.0, 0.5, FRAME_SAMPLES));
        let settled = settle(&mut filter, 7_500.0, 0.5, 15);
        let output_rms = rms(&settled);
        assert!(
            output_rms < input_rms * 0.7,
            "expected sibilant band to be cut, input_rms={input_rms} output_rms={output_rms}"
        );
    }

    #[test]
    fn normal_voiced_speech_passes_through_near_unchanged() {
        // 300Hz stands in for a normal vowel, well below the detect band,
        // even at a loud amplitude -- must not get pulled down.
        let mut filter = DeesserFilter::new();
        let input_rms = rms(&sine(300.0, 0.5, FRAME_SAMPLES));
        let settled = settle(&mut filter, 300.0, 0.5, 15);
        let output_rms = rms(&settled);
        assert!(
            (output_rms - input_rms).abs() < input_rms * 0.05,
            "expected voiced speech untouched, input_rms={input_rms} output_rms={output_rms}"
        );
    }

    #[test]
    fn quiet_high_frequency_content_is_not_cut() {
        // Quiet high-frequency air/room tone shouldn't trip the de-esser.
        let mut filter = DeesserFilter::new();
        let input_rms = rms(&sine(7_500.0, 0.02, FRAME_SAMPLES));
        let settled = settle(&mut filter, 7_500.0, 0.02, 15);
        let output_rms = rms(&settled);
        assert!(
            (output_rms - input_rms).abs() < input_rms * 0.1,
            "expected quiet high content untouched, input_rms={input_rms} output_rms={output_rms}"
        );
    }

    #[test]
    fn output_stays_finite_with_non_finite_input() {
        let mut filter = DeesserFilter::new();
        let mut frame = vec![0.0f32, f32::NAN, f32::INFINITY, -0.5, 0.5];
        filter.process(&mut frame);
        assert!(frame.iter().all(|s| s.is_finite()));
    }

    #[test]
    fn empty_frame_is_a_no_op() {
        let mut filter = DeesserFilter::new();
        let mut frame: Vec<f32> = Vec::new();
        filter.process(&mut frame);
        assert!(frame.is_empty());
    }
}
