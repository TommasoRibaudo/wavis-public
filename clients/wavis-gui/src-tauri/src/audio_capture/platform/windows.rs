//! Windows audio capture via WASAPI loopback.
//!
//! All COM/WASAPI objects live on the capture thread (not Send). The thread signals
//! init status back to the caller via mpsc before entering the capture loop.
//!
//! Process exclusion: prefers PROCESS_LOOPBACK_MODE_EXCLUDE_TARGET_PROCESS_TREE
//! (Win 10 21H1+) to exclude the WebView2 browser process tree. Falls back to
//! excluding the Tauri main process only when the browser PID cannot be resolved.

use std::sync::atomic::AtomicBool;
use std::sync::Arc;
use std::time::Duration;

use tauri::{AppHandle, Manager, State};

use super::super::audio_capture_state::{
    AudioCaptureState, AudioShareStartResult, WasapiCaptureHandle,
};

pub(super) fn resolve_monitor() -> Result<String, String> {
    Ok("system".to_string())
}

pub(super) fn resolve_monitor_fast() -> Result<String, String> {
    resolve_monitor()
}

/// Start capturing system audio via WASAPI loopback on Windows.
///
/// Opens the default render endpoint in loopback mode, attempts process-specific
/// exclusion (Windows 10 21H1+), and spawns a capture thread that reads 960-sample
/// frames (20ms @ 48kHz mono) and streams them to the JS frontend via Tauri events.
/// The JS side receives PCM frames, feeds them into an AudioWorklet, and publishes
/// the resulting MediaStreamTrack via the LiveKit JS SDK.
pub(super) fn start(
    source_id: String,
    _state: State<'_, crate::media::MediaState>,
    audio_capture: State<'_, AudioCaptureState>,
    app: AppHandle,
) -> Result<AudioShareStartResult, String> {
    use std::sync::atomic::Ordering;

    {
        let guard = audio_capture
            .active
            .lock()
            .map_err(|e| format!("audio capture lock: {e}"))?;
        if guard.is_some() {
            return Err("audio share already in progress".to_string());
        }
    }

    if source_id.is_empty() {
        return Err("audio source not found: (empty source id)".to_string());
    }

    let source_id = resolve_windows_loopback_source_id(&app, &source_id)?;

    // WASAPI/COM objects are thread-affine, so the capture thread owns their lifecycle.
    let stop_flag = Arc::new(AtomicBool::new(false));
    let stop_flag_thread = Arc::clone(&stop_flag);
    let source_id_thread = source_id.clone();
    let source_id_handle = source_id.clone();
    let app_handle = app.clone();
    let (init_tx, init_rx) = std::sync::mpsc::channel::<Result<bool, String>>();

    let capture_thread = std::thread::Builder::new()
        .name("wasapi-audio-capture".into())
        .spawn(move || {
            wasapi_capture_thread(&source_id_thread, &stop_flag_thread, &app_handle, init_tx);
        })
        .map_err(|e| format!("failed to spawn audio capture thread: {e}"))?;

    let loopback_exclusion_active = match init_rx.recv_timeout(Duration::from_secs(5)) {
        Ok(Ok(exclusion)) => exclusion,
        Ok(Err(e)) => {
            stop_flag.store(true, Ordering::Relaxed);
            let _ = capture_thread.join();
            return Err(e);
        }
        Err(_) => {
            stop_flag.store(true, Ordering::Relaxed);
            let _ = capture_thread.join();
            return Err("WASAPI initialization timed out".to_string());
        }
    };

    let handle = WasapiCaptureHandle {
        capture_thread,
        stop_flag,
        source_id: source_id_handle,
        loopback_exclusion_active,
    };

    {
        let mut guard = audio_capture
            .active
            .lock()
            .map_err(|e| format!("audio capture lock: {e}"))?;
        *guard = Some(handle);
    }

    log::info!(
        "[audio_capture] audio_share_start_windows: capturing system audio via WASAPI loopback"
    );

    Ok(AudioShareStartResult {
        loopback_exclusion_available: true,
        real_output_device_id: None,
        real_output_device_name: None,
        requires_mute_for_echo_prevention: false,
    })
}

