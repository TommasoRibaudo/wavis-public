//! Windows WASAPI microphone capture with Rust-side noise suppression.
//!
//! Captures from the default (or named) capture endpoint at 48 kHz mono f32,
//! processes every 960-sample frame (20 ms) through the shared Rust
//! `DenoiseFilter` (RNNoise + tuned gate), and emits base64 i16 LE PCM to
//! the JS frontend via the `native_mic_frame` Tauri event.
//!
//! Data flow:
//!   WASAPI mic capture (f32, 48 kHz, mono)
//!     → accumulate 960 samples
//!     → DenoiseFilter::process() — RNNoise + post-denoise gate
//!     → f32 → i16 LE
//!     → base64
//!     → Tauri event "native_mic_frame"
//!     → JS NativeMicBridge → AudioWorklet ring buffer → MediaStreamTrack
//!     → LiveKit publishTrack(track, { source: Microphone })
//!
//! The Rust filter is the same `DenoiseFilter` that already works on Linux.
//! Speech-preservation tuning (slower close, gain ramps, close-hold) is
//! inherited — do NOT reimplement it here.
//!
//! Device selection: v1 always opens the default capture endpoint. The
//! `native_mic_set_input_device` command accepts a device_id but currently
//! just restarts with the default device. Proper WASAPI device enumeration
//! by friendly name is deferred to a follow-up.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;
use std::time::Duration;

use tauri::{AppHandle, Emitter, State};
use wavis_client_shared::denoise_filter::DenoiseFilter;

const LOG: &str = "[native_mic]";
/// 960 samples = 20 ms at 48 kHz mono — matches DenoiseFilter::process() contract.
const FRAME_SAMPLES: usize = 960;

// ─── State ──────────────────────────────────────────────────────────────────

struct NativeMicHandle {
    stop_flag: Arc<AtomicBool>,
    thread: JoinHandle<()>,
    filter: Arc<DenoiseFilter>,
}

pub struct NativeMicState {
    inner: Mutex<Option<NativeMicHandle>>,
}

impl NativeMicState {
    pub fn new() -> Self {
        Self {
            inner: Mutex::new(None),
        }
    }
}

// ─── Tauri Commands ─────────────────────────────────────────────────────────

/// Start the native mic capture bridge.
/// Idempotent: stops any existing session before starting a new one.
#[tauri::command]
pub fn native_mic_start(
    denoise_enabled: bool,
    device_id: Option<String>,
    state: State<'_, NativeMicState>,
    app: AppHandle,
) -> Result<(), String> {
    stop_inner(&state)?;

    let filter = Arc::new(DenoiseFilter::new(denoise_enabled));
    let stop_flag = Arc::new(AtomicBool::new(false));

    let filter_thread = Arc::clone(&filter);
    let stop_flag_thread = Arc::clone(&stop_flag);
    let app_handle = app.clone();
    let device_id_thread = device_id.clone();

    let thread = std::thread::Builder::new()
        .name("native-mic-capture".into())
        .spawn(move || {
            native_mic_capture_thread(
                device_id_thread.as_deref(),
                &stop_flag_thread,
                &filter_thread,
                &app_handle,
            );
        })
        .map_err(|e| format!("failed to spawn native mic capture thread: {e}"))?;

    let mut guard = state.inner.lock().map_err(|e| format!("lock: {e}"))?;
    *guard = Some(NativeMicHandle {
        stop_flag,
        thread,
        filter,
    });

    log::info!(
        "{LOG} started (denoise={denoise_enabled}, device={:?})",
        device_id
    );
    Ok(())
}

/// Stop the native mic capture bridge.
#[tauri::command]
pub fn native_mic_stop(state: State<'_, NativeMicState>) -> Result<(), String> {
    stop_inner(&state)
}

/// Toggle noise suppression on the running session without restart.
/// If enabling, also resets the RNNoise/gate state to avoid artifacts
/// from stale GRU state accumulated while the filter was bypassed.
#[tauri::command]
pub fn native_mic_set_denoise_enabled(
    enabled: bool,
    state: State<'_, NativeMicState>,
) -> Result<(), String> {
    let guard = state.inner.lock().map_err(|e| format!("lock: {e}"))?;
    if let Some(handle) = guard.as_ref() {
        if enabled {
            handle.filter.reset_state();
        }
        handle.filter.set_enabled(enabled);
        log::info!("{LOG} denoise toggled to {enabled}");
    }
    Ok(())
}

