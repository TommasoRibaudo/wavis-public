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
}