fn resolve_windows_loopback_source_id(app: &AppHandle, source_id: &str) -> Result<String, String> {
    if source_id != "system" {
        return Ok(source_id.to_string());
    }

    let webview = app.get_webview_window("main").ok_or_else(|| {
        "failed to resolve WebView2 browser process: main webview not found".to_string()
    })?;

    let (tx, rx) = std::sync::mpsc::channel::<Result<u32, String>>();
    webview
        .with_webview(move |webview: tauri::webview::PlatformWebview| {
            let result = (|| -> Result<u32, String> {
                unsafe {
                    let controller = webview.controller();
                    let core = controller
                        .CoreWebView2()
                        .map_err(|e| format!("failed to access CoreWebView2: {e}"))?;
                    let mut pid = 0u32;
                    core.BrowserProcessId(&mut pid)
                        .map_err(|e| format!("failed to get WebView2 browser process ID: {e}"))?;
                    if pid == 0 {
                        Err("WebView2 browser process ID was 0".to_string())
                    } else {
                        Ok(pid)
                    }
                }
            })();

            let _ = tx.send(result);
        })
        .map_err(|e| format!("failed to access main webview: {e}"))?;

    let browser_pid = rx
        .recv_timeout(Duration::from_secs(5))
        .map_err(|_| "timed out resolving WebView2 browser process ID".to_string())??;

    log::info!(
        "[audio_capture] resolved WebView2 browser process ID {browser_pid} for WASAPI self-exclusion"
    );

    Ok(format!("exclude_pid:{browser_pid}"))
}

