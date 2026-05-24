#[cfg(feature = "real-backends")]
use std::time::{Duration, Instant};

#[cfg(feature = "real-backends")]
use wavis_client_shared::audio_pipeline::FRAME_SAMPLES;
#[cfg(all(feature = "real-backends", feature = "deepfilternet-backend"))]
use wavis_client_shared::denoise_filter::NoiseSuppressorKind;
#[cfg(feature = "real-backends")]
use wavis_client_shared::denoise_filter::{DenoiseFilter, InfallibleNoiseSuppressorKind};

#[cfg(feature = "real-backends")]
const SAMPLE_RATE: f32 = 48_000.0;
#[cfg(feature = "real-backends")]
const FRAMES_PER_CASE: usize = 240;

#[cfg(feature = "real-backends")]
fn main() {
    run_infallible_backend("none", InfallibleNoiseSuppressorKind::None);
    run_infallible_backend("rnnoise", InfallibleNoiseSuppressorKind::Rnnoise);
    run_deepfilternet_backend();
}

#[cfg(feature = "real-backends")]
fn run_infallible_backend(label: &str, kind: InfallibleNoiseSuppressorKind) {
    let startup = Instant::now();
    let filter = DenoiseFilter::with_infallible_backend(kind, true);
    run_filter(label, filter, startup.elapsed());
}

#[cfg(all(feature = "real-backends", feature = "deepfilternet-backend"))]
fn run_deepfilternet_backend() {
    let startup = Instant::now();
    match DenoiseFilter::try_with_backend(NoiseSuppressorKind::DeepFilterNetExperimental, true) {
        Ok(filter) => run_filter("deepfilternet-experimental", filter, startup.elapsed()),
        Err(err) => {
            println!(
                "backend=deepfilternet-experimental startup_ms={:.3} unavailable=\"{}\"",
                startup.elapsed().as_secs_f64() * 1000.0,
                err
            );
        }
    }
}

#[cfg(all(feature = "real-backends", not(feature = "deepfilternet-backend")))]
fn run_deepfilternet_backend() {
    println!("backend=deepfilternet-experimental unavailable=\"feature deepfilternet-backend not enabled\"");
}

#[cfg(feature = "real-backends")]
fn run_filter(label: &str, filter: DenoiseFilter, startup_elapsed: Duration) {
    println!(
        "backend={} startup_ms={:.3} model_latency_samples={} stream_startup_latency_samples={}",
        label,
        startup_elapsed.as_secs_f64() * 1000.0,
        filter.algorithmic_latency_samples(),
        filter.stream_startup_latency_samples()
    );
    for case in [
        "silence",
        "white_noise",
        "pinkish_noise",
        "hum_noise",
        "impulses",
        "speech_like",
        "speech_start_after_silence",
        "speech_clicks",
        "short_words",
        "quiet_voice",
        "two_speech_like",
    ] {
        filter.reset_state();
        let mut total = Duration::ZERO;
        let mut max = Duration::ZERO;
        let mut output_rms = 0.0;
        let mut finite = true;
        let mut output_len = 0;
        let mut input_samples = Vec::with_capacity(FRAMES_PER_CASE * FRAME_SAMPLES);
        let mut output_samples = Vec::with_capacity(FRAMES_PER_CASE * FRAME_SAMPLES);

        for frame_idx in 0..FRAMES_PER_CASE {
            let mut frame = synthetic_frame(case, frame_idx);
            input_samples.extend_from_slice(&frame);
            let start = Instant::now();
            filter.process(&mut frame);
            let elapsed = start.elapsed();
            total += elapsed;
            max = max.max(elapsed);
            output_rms += rms(&frame);
            finite = finite && frame.iter().all(|sample| sample.is_finite());
            output_len = frame.len();
            output_samples.extend_from_slice(&frame);
        }

        let avg = total.as_secs_f64() * 1000.0 / FRAMES_PER_CASE as f64;
        let max = max.as_secs_f64() * 1000.0;
        output_rms /= FRAMES_PER_CASE as f64;
        let peak_offset = peak_offset_samples(&input_samples, &output_samples)
            .map(|offset| offset.to_string())
            .unwrap_or_else(|| "n/a".to_string());
        println!(
            "backend={label} case={case} avg_frame_ms={avg:.4} max_frame_ms={max:.4} output_rms={output_rms:.6} finite={finite} output_len={output_len} peak_offset_samples={peak_offset}"
        );
    }
}

