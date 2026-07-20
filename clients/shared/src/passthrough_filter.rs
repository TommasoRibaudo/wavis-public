use crate::biquad::Biquad;
use std::collections::HashSet;
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

#[cfg(test)]
mod tests {
    use super::*;
    use std::f32::consts::PI;

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
