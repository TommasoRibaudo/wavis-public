//! Remote audio mixing and local capture pipeline for the LiveKit SFU path.
//!
//! Provides [`run_mix_task`] (20ms mixing loop), [`run_participant_audio_decoder`]
//! (per-participant PCM decoder), and [`convert_audio_frame`] (normalises arbitrary
//! PCM → mono 48kHz f32). All items are `pub(super)`.

use livekit::track::TrackSource;
use livekit::webrtc::audio_stream::native::NativeAudioStream;
use std::sync::atomic::AtomicBool;
use std::sync::{Arc, Mutex};
use tokio_stream::StreamExt;

#[cfg(feature = "real-backends")]
use crate::audio_mixer::mix_audio_tracks;
#[cfg(feature = "real-backends")]
use crate::audio_pipeline::FRAME_SAMPLES;
#[cfg(feature = "real-backends")]
use crate::cpal_audio::AudioBuffer;
#[cfg(feature = "real-backends")]
use crate::cpal_audio::PeerVolumes;
#[cfg(feature = "real-backends")]
use std::collections::{HashMap, HashSet, VecDeque};

type AudioFrameCallback = Arc<Mutex<Option<Box<dyn Fn(&str, &[f32]) + Send + 'static>>>>;

#[cfg(feature = "real-backends")]
pub(super) struct ParticipantAudioDecoderContext {
    pub volume_key: String,
    pub source: TrackSource,
    pub queue_key: String,
    pub audio_cb: AudioFrameCallback,
    pub closing: Arc<AtomicBool>,
    pub peer_volumes: Arc<Mutex<Option<PeerVolumes>>>,
    pub screen_share_audio_enabled: Arc<Mutex<HashSet<String>>>,
    pub remote_queues: Arc<Mutex<HashMap<String, VecDeque<f32>>>>,
}

// ---------------------------------------------------------------------------
// Audio frame conversion
// ---------------------------------------------------------------------------

/// Converts raw PCM audio samples to mono 48kHz f32.
///
/// Steps:
/// 1. If `channels == 0`, treat as mono (pass through).
/// 2. If `channels > 1`, down-mix to mono by averaging all channels per frame.
/// 3. If `sample_rate != 48000`, resample to 48kHz via linear interpolation.
///
/// Edge cases:
/// - Empty input → empty output.
/// - `sample_rate == 0` → empty output (avoids division by zero).
pub(super) fn convert_audio_frame(samples: &[f32], sample_rate: u32, channels: u32) -> Vec<f32> {
    // Guard: avoid division by zero.
    if sample_rate == 0 {
        return Vec::new();
    }

    // --- Step 0: sanitize NaN/Inf inputs ---
    // Any NaN or infinite sample is replaced with 0.0 so that downstream
    // averaging and interpolation never propagate non-finite values.
    let samples: Vec<f32> = samples
        .iter()
        .map(|&s| {
            if s.is_nan() || s.is_infinite() {
                0.0
            } else {
                s
            }
        })
        .collect();
    let samples: &[f32] = &samples;

    // --- Step 1: down-mix to mono ---
    let mono: Vec<f32> = if channels <= 1 {
        // Already mono (or channels == 0 treated as mono).
        samples.to_vec()
    } else {
        let ch = channels as usize;
        // Each frame is `ch` consecutive samples; average them.
        samples
            .chunks_exact(ch)
            .map(|frame| frame.iter().sum::<f32>() / ch as f32)
            .collect()
    };

    // --- Step 2: resample to 48kHz via linear interpolation ---
    if sample_rate == 48_000 {
        return mono;
    }

    if mono.is_empty() {
        return Vec::new();
    }

    // Output length: ceil(mono.len() * 48000 / sample_rate)
    let in_len = mono.len();
    let out_len = (in_len as u64 * 48_000).div_ceil(sample_rate as u64);
    let out_len = out_len as usize;

    let mut out = Vec::with_capacity(out_len);
    for i in 0..out_len {
        // Fractional input position.
        let t = i as f64 * sample_rate as f64 / 48_000.0_f64;
        let lo = t.floor() as usize;
        let hi = (lo + 1).min(in_len - 1);
        let frac = (t - lo as f64) as f32;
        out.push(mono[lo] + frac * (mono[hi] - mono[lo]));
    }

    out
}

// ---------------------------------------------------------------------------
// Background task bodies
// ---------------------------------------------------------------------------

