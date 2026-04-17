//! Lightweight audio metering: RMS, peak, and clipped-sample counters.
//!
//! Designed for real-time use — no allocations, no locks, just atomic counters
//! and simple f32 math. Place meters at capture output, after processing,
//! and before playback to diagnose signal-chain issues.

use std::sync::atomic::{AtomicU64, Ordering};

/// Atomic audio meter that tracks RMS, peak, and clipping events.
///
/// All counters are lock-free and can be read from any thread.
/// Call `analyze` from the audio path, and `snapshot` + `reset`
/// from a monitoring thread.
pub struct AudioMeter {
    /// Label for logging (e.g. "capture", "post-apm", "pre-playback").
    pub label: &'static str,
    /// Sum of squared samples (for RMS). Stored as u64 bits of f64.
    sum_sq: AtomicU64,
    /// Number of samples accumulated since last reset.
    sample_count: AtomicU64,
    /// Peak absolute sample value. Stored as u64 bits of f32.
    peak: AtomicU64,
    /// Number of samples that hit or exceeded 1.0 (clipping).
    clipped: AtomicU64,
    /// Total frames analyzed since last reset.
    frame_count: AtomicU64,
}

/// Snapshot of meter readings, safe to log or display.
#[derive(Debug, Clone)]
pub struct MeterSnapshot {
    pub label: &'static str,
    pub rms: f32,
    pub peak: f32,
    pub clipped_samples: u64,
    pub frames: u64,
}

impl AudioMeter {
    /// Create a new meter with the given label.
    pub const fn new(label: &'static str) -> Self {
        Self {
            label,
            sum_sq: AtomicU64::new(0),
            sample_count: AtomicU64::new(0),
            peak: AtomicU64::new(0),
            clipped: AtomicU64::new(0),
            frame_count: AtomicU64::new(0),
        }
    }

    /// Analyze a buffer of f32 samples. Call this from the audio path.
    /// No allocations, no locks — just arithmetic and atomics.
    pub fn analyze(&self, samples: &[f32]) {
        let mut local_sum_sq: f64 = 0.0;
        let mut local_peak: f32 = 0.0;
        let mut local_clipped: u64 = 0;

        for &s in samples {
            let abs = s.abs();
            local_sum_sq += (abs as f64) * (abs as f64);
            if abs > local_peak {
                local_peak = abs;
            }
            if abs >= 1.0 {
                local_clipped += 1;
            }
        }

        // Accumulate into atomics (relaxed ordering is fine for counters).
        let prev_sum = f64::from_bits(self.sum_sq.load(Ordering::Relaxed));
        self.sum_sq
            .store((prev_sum + local_sum_sq).to_bits(), Ordering::Relaxed);
        self.sample_count
            .fetch_add(samples.len() as u64, Ordering::Relaxed);

        // Update peak (max).
        let prev_peak = f32::from_bits(self.peak.load(Ordering::Relaxed) as u32);
        if local_peak > prev_peak {
            self.peak
                .store(local_peak.to_bits() as u64, Ordering::Relaxed);
        }

        self.clipped.fetch_add(local_clipped, Ordering::Relaxed);
        self.frame_count.fetch_add(1, Ordering::Relaxed);
    }

    /// Take a snapshot of current readings.
    pub fn snapshot(&self) -> MeterSnapshot {
        let sum_sq = f64::from_bits(self.sum_sq.load(Ordering::Relaxed));
        let count = self.sample_count.load(Ordering::Relaxed);
        let peak = f32::from_bits(self.peak.load(Ordering::Relaxed) as u32);
        let clipped = self.clipped.load(Ordering::Relaxed);
        let frames = self.frame_count.load(Ordering::Relaxed);

        let rms = if count > 0 {
            (sum_sq / count as f64).sqrt() as f32
        } else {
            0.0
        };

        MeterSnapshot {
            label: self.label,
            rms,
            peak,
            clipped_samples: clipped,
            frames,
        }
    }

    /// Reset all counters. Call from the monitoring thread after logging.
    pub fn reset(&self) {
        self.sum_sq.store(0, Ordering::Relaxed);
        self.sample_count.store(0, Ordering::Relaxed);
        self.peak.store(0, Ordering::Relaxed);
        self.clipped.store(0, Ordering::Relaxed);
        self.frame_count.store(0, Ordering::Relaxed);
    }
}

impl std::fmt::Display for MeterSnapshot {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "[{}] RMS={:.4} peak={:.4} clipped={} frames={}",
            self.label, self.rms, self.peak, self.clipped_samples, self.frames
        )
    }
}

// ===========================================================================
// Tests
// ===========================================================================

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn silence_produces_zero_rms_and_peak() {
        let meter = AudioMeter::new("test");
        let silence = vec![0.0f32; 960];
        meter.analyze(&silence);
        let snap = meter.snapshot();
        assert_eq!(snap.rms, 0.0);
        assert_eq!(snap.peak, 0.0);
        assert_eq!(snap.clipped_samples, 0);
        assert_eq!(snap.frames, 1);
    }

    #[test]
    fn full_scale_sine_reports_clipping_at_one() {
        let meter = AudioMeter::new("test");
        // Samples at exactly 1.0 count as clipped.
        let samples = vec![1.0f32; 480];
        meter.analyze(&samples);
        let snap = meter.snapshot();
        assert_eq!(snap.clipped_samples, 480);
        assert!((snap.peak - 1.0).abs() < f32::EPSILON);
        assert!((snap.rms - 1.0).abs() < 0.001);
    }

    #[test]
    fn half_amplitude_sine_rms() {
        let meter = AudioMeter::new("test");
        let samples: Vec<f32> = (0..48000)
            .map(|i| 0.5 * (2.0 * std::f32::consts::PI * 440.0 * i as f32 / 48000.0).sin())
            .collect();
        meter.analyze(&samples);
        let snap = meter.snapshot();
        // RMS of a sine wave = amplitude / sqrt(2) ≈ 0.5 / 1.414 ≈ 0.354
        assert!((snap.rms - 0.354).abs() < 0.01, "RMS was {}", snap.rms);
        assert!(snap.peak <= 0.501);
        assert_eq!(snap.clipped_samples, 0);
    }

    #[test]
    fn reset_clears_all_counters() {
        let meter = AudioMeter::new("test");
        meter.analyze(&[0.5, -0.5, 1.0]);
        meter.reset();
        let snap = meter.snapshot();
        assert_eq!(snap.rms, 0.0);
        assert_eq!(snap.peak, 0.0);
        assert_eq!(snap.clipped_samples, 0);
        assert_eq!(snap.frames, 0);
    }
}
