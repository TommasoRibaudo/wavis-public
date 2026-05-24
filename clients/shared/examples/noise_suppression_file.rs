#[cfg(feature = "real-backends")]
use std::{
    error::Error,
    fs,
    path::{Path, PathBuf},
    process::Command,
    time::{Duration, Instant, SystemTime},
};

#[cfg(feature = "real-backends")]
use hound::{SampleFormat, WavReader, WavSpec, WavWriter};
#[cfg(feature = "real-backends")]
use serde_json::json;
#[cfg(all(feature = "real-backends", feature = "deepfilternet-backend"))]
use wavis_client_shared::denoise_filter::NoiseSuppressorKind;
#[cfg(feature = "real-backends")]
use wavis_client_shared::{
    audio_pipeline::FRAME_SAMPLES,
    denoise_filter::{DenoiseFilter, InfallibleNoiseSuppressorKind},
};

#[cfg(feature = "real-backends")]
const SAMPLE_RATE: u32 = 48_000;
#[cfg(feature = "real-backends")]
const OUTPUT_ROOT: &str = "phase6-audio/output";
#[cfg(feature = "real-backends")]
const MANIFEST_FILE: &str = "attempt_manifest.json";
#[cfg(feature = "real-backends")]
const CORRELATION_OFFSETS: [isize; 13] = [
    -2880, -2400, -1920, -1440, -960, -480, 0, 480, 960, 1440, 1920, 2400, 2880,
];

#[cfg(feature = "real-backends")]
fn main() {
    if let Err(err) = run() {
        eprintln!("error: {err}");
        std::process::exit(1);
    }
}

#[cfg(feature = "real-backends")]
fn run() -> Result<(), Box<dyn Error>> {
    let started = Instant::now();
    let started_unix_seconds = unix_seconds_now()?;
    let command = std::env::args().collect::<Vec<_>>().join(" ");
    let args = AttemptArgs::parse()?;

    let attempt_dir = Path::new(OUTPUT_ROOT).join(&args.attempt);
    if attempt_dir.exists() && !args.overwrite {
        return Err(format!(
            "{} already exists; choose a new --attempt name or pass --overwrite",
            attempt_dir.display()
        )
        .into());
    }
    fs::create_dir_all(&attempt_dir)?;

    let mut input_manifests = Vec::new();
    let mut result_manifests = Vec::new();
    for input in &args.inputs {
        let samples = read_wav_mono_48k(input)?;
        input_manifests.push(json!({
            "path": input.display().to_string(),
            "samples": samples.len(),
            "duration_seconds": samples.len() as f64 / SAMPLE_RATE as f64,
        }));
        result_manifests.extend(process_input(input, &attempt_dir, &samples)?);
    }

    let manifest = json!({
        "attempt": args.attempt,
        "command": command,
        "timestamp_utc_unix_seconds": started_unix_seconds,
        "duration_ms": started.elapsed().as_secs_f64() * 1000.0,
        "git": git_summary(),
        "inputs": input_manifests,
        "backend_results": result_manifests,
        "notes": {
            "deepfilternet_status": "blocked_pending_harness_listening",
            "rnnoise_remains_default": true,
            "correlation_offsets_samples": CORRELATION_OFFSETS,
        },
    });
    fs::write(
        attempt_dir.join(MANIFEST_FILE),
        serde_json::to_string_pretty(&manifest)?,
    )?;

    Ok(())
}

#[cfg(feature = "real-backends")]
struct AttemptArgs {
    attempt: String,
    overwrite: bool,
    inputs: Vec<PathBuf>,
}

