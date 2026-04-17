/// Mixes multiple PCM audio tracks into a single output buffer.
///
/// Zeros the output buffer, sums samples across all tracks, and clamps
/// each output sample to [-1.0, 1.0]. Tracks shorter than the output
/// buffer only contribute up to their length.
///
/// This is a pure function — no state, no I/O.
pub fn mix_audio_tracks(tracks: &[&[f32]], output: &mut [f32]) {
    // Zero the output buffer first
    for sample in output.iter_mut() {
        *sample = 0.0;
    }

    // Sum PCM samples across all tracks
    for track in tracks {
        let len = track.len().min(output.len());
        for i in 0..len {
            output[i] += track[i];
        }
    }

    // Clamp each output sample to [-1.0, 1.0]
    for sample in output.iter_mut() {
        *sample = sample.clamp(-1.0, 1.0);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use proptest::prelude::*;

    // Unit tests

    #[test]
    fn test_empty_tracks_zeroes_output() {
        let mut output = [0.5f32; 4];
        mix_audio_tracks(&[], &mut output);
        assert_eq!(output, [0.0f32; 4]);
    }

    #[test]
    fn test_single_track_passthrough() {
        let track = [0.1f32, 0.5, -0.3, 0.8];
        let mut output = [0.0f32; 4];
        mix_audio_tracks(&[&track], &mut output);
        for (a, b) in output.iter().zip(track.iter()) {
            assert!((a - b).abs() < 1e-6, "expected {b}, got {a}");
        }
    }

    #[test]
    fn test_clamping_positive() {
        let a = [0.8f32; 4];
        let b = [0.8f32; 4];
        let mut output = [0.0f32; 4];
        mix_audio_tracks(&[&a, &b], &mut output);
        // 0.8 + 0.8 = 1.6, clamped to 1.0
        assert_eq!(output, [1.0f32; 4]);
    }

    #[test]
    fn test_clamping_negative() {
        let a = [-0.8f32; 4];
        let b = [-0.8f32; 4];
        let mut output = [0.0f32; 4];
        mix_audio_tracks(&[&a, &b], &mut output);
        // -0.8 + -0.8 = -1.6, clamped to -1.0
        assert_eq!(output, [-1.0f32; 4]);
    }

    #[test]
    fn test_shorter_track_only_contributes_to_its_length() {
        let long_track = [0.5f32; 8];
        let short_track = [0.3f32; 4];
        let mut output = [0.0f32; 8];
        mix_audio_tracks(&[&long_track, &short_track], &mut output);
        // First 4 samples: 0.5 + 0.3 = 0.8
        for &s in &output[..4] {
            assert!((s - 0.8).abs() < 1e-6, "expected 0.8, got {s}");
        }
        // Last 4 samples: only long_track contributes = 0.5
        for &s in &output[4..] {
            assert!((s - 0.5).abs() < 1e-6, "expected 0.5, got {s}");
        }
    }

    // Property 10: Audio mixing produces clamped sum
    // Feature: sfu-multi-party-voice, Property 10: Audio mixing produces clamped sum
    // Validates: Requirements 3.7

    /// Strategy: generate 0–5 PCM buffers of equal length, with f32 values in [-2.0, 2.0]
    fn pcm_buffers_strategy() -> impl Strategy<Value = Vec<Vec<f32>>> {
        let buf_len = 1usize..=64usize;
        buf_len.prop_flat_map(|len| {
            let n_tracks = 0usize..=5usize;
            n_tracks.prop_flat_map(move |n| {
                proptest::collection::vec(proptest::collection::vec(-2.0f32..=2.0f32, len), n)
            })
        })
    }

    proptest! {
        /// Property 10: Audio mixing produces clamped sum
        ///
        /// For any set of N PCM buffers (0 ≤ N ≤ 5) of equal length,
        /// `mix_audio_tracks` output equals the sum of corresponding input
        /// samples clamped to [-1.0, 1.0], and output never contains values
        /// outside [-1.0, 1.0].
        ///
        /// Tag: Feature: sfu-multi-party-voice, Property 10: Audio mixing produces clamped sum
        /// Validates: Requirements 3.7
        #[test]
        fn prop_audio_mixing_produces_clamped_sum(buffers in pcm_buffers_strategy()) {
            if buffers.is_empty() {
                // No tracks: output should be all zeros
                let mut output = vec![0.5f32; 8];
                mix_audio_tracks(&[], &mut output);
                for &s in &output {
                    prop_assert_eq!(s, 0.0);
                }
                return Ok(());
            }

            let buf_len = buffers[0].len();
            let track_refs: Vec<&[f32]> = buffers.iter().map(|v| v.as_slice()).collect();
            let mut output = vec![0.0f32; buf_len];

            mix_audio_tracks(&track_refs, &mut output);

            for i in 0..buf_len {
                // Compute expected: sum of all tracks at index i, clamped
                let sum: f32 = buffers.iter().map(|t| t[i]).sum();
                let expected = sum.clamp(-1.0, 1.0);

                // Output must equal clamped sum
                prop_assert!(
                    (output[i] - expected).abs() < 1e-5,
                    "sample[{i}]: expected {expected}, got {}",
                    output[i]
                );

                // Output must always be in [-1.0, 1.0]
                prop_assert!(
                    output[i] >= -1.0 && output[i] <= 1.0,
                    "sample[{i}] out of range: {}",
                    output[i]
                );
            }
        }
    }
}
