//! Resampler helper using `rubato::SincFixedIn` for high-quality sample rate
//! conversion between device rates and the pipeline's native 48 kHz.
//!
//! Gated behind the `real-backends` feature flag.

use crate::audio_pipeline::{AudioPipelineError, FRAME_SAMPLES};
use rubato::{
    Resampler, SincFixedIn, SincInterpolationParameters, SincInterpolationType, WindowFunction,
};

/// Create a `rubato::SincFixedIn` resampler for converting mono audio
/// from `from_rate` Hz to `to_rate` Hz.
///
/// Uses sinc length 256 with a Blackman-Harris² window as specified
/// by the design document (Requirement 4.5).
pub fn create_resampler(
    from_rate: u32,
    to_rate: u32,
) -> Result<SincFixedIn<f32>, AudioPipelineError> {
    let params = SincInterpolationParameters {
        sinc_len: 256,
        f_cutoff: 0.95,
        oversampling_factor: 256,
        interpolation: SincInterpolationType::Linear,
        window: WindowFunction::BlackmanHarris2,
    };
    SincFixedIn::new(
        to_rate as f64 / from_rate as f64,
        2.0,
        params,
        FRAME_SAMPLES, // 960 frames = 20 ms at 48 kHz
        1,             // mono
    )
    .map_err(|e| AudioPipelineError::ResampleError(e.to_string()))
}

/// Resample a mono f32 buffer using the given resampler.
///
/// Returns the resampled output samples.
pub fn resample(
    resampler: &mut SincFixedIn<f32>,
    input: &[f32],
) -> Result<Vec<f32>, AudioPipelineError> {
    let input_frames = vec![input.to_vec()];
    let output = resampler
        .process(&input_frames, None)
        .map_err(|e| AudioPipelineError::ResampleError(e.to_string()))?;
    Ok(output.into_iter().next().unwrap_or_default())
}

// ===========================================================================
// Tests
// ===========================================================================

#[cfg(test)]
mod tests {
    use super::*;
    use proptest::prelude::*;
    use rubato::Resampler;

    // -----------------------------------------------------------------------
    // Property 10: Resampler output length proportionality
    // **Validates: Requirements 4.2, 4.3**
    //
    // For any input sample rate R ≠ 48000 and a chunk of N input samples,
    // the resampler output length should be approximately N × (48000 / R),
    // within ±1 sample tolerance for rounding.
    // -----------------------------------------------------------------------

    proptest! {
        #![proptest_config(ProptestConfig { cases: 100, .. ProptestConfig::default() })]

        /// Feature: audio-quality-overhaul, Property 10: Resampler output length proportionality
        #[test]
        fn resampler_output_length_proportionality(
            // Pick from common non-48kHz sample rates.
            from_rate in prop_oneof![
                Just(8000u32),
                Just(16000u32),
                Just(22050u32),
                Just(44100u32),
                Just(96000u32),
            ],
        ) {
            let to_rate = 48000u32;
            let mut resampler = create_resampler(from_rate, to_rate).unwrap();

            // Warm up the resampler: the sinc filter has an internal delay
            // (reported by output_delay()) that causes the first few calls
            // to produce fewer output samples. Process warmup chunks first.
            let warmup_chunks = 3;
            let input_len = resampler.input_frames_next();
            for _ in 0..warmup_chunks {
                let warmup_input = vec![vec![0.0f32; input_len]];
                let _ = resampler.process(&warmup_input, None).unwrap();
            }

            // Now measure steady-state output length.
            let input = vec![vec![0.0f32; input_len]];
            let output = resampler.process(&input, None).unwrap();
            let output_len = output[0].len();

            let expected = (input_len as f64 * (to_rate as f64 / from_rate as f64)).round() as usize;

            // Allow ±2 sample tolerance for rounding.
            let diff = (output_len as isize - expected as isize).unsigned_abs();
            prop_assert!(
                diff <= 2,
                "from_rate={}, input_len={}, output_len={}, expected≈{}, diff={}",
                from_rate, input_len, output_len, expected, diff
            );
        }
    }

    // -----------------------------------------------------------------------
    // P2-6: round-trip tone fidelity (48kHz -> 44.1kHz -> 48kHz)
    // -----------------------------------------------------------------------

    /// Feed `input` through `resampler` in fixed-size chunks (as `resample()`
    /// requires) and concatenate the output. Drops a trailing partial chunk.
    fn process_all(resampler: &mut SincFixedIn<f32>, input: &[f32]) -> Vec<f32> {
        let chunk = resampler.input_frames_next();
        let mut out = Vec::new();
        let mut offset = 0;
        while offset + chunk <= input.len() {
            out.extend(resample(resampler, &input[offset..offset + chunk]).unwrap());
            offset += chunk;
        }
        out
    }