#[cfg(feature = "real-backends")]
impl AttemptArgs {
    fn parse() -> Result<Self, Box<dyn Error>> {
        let mut attempt = None;
        let mut overwrite = false;
        let mut inputs = Vec::new();
        let mut args = std::env::args_os().skip(1);

        while let Some(arg) = args.next() {
            if arg == "--overwrite" {
                overwrite = true;
                continue;
            }

            if arg == "--attempt" {
                let value = args.next().ok_or("missing value after --attempt")?;
                attempt = Some(value.into_string().map_err(|_| {
                    "--attempt must be valid UTF-8 and contain only portable filename characters"
                })?);
                continue;
            }

            let arg_string = arg.to_string_lossy();
            if let Some(value) = arg_string.strip_prefix("--attempt=") {
                attempt = Some(value.to_string());
                continue;
            }

            inputs.push(PathBuf::from(arg));
        }

        let attempt = attempt.ok_or(
            "usage: cargo run -p wavis-client-shared --features real-backends,deepfilternet-backend --example noise_suppression_file -- --attempt <attempt-name> [--overwrite] phase6-audio/input/<sample>.wav ...",
        )?;
        validate_attempt_name(&attempt)?;
        if inputs.is_empty() {
            return Err("provide at least one 48 kHz WAV input path".into());
        }

        Ok(Self {
            attempt,
            overwrite,
            inputs,
        })
    }
}

#[cfg(feature = "real-backends")]
fn validate_attempt_name(attempt: &str) -> Result<(), Box<dyn Error>> {
    if attempt.is_empty()
        || !attempt
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_'))
    {
        return Err(
            "attempt name must be non-empty and use only ASCII letters, numbers, '-' or '_'".into(),
        );
    }
    Ok(())
}

#[cfg(feature = "real-backends")]
fn process_input(
    input_path: &Path,
    attempt_dir: &Path,
    samples: &[f32],
) -> Result<Vec<serde_json::Value>, Box<dyn Error>> {
    let stem = input_path
        .file_stem()
        .and_then(|stem| stem.to_str())
        .ok_or("input file must have a valid UTF-8 file stem")?;

    let mut results = Vec::new();
    results.push(run_backend(
        input_path,
        attempt_dir,
        stem,
        "none",
        samples,
        DenoiseFilter::with_infallible_backend(InfallibleNoiseSuppressorKind::None, true),
    )?);
    results.push(run_backend(
        input_path,
        attempt_dir,
        stem,
        "rnnoise",
        samples,
        DenoiseFilter::with_infallible_backend(InfallibleNoiseSuppressorKind::Rnnoise, true),
    )?);
    results.extend(run_deepfilternet_backend(
        input_path,
        attempt_dir,
        stem,
        samples,
    )?);

    Ok(results)
}

#[cfg(feature = "real-backends")]
fn run_backend(
    input_path: &Path,
    attempt_dir: &Path,
    stem: &str,
    backend: &str,
    samples: &[f32],
    filter: DenoiseFilter,
) -> Result<serde_json::Value, Box<dyn Error>> {
    run_backend_with_mode(
        input_path,
        attempt_dir,
        stem,
        backend,
        samples,
        filter,
        FileOutputMode::Stream,
    )
}