/// WASAPI capture thread entry point.
///
/// All COM objects are created and used on this thread to satisfy COM
/// threading requirements.
fn wasapi_capture_thread(
    source_id: &str,
    stop_flag: &AtomicBool,
    app: &AppHandle,
    init_tx: std::sync::mpsc::Sender<Result<bool, String>>,
) {
    use windows::Win32::Media::Audio::{
        IAudioCaptureClient, AUDCLNT_SHAREMODE_SHARED, AUDCLNT_STREAMFLAGS_LOOPBACK, WAVEFORMATEX,
    };
    use windows::Win32::System::Com::{CoInitializeEx, COINIT_MULTITHREADED};

    const WAVE_FORMAT_IEEE_FLOAT: u16 = 0x0003;
    const SAMPLE_RATE: u32 = 48_000;
    const CHANNELS: u16 = 1;
    const BITS_PER_SAMPLE: u16 = 32;
    const BLOCK_ALIGN: u16 = CHANNELS * (BITS_PER_SAMPLE / 8);
    const AVG_BYTES_PER_SEC: u32 = SAMPLE_RATE * BLOCK_ALIGN as u32;
    const BUFFER_DURATION: i64 = 200 * 10_000;

    unsafe {
        let _ = CoInitializeEx(None, COINIT_MULTITHREADED);
    }

    let wave_format = WAVEFORMATEX {
        wFormatTag: WAVE_FORMAT_IEEE_FLOAT,
        nChannels: CHANNELS,
        nSamplesPerSec: SAMPLE_RATE,
        nAvgBytesPerSec: AVG_BYTES_PER_SEC,
        nBlockAlign: BLOCK_ALIGN,
        wBitsPerSample: BITS_PER_SAMPLE,
        cbSize: 0,
    };

    enum LoopbackTarget {
        Include(u32),
        Exclude(u32),
        LegacySelfExclude,
    }

    let target = if let Some(pid_str) = source_id.strip_prefix("pid:") {
        match pid_str.parse::<u32>() {
            Ok(pid) => LoopbackTarget::Include(pid),
            Err(_) => {
                let _ = init_tx.send(Err(format!("invalid process ID: {pid_str}")));
                return;
            }
        }
    } else if let Some(pid_str) = source_id.strip_prefix("exclude_pid:") {
        match pid_str.parse::<u32>() {
            Ok(pid) => LoopbackTarget::Exclude(pid),
            Err(_) => {
                let _ = init_tx.send(Err(format!("invalid exclusion process ID: {pid_str}")));
                return;
            }
        }
    } else {
        LoopbackTarget::LegacySelfExclude
    };

    let (audio_client, is_process_loopback) = match target {
        LoopbackTarget::Include(pid) => match activate_process_loopback(pid) {
            Ok(client) => {
                log::info!("[audio_capture] per-process loopback activated for PID {pid}");
                (client, true)
            }
            Err(e) => {
                log::warn!(
                    "[audio_capture] per-process loopback failed for PID {pid}: {e} - \
                     falling back to system-wide loopback"
                );
                match activate_system_loopback() {
                    Ok(client) => (client, false),
                    Err(e2) => {
                        let _ = init_tx.send(Err(format!(
                            "per-process loopback failed ({e}), system fallback also failed: {e2}"
                        )));
                        return;
                    }
                }
            }
        },
        LoopbackTarget::Exclude(pid) => match activate_exclude_process_loopback(pid) {
            Ok(client) => {
                log::info!(
                    "[audio_capture] system loopback with WebView2 self-exclusion activated (PID {pid})"
                );
                (client, true)
            }
            Err(e) => {
                log::warn!(
                    "[audio_capture] WebView2 self-exclusion loopback failed for PID {pid}: {e} - \
                     blocking system audio capture to prevent feedback loop"
                );
                let _ = init_tx.send(Err(format!(
                    "system audio sharing requires WebView2 process exclusion to prevent echo, \
                     but exclusion failed for PID {pid}: {e}"
                )));
                return;
            }
        },
        LoopbackTarget::LegacySelfExclude => match activate_exclude_self_loopback() {
            Ok(client) => {
                log::info!(
                    "[audio_capture] system loopback with legacy self-exclusion activated (PID {})",
                    std::process::id()
                );
                (client, true)
            }
            Err(e) => {
                log::warn!(
                    "[audio_capture] legacy self-exclusion loopback failed: {e} - \
                     blocking system audio capture to prevent feedback loop"
                );
                let _ = init_tx.send(Err(format!(
                    "system audio sharing requires loopback exclusion to prevent echo, \
                     but self-exclusion failed: {e}"
                )));
                return;
            }
        },
    };

    if !is_process_loopback {
        if let Err(e) = unsafe {
            audio_client.Initialize(
                AUDCLNT_SHAREMODE_SHARED,
                AUDCLNT_STREAMFLAGS_LOOPBACK,
                BUFFER_DURATION,
                0,
                &wave_format,
                None,
            )
        } {
            let _ = init_tx.send(Err(format!(
                "failed to initialize audio client in loopback mode: {e}"
            )));
            return;
        }
    }

    let capture_client: IAudioCaptureClient = match unsafe { audio_client.GetService() } {
        Ok(c) => c,
        Err(e) => {
            let _ = init_tx.send(Err(format!("failed to get IAudioCaptureClient: {e}")));
            return;
        }
    };

    if let Err(e) = unsafe { audio_client.Start() } {
        let _ = init_tx.send(Err(format!("failed to start audio client: {e}")));
        return;
    }

    let _ = init_tx.send(Ok(is_process_loopback));

    wasapi_capture_loop(capture_client, &audio_client, stop_flag, app);

    unsafe {
        let _ = audio_client.Stop();
    }
}

