use std::collections::HashSet;
use std::f32::consts::PI;
use std::sync::{Arc, Mutex};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PassthroughFilterSettings {
    pub enabled: bool,
    pub strength: u8,
}

impl Default for PassthroughFilterSettings {
    fn default() -> Self {
        Self {
            enabled: true,
            strength: 50,
        }
    }
}

#[derive(Clone, Debug)]
pub struct PassthroughFilters {
    settings: Arc<Mutex<PassthroughFilterSettings>>,
    passthrough_participants: Arc<Mutex<HashSet<String>>>,
}

impl PassthroughFilters {
    pub fn new() -> Self {
        Self {
            settings: Arc::new(Mutex::new(PassthroughFilterSettings::default())),
            passthrough_participants: Arc::new(Mutex::new(HashSet::new())),
        }
    }

    pub fn set_settings(&self, enabled: bool, strength: u8) {
        *self.settings.lock().unwrap() = PassthroughFilterSettings {
            enabled,
            strength: strength.min(100),
        };
    }

    pub fn settings(&self) -> PassthroughFilterSettings {
        *self.settings.lock().unwrap()
    }

    pub fn set_participant_passthrough(&self, participant_id: &str, enabled: bool) {
        let mut participants = self.passthrough_participants.lock().unwrap();
        if enabled {
            participants.insert(participant_id.to_string());
        } else {
            participants.remove(participant_id);
        }
    }

    pub fn is_participant_passthrough(&self, participant_id: &str) -> bool {
        self.passthrough_participants
            .lock()
            .unwrap()
            .contains(participant_id)
    }
}

impl Default for PassthroughFilters {
    fn default() -> Self {
        Self::new()
    }
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct PassthroughFilterParams {
    pub cutoff_hz: f32,
    pub high_shelf_db: f32,
}

pub fn passthrough_filter_params(strength: u8) -> PassthroughFilterParams {
    let strength = strength.min(100) as f32;
    if strength <= 50.0 {
        let t = strength / 50.0;
        PassthroughFilterParams {
            cutoff_hz: 14_000.0 + (6_000.0 - 14_000.0) * t,
            high_shelf_db: -3.0 * t,
        }
    } else {
        let t = (strength - 50.0) / 50.0;
        PassthroughFilterParams {
            cutoff_hz: 6_000.0 + (4_000.0 - 6_000.0) * t,
            high_shelf_db: -3.0 + (-6.0 + 3.0) * t,
        }
    }
}

#[derive(Debug, Clone)]
pub struct PassthroughBiquadChain {
    low_pass: Biquad,
    high_shelf: Biquad,
    last_settings: Option<PassthroughFilterSettings>,
}

impl PassthroughBiquadChain {
    pub fn new() -> Self {
        Self {
            low_pass: Biquad::identity(),
            high_shelf: Biquad::identity(),
            last_settings: None,
        }
    }

    pub fn process_frame(&mut self, samples: &mut [f32], settings: PassthroughFilterSettings) {
        if !settings.enabled || settings.strength == 0 {
            return;
        }
        if self.last_settings != Some(settings) {
            let params = passthrough_filter_params(settings.strength);
            self.low_pass = Biquad::low_pass(48_000.0, params.cutoff_hz, 0.707);
            self.high_shelf = Biquad::high_shelf(48_000.0, 3_000.0, params.high_shelf_db, 0.707);
            self.last_settings = Some(settings);
        }
        for sample in samples {
            let filtered = self.high_shelf.process(self.low_pass.process(*sample));
            *sample = if filtered.is_finite() { filtered } else { 0.0 };
        }
    }
}

impl Default for PassthroughBiquadChain {
    fn default() -> Self {
        Self::new()
    }
}

#[derive(Debug, Clone)]
struct Biquad {
    b0: f32,
    b1: f32,
    b2: f32,
    a1: f32,
    a2: f32,
    z1: f32,
    z2: f32,
}

impl Biquad {
    fn identity() -> Self {
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

    fn low_pass(sample_rate: f32, cutoff_hz: f32, q: f32) -> Self {
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
        Self::normalize(b0, b1, b2, a0, a1, a2)
    }

    fn high_shelf(sample_rate: f32, frequency_hz: f32, gain_db: f32, slope: f32) -> Self {
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
        Self::normalize(b0, b1, b2, a0, a1, a2)
    }

    fn normalize(b0: f32, b1: f32, b2: f32, a0: f32, a1: f32, a2: f32) -> Self {
        if !a0.is_finite() || a0.abs() < f32::EPSILON {
            return Self::identity();
        }
        Self {
            b0: b0 / a0,
            b1: b1 / a0,
            b2: b2 / a0,
            a1: a1 / a0,
            a2: a2 / a0,
            z1: 0.0,
            z2: 0.0,
        }
    }

    fn process(&mut self, input: f32) -> f32 {
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

    fn sine(freq: f32, samples: usize) -> Vec<f32> {
        (0..samples)
            .map(|i| (2.0 * PI * freq * i as f32 / 48_000.0).sin() * 0.5)
            .collect()
    }

    fn rms(samples: &[f32]) -> f32 {
        (samples.iter().map(|s| s * s).sum::<f32>() / samples.len() as f32).sqrt()
    }

    #[test]
    fn strength_mapping_clamps_and_interpolates() {
        assert_eq!(passthrough_filter_params(0).cutoff_hz, 14_000.0);
        assert_eq!(passthrough_filter_params(50).cutoff_hz, 6_000.0);
        assert_eq!(passthrough_filter_params(100).cutoff_hz, 4_000.0);
        assert_eq!(passthrough_filter_params(u8::MAX).cutoff_hz, 4_000.0);
    }

    #[test]
    fn disabled_filter_is_bypass() {
        let mut chain = PassthroughBiquadChain::new();
        let mut samples = sine(8_000.0, 960);
        let original = samples.clone();
        chain.process_frame(
            &mut samples,
            PassthroughFilterSettings {
                enabled: false,
                strength: 100,
            },
        );
        assert_eq!(samples, original);
    }

    #[test]
    fn high_frequency_is_attenuated_more_than_low_frequency() {
        let mut low_chain = PassthroughBiquadChain::new();
        let mut high_chain = PassthroughBiquadChain::new();
        let mut low = sine(500.0, 4_800);
        let mut high = sine(10_000.0, 4_800);
        let settings = PassthroughFilterSettings {
            enabled: true,
            strength: 100,
        };
        let low_before = rms(&low);
        let high_before = rms(&high);
        low_chain.process_frame(&mut low, settings);
        high_chain.process_frame(&mut high, settings);
        let low_ratio = rms(&low) / low_before;
        let high_ratio = rms(&high) / high_before;
        assert!(high_ratio < low_ratio * 0.75);
    }

    #[test]
    fn output_stays_finite() {
        let mut chain = PassthroughBiquadChain::new();
        let mut samples = vec![0.0, f32::NAN, f32::INFINITY, -0.5, 0.5];
        chain.process_frame(
            &mut samples,
            PassthroughFilterSettings {
                enabled: true,
                strength: 100,
            },
        );
        assert!(samples.iter().all(|sample| sample.is_finite()));
    }
}