#[cfg(feature = "real-backends")]
fn run_backend_with_mode(
    input_path: &Path,
    attempt_dir: &Path,
    stem: &str,
    backend: &str,
    samples: &[f32],
    filter: DenoiseFilter,
    mode: FileOutputMode,
) -> Result<serde_json::Value, Box<dyn Error>> {
    filter.reset_state();
    let model_latency_samples = filter.algorithmic_latency_samples();
    let stream_startup_latency_samples = filter.stream_startup_latency_samples();
    let compensation_owner = mode.compensation_owner(&filter);
    let output = match mode {
        FileOutputMode::Stream => process_samples(samples, &filter),
        FileOutputMode::StreamPostProcessed(post_process) => {
            process_samples_stream_post_processed(samples, &filter, post_process)
        }
        FileOutputMode::OfflineCompensated => {
            process_samples_offline_compensated(samples, &filter, PostProcessPolicy::None)
        }
    };
    let output_path = attempt_dir.join(format!("{stem}_{backend}.wav"));
    write_wav_mono_16(&output_path, &output.samples)?;
    let compensation_baseline_offset_samples = match compensation_owner {
        "backend" => stream_startup_latency_samples as isize,
        _ => 0,
    };
    let diagnostics = correlation_diagnostics(
        samples,
        &output.samples,
        compensation_baseline_offset_samples,
    );
    let boundary_cluster = diagnostics.boundary_cluster();
    let best_offset = diagnostics.best_offset_samples.unwrap_or(0);

    println!(
        "input={} backend={} output={} avg_frame_ms={:.4} max_frame_ms={:.4} finite={} input_len={} output_len={} model_latency_samples={} stream_startup_latency_samples={} compensation_owner={} compensation_baseline_offset_samples={} best_corr_residual_offset_samples={} best_corr={:.6} boundary_cluster={}",
        input_path.display(),
        backend,
        output_path.display(),
        output.avg_frame_time.as_secs_f64() * 1000.0,
        output.max_frame_time.as_secs_f64() * 1000.0,
        output.finite,
        samples.len(),
        output.samples.len(),
        model_latency_samples,
        stream_startup_latency_samples,
        compensation_owner,
        compensation_baseline_offset_samples,
        best_offset,
        diagnostics.best_correlation,
        boundary_cluster,
    );

    Ok(json!({
        "input": input_path.display().to_string(),
        "backend": backend,
        "output": output_path.display().to_string(),
        "avg_frame_ms": output.avg_frame_time.as_secs_f64() * 1000.0,
        "max_frame_ms": output.max_frame_time.as_secs_f64() * 1000.0,
        "finite": output.finite,
        "input_len": samples.len(),
        "output_len": output.samples.len(),
        "output_len_matches_input": output.samples.len() == samples.len(),
        "model_latency_samples": model_latency_samples,
        "stream_startup_latency_samples": stream_startup_latency_samples,
        "compensation_owner": compensation_owner,
        "compensation_baseline_offset_samples": compensation_baseline_offset_samples,
        "legacy_latency_samples": model_latency_samples,
        "correlation": {
            "candidate_offsets": diagnostics.candidate_manifest(),
            "best_residual_offset_samples": diagnostics.best_offset_samples,
            "best_correlation": diagnostics.best_correlation,
            "boundary_cluster": boundary_cluster,
            "regressed_to_plus_1440": diagnostics.best_offset_samples == Some(1440),
            "regressed_to_plus_1920": diagnostics.best_offset_samples == Some(1920),
        },
    }))
}

#[cfg(all(feature = "real-backends", feature = "deepfilternet-backend"))]
fn run_deepfilternet_backend(
    input_path: &Path,
    attempt_dir: &Path,
    stem: &str,
    samples: &[f32],
) -> Result<Vec<serde_json::Value>, Box<dyn Error>> {
    let mut results = Vec::new();
    let full_filter =
        DenoiseFilter::try_with_backend(NoiseSuppressorKind::DeepFilterNetExperimental, true)?;
    results.push(run_backend_with_mode(
        input_path,
        attempt_dir,
        stem,
        "deepfilternet_full",
        samples,
        full_filter,
        FileOutputMode::Stream,
    )?);

    let atten12_filter = DenoiseFilter::try_deepfilternet_with_atten_lim_db(12.0, true)?;
    results.push(run_backend_with_mode(
        input_path,
        attempt_dir,
        stem,
        "deepfilternet_atten12",
        samples,
        atten12_filter,
        FileOutputMode::Stream,
    )?);

    for (backend, wet_mix) in [("deepfilternet_blend25", 0.25), ("deepfilternet_blend50", 0.50)] {
        let blend_filter =
            DenoiseFilter::try_with_backend(NoiseSuppressorKind::DeepFilterNetExperimental, true)?;
        results.push(run_backend_with_mode(
            input_path,
            attempt_dir,
            stem,
            backend,
            samples,
            blend_filter,
            FileOutputMode::StreamPostProcessed(PostProcessPolicy::Blend { wet_mix }),
        )?);
    }

    Ok(results)
}

#[cfg(all(feature = "real-backends", not(feature = "deepfilternet-backend")))]
fn run_deepfilternet_backend(
    input_path: &Path,
    attempt_dir: &Path,
    stem: &str,
    _samples: &[f32],
) -> Result<Vec<serde_json::Value>, Box<dyn Error>> {
    let output_path = attempt_dir.join(format!("{stem}_deepfilternet.wav"));
    println!(
        "input={} backend=deepfilternet output={} unavailable=\"feature deepfilternet-backend not enabled\"",
        input_path.display(),
        output_path.display()
    );
    Ok(vec![json!({
        "input": input_path.display().to_string(),
        "backend": "deepfilternet",
        "output": output_path.display().to_string(),
        "available": false,
        "unavailable": "feature deepfilternet-backend not enabled",
    })])
}