/// Remote audio mixer loop — consumes per-subscribed-audio-track decoded PCM
/// queues and writes one time-aligned 20ms mixed frame to the playback buffer
/// per tick.
///
/// Spawned once per connection in `livekit_connection::connect()`.
#[cfg(feature = "real-backends")]
pub(super) async fn run_mix_task(
    closing: Arc<AtomicBool>,
    playback_buffer: Arc<Mutex<Option<AudioBuffer>>>,
    remote_queues: Arc<Mutex<HashMap<String, VecDeque<f32>>>>,
) {
    use tokio::time::{interval, Duration, MissedTickBehavior};

    const MAX_QUEUE_SAMPLES: usize = FRAME_SAMPLES * 10; // 200ms per participant
    const TARGET_QUEUE_SAMPLES: usize = FRAME_SAMPLES * 5; // trim back to 100ms

    let mut ticker = interval(Duration::from_millis(20));
    ticker.set_missed_tick_behavior(MissedTickBehavior::Skip);

    let mut mixed = vec![0.0f32; FRAME_SAMPLES];
    let mut warn_no_buffer_once = false;

    loop {
        ticker.tick().await;

        if closing.load(std::sync::atomic::Ordering::SeqCst) {
            break;
        }

        let tracks: Vec<Vec<f32>> = {
            let mut queues = remote_queues.lock().unwrap();
            let mut frames = Vec::with_capacity(queues.len());

            for queue in queues.values_mut() {
                if queue.is_empty() {
                    continue;
                }

                let mut frame = vec![0.0f32; FRAME_SAMPLES];
                let mut filled = 0usize;
                while filled < FRAME_SAMPLES {
                    if let Some(sample) = queue.pop_front() {
                        frame[filled] = sample;
                        filled += 1;
                    } else {
                        break;
                    }
                }

                if filled == 0 {
                    continue;
                }
                if filled < FRAME_SAMPLES {
                    let hold = frame[filled - 1];
                    frame[filled..].fill(hold);
                }
                frames.push(frame);
            }

            // Keep only active participants with queued audio.
            queues.retain(|_, q| !q.is_empty());

            // Apply bounded queue policy per participant to cap latency.
            for queue in queues.values_mut() {
                if queue.len() > MAX_QUEUE_SAMPLES {
                    let to_drop = queue.len() - TARGET_QUEUE_SAMPLES;
                    queue.drain(..to_drop);
                }
            }

            frames
        };

        if tracks.is_empty() {
            continue;
        }

        mixed.fill(0.0);
        let refs: Vec<&[f32]> = tracks.iter().map(|track| track.as_slice()).collect();
        mix_audio_tracks(&refs, &mut mixed);

        // Prevent hard clipping artifacts when multiple people talk.
        if tracks.len() > 1 {
            let gain = 1.0 / (tracks.len() as f32).sqrt();
            for sample in &mut mixed {
                *sample = (*sample * gain).clamp(-1.0, 1.0);
            }
        }

        if let Some(pb) = playback_buffer.lock().unwrap().as_ref() {
            pb.write_mono(&mixed);
        } else if !warn_no_buffer_once {
            log::warn!(
                "livekit_audio: playback_buffer not set — mixed audio will not be played back"
            );
            warn_no_buffer_once = true;
        }
    }
}

/// Per-participant audio decoder — reads frames from `stream`, converts to
/// mono 48kHz f32, applies peer volume gain, and writes to `remote_queues`
/// for mixing. Also fires the `audio_cb` callback for each frame.
///
/// Spawned once per `TrackSubscribed` (Audio) event in
/// `livekit_connection::connect()`.
#[cfg(feature = "real-backends")]
pub(super) async fn run_participant_audio_decoder(
    mut stream: NativeAudioStream,
    participant_id: String,
    ctx: ParticipantAudioDecoderContext,
) {
    let mut frames_received: u64 = 0;
    let mut total_samples: u64 = 0;

    while let Some(frame) = stream.next().await {
        if ctx.closing.load(std::sync::atomic::Ordering::SeqCst) {
            break;
        }
        // Convert i16 → f32 then through our mono/48kHz normaliser.
        let f32_samples: Vec<f32> = frame.data.iter().map(|&s| s as f32 / 32768.0).collect();
        let converted = convert_audio_frame(&f32_samples, frame.sample_rate, frame.num_channels);

        let gain = if let Some(ref vols) = *ctx.peer_volumes.lock().unwrap() {
            vols.gain(&ctx.volume_key)
        } else {
            1.0
        };

        if ctx.source == TrackSource::ScreenshareAudio
            && !ctx
                .screen_share_audio_enabled
                .lock()
                .unwrap()
                .contains(&participant_id)
        {
            continue;
        }

        let mut queues = ctx.remote_queues.lock().unwrap();
        let queue = queues.entry(ctx.queue_key.clone()).or_default();

        if (gain - 1.0).abs() < f32::EPSILON {
            queue.extend(converted.iter().copied());
        } else {
            queue.extend(converted.iter().map(|&s| s * gain));
        }

        frames_received += 1;
        total_samples += converted.len() as u64;
        if frames_received.is_multiple_of(250) {
            log::info!(
                "frame_delivery=stats participant={participant_id} frames_received={frames_received} total_samples={total_samples}"
            );
        }
        if let Some(cb) = ctx.audio_cb.lock().unwrap().as_ref() {
            cb(&participant_id, &converted);
        }
    }

    ctx.remote_queues.lock().unwrap().remove(&ctx.queue_key);
}

