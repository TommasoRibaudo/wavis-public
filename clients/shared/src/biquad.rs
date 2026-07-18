//! Shared RBJ biquad filter primitive used by DSP stages that need cheap,
//! stable low-pass/high-pass/high-shelf filtering (passthrough tone shaping,
//! de-essing). Coefficients can be retuned in place without resetting the
//! filter's internal state, so callers that change parameters every frame
//! (e.g. a dynamic shelf gain) don't introduce clicks.

use std::f32::consts::PI;

#[derive(Debug, Clone)]
pub(crate) struct Biquad {
    b0: f32,
    b1: f32,
    b2: f32,
    a1: f32,
    a2: f32,
    z1: f32,
    z2: f32,
}

impl Biquad {
    pub(crate) fn identity() -> Self {
        Self {
            b0: 1.0,
            b1: 0.0,
            b2: 0.0,
            a1: 0.0,
            a2: 0.0,
            z1: 0.0,
            z2: 0.0,
        }
    }

    pub(crate) fn low_pass(sample_rate: f32, cutoff_hz: f32, q: f32) -> Self {
        let mut biquad = Self::identity();
        biquad.retune_low_pass(sample_rate, cutoff_hz, q);
        biquad
    }

    pub(crate) fn high_pass(sample_rate: f32, cutoff_hz: f32, q: f32) -> Self {
        let mut biquad = Self::identity();
        biquad.retune_high_pass(sample_rate, cutoff_hz, q);
        biquad
    }

    pub(crate) fn high_shelf(
        sample_rate: f32,
        frequency_hz: f32,
        gain_db: f32,
        slope: f32,
    ) -> Self {
        let mut biquad = Self::identity();
        biquad.retune_high_shelf(sample_rate, frequency_hz, gain_db, slope);
        biquad
    }

    pub(crate) fn retune_low_pass(&mut self, sample_rate: f32, cutoff_hz: f32, q: f32) {
        let cutoff = cutoff_hz.clamp(20.0, sample_rate * 0.45);
        let omega = 2.0 * PI * cutoff / sample_rate;
        let sin = omega.sin();
        let cos = omega.cos();
        let alpha = sin / (2.0 * q.max(0.001));
        let b0 = (1.0 - cos) * 0.5;
        let b1 = 1.0 - cos;
        let b2 = (1.0 - cos) * 0.5;
        let a0 = 1.0 + alpha;
        let a1 = -2.0 * cos;
        let a2 = 1.0 - alpha;
        self.set_normalized(b0, b1, b2, a0, a1, a2);
    }

    pub(crate) fn retune_high_pass(&mut self, sample_rate: f32, cutoff_hz: f32, q: f32) {
        let cutoff = cutoff_hz.clamp(20.0, sample_rate * 0.45);
        let omega = 2.0 * PI * cutoff / sample_rate;
        let sin = omega.sin();
        let cos = omega.cos();
        let alpha = sin / (2.0 * q.max(0.001));
        let b0 = (1.0 + cos) * 0.5;
        let b1 = -(1.0 + cos);
        let b2 = (1.0 + cos) * 0.5;
        let a0 = 1.0 + alpha;
        let a1 = -2.0 * cos;
        let a2 = 1.0 - alpha;
        self.set_normalized(b0, b1, b2, a0, a1, a2);
    }