#[cfg(feature = "real-backends")]
struct ProcessedOutput {
    samples: Vec<f32>,
    avg_frame_time: Duration,
    max_frame_time: Duration,
    finite: bool,
}

#[cfg(feature = "real-backends")]
#[derive(Clone, Copy)]
#[allow(dead_code)]
enum FileOutputMode {
    Stream,
    StreamPostProcessed(PostProcessPolicy),
    OfflineCompensated,
}

#[cfg(feature = "real-backends")]
impl FileOutputMode {
    fn compensation_owner(self, filter: &DenoiseFilter) -> &'static str {
        match self {
            FileOutputMode::Stream | FileOutputMode::StreamPostProcessed(_) => {
                if filter.stream_startup_latency_samples() > 0 {
                    "backend"
                } else {
                    "none"
                }
            }
            FileOutputMode::OfflineCompensated => "harness",
        }
    }
}

#[cfg(feature = "real-backends")]
#[cfg(feature = "real-backends")]
#[derive(Clone, Copy)]
#[allow(dead_code)]
enum PostProcessPolicy {
    None,
    Blend { wet_mix: f32 },
}

#[cfg(feature = "real-backends")]
fn process_samples(samples: &[f32], filter: &DenoiseFilter) -> ProcessedOutput {
    let mut processed = Vec::with_capacity(samples.len());
    let mut total = Duration::ZERO;
    let mut max = Duration::ZERO;
    let mut finite = true;
    let mut frame_count = 0;

    for chunk in samples.chunks(FRAME_SAMPLES) {
        let mut frame = vec![0.0; FRAME_SAMPLES];
        frame[..chunk.len()].copy_from_slice(chunk);

        let start = Instant::now();
        filter.process(&mut frame);
        let elapsed = start.elapsed();

        total += elapsed;
        max = max.max(elapsed);
        finite = finite && frame.iter().all(|sample| sample.is_finite());
        processed.extend_from_slice(&frame[..chunk.len()]);
        frame_count += 1;
    }

    let avg_frame_time = if frame_count == 0 {
        Duration::ZERO
    } else {
        Duration::from_secs_f64(total.as_secs_f64() / frame_count as f64)
    };

    ProcessedOutput {
        samples: processed,
        avg_frame_time,
        max_frame_time: max,
        finite,
    }
}

#[cfg(feature = "real-backends")]
fn process_samples_stream_post_processed(
    samples: &[f32],
    filter: &DenoiseFilter,
    post_process: PostProcessPolicy,
) -> ProcessedOutput {
    let mut output = process_samples(samples, filter);
    apply_post_process(samples, &mut output.samples, post_process);
    output
}

#[cfg(feature = "real-backends")]
fn process_samples_offline_compensated(
    samples: &[f32],
    filter: &DenoiseFilter,
    post_process: PostProcessPolicy,
) -> ProcessedOutput {
    let latency_samples = filter.algorithmic_latency_samples();
    let target_raw_len = samples.len() + latency_samples;
    let mut raw_output = Vec::with_capacity(target_raw_len + FRAME_SAMPLES);
    let mut total = Duration::ZERO;
    let mut max = Duration::ZERO;
    let mut finite = true;
    let mut frame_count = 0;

    for chunk in samples.chunks(FRAME_SAMPLES) {
        let mut frame = vec![0.0; FRAME_SAMPLES];
        frame[..chunk.len()].copy_from_slice(chunk);
        process_file_frame(
            filter,
            &mut frame,
            &mut raw_output,
            &mut total,
            &mut max,
            &mut finite,
            &mut frame_count,
        );
    }

    while raw_output.len() < target_raw_len {
        let mut frame = vec![0.0; FRAME_SAMPLES];
        process_file_frame(
            filter,
            &mut frame,
            &mut raw_output,
            &mut total,
            &mut max,
            &mut finite,
            &mut frame_count,
        );
    }

    let start = latency_samples;
    let end = start + samples.len();
    let mut processed = raw_output[start..end].to_vec();

    apply_post_process(samples, &mut processed, post_process);

    let avg_frame_time = if frame_count == 0 {
        Duration::ZERO
    } else {
        Duration::from_secs_f64(total.as_secs_f64() / frame_count as f64)
    };

    ProcessedOutput {
        samples: processed,
        avg_frame_time,
        max_frame_time: max,
        finite,
    }
}