/// Per-participant audio decoder (no-playback variant) — reads frames from
/// `stream`, converts to mono 48kHz f32, and fires the `audio_cb` callback.
/// Used when the `real-backends` feature is disabled.
#[cfg(not(feature = "real-backends"))]
pub(super) async fn run_participant_audio_decoder(
    mut stream: NativeAudioStream,
    participant_id: String,
    _queue_key: String,
    audio_cb: AudioFrameCallback,
    closing: Arc<AtomicBool>,
) {
    let mut frames_received: u64 = 0;
    let mut total_samples: u64 = 0;

    while let Some(frame) = stream.next().await {
        if closing.load(std::sync::atomic::Ordering::SeqCst) {
            break;
        }
        let f32_samples: Vec<f32> = frame.data.iter().map(|&s| s as f32 / 32768.0).collect();
        let converted = convert_audio_frame(&f32_samples, frame.sample_rate, frame.num_channels);

        frames_received += 1;
        total_samples += converted.len() as u64;
        if frames_received.is_multiple_of(250) {
            log::info!(
                "frame_delivery=stats participant={participant_id} frames_received={frames_received} total_samples={total_samples}"
            );
        }
        if let Some(cb) = audio_cb.lock().unwrap().as_ref() {
            cb(&participant_id, &converted);
        }
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::convert_audio_frame;
    use proptest::prelude::*;

    // -----------------------------------------------------------------------
    // Feature: livekit-client-connection, Property 3: Audio conversion produces mono 48kHz output
    // **Validates: Requirements 3.2, 3.5**
    //
    // For any audio frame with arbitrary sample rate (8kHz–96kHz), channel count
    // (1–2), and sample data, the audio conversion function SHALL produce output
    // that is mono (1 channel) with a sample count proportional to
    // input_samples * 48000 / input_sample_rate.
    // -----------------------------------------------------------------------

    proptest! {
        #[test]
        fn prop_convert_audio_frame_mono_48khz(
            (channels, sample_rate, num_frames, samples) in
                (1u32..=2u32, 8000u32..=96000u32, 0usize..=480usize)
                    .prop_flat_map(|(channels, sample_rate, num_frames)| {
                        let len = num_frames * channels as usize;
                        (
                            Just(channels),
                            Just(sample_rate),
                            Just(num_frames),
                            proptest::collection::vec(-1.0f32..=1.0f32, len..=len),
                        )
                    })
        ) {
            let out = convert_audio_frame(&samples, sample_rate, channels);
            let expected_len = (num_frames as u64 * 48_000).div_ceil(sample_rate as u64);
            prop_assert_eq!(
                out.len(),
                expected_len as usize,
                "channels={}, sample_rate={}, num_frames={}",
                channels, sample_rate, num_frames
            );
            // Non-empty input must produce non-empty output
            if num_frames > 0 {
                prop_assert!(!out.is_empty(), "expected non-empty output for non-empty input");
            }
        }
    }

    // -----------------------------------------------------------------------
    // Feature: livekit-audio-fix, Property 5: NaN/Inf sanitization
    // **Validates: Requirements 5.6**
    //
    // For any audio frame containing NaN or infinity values in the input samples,
    // convert_audio_frame SHALL produce output containing no NaN or infinity
    // values (all such inputs are sanitized to 0.0 before processing).
    // -----------------------------------------------------------------------

    /// Strategy that produces a single f32 that is either a normal finite value
    /// or one of the three non-finite sentinels: NaN, +Inf, -Inf.
    fn possibly_nonfinite() -> impl Strategy<Value = f32> {
        prop_oneof![
            // ~70 % normal finite samples
            7 => -1.0f32..=1.0f32,
            // ~10 % each for the three non-finite sentinels
            1 => Just(f32::NAN),
            1 => Just(f32::INFINITY),
            1 => Just(f32::NEG_INFINITY),
        ]
    }

    // -----------------------------------------------------------------------
    // Feature: livekit-audio-fix, Property 4: Amplitude preservation under conversion
    // **Validates: Requirements 5.5**
    //
    // For any audio frame where all input samples are in the range [-1.0, 1.0],
    // all output samples from convert_audio_frame SHALL also be in the range
    // [-1.0, 1.0], regardless of channel count or sample rate.
    // -----------------------------------------------------------------------

    proptest! {
        #![proptest_config(ProptestConfig::with_cases(100))]
        #[test]
        fn prop_amplitude_preservation(
            (channels, sample_rate, samples) in
                (1u32..=8u32, 8000u32..=96000u32)
                    .prop_flat_map(|(channels, sample_rate)| {
                        let max_frames = 480usize;
                        proptest::collection::vec(
                            -1.0f32..=1.0f32,
                            0..=(max_frames * channels as usize),
                        )
                        .prop_map(move |mut s| {
                            // Truncate to a multiple of `channels` so we have
                            // only complete frames (matches chunks_exact behaviour).
                            let ch = channels as usize;
                            let trim = s.len() - (s.len() % ch);
                            s.truncate(trim);
                            (channels, sample_rate, s)
                        })
                    })
        ) {
            let out = convert_audio_frame(&samples, sample_rate, channels);
            for (i, &s) in out.iter().enumerate() {
                prop_assert!(
                    (-1.0..=1.0).contains(&s),
                    "output sample[{i}] = {s} is out of [-1.0, 1.0] \
                     (channels={channels}, sample_rate={sample_rate}, \
                     input_len={})",
                    samples.len()
                );
            }
        }
    }

    // -----------------------------------------------------------------------
    // Feature: livekit-audio-fix, Property 3: Channel downmix correctness
    // **Validates: Requirements 5.1**
    //
    // For any audio frame with C channels (C >= 2) and N complete frames of
    // samples, convert_audio_frame SHALL produce exactly N mono samples, where
    // each output sample equals the arithmetic mean of the C channel samples in
    // the corresponding input frame.
    // -----------------------------------------------------------------------

    proptest! {
        #![proptest_config(ProptestConfig::with_cases(100))]
        #[test]
        fn prop_channel_downmix(
            (channels, num_frames, samples) in
                (2u32..=8u32, 1usize..=480usize)
                    .prop_flat_map(|(channels, num_frames)| {
                        let len = num_frames * channels as usize;
                        proptest::collection::vec(-1.0f32..=1.0f32, len..=len)
                            .prop_map(move |s| (channels, num_frames, s))
                    })
        ) {
            // Use sample_rate=48000 to bypass resampling and isolate downmix.
            let out = convert_audio_frame(&samples, 48_000, channels);

            // Output must have exactly num_frames mono samples.
            prop_assert_eq!(
                out.len(),
                num_frames,
                "expected {} output samples, got {} (channels={}, input_len={})",
                num_frames, out.len(), channels, samples.len()
            );

            let ch = channels as usize;
            let tolerance = f32::EPSILON * channels as f32;
            for (i, &out_sample) in out.iter().enumerate() {
                let frame = &samples[i * ch..(i + 1) * ch];
                let expected_mean = frame.iter().sum::<f32>() / ch as f32;
                prop_assert!(
                    (out_sample - expected_mean).abs() <= tolerance,
                    "output[{i}] = {out_sample} differs from mean {expected_mean} \
                     by more than epsilon*channels={tolerance} \
                     (channels={channels}, frame={frame:?})"
                );
            }
        }
    }

    proptest! {
        #![proptest_config(ProptestConfig::with_cases(100))]
        #[test]
        fn prop_nan_inf_sanitization(
            (channels, sample_rate, samples) in
                (1u32..=8u32, 8000u32..=96000u32)
                    .prop_flat_map(|(channels, sample_rate)| {
                        // 0–480 complete frames worth of samples
                        let max_frames = 480usize;
                        proptest::collection::vec(
                            possibly_nonfinite(),
                            0..=(max_frames * channels as usize),
                        )
                        .prop_map(move |mut s| {
                            // Truncate to a multiple of `channels` so we have
                            // only complete frames (matches chunks_exact behaviour).
                            let ch = channels as usize;
                            let trim = s.len() - (s.len() % ch);
                            s.truncate(trim);
                            (channels, sample_rate, s)
                        })
                    })
        ) {
            let out = convert_audio_frame(&samples, sample_rate, channels);
            for (i, &s) in out.iter().enumerate() {
                prop_assert!(
                    s.is_finite(),
                    "output sample[{i}] = {s} is not finite \
                     (channels={channels}, sample_rate={sample_rate}, \
                     input_len={})",
                    samples.len()
                );
            }
        }
    }
}