    pub(crate) fn retune_high_shelf(
        &mut self,
        sample_rate: f32,
        frequency_hz: f32,
        gain_db: f32,
        slope: f32,
    ) {
        let frequency = frequency_hz.clamp(20.0, sample_rate * 0.45);
        let a = 10.0_f32.powf(gain_db / 40.0);
        let omega = 2.0 * PI * frequency / sample_rate;
        let sin = omega.sin();
        let cos = omega.cos();
        let sqrt_a = a.sqrt();
        let alpha = (sin / 2.0) * ((a + 1.0 / a) * (1.0 / slope.max(0.001) - 1.0) + 2.0).sqrt();

        let b0 = a * ((a + 1.0) + (a - 1.0) * cos + 2.0 * sqrt_a * alpha);
        let b1 = -2.0 * a * ((a - 1.0) + (a + 1.0) * cos);
        let b2 = a * ((a + 1.0) + (a - 1.0) * cos - 2.0 * sqrt_a * alpha);
        let a0 = (a + 1.0) - (a - 1.0) * cos + 2.0 * sqrt_a * alpha;
        let a1 = 2.0 * ((a - 1.0) - (a + 1.0) * cos);
        let a2 = (a + 1.0) - (a - 1.0) * cos - 2.0 * sqrt_a * alpha;
        self.set_normalized(b0, b1, b2, a0, a1, a2);
    }

    fn set_normalized(&mut self, b0: f32, b1: f32, b2: f32, a0: f32, a1: f32, a2: f32) {
        if !a0.is_finite() || a0.abs() < f32::EPSILON {
            self.b0 = 1.0;
            self.b1 = 0.0;
            self.b2 = 0.0;
            self.a1 = 0.0;
            self.a2 = 0.0;
            return;
        }
        self.b0 = b0 / a0;
        self.b1 = b1 / a0;
        self.b2 = b2 / a0;
        self.a1 = a1 / a0;
        self.a2 = a2 / a0;
    }

    pub(crate) fn process(&mut self, input: f32) -> f32 {
        let input = if input.is_finite() { input } else { 0.0 };
        let output = input * self.b0 + self.z1;
        self.z1 = input * self.b1 + self.z2 - self.a1 * output;
        self.z2 = input * self.b2 - self.a2 * output;
        output
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sine(freq: f32, samples: usize, sample_rate: f32) -> Vec<f32> {
        (0..samples)
            .map(|i| (2.0 * PI * freq * i as f32 / sample_rate).sin() * 0.5)
            .collect()
    }

    fn rms(samples: &[f32]) -> f32 {
        (samples.iter().map(|s| s * s).sum::<f32>() / samples.len() as f32).sqrt()
    }

    #[test]
    fn retune_high_shelf_preserves_state_across_gain_changes() {
        // Retuning mid-stream must not reset z1/z2 -- otherwise a per-frame
        // gain update (as used by the de-esser) would click every frame.
        let mut biquad = Biquad::high_shelf(48_000.0, 5_000.0, -6.0, 0.707);
        let warm = sine(8_000.0, 200, 48_000.0);
        for s in &warm {
            biquad.process(*s);
        }
        let before_state = (biquad.z1, biquad.z2);
        biquad.retune_high_shelf(48_000.0, 5_000.0, -3.0, 0.707);
        assert_eq!((biquad.z1, biquad.z2), before_state);
    }

    #[test]
    fn high_pass_attenuates_low_frequency_more_than_high() {
        let mut biquad_low = Biquad::high_pass(48_000.0, 5_000.0, 0.707);
        let mut biquad_high = Biquad::high_pass(48_000.0, 5_000.0, 0.707);
        let low: Vec<f32> = sine(300.0, 4_800, 48_000.0)
            .into_iter()
            .map(|s| biquad_low.process(s))
            .collect();
        let high: Vec<f32> = sine(8_000.0, 4_800, 48_000.0)
            .into_iter()
            .map(|s| biquad_high.process(s))
            .collect();
        assert!(rms(&low) < rms(&high) * 0.1);
    }

    #[test]
    fn zero_gain_high_shelf_is_near_unity() {
        let mut biquad = Biquad::high_shelf(48_000.0, 5_000.0, 0.0, 0.707);
        let input = sine(8_000.0, 4_800, 48_000.0);
        let output: Vec<f32> = input.iter().map(|s| biquad.process(*s)).collect();
        assert!((rms(&output) - rms(&input)).abs() < 0.01);
    }
}
