#[cfg(feature = "real-backends")]
use std::time::{Duration, Instant};

#[cfg(feature = "real-backends")]
use wavis_client_shared::audio_pipeline::FRAME_SAMPLES;
#[cfg(feature = "real-backends")]
use wavis_client_shared::denoise_filter::DenoiseFilter;

#[cfg(feature = "real-backends")]
const SAMPLE_RATE: f32 = 48_000.0;
#[cfg(feature = "real-backends")]
const FRAMES_PER_CASE: usize = 240;

#[cfg(feature = "real-backends")]
fn main() {
    let startup = Instant::now();
    let filter = DenoiseFilter::new(true);
    let startup_elapsed = startup.elapsed();

    println!("startup_ms={:.3}", startup_elapsed.as_secs_f64() * 1000.0);

    for case in [
        "silence",
        "white_noise",
        "pinkish_noise",
        "hum_noise",
        "impulses",
        "speech_like",
        "speech_clicks",
        "two_speech_like",
    ] {
        filter.reset_state();
        let mut total = Duration::ZERO;
        let mut max = Duration::ZERO;

        for frame_idx in 0..FRAMES_PER_CASE {
            let mut frame = synthetic_frame(case, frame_idx);
            let start = Instant::now();
            filter.process(&mut frame);
            let elapsed = start.elapsed();
            total += elapsed;
            max = max.max(elapsed);
        }

        let avg = total.as_secs_f64() * 1000.0 / FRAMES_PER_CASE as f64;
        let max = max.as_secs_f64() * 1000.0;
        println!("{case} avg_frame_ms={avg:.4} max_frame_ms={max:.4}");
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
            "speech_clicks" => {
                let click = if n % 3200 == 0 { 0.5 } else { 0.0 };
                speech_like(t, 180.0, 0.20) + click
            }
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