#[cfg(feature = "real-backends")]
fn apply_post_process(input: &[f32], processed: &mut [f32], post_process: PostProcessPolicy) {
    match post_process {
        PostProcessPolicy::None => {}
        PostProcessPolicy::Blend { wet_mix } => apply_blend(input, processed, wet_mix),
    }
}

#[cfg(feature = "real-backends")]
fn process_file_frame(
    filter: &DenoiseFilter,
    frame: &mut [f32],
    raw_output: &mut Vec<f32>,
    total: &mut Duration,
    max: &mut Duration,
    finite: &mut bool,
    frame_count: &mut usize,
) {
    let start = Instant::now();
    filter.process(frame);
    let elapsed = start.elapsed();

    *total += elapsed;
    *max = (*max).max(elapsed);
    *finite = *finite && frame.iter().all(|sample| sample.is_finite());
    raw_output.extend_from_slice(frame);
    *frame_count += 1;
}

#[cfg(feature = "real-backends")]
fn apply_blend(input: &[f32], processed: &mut [f32], wet_mix: f32) {
    let wet_mix = wet_mix.clamp(0.0, 1.0);
    for (input_sample, processed_sample) in input.iter().zip(processed) {
        *processed_sample = *input_sample * (1.0 - wet_mix) + *processed_sample * wet_mix;
    }
}

#[cfg(feature = "real-backends")]
struct CorrelationDiagnostics {
    candidates: Vec<(isize, f64)>,
    best_offset_samples: Option<isize>,
    best_correlation: f64,
}

#[cfg(feature = "real-backends")]
impl CorrelationDiagnostics {
    fn boundary_cluster(&self) -> bool {
        matches!(self.best_offset_samples, Some(offset) if offset != 0 && offset.abs() % 480 == 0)
    }

    fn candidate_manifest(&self) -> Vec<serde_json::Value> {
        self.candidates
            .iter()
            .map(|(offset, correlation)| {
                json!({
                    "offset_samples": offset,
                    "correlation": correlation,
                })
            })
            .collect()
    }
}

#[cfg(feature = "real-backends")]
fn correlation_diagnostics(
    input: &[f32],
    output: &[f32],
    baseline_offset: isize,
) -> CorrelationDiagnostics {
    let candidates: Vec<(isize, f64)> = CORRELATION_OFFSETS
        .iter()
        .copied()
        .map(|offset| {
            (
                offset,
                normalized_correlation_at_offset(input, output, baseline_offset + offset),
            )
        })
        .collect();
    let (best_offset_samples, best_correlation) = candidates
        .iter()
        .copied()
        .max_by(|(_, a), (_, b)| a.abs().total_cmp(&b.abs()))
        .map(|(offset, correlation)| (Some(offset), correlation))
        .unwrap_or((None, 0.0));

    CorrelationDiagnostics {
        candidates,
        best_offset_samples,
        best_correlation,
    }
}

#[cfg(feature = "real-backends")]
fn normalized_correlation_at_offset(input: &[f32], output: &[f32], offset: isize) -> f64 {
    let input_start = if offset < 0 { offset.unsigned_abs() } else { 0 };
    let output_start = if offset > 0 { offset as usize } else { 0 };
    if input_start >= input.len() || output_start >= output.len() {
        return 0.0;
    }

    let len = (input.len() - input_start).min(output.len() - output_start);
    if len == 0 {
        return 0.0;
    }

    let mut dot = 0.0;
    let mut input_energy = 0.0;
    let mut output_energy = 0.0;
    for i in 0..len {
        let input_sample = input[input_start + i] as f64;
        let output_sample = output[output_start + i] as f64;
        dot += input_sample * output_sample;
        input_energy += input_sample * input_sample;
        output_energy += output_sample * output_sample;
    }

    if input_energy <= f64::EPSILON || output_energy <= f64::EPSILON {
        0.0
    } else {
        dot / (input_energy.sqrt() * output_energy.sqrt())
    }
}