/// Switch to a different input device. Preserves the current denoise state
/// across the restart.
///
/// NOTE (v1 limitation): device_id is accepted for API compatibility but
/// the restart always opens the default capture endpoint. Full device
/// selection via WASAPI property-store enumeration is deferred.
#[tauri::command]
pub fn native_mic_set_input_device(
    device_id: String,
    state: State<'_, NativeMicState>,
    app: AppHandle,
) -> Result<(), String> {
    let denoise_enabled = {
        let guard = state.inner.lock().map_err(|e| format!("lock: {e}"))?;
        guard.as_ref().is_none_or(|h| h.filter.is_enabled())
    };
    stop_inner(&state)?;
    native_mic_start(denoise_enabled, Some(device_id), state, app)
}

// ─── Internal helpers ────────────────────────────────────────────────────────

fn stop_inner(state: &State<'_, NativeMicState>) -> Result<(), String> {
    let handle = {
        let mut guard = state.inner.lock().map_err(|e| format!("lock: {e}"))?;
        guard.take()
    };
    let Some(handle) = handle else {
        return Ok(());
    };
    handle.stop_flag.store(true, Ordering::SeqCst);
    // Join with a timeout on a helper thread so stop() is non-blocking.
    let (done_tx, done_rx) = std::sync::mpsc::channel::<()>();
    let _ = std::thread::Builder::new()
        .name("native-mic-join".into())
        .spawn(move || {
            let _ = handle.thread.join();
            let _ = done_tx.send(());
        });
    match done_rx.recv_timeout(Duration::from_secs(3)) {
        Ok(()) => log::info!("{LOG} capture thread joined"),
        Err(_) => log::warn!("{LOG} capture thread join timed out"),
    }
    Ok(())
}

fn base64_encode(input: &[u8]) -> String {
    use base64::Engine;
    base64::engine::general_purpose::STANDARD.encode(input)
}

// ─── Capture thread ──────────────────────────────────────────────────────────

fn native_mic_capture_thread(
    _device_id: Option<&str>,
    stop_flag: &AtomicBool,
    filter: &DenoiseFilter,
    app: &AppHandle,
) {
    use windows::Win32::Media::Audio::{
        eCapture, eConsole, IAudioCaptureClient, IAudioClient, IMMDeviceEnumerator,
        MMDeviceEnumerator, AUDCLNT_SHAREMODE_SHARED, AUDCLNT_STREAMFLAGS_AUTOCONVERTPCM,
        AUDCLNT_STREAMFLAGS_SRC_DEFAULT_QUALITY, WAVEFORMATEX,
    };
    use windows::Win32::System::Com::{
        CoCreateInstance, CoInitializeEx, CLSCTX_ALL, COINIT_MULTITHREADED,
    };

    const WAVE_FORMAT_IEEE_FLOAT: u16 = 0x0003;
    const SAMPLE_RATE: u32 = 48_000;
    const CHANNELS: u16 = 1;
    const BITS_PER_SAMPLE: u16 = 32;
    const BLOCK_ALIGN: u16 = CHANNELS * (BITS_PER_SAMPLE / 8);
    const AVG_BYTES_PER_SEC: u32 = SAMPLE_RATE * BLOCK_ALIGN as u32;
    const BUFFER_DURATION: i64 = 200 * 10_000; // 200 ms in 100-ns units

    unsafe {
        let _ = CoInitializeEx(None, COINIT_MULTITHREADED);
    }

    let audio_client: IAudioClient = unsafe {
        let enumerator: IMMDeviceEnumerator =
            match CoCreateInstance(&MMDeviceEnumerator, None, CLSCTX_ALL) {
                Ok(e) => e,
                Err(e) => {
                    log::error!("{LOG} CoCreateInstance MMDeviceEnumerator failed: {e}");
                    let _ = app.emit(
                        "native_mic_stopped",
                        serde_json::json!({ "reason": e.to_string() }),
                    );
                    return;
                }
            };

        let device = match enumerator.GetDefaultAudioEndpoint(eCapture, eConsole) {
            Ok(d) => d,
            Err(e) => {
                log::error!("{LOG} GetDefaultAudioEndpoint(eCapture) failed: {e}");
                let _ = app.emit(
                    "native_mic_stopped",
                    serde_json::json!({ "reason": e.to_string() }),
                );
                return;
            }
        };

        match device.Activate::<IAudioClient>(CLSCTX_ALL, None) {
            Ok(c) => c,
            Err(e) => {
                log::error!("{LOG} IAudioClient Activate failed: {e}");
                let _ = app.emit(
                    "native_mic_stopped",
                    serde_json::json!({ "reason": e.to_string() }),
                );
                return;
            }
        }
    };

    let wave_format = WAVEFORMATEX {
        wFormatTag: WAVE_FORMAT_IEEE_FLOAT,
        nChannels: CHANNELS,
        nSamplesPerSec: SAMPLE_RATE,
        nAvgBytesPerSec: AVG_BYTES_PER_SEC,
        nBlockAlign: BLOCK_ALIGN,
        wBitsPerSample: BITS_PER_SAMPLE,
        cbSize: 0,
    };

    if let Err(e) = unsafe {
        audio_client.Initialize(
            AUDCLNT_SHAREMODE_SHARED,
            AUDCLNT_STREAMFLAGS_AUTOCONVERTPCM | AUDCLNT_STREAMFLAGS_SRC_DEFAULT_QUALITY,
            BUFFER_DURATION,
            0,
            &wave_format,
            None,
        )
    } {
        log::error!("{LOG} IAudioClient::Initialize failed: {e}");
        let _ = app.emit(
            "native_mic_stopped",
            serde_json::json!({ "reason": e.to_string() }),
        );
        return;
    }

    let capture_client: IAudioCaptureClient = match unsafe { audio_client.GetService() } {
        Ok(c) => c,
        Err(e) => {
            log::error!("{LOG} GetService IAudioCaptureClient failed: {e}");
            let _ = app.emit(
                "native_mic_stopped",
                serde_json::json!({ "reason": e.to_string() }),
            );
            return;
        }
    };

    if let Err(e) = unsafe { audio_client.Start() } {
        log::error!("{LOG} IAudioClient::Start failed: {e}");
        let _ = app.emit(
            "native_mic_stopped",
            serde_json::json!({ "reason": e.to_string() }),
        );
        return;
    }

    log::info!("{LOG} WASAPI mic capture loop started");

    native_mic_capture_loop(&capture_client, stop_flag, filter, app);

    unsafe {
        let _ = audio_client.Stop();
    }
    log::info!("{LOG} WASAPI mic capture loop stopped");
}