fn activate_system_loopback() -> Result<windows::Win32::Media::Audio::IAudioClient, String> {
    use windows::Win32::Media::Audio::{
        eConsole, eRender, IAudioClient, IMMDeviceEnumerator, MMDeviceEnumerator,
    };
    use windows::Win32::System::Com::{CoCreateInstance, CLSCTX_ALL};

    unsafe {
        let enumerator: IMMDeviceEnumerator =
            CoCreateInstance(&MMDeviceEnumerator, None, CLSCTX_ALL)
                .map_err(|e| format!("failed to create MMDeviceEnumerator: {e}"))?;

        let device = enumerator
            .GetDefaultAudioEndpoint(eRender, eConsole)
            .map_err(|e| format!("failed to get default audio endpoint: {e}"))?;

        let audio_client: IAudioClient = device
            .Activate(CLSCTX_ALL, None)
            .map_err(|e| format!("failed to activate IAudioClient: {e}"))?;

        Ok(audio_client)
    }
}

fn activate_process_loopback(
    target_pid: u32,
) -> Result<windows::Win32::Media::Audio::IAudioClient, String> {
    activate_process_loopback_with_mode(
        target_pid,
        windows::Win32::Media::Audio::PROCESS_LOOPBACK_MODE_INCLUDE_TARGET_PROCESS_TREE,
    )
}

fn activate_exclude_self_loopback() -> Result<windows::Win32::Media::Audio::IAudioClient, String> {
    activate_exclude_process_loopback(std::process::id())
}

fn activate_exclude_process_loopback(
    target_pid: u32,
) -> Result<windows::Win32::Media::Audio::IAudioClient, String> {
    activate_process_loopback_with_mode(
        target_pid,
        windows::Win32::Media::Audio::PROCESS_LOOPBACK_MODE_EXCLUDE_TARGET_PROCESS_TREE,
    )
}