#[cfg(feature = "real-backends")]
fn unix_seconds_now() -> Result<u64, Box<dyn Error>> {
    Ok(SystemTime::now()
        .duration_since(SystemTime::UNIX_EPOCH)?
        .as_secs())
}

#[cfg(feature = "real-backends")]
fn git_summary() -> serde_json::Value {
    json!({
        "commit": run_git(&["rev-parse", "--short", "HEAD"]).unwrap_or_else(|| "unknown".to_string()),
        "status_short": run_git(&["status", "--short"]).unwrap_or_else(|| "unknown".to_string()),
    })
}

#[cfg(feature = "real-backends")]
fn run_git(args: &[&str]) -> Option<String> {
    let output = Command::new("git")
        .arg("-c")
        .arg("safe.directory=D:/Dev Projects/Coastal/wavis-public")
        .args(args)
        .output()
        .ok()?;
    if !output.status.success() {
        return None;
    }

    Some(String::from_utf8_lossy(&output.stdout).trim().to_string())
}

#[cfg(feature = "real-backends")]
fn read_wav_mono_48k(path: &Path) -> Result<Vec<f32>, Box<dyn Error>> {
    let mut reader = WavReader::open(path)?;
    let spec = reader.spec();

    if spec.sample_rate != SAMPLE_RATE {
        return Err(format!(
            "{} is {} Hz; export or convert it to a 48 kHz WAV first",
            path.display(),
            spec.sample_rate
        )
        .into());
    }
    if spec.channels == 0 {
        return Err(format!("{} has zero channels", path.display()).into());
    }

    let channels = spec.channels as usize;
    let interleaved = read_interleaved_samples(&mut reader, spec)?;
    if interleaved.len() % channels != 0 {
        return Err(format!(
            "{} has an incomplete interleaved sample frame",
            path.display()
        )
        .into());
    }

    let mut mono = Vec::with_capacity(interleaved.len() / channels);
    for frame in interleaved.chunks_exact(channels) {
        mono.push(frame.iter().sum::<f32>() / channels as f32);
    }

    Ok(mono)
}

#[cfg(feature = "real-backends")]
fn read_interleaved_samples(
    reader: &mut WavReader<std::io::BufReader<std::fs::File>>,
    spec: WavSpec,
) -> Result<Vec<f32>, Box<dyn Error>> {
    match spec.sample_format {
        SampleFormat::Float => reader
            .samples::<f32>()
            .map(|sample| sample.map_err(Into::into))
            .collect(),
        SampleFormat::Int => {
            let max = max_pcm_amplitude(spec.bits_per_sample)?;
            reader
                .samples::<i32>()
                .map(|sample| sample.map(|sample| sample as f32 / max).map_err(Into::into))
                .collect()
        }
    }
}

#[cfg(feature = "real-backends")]
fn max_pcm_amplitude(bits_per_sample: u16) -> Result<f32, Box<dyn Error>> {
    if !(1..=32).contains(&bits_per_sample) {
        return Err(format!("unsupported PCM bit depth: {bits_per_sample}").into());
    }
    let shift = u32::from(bits_per_sample - 1);
    Ok((1u64 << shift) as f32)
}

#[cfg(feature = "real-backends")]
fn write_wav_mono_16(path: &Path, samples: &[f32]) -> Result<(), Box<dyn Error>> {
    let spec = WavSpec {
        channels: 1,
        sample_rate: SAMPLE_RATE,
        bits_per_sample: 16,
        sample_format: SampleFormat::Int,
    };
    let mut writer = WavWriter::create(path, spec)?;
    for sample in samples {
        let sample = sample.clamp(-1.0, 1.0);
        writer.write_sample((sample * i16::MAX as f32) as i16)?;
    }
    writer.finalize()?;
    Ok(())
}

#[cfg(not(feature = "real-backends"))]
fn main() {
    println!("noise_suppression_file requires --features real-backends");
}