    /// Pearson correlation coefficient between two equal-length-or-truncated
    /// signals. 1.0 = perfectly correlated shape, 0.0 = uncorrelated.
    fn pearson_correlation(a: &[f32], b: &[f32]) -> f64 {
        let n = a.len().min(b.len());
        let mean_a: f64 = a[..n].iter().map(|&v| v as f64).sum::<f64>() / n as f64;
        let mean_b: f64 = b[..n].iter().map(|&v| v as f64).sum::<f64>() / n as f64;

        let (mut num, mut den_a, mut den_b) = (0.0f64, 0.0f64, 0.0f64);
        for i in 0..n {
            let da = a[i] as f64 - mean_a;
            let db = b[i] as f64 - mean_b;
            num += da * db;
            den_a += da * da;
            den_b += db * db;
        }
        if den_a <= 0.0 || den_b <= 0.0 {
            return 0.0;
        }
        num / (den_a.sqrt() * den_b.sqrt())
    }

    /// A single down+up conversion round trip should reproduce a steady tone
    /// almost exactly: same frequency/shape, near-identical amplitude, just
    /// shifted by the sinc filters' combined (unknown but small) group delay.
    /// This exercises the full resample() call path end to end, unlike the
    /// proportionality property above which only checks output length.
    #[test]
    fn resample_roundtrip_preserves_tone_fidelity() {
        let source_rate = 48_000u32;
        let intermediate_rate = 44_100u32;
        let freq_hz = 440.0f64;
        let total_samples = source_rate as usize / 2; // 500ms

        let input: Vec<f32> = (0..total_samples)
            .map(|i| {
                let t = i as f64 / source_rate as f64;
                (2.0 * std::f64::consts::PI * freq_hz * t).sin() as f32
            })
            .collect();

        let mut down = create_resampler(source_rate, intermediate_rate).unwrap();
        let downsampled = process_all(&mut down, &input);

        let mut up = create_resampler(intermediate_rate, source_rate).unwrap();
        let roundtrip = process_all(&mut up, &downsampled);

        // Skip a generous margin at the head to clear both filters' transient
        // response, then compare a steady-state window against the original.
        let skip = source_rate as usize / 8; // 125ms — far more than the sinc
                                             // filters' combined group delay.
        let compare_len = source_rate as usize / 8; // 125ms comparison window
        assert!(
            skip + compare_len <= input.len() && skip + compare_len <= roundtrip.len(),
            "test signal too short for the configured skip/compare window"
        );
        let reference = &input[skip..skip + compare_len];

        // The two-stage resample introduces an unknown but small integer
        // sample delay; search a generous window for the best alignment
        // rather than computing the exact combined filter group delay.
        let search_radius: isize = 1000;
        let mut best_corr = f64::MIN;
        let mut best_lag: isize = 0;
        for lag in -search_radius..=search_radius {
            let start = skip as isize + lag;
            if start < 0 {
                continue;
            }
            let start = start as usize;
            if start + compare_len > roundtrip.len() {
                continue;
            }
            let corr = pearson_correlation(reference, &roundtrip[start..start + compare_len]);
            if corr > best_corr {
                best_corr = corr;
                best_lag = lag;
            }
        }

        // A 256-tap sinc filter with a Blackman-Harris2 window should
        // reproduce a single steady tone almost losslessly after a down+up
        // round trip. 0.98 leaves headroom for stopband ripple/rounding
        // while still failing on a broken resampler (wrong ratio, aliasing,
        // dropped/duplicated samples, silence).
        assert!(
            best_corr > 0.98,
            "round-trip tone correlation too low: {best_corr} (best lag {best_lag} samples)"
        );

        // Correlation alone is scale-invariant (it would pass even if the
        // signal were, say, halved in amplitude), so also check the
        // round trip didn't materially change the signal's energy.
        let best_start = (skip as isize + best_lag) as usize;
        let aligned = &roundtrip[best_start..best_start + compare_len];
        let rms = |s: &[f32]| -> f64 {
            (s.iter().map(|&v| (v as f64).powi(2)).sum::<f64>() / s.len() as f64).sqrt()
        };
        let rms_ratio = rms(aligned) / rms(reference);
        assert!(
            (0.85..=1.15).contains(&rms_ratio),
            "round-trip RMS amplitude drifted too far: ratio={rms_ratio}"
        );
    }
}