fn activate_process_loopback_with_mode(
    target_pid: u32,
    mode: windows::Win32::Media::Audio::PROCESS_LOOPBACK_MODE,
) -> Result<windows::Win32::Media::Audio::IAudioClient, String> {
    use std::sync::{Arc, Condvar, Mutex};

    use windows::core::{IUnknown, Interface, GUID, HRESULT};
    use windows::Win32::Media::Audio::{
        ActivateAudioInterfaceAsync, IActivateAudioInterfaceAsyncOperation,
        IActivateAudioInterfaceCompletionHandler, IAudioClient, AUDIOCLIENT_ACTIVATION_PARAMS,
        AUDIOCLIENT_ACTIVATION_PARAMS_0, AUDIOCLIENT_ACTIVATION_TYPE_PROCESS_LOOPBACK,
        AUDIOCLIENT_PROCESS_LOOPBACK_PARAMS, VIRTUAL_AUDIO_DEVICE_PROCESS_LOOPBACK,
    };

    #[derive(Clone)]
    struct CompletionState {
        done: Arc<(Mutex<bool>, Condvar)>,
        result: Arc<Mutex<Option<Result<IAudioClient, String>>>>,
    }

    #[repr(C)]
    struct CompletionHandler {
        vtable: *const CompletionHandlerVtbl,
        ref_count: std::sync::atomic::AtomicU32,
        state: CompletionState,
    }

    #[repr(C)]
    struct CompletionHandlerVtbl {
        query_interface: unsafe extern "system" fn(
            *mut std::ffi::c_void,
            *const GUID,
            *mut *mut std::ffi::c_void,
        ) -> HRESULT,
        add_ref: unsafe extern "system" fn(*mut std::ffi::c_void) -> u32,
        release: unsafe extern "system" fn(*mut std::ffi::c_void) -> u32,
        activate_completed:
            unsafe extern "system" fn(*mut std::ffi::c_void, *mut std::ffi::c_void) -> HRESULT,
    }

    static VTABLE: CompletionHandlerVtbl = CompletionHandlerVtbl {
        query_interface: handler_query_interface,
        add_ref: handler_add_ref,
        release: handler_release,
        activate_completed: handler_activate_completed,
    };

    const IID_IAGILE_OBJECT: GUID = GUID::from_u128(0x94ea2b94_e9cc_49e0_c0ff_ee64ca8f5b90);

    unsafe extern "system" fn handler_query_interface(
        this: *mut std::ffi::c_void,
        iid: *const GUID,
        ppv: *mut *mut std::ffi::c_void,
    ) -> HRESULT {
        let iid = &*iid;
        if *iid == IActivateAudioInterfaceCompletionHandler::IID
            || *iid == IUnknown::IID
            || *iid == IID_IAGILE_OBJECT
        {
            handler_add_ref(this);
            *ppv = this;
            HRESULT(0)
        } else {
            *ppv = std::ptr::null_mut();
            HRESULT(0x80004002_u32 as i32)
        }
    }

    unsafe extern "system" fn handler_add_ref(this: *mut std::ffi::c_void) -> u32 {
        let handler = &*(this as *const CompletionHandler);
        handler
            .ref_count
            .fetch_add(1, std::sync::atomic::Ordering::Relaxed)
            + 1
    }

    unsafe extern "system" fn handler_release(this: *mut std::ffi::c_void) -> u32 {
        let handler = &*(this as *const CompletionHandler);
        let prev = handler
            .ref_count
            .fetch_sub(1, std::sync::atomic::Ordering::Release);
        if prev == 1 {
            std::sync::atomic::fence(std::sync::atomic::Ordering::Acquire);
            drop(Box::from_raw(this as *mut CompletionHandler));
        }
        prev - 1
    }

    unsafe extern "system" fn handler_activate_completed(
        this: *mut std::ffi::c_void,
        operation_raw: *mut std::ffi::c_void,
    ) -> HRESULT {
        let handler = &*(this as *const CompletionHandler);

        let result = (|| -> Result<IAudioClient, String> {
            let operation: IActivateAudioInterfaceAsyncOperation =
                IActivateAudioInterfaceAsyncOperation::from_raw_borrowed(
                    &(operation_raw as *mut _),
                )
                .ok_or_else(|| "null operation pointer".to_string())?
                .clone();

            let mut hr = HRESULT(0);
            let mut activated: Option<IUnknown> = None;

            operation
                .GetActivateResult(&mut hr, &mut activated)
                .map_err(|e| format!("GetActivateResult failed: {e}"))?;

            hr.ok()
                .map_err(|e| format!("activation HRESULT error: {e}"))?;

            let unknown =
                activated.ok_or_else(|| "activation returned null interface".to_string())?;

            let client: IAudioClient = unknown
                .cast()
                .map_err(|e| format!("cast to IAudioClient failed: {e}"))?;

            Ok(client)
        })();

        if let Ok(mut r) = handler.state.result.lock() {
            *r = Some(result);
        }
        let (lock, cvar) = &*handler.state.done;
        if let Ok(mut done) = lock.lock() {
            *done = true;
            cvar.notify_one();
        }

        HRESULT(0)
    }

    let loopback_params = AUDIOCLIENT_PROCESS_LOOPBACK_PARAMS {
        TargetProcessId: target_pid,
        ProcessLoopbackMode: mode,
    };

    let activation_params = AUDIOCLIENT_ACTIVATION_PARAMS {
        ActivationType: AUDIOCLIENT_ACTIVATION_TYPE_PROCESS_LOOPBACK,
        Anonymous: AUDIOCLIENT_ACTIVATION_PARAMS_0 {
            ProcessLoopbackParams: loopback_params,
        },
    };

    let params_ptr = &activation_params as *const AUDIOCLIENT_ACTIVATION_PARAMS as *const u8;
    let params_size = std::mem::size_of::<AUDIOCLIENT_ACTIVATION_PARAMS>() as u32;

    #[cfg(target_pointer_width = "64")]
    const PROPVARIANT_SIZE: usize = 24;
    #[cfg(target_pointer_width = "32")]
    const PROPVARIANT_SIZE: usize = 16;

    let mut raw_pv = [0u8; PROPVARIANT_SIZE];
    raw_pv[0] = 0x41;
    raw_pv[1] = 0x00;
    raw_pv[8..12].copy_from_slice(&params_size.to_ne_bytes());
    let ptr_bytes = (params_ptr as usize).to_ne_bytes();
    #[cfg(target_pointer_width = "64")]
    raw_pv[16..24].copy_from_slice(&ptr_bytes);
    #[cfg(target_pointer_width = "32")]
    raw_pv[12..16].copy_from_slice(&ptr_bytes);

    let pv_ptr = raw_pv.as_ptr() as *const windows::core::PROPVARIANT;

    #[allow(clippy::arc_with_non_send_sync)]
    let completion_state = CompletionState {
        done: Arc::new((Mutex::new(false), Condvar::new())),
        result: Arc::new(Mutex::new(None)),
    };

    let handler: IActivateAudioInterfaceCompletionHandler = unsafe {
        let boxed = Box::into_raw(Box::new(CompletionHandler {
            vtable: &VTABLE,
            ref_count: std::sync::atomic::AtomicU32::new(1),
            state: completion_state.clone(),
        }));
        IActivateAudioInterfaceCompletionHandler::from_raw(boxed as *mut std::ffi::c_void)
    };

    let _operation = unsafe {
        ActivateAudioInterfaceAsync(
            VIRTUAL_AUDIO_DEVICE_PROCESS_LOOPBACK,
            &IAudioClient::IID,
            Some(pv_ptr),
            &handler,
        )
        .map_err(|e| format!("ActivateAudioInterfaceAsync failed: {e}"))?
    };

    let (lock, cvar) = &*completion_state.done;
    let guard = lock.lock().map_err(|e| format!("lock: {e}"))?;
    let result = cvar
        .wait_timeout_while(guard, Duration::from_secs(5), |done| !*done)
        .map_err(|e| format!("wait: {e}"))?;

    if !*result.0 {
        return Err("ActivateAudioInterfaceAsync timed out".to_string());
    }

    let client = completion_state
        .result
        .lock()
        .map_err(|e| format!("lock: {e}"))?
        .take()
        .ok_or_else(|| "no result from activation".to_string())??;

    let wave_format = windows::Win32::Media::Audio::WAVEFORMATEX {
        wFormatTag: 0x0003,
        nChannels: 1,
        nSamplesPerSec: 48_000,
        nAvgBytesPerSec: 48_000 * 4,
        nBlockAlign: 4,
        wBitsPerSample: 32,
        cbSize: 0,
    };

    unsafe {
        client
            .Initialize(
                windows::Win32::Media::Audio::AUDCLNT_SHAREMODE_SHARED,
                windows::Win32::Media::Audio::AUDCLNT_STREAMFLAGS_LOOPBACK
                    | windows::Win32::Media::Audio::AUDCLNT_STREAMFLAGS_AUTOCONVERTPCM
                    | windows::Win32::Media::Audio::AUDCLNT_STREAMFLAGS_SRC_DEFAULT_QUALITY,
                200 * 10_000,
                0,
                &wave_format,
                None,
            )
            .map_err(|e| format!("failed to initialize per-process audio client: {e}"))?;
    }

    Ok(client)
}