#[cfg(not(feature = "real-backends"))]
fn main() {
    println!("noise_suppression_measure requires --features real-backends");
}

#[cfg(feature = "real-backends")]
fn synthetic_frame(case: &str, frame_idx: usize) -> Vec<f32> {
    let mut frame = vec![0.0f32; FRAME_SAMPLES];
    let mut rng = frame_idx as u64 + 1;
    let mut pink_state = 0.0f32;

    for (i, sample) in frame.iter_mut().enumerate() {
        let n = frame_idx * FRAME_SAMPLES + i;
        let t = n as f32 / SAMPLE_RATE;
        let white = {
            rng = rng.wrapping_mul(6364136223846793005).wrapping_add(1);
            ((rng >> 33) as f32 / u32::MAX as f32) * 2.0 - 1.0
        };
        pink_state = 0.98 * pink_state + 0.02 * white;

        *sample = match case {
            "silence" => 0.0,
            "white_noise" => white * 0.08,
            "pinkish_noise" => pink_state * 0.24,
            "hum_noise" => {
                let hum = (2.0 * std::f32::consts::PI * 60.0 * t).sin() * 0.08;
                hum + white * 0.03
            }
            "impulses" => {
                if n % 2400 == 0 {
                    0.7
                } else {
                    white * 0.01
                }
            }
            "speech_like" => speech_like(t, 180.0, 0.22),
            "speech_start_after_silence" => {
                if n < SAMPLE_RATE as usize {
                    0.0
                } else {
                    speech_like(t - 1.0, 185.0, 0.22)
                }
            }
            "speech_clicks" => {
                let click = if n % 3200 == 0 { 0.5 } else { 0.0 };
                speech_like(t, 180.0, 0.20) + click
            }
            "short_words" => {
                let word_period = SAMPLE_RATE as usize / 2;
                let word_len = (SAMPLE_RATE as usize / 10).max(1);
                let in_word = n % word_period < word_len;
                if in_word {
                    speech_like(t, 210.0, 0.22)
                } else {
                    0.0
                }
            }
            "quiet_voice" => speech_like(t, 180.0, 0.045),
            "two_speech_like" => speech_like(t, 180.0, 0.16) + speech_like(t, 255.0, 0.14),
            _ => 0.0,
        };
    }

    frame
}

#[cfg(feature = "real-backends")]
fn speech_like(t: f32, fundamental: f32, amplitude: f32) -> f32 {
    let envelope = ((2.0 * std::f32::consts::PI * 3.2 * t).sin() * 0.5 + 0.5).max(0.2);
    let f1 = (2.0 * std::f32::consts::PI * fundamental * t).sin();
    let f2 = (2.0 * std::f32::consts::PI * fundamental * 2.4 * t).sin() * 0.45;
    let f3 = (2.0 * std::f32::consts::PI * fundamental * 4.1 * t).sin() * 0.25;
    (f1 + f2 + f3) * amplitude * envelope
}

#[cfg(feature = "real-backends")]
fn rms(frame: &[f32]) -> f64 {
    let sum_sq: f64 = frame
        .iter()
        .map(|sample| {
            let sample = *sample as f64;
            sample * sample
        })
        .sum();
    (sum_sq / frame.len() as f64).sqrt()
}

#[cfg(feature = "real-backends")]
fn peak_offset_samples(input: &[f32], output: &[f32]) -> Option<isize> {
    let input_peak = peak_index(input)?;
    let output_peak = peak_index(output)?;
    Some(output_peak as isize - input_peak as isize)
}

#[cfg(feature = "real-backends")]
fn peak_index(samples: &[f32]) -> Option<usize> {
    let (index, peak) = samples
        .iter()
        .enumerate()
        .max_by(|(_, a), (_, b)| a.abs().total_cmp(&b.abs()))?;
    if peak.abs() < 0.001 {
        None
    } else {
        Some(index)
    }
}