fn native_mic_capture_loop(
    capture_client: &windows::Win32::Media::Audio::IAudioCaptureClient,
    stop_flag: &AtomicBool,
    filter: &DenoiseFilter,
    app: &AppHandle,
) {
    // f32 accumulator — drain in 960-sample (20 ms) chunks for denoise.
    let mut f32_accum: Vec<f32> = Vec::with_capacity(FRAME_SAMPLES * 2);

    loop {
        if stop_flag.load(Ordering::Relaxed) {
            break;
        }

        // 10 ms sleep keeps latency low while avoiding busy-spin.
        std::thread::sleep(Duration::from_millis(10));

        // Drain all available WASAPI buffers in one pass.
        loop {
            if stop_flag.load(Ordering::Relaxed) {
                break;
            }

            let mut buffer_ptr: *mut u8 = std::ptr::null_mut();
            let mut num_frames: u32 = 0;
            let mut flags: u32 = 0;

            let hr = unsafe {
                capture_client.GetBuffer(&mut buffer_ptr, &mut num_frames, &mut flags, None, None)
            };

            if let Err(e) = hr {
                log::warn!("{LOG} GetBuffer error: {e}");
                break;
            }

            if num_frames == 0 {
                unsafe {
                    let _ = capture_client.ReleaseBuffer(0);
                }
                break;
            }

            let sample_count = num_frames as usize;
            const AUDCLNT_BUFFERFLAGS_SILENT: u32 = 0x2;

            if buffer_ptr.is_null() || (flags & AUDCLNT_BUFFERFLAGS_SILENT != 0) {
                // Silent frame — push zeros into accumulator so timing stays correct.
                f32_accum.extend(std::iter::repeat_n(0.0f32, sample_count));
                unsafe {
                    let _ = capture_client.ReleaseBuffer(num_frames);
                }
                continue;
            }

            let float_slice =
                unsafe { std::slice::from_raw_parts(buffer_ptr as *const f32, sample_count) };
            f32_accum.extend_from_slice(float_slice);
            unsafe {
                let _ = capture_client.ReleaseBuffer(num_frames);
            }
        }

        // Drain complete 960-sample frames: denoise in-place, convert to i16, emit.
        while f32_accum.len() >= FRAME_SAMPLES {
            let mut frame: Vec<f32> = f32_accum.drain(..FRAME_SAMPLES).collect();

            // DenoiseFilter::process takes f32 in [-1.0, 1.0] and handles i16
            // scaling internally. When disabled it passes through unmodified.
            filter.process(&mut frame);

            let bytes: Vec<u8> = frame
                .iter()
                .flat_map(|&s| {
                    let i16_val = (s.clamp(-1.0, 1.0) * i16::MAX as f32) as i16;
                    i16_val.to_le_bytes()
                })
                .collect();

            let b64 = base64_encode(&bytes);

            if let Err(e) = app.emit("native_mic_frame", &b64) {
                log::warn!("{LOG} emit native_mic_frame failed: {e}");
                let _ = app.emit(
                    "native_mic_stopped",
                    serde_json::json!({ "reason": "Event emission failed" }),
                );
                return;
            }
        }
    }
}