fn wasapi_capture_loop(
    capture_client: windows::Win32::Media::Audio::IAudioCaptureClient,
    _audio_client: &windows::Win32::Media::Audio::IAudioClient,
    stop_flag: &AtomicBool,
    app: &AppHandle,
) {
    use std::sync::atomic::Ordering;

    use tauri::Emitter;

    const FRAME_SAMPLES: usize = 960;

    log::info!("[audio_capture] WASAPI capture loop started");

    let mut accum: Vec<i16> = Vec::with_capacity(FRAME_SAMPLES * 2);

    loop {
        if stop_flag.load(Ordering::Relaxed) {
            break;
        }

        std::thread::sleep(Duration::from_millis(20));

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
                const AUDCLNT_E_DEVICE_INVALIDATED: i32 = 0x88890004_u32 as i32;
                if e.code().0 == AUDCLNT_E_DEVICE_INVALIDATED {
                    log::warn!(
                        "[audio_capture] Default audio device changed - stopping WASAPI capture"
                    );
                    let _ = app.emit(
                        "wasapi_audio_stopped",
                        serde_json::json!({
                            "reason": "Audio device changed"
                        }),
                    );
                    let _ = app.emit(
                        "share_error",
                        serde_json::json!({
                            "message": "Audio device changed \u{2014} audio share stopped"
                        }),
                    );
                    return;
                }
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
                accum.extend(std::iter::repeat_n(0i16, sample_count));
                unsafe {
                    let _ = capture_client.ReleaseBuffer(num_frames);
                }
                continue;
            }

            let float_slice =
                unsafe { std::slice::from_raw_parts(buffer_ptr as *const f32, sample_count) };

            for &sample in float_slice {
                let clamped = sample.clamp(-1.0, 1.0);
                let i16_val = (clamped * i16::MAX as f32) as i16;
                accum.push(i16_val);
            }

            unsafe {
                let _ = capture_client.ReleaseBuffer(num_frames);
            }
        }

        while accum.len() >= FRAME_SAMPLES {
            let frame: Vec<i16> = accum.drain(..FRAME_SAMPLES).collect();
            let bytes: Vec<u8> = frame.iter().flat_map(|s| s.to_le_bytes()).collect();
            let b64 = base64_encode(&bytes);

            if let Err(e) = app.emit("wasapi_audio_frame", &b64) {
                log::warn!("[audio_capture] emit wasapi_audio_frame failed: {e}");
                let _ = app.emit(
                    "wasapi_audio_stopped",
                    serde_json::json!({ "reason": "Event emission failed" }),
                );
                return;
            }
        }
    }

    log::info!("[audio_capture] WASAPI capture loop stopped");
}

/// Base64-encode a byte slice using the existing `base64` dependency.
fn base64_encode(input: &[u8]) -> String {
    use base64::Engine;

    base64::engine::general_purpose::STANDARD.encode(input)
}

/// Stop the active audio capture session on Windows.
///
/// Takes the `WasapiCaptureHandle` from state, signals the capture thread to
/// stop, and waits up to 3 seconds for the thread to join.
pub(super) fn stop(
    _state: State<'_, crate::media::MediaState>,
    audio_capture: State<'_, AudioCaptureState>,
) -> Result<(), String> {
    use std::sync::atomic::Ordering;

    let handle = {
        let mut guard = audio_capture
            .active
            .lock()
            .map_err(|e| format!("audio capture lock: {e}"))?;
        guard.take()
    };

    let Some(handle) = handle else {
        return Ok(());
    };

    // SeqCst ensures the stop signal is visible before the join wait begins.
    handle.stop_flag.store(true, Ordering::SeqCst);

    let (done_tx, done_rx) = std::sync::mpsc::channel::<()>();
    let join_thread = std::thread::Builder::new()
        .name("wasapi-join".into())
        .spawn(move || {
            let result = handle.capture_thread.join();
            if result.is_err() {
                log::warn!("[audio_capture] WASAPI capture thread panicked during join");
            }
            let _ = done_tx.send(());
        });

    match join_thread {
        Ok(_) => {
            if done_rx.recv_timeout(Duration::from_secs(3)).is_err() {
                log::warn!(
                    "[audio_capture] WASAPI capture thread join timed out after 3s, continuing cleanup"
                );
            }
        }
        Err(e) => {
            log::warn!("[audio_capture] failed to spawn join thread: {e}, skipping thread join");
        }
    }

    log::info!("[audio_capture] audio_share_stop_windows: capture stopped and cleaned up");

    Ok(())
}
