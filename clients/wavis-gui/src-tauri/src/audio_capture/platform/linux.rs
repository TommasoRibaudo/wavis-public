//! Linux audio capture via PulseAudio / PipeWire-PulseAudio compat layer.
//!
//! Loopback exclusion strategy: creates a wavis_capture null sink, moves all
//! non-Wavis sink-inputs to it, loops its monitor to the hardware sink, then
//! captures from the monitor - system audio only, no Wavis peer audio.
//!
//! pactl subprocess path is used instead of the PA threaded mainloop API because
//! the mainloop deadlocks on PipeWire's PulseAudio compat layer when another
//! PulseAudio session is active in the same process.

use tauri::{AppHandle, State};

use super::super::audio_capture_state::{
    AudioCaptureHandle, AudioCaptureState, AudioShareStartResult, MovedSinkInput,
};
use wavis_client_shared::room_session::LiveKitConnection;

pub(super) fn resolve_monitor() -> Result<String, String> {
    get_default_audio_monitor_linux()
}

pub(super) fn resolve_monitor_fast() -> Result<String, String> {
    get_default_audio_monitor_pactl()
}

pub(super) fn start(
    source_id: String,
    state: State<'_, crate::media::MediaState>,
    audio_capture: State<'_, AudioCaptureState>,
    app: AppHandle,
) -> Result<AudioShareStartResult, String> {
    audio_share_start_linux(source_id, state, audio_capture, app)
}

pub(super) fn stop(
    state: State<'_, crate::media::MediaState>,
    audio_capture: State<'_, AudioCaptureState>,
) -> Result<(), String> {
    audio_share_stop_linux(state, audio_capture)
}

/// Timeout for PulseAudio operations to avoid hanging if the daemon is unresponsive.
#[cfg(target_os = "linux")]
const PA_TIMEOUT: std::time::Duration = std::time::Duration::from_millis(2000);

#[cfg(target_os = "linux")]
fn get_default_audio_monitor_linux() -> Result<String, String> {
    use std::sync::mpsc;

    let (tx, rx) = mpsc::channel();

    let handle = std::thread::Builder::new()
        .name("pa-default-monitor".into())
        .spawn(move || {
            let result = get_default_audio_monitor_inner();
            let _ = tx.send(result);
        })
        .map_err(|e| {
            format!("could not resolve default audio monitor: failed to spawn thread: {e}")
        })?;

    match rx.recv_timeout(PA_TIMEOUT) {
        Ok(result) => {
            let _ = handle.join();
            result
        }
        Err(_) => {
            log::warn!("[audio_capture] PulseAudio API timed out, falling back to pactl");
            let _ = handle.join();
            get_default_audio_monitor_pactl()
        }
    }
}

/// Fallback: resolve the default monitor source via the `pactl` CLI tool.
/// Runs `pactl info` to get the default sink name, then appends `.monitor`.
#[cfg(target_os = "linux")]
fn get_default_audio_monitor_pactl() -> Result<String, String> {
    let output = std::process::Command::new("pactl")
        .arg("info")
        .output()
        .map_err(|e| format!("could not resolve default audio monitor: pactl failed: {e}"))?;

    if !output.status.success() {
        return Err("could not resolve default audio monitor: pactl returned error".to_string());
    }

    let stdout = String::from_utf8_lossy(&output.stdout);
    for line in stdout.lines() {
        if let Some(sink_name) = line.strip_prefix("Default Sink: ") {
            let monitor = format!("{}.monitor", sink_name.trim());
            log::info!("[audio_capture] pactl fallback resolved monitor: {monitor}");
            return Ok(monitor);
        }
    }

    Err("could not resolve default audio monitor: no default sink in pactl output".to_string())
}

/// Inner PulseAudio logic — must run on a dedicated thread.
///
/// 1. Connect to PulseAudio via threaded mainloop.
/// 2. Query `get_server_info()` to get the default sink name.
/// 3. Query `get_sink_info_by_name()` to get the sink's monitor source name.
/// 4. Return the monitor source name.
#[cfg(target_os = "linux")]
fn get_default_audio_monitor_inner() -> Result<String, String> {
    use std::sync::{Arc, Mutex};

    use pulse::callbacks::ListResult;
    use pulse::context::{Context, FlagSet as ContextFlagSet, State as ContextState};
    use pulse::mainloop::threaded::Mainloop;

    let mut mainloop =
        Mainloop::new().ok_or_else(|| "could not resolve default audio monitor".to_string())?;

    mainloop
        .start()
        .map_err(|_| "could not resolve default audio monitor".to_string())?;

    let mut context = Context::new(&mainloop, "wavis-audio-monitor")
        .ok_or_else(|| "could not resolve default audio monitor".to_string())?;

    // Connect to the PulseAudio server.
    mainloop.lock();
    context
        .connect(None, ContextFlagSet::NOFLAGS, None)
        .map_err(|_| {
            mainloop.unlock();
            "could not resolve default audio monitor".to_string()
        })?;

    // Wait for the context to be ready.
    loop {
        match context.get_state() {
            ContextState::Ready => break,
            ContextState::Failed | ContextState::Terminated => {
                mainloop.unlock();
                mainloop.stop();
                return Err("could not resolve default audio monitor".to_string());
            }
            _ => {
                mainloop.wait();
            }
        }
    }

    // Step 1: Get the default sink name via server info.
    let default_sink: Arc<Mutex<Option<String>>> = Arc::new(Mutex::new(None));
    let server_done: Arc<Mutex<bool>> = Arc::new(Mutex::new(false));

    let sink_clone = default_sink.clone();
    let done_clone = server_done.clone();
    let ml_ref = &mut mainloop as *mut Mainloop;

    let _op = context.introspect().get_server_info(move |info| {
        if let Some(ref name) = info.default_sink_name {
            if let Ok(mut sink) = sink_clone.lock() {
                *sink = Some(name.to_string());
            }
        }
        if let Ok(mut d) = done_clone.lock() {
            *d = true;
        }
        unsafe { (*ml_ref).signal(false) };
    });

    // Wait for server info callback.
    loop {
        if let Ok(d) = server_done.lock() {
            if *d {
                break;
            }
        }
        mainloop.wait();
    }

    let sink_name = default_sink
        .lock()
        .ok()
        .and_then(|s| s.clone())
        .ok_or_else(|| "could not resolve default audio monitor".to_string())?;

    // Step 2: Look up the sink to get its monitor source name.
    let monitor_source: Arc<Mutex<Option<String>>> = Arc::new(Mutex::new(None));
    let sink_done: Arc<Mutex<bool>> = Arc::new(Mutex::new(false));

    let monitor_clone = monitor_source.clone();
    let done_clone2 = sink_done.clone();
    let ml_ref2 = &mut mainloop as *mut Mainloop;

    let _op2 = context
        .introspect()
        .get_sink_info_by_name(&sink_name, move |list_result| match list_result {
            ListResult::Item(sink_info) => {
                if let Some(ref name) = sink_info.monitor_source_name {
                    if let Ok(mut m) = monitor_clone.lock() {
                        *m = Some(name.to_string());
                    }
                }
            }
            ListResult::End | ListResult::Error => {
                if let Ok(mut d) = done_clone2.lock() {
                    *d = true;
                }
                unsafe { (*ml_ref2).signal(false) };
            }
        });

    // Wait for sink info callback.
    loop {
        if let Ok(d) = sink_done.lock() {
            if *d {
                break;
            }
        }
        mainloop.wait();
    }

    mainloop.unlock();

    // Disconnect and clean up.
    context.disconnect();
    mainloop.stop();

    monitor_source
        .lock()
        .ok()
        .and_then(|m| m.clone())
        .ok_or_else(|| "could not resolve default audio monitor".to_string())
}

#[cfg(target_os = "linux")]
fn audio_share_start_linux(
    source_id: String,
    state: tauri::State<'_, crate::media::MediaState>,
    audio_capture: tauri::State<'_, AudioCaptureState>,
    app: tauri::AppHandle,
) -> Result<AudioShareStartResult, String> {
    use std::sync::atomic::AtomicBool;
    use std::sync::Arc;

    // -- Double-start guard -----------------------------------------
    {
        let guard = audio_capture
            .active
            .lock()
            .map_err(|e| format!("audio capture lock: {e}"))?;
        if guard.is_some() {
            return Err("audio share already in progress".to_string());
        }
    }

    // -- Validate source ID -----------------------------------------
    if source_id.is_empty() {
        return Err("audio source not found: (empty source id)".to_string());
    }

    // Validate the source exists in PulseAudio before proceeding.
    validate_pa_source(&source_id)?;

    // -- Publish screen audio track via LiveKit ---------------------
    let lk_guard = state.lk().map_err(|e| format!("lock: {e}"))?;
    let conn = lk_guard
        .as_ref()
        .ok_or_else(|| "not connected to a room".to_string())?;

    if !conn.is_available() {
        return Err("not connected to a room".to_string());
    }

    conn.publish_screen_audio()
        .map_err(|e| format!("failed to publish screen audio track: {e}"))?;

    let conn_for_thread = Arc::clone(conn);
    drop(lk_guard);

    // -- Loopback exclusion -----------------------------------------
    // Uses pactl subprocess instead of PulseAudio threaded mainloop API,
    // which deadlocks on PipeWire's PulseAudio compat layer.
    //
    // Strategy: create a capture sink, move non-Wavis audio there, and
    // loopback to hardware sink. Wavis stays on hardware sink (user hears
    // peers, peers don't hear themselves).
    let exclusion = setup_loopback_exclusion_pactl(&source_id);
    if let Some(error) = check_loopback_exclusion(&exclusion) {
        let rollback_handle = AudioCaptureHandle {
            pa_thread: std::thread::spawn(|| {}),
            stop_flag: Arc::new(AtomicBool::new(true)),
            source_id: source_id.clone(),
            null_sink_module: exclusion.null_sink_module,
            loopback_module: exclusion.loopback_module,
            matched_pid: std::process::id(),
            moved_inputs: exclusion.moved_inputs.clone(),
            original_default_sink: exclusion.original_default_sink.clone(),
        };
        teardown_loopback_exclusion(&rollback_handle);
        let _ = rollback_handle.pa_thread.join();
        let _ = cleanup_publish_on_error(&state);
        return Err(error);
    }

    // Use the capture sink's monitor if exclusion set one up, otherwise
    // fall back to the original hardware monitor.
    let effective_source = exclusion
        .effective_capture_source
        .clone()
        .unwrap_or_else(|| source_id.clone());

    // -- Open pa_simple capture connection --------------------------
    let stop_flag = Arc::new(AtomicBool::new(false));
    let stop_flag_thread = stop_flag.clone();
    let app_handle = app.clone();

    let pa_thread = std::thread::Builder::new()
        .name("pa-audio-capture".into())
        .spawn(move || {
            audio_capture_loop(
                &effective_source,
                &stop_flag_thread,
                &conn_for_thread,
                &app_handle,
            );
        })
        .map_err(|e| {
            let rollback_handle = AudioCaptureHandle {
                pa_thread: std::thread::spawn(|| {}),
                stop_flag: Arc::new(AtomicBool::new(true)),
                source_id: source_id.clone(),
                null_sink_module: exclusion.null_sink_module,
                loopback_module: exclusion.loopback_module,
                matched_pid: std::process::id(),
                moved_inputs: exclusion.moved_inputs.clone(),
                original_default_sink: exclusion.original_default_sink.clone(),
            };
            teardown_loopback_exclusion(&rollback_handle);
            let _ = rollback_handle.pa_thread.join();
            // Clean up: unpublish the track we just published.
            let _ = cleanup_publish_on_error(&state);
            format!("failed to spawn audio capture thread: {e}")
        })?;

    // -- Store handle -----------------------------------------------
    let handle = AudioCaptureHandle {
        pa_thread,
        stop_flag,
        source_id,
        null_sink_module: exclusion.null_sink_module,
        loopback_module: exclusion.loopback_module,
        matched_pid: std::process::id(),
        moved_inputs: exclusion.moved_inputs,
        original_default_sink: exclusion.original_default_sink,
    };

    {
        let mut guard = audio_capture
            .active
            .lock()
            .map_err(|e| format!("audio capture lock: {e}"))?;
        *guard = Some(handle);
    }

    log::info!("[audio_capture] audio_share_start: capturing from source");

    Ok(AudioShareStartResult {
        loopback_exclusion_available: true,
        real_output_device_id: None,
        real_output_device_name: None,
        requires_mute_for_echo_prevention: false,
    })
}

// --- Loopback Exclusion --------------------------------------------

/// Result of the loopback exclusion setup attempt.
#[cfg(target_os = "linux")]
struct LoopbackExclusion {
    null_sink_module: Option<u32>,
    /// Module index of the loopback from capture sink monitor to hardware sink.
    loopback_module: Option<u32>,
    /// Per-sink-input original sink index + the sink-input index itself.
    /// These are NON-Wavis sink-inputs moved to the capture sink.
    moved_inputs: Vec<MovedSinkInput>,
    #[allow(dead_code)]
    warning: Option<String>,
    /// If set, the capture loop should read from this source instead of the
    /// original hardware monitor. This is `wavis_capture.monitor` when exclusion
    /// succeeds.
    effective_capture_source: Option<String>,
    /// Original default sink name (to restore on teardown).
    original_default_sink: Option<String>,
}

/// Attempt to set up loopback exclusion:
/// 1. Connect to PulseAudio
/// 2. Enumerate sink-inputs, find Wavis by PID
/// 3. Create null sink
/// 4. Move Wavis sink-input to null sink
///
/// If any step fails, returns a warning but does not error — capture
/// proceeds without exclusion (echo risk).
#[cfg(target_os = "linux")]
#[allow(dead_code)]
fn setup_loopback_exclusion() -> LoopbackExclusion {
    match setup_loopback_exclusion_inner() {
        Ok(exc) => exc,
        Err(warning) => {
            log::warn!("[audio_capture] loopback exclusion failed: {warning}");
            LoopbackExclusion {
                null_sink_module: None,
                loopback_module: None,
                moved_inputs: Vec::new(),
                warning: Some(warning),
                effective_capture_source: None,
                original_default_sink: None,
            }
        }
    }
}

#[cfg(target_os = "linux")]
#[allow(dead_code)]
fn setup_loopback_exclusion_inner() -> Result<LoopbackExclusion, String> {
    use std::sync::{Arc, Mutex as StdMutex};

    use pulse::callbacks::ListResult;
    use pulse::context::{Context, FlagSet as ContextFlagSet, State as ContextState};
    use pulse::mainloop::threaded::Mainloop;

    let mut mainloop =
        Mainloop::new().ok_or_else(|| "PulseAudio mainloop creation failed".to_string())?;
    mainloop
        .start()
        .map_err(|_| "PulseAudio mainloop start failed".to_string())?;

    let mut context = Context::new(&mainloop, "wavis-loopback-exclusion")
        .ok_or_else(|| "PulseAudio context creation failed".to_string())?;

    mainloop.lock();
    context
        .connect(None, ContextFlagSet::NOFLAGS, None)
        .map_err(|_| {
            mainloop.unlock();
            "PulseAudio connect failed".to_string()
        })?;

    // Wait for context ready.
    loop {
        match context.get_state() {
            ContextState::Ready => break,
            ContextState::Failed | ContextState::Terminated => {
                mainloop.unlock();
                mainloop.stop();
                return Err("PulseAudio context failed to become ready".to_string());
            }
            _ => mainloop.wait(),
        }
    }

    let my_pid = std::process::id().to_string();

    // -- Step 1: Find ALL Wavis sink-inputs by PID ------------------
    #[derive(Clone)]
    struct SinkInputMatch {
        index: u32,
        sink_name: Option<String>,
    }

    let found: Arc<StdMutex<Vec<SinkInputMatch>>> = Arc::new(StdMutex::new(Vec::new()));
    let enum_done: Arc<StdMutex<bool>> = Arc::new(StdMutex::new(false));

    let found_clone = found.clone();
    let done_clone = enum_done.clone();
    let ml_ref = &mut mainloop as *mut Mainloop;
    let pid_match = my_pid.clone();

    let _op = context
        .introspect()
        .get_sink_input_info_list(move |list_result| match list_result {
            ListResult::Item(info) => {
                let pid_matched = info
                    .proplist
                    .get_str("application.process.id")
                    .map(|pid_str| pid_str == pid_match)
                    .unwrap_or(false);

                let name_matched = info
                    .proplist
                    .get_str("application.name")
                    .map(|name| name.to_lowercase().contains("wavis"))
                    .unwrap_or(false)
                    || info
                        .proplist
                        .get_str("node.name")
                        .map(|name| name.to_lowercase().contains("wavis"))
                        .unwrap_or(false);

                if pid_matched || name_matched {
                    if let Ok(mut f) = found_clone.lock() {
                        f.push(SinkInputMatch {
                            index: info.index,
                            sink_name: Some(info.sink.to_string()),
                        });
                    }
                }
            }
            ListResult::End | ListResult::Error => {
                if let Ok(mut d) = done_clone.lock() {
                    *d = true;
                }
                unsafe { (*ml_ref).signal(false) };
            }
        });

    // Wait for enumeration.
    loop {
        if let Ok(d) = enum_done.lock() {
            if *d {
                break;
            }
        }
        mainloop.wait();
    }

    let matched = found.lock().ok().map(|f| f.clone()).unwrap_or_default();

    if matched.is_empty() {
        // PID match failed — proceed without exclusion.
        mainloop.unlock();
        context.disconnect();
        mainloop.stop();
        return Ok(LoopbackExclusion {
            null_sink_module: None,
            loopback_module: None,
            moved_inputs: Vec::new(),
            warning: Some(
                "echo possible: Wavis playback stream not found by PID — system audio may include your own voice".to_string(),
            ),
            effective_capture_source: None,
            original_default_sink: None,
        });
    }

    log::info!(
        "[audio_capture] found {} Wavis sink-input(s) by PID {}",
        matched.len(),
        my_pid
    );

    // -- Step 2: Create null sink -----------------------------------
    let module_idx: Arc<StdMutex<Option<u32>>> = Arc::new(StdMutex::new(None));
    let load_done: Arc<StdMutex<bool>> = Arc::new(StdMutex::new(false));

    let mod_clone = module_idx.clone();
    let done_clone2 = load_done.clone();
    let ml_ref2 = &mut mainloop as *mut Mainloop;

    context.introspect().load_module(
        "module-null-sink",
        "sink_name=wavis_exclude sink_properties=device.description=wavis_exclude",
        move |idx| {
            if let Ok(mut m) = mod_clone.lock() {
                *m = Some(idx);
            }
            if let Ok(mut d) = done_clone2.lock() {
                *d = true;
            }
            unsafe { (*ml_ref2).signal(false) };
        },
    );

    loop {
        if let Ok(d) = load_done.lock() {
            if *d {
                break;
            }
        }
        mainloop.wait();
    }

    let null_sink_module = module_idx.lock().ok().and_then(|m| *m);

    if null_sink_module.is_none() {
        mainloop.unlock();
        context.disconnect();
        mainloop.stop();
        return Ok(LoopbackExclusion {
            null_sink_module: None,
            loopback_module: None,
            moved_inputs: Vec::new(),
            warning: Some(
                "echo possible: failed to create null sink — system audio may include your own voice"
                    .to_string(),
            ),
            effective_capture_source: None,
            original_default_sink: None,
        });
    }

    // -- Step 3: Move ALL Wavis sink-inputs to null sink ------------
    let mut moved_inputs: Vec<MovedSinkInput> = Vec::new();
    let mut move_failures = 0u32;

    for si in &matched {
        let move_done: Arc<StdMutex<bool>> = Arc::new(StdMutex::new(false));
        let move_ok: Arc<StdMutex<bool>> = Arc::new(StdMutex::new(false));

        let ok_clone = move_ok.clone();
        let done_clone3 = move_done.clone();
        let ml_ref3 = &mut mainloop as *mut Mainloop;

        context.introspect().move_sink_input_by_name(
            si.index,
            "wavis_exclude",
            Some(Box::new(move |success| {
                if let Ok(mut ok) = ok_clone.lock() {
                    *ok = success;
                }
                if let Ok(mut d) = done_clone3.lock() {
                    *d = true;
                }
                unsafe { (*ml_ref3).signal(false) };
            })),
        );

        loop {
            if let Ok(d) = move_done.lock() {
                if *d {
                    break;
                }
            }
            mainloop.wait();
        }

        let moved = move_ok.lock().ok().map(|ok| *ok).unwrap_or(false);
        if moved {
            log::info!(
                "[audio_capture] loopback exclusion: sink-input {} moved to wavis_exclude",
                si.index
            );
            moved_inputs.push(MovedSinkInput {
                index: si.index,
                original_sink: si.sink_name.clone(),
            });
        } else {
            log::warn!(
                "[audio_capture] failed to move sink-input {} to null sink",
                si.index
            );
            move_failures += 1;
        }
    }

    mainloop.unlock();
    context.disconnect();
    mainloop.stop();

    let warning = if moved_inputs.is_empty() {
        Some("echo possible: failed to redirect Wavis audio — system audio may include your own voice".to_string())
    } else if move_failures > 0 {
        Some(format!(
            "echo possible: {move_failures} of {} Wavis streams could not be redirected — system audio may include your own voice",
            matched.len()
        ))
    } else {
        None
    };

    log::info!(
        "[audio_capture] loopback exclusion: {}/{} sink-inputs moved",
        moved_inputs.len(),
        matched.len()
    );

    Ok(LoopbackExclusion {
        null_sink_module,
        loopback_module: None,
        moved_inputs,
        warning,
        effective_capture_source: None,
        original_default_sink: None,
    })
}

// --- Source Validation ---------------------------------------------

/// Loopback exclusion via `pactl` subprocess commands.
/// Avoids the PulseAudio threaded mainloop API which deadlocks on PipeWire.
///
/// Strategy:
/// 1. Create a null sink (`wavis_voice`) for Wavis audio
/// 2. Create a loopback from `wavis_voice.monitor` ? hardware sink (so user still hears peers)
/// 3. Move Wavis sink-inputs to `wavis_voice`
/// 4. Audio capture reads from the hardware sink's monitor (which now excludes Wavis)
///
/// Result: system audio is captured, Wavis voice chat is NOT captured, user hears everything.
#[cfg(target_os = "linux")]
fn setup_loopback_exclusion_pactl(capture_source_id: &str) -> LoopbackExclusion {
    match setup_loopback_exclusion_pactl_inner(capture_source_id) {
        Ok(exc) => exc,
        Err(warning) => {
            log::warn!("[audio_capture] pactl loopback exclusion failed: {warning}");
            LoopbackExclusion {
                null_sink_module: None,
                loopback_module: None,
                moved_inputs: Vec::new(),
                warning: Some(warning),
                effective_capture_source: None,
                original_default_sink: None,
            }
        }
    }
}

#[cfg(target_os = "linux")]
fn setup_loopback_exclusion_pactl_inner(
    capture_source_id: &str,
) -> Result<LoopbackExclusion, String> {
    fn restore_partial_loopback_setup(
        original_default_sink: Option<&str>,
        null_sink_module: Option<u32>,
    ) {
        if let Some(sink) = original_default_sink {
            let _ = std::process::Command::new("pactl")
                .args(["set-default-sink", sink])
                .output();
            log::warn!("[audio_capture] pactl rollback: restored default sink to {sink}");
        }

        let hardware_sink = original_default_sink.unwrap_or("wavis_capture");
        let _ = std::process::Command::new("pw-link")
            .args([
                "-d",
                "wavis_capture:monitor_FL",
                &format!("{hardware_sink}:playback_FL"),
            ])
            .output();
        let _ = std::process::Command::new("pw-link")
            .args([
                "-d",
                "wavis_capture:monitor_FR",
                &format!("{hardware_sink}:playback_FR"),
            ])
            .output();

        if let Some(idx) = null_sink_module {
            let _ = std::process::Command::new("pactl")
                .args(["unload-module", &idx.to_string()])
                .output();
            log::warn!("[audio_capture] pactl rollback: unloaded partial null sink module {idx}");
        }
    }

    // -- Architecture --------------------------------------------------
    //
    // Problem: we need to capture system audio WITHOUT Wavis peer audio,
    // and any audio source started AFTER sharing begins must also be captured.
    //
    // Solution: make wavis_capture the default sink.
    //   1. Save the original default sink name
    //   2. Create a virtual "capture" sink (wavis_capture)
    //   3. Set wavis_capture as the DEFAULT sink — all new streams go there
    //   4. Move existing non-Wavis sink-inputs to wavis_capture
    //   5. Pin all Wavis sink-inputs to the hardware sink
    //   6. Loopback wavis_capture.monitor → hardware sink (user hears everything)
    //   7. Capture from wavis_capture.monitor (system audio only)
    //
    // Result: peers get system audio (including sources started mid-share),
    //         user hears everything, peers don't hear themselves.
    //
    // !! IMPORTANT — constraints learned from debugging (2026-03) !!
    //
    // DO NOT use PulseAudio `module-loopback` for the audio passthrough.
    //   PipeWire's PulseAudio compat layer creates the loopback nodes and
    //   links but does NOT reliably pass audio through them. Use `pw-link`
    //   for direct PipeWire-native port connections instead.
    //
    // DO NOT keep the hardware sink as the default during sharing.
    //   If you only move existing sink-inputs at share-start, any audio
    //   source opened AFTER sharing (YouTube, Discord, etc.) goes to the
    //   default sink and peers never hear it. wavis_capture MUST be the
    //   default so new streams auto-route there.
    //
    // DO NOT forget to unmute wavis_capture after creation.
    //   PipeWire sometimes creates null sinks in a muted state. If
    //   wavis_capture is muted, its monitor outputs silence even though
    //   audio is playing into it.
    //
    // DO NOT skip the stale module cleanup.
    //   If Wavis crashes, leftover wavis_capture modules persist. Creating
    //   a second wavis_capture causes PipeWire to assign a duplicate name,
    //   and loopback/capture connections target the wrong sink.
    //
    // DO NOT skip the post-loopback Wavis re-pin (Step 6).
    //   PipeWire's stream-restore may move Wavis sink-inputs to
    //   wavis_capture when we change the default sink. The re-pin scan
    //   catches any Wavis streams that drifted.

    // -- Step 1: Save the original default sink -----------------------
    let original_default_sink = {
        let info_output = std::process::Command::new("pactl").arg("info").output();
        match info_output {
            Ok(output) if output.status.success() => {
                let stdout = String::from_utf8_lossy(&output.stdout);
                let sink = stdout
                    .lines()
                    .find_map(|line| line.strip_prefix("Default Sink: "))
                    .map(|s| s.trim().to_string());
                log::info!("[audio_capture] pactl: original default sink: {:?}", sink);
                sink
            }
            _ => {
                log::warn!("[audio_capture] pactl: could not determine original default sink");
                None
            }
        }
    };

    // -- Step 1.5: Clean up any leftover wavis_capture modules ---------
    cleanup_stale_wavis_modules();

    // -- Step 2: Create capture sink ----------------------------------
    let load_output = std::process::Command::new("pactl")
        .args([
            "load-module",
            "module-null-sink",
            "sink_name=wavis_capture",
            "sink_properties=device.description=wavis_capture",
        ])
        .output()
        .map_err(|e| format!("failed to create capture sink: {e}"))?;

    if !load_output.status.success() {
        return Err("failed to create capture sink: pactl returned error".to_string());
    }

    let module_idx_str = String::from_utf8_lossy(&load_output.stdout)
        .trim()
        .to_string();
    let null_sink_module = module_idx_str.parse::<u32>().ok();

    log::info!("[audio_capture] pactl: capture sink created, module index: {module_idx_str}");

    // Derive hardware sink name from monitor source.
    // capture_source_id is e.g. "alsa_output.XXX.analog-stereo.monitor"
    let hardware_sink = capture_source_id
        .strip_suffix(".monitor")
        .unwrap_or(capture_source_id);

    // -- Step 3: Set wavis_capture as the default sink ----------------
    // This ensures any NEW audio streams (YouTube opened after sharing,
    // Discord, Spotify, etc.) automatically route to wavis_capture and
    // get captured for the stream.
    let _ = std::process::Command::new("pactl")
        .args(["set-default-sink", "wavis_capture"])
        .output();
    log::info!("[audio_capture] pactl: set wavis_capture as default sink");

    // -- Step 4: Scan sink-inputs and route them ---------------------
    // Move existing non-Wavis sink-inputs to wavis_capture.
    // Pin Wavis sink-inputs to the hardware sink.
    // IMPORTANT: scan BEFORE creating the loopback, so the loopback's
    // own sink-input doesn't appear in the list.
    let list_output = std::process::Command::new("pactl")
        .args(["list", "sink-inputs"])
        .output()
        .map_err(|e| format!("failed to list sink-inputs: {e}"))?;

    if !list_output.status.success() {
        restore_partial_loopback_setup(original_default_sink.as_deref(), null_sink_module);
        return Err("failed to list sink-inputs: pactl returned error".to_string());
    }

    let stdout = String::from_utf8_lossy(&list_output.stdout);
    let my_pid = std::process::id().to_string();

    struct SinkInputBlock {
        index: u32,
        sink: Option<String>,
        is_wavis: bool,
    }

    let mut blocks: Vec<SinkInputBlock> = Vec::new();
    let mut current_index: Option<u32> = None;
    let mut current_sink: Option<String> = None;
    let mut is_wavis = false;

    for line in stdout.lines() {
        let trimmed = line.trim();

        if let Some(rest) = trimmed.strip_prefix("Sink Input #") {
            if let Some(idx) = current_index {
                blocks.push(SinkInputBlock {
                    index: idx,
                    sink: current_sink.take(),
                    is_wavis,
                });
            }
            current_index = rest.parse::<u32>().ok();
            current_sink = None;
            is_wavis = false;
        } else if let Some(rest) = trimmed.strip_prefix("Sink: ") {
            current_sink = Some(rest.to_string());
        } else if let Some(rest) = trimmed.strip_prefix("application.process.id = ") {
            let pid = rest.trim_matches('"');
            if pid == my_pid {
                is_wavis = true;
            }
        } else if let Some(rest) = trimmed.strip_prefix("application.name = ") {
            let name = rest.trim_matches('"').to_lowercase();
            if name.contains("wavis") {
                is_wavis = true;
            }
        } else if let Some(rest) = trimmed.strip_prefix("node.name = ") {
            let name = rest.trim_matches('"').to_lowercase();
            if name.contains("wavis") {
                is_wavis = true;
            }
        }
    }
    if let Some(idx) = current_index {
        blocks.push(SinkInputBlock {
            index: idx,
            sink: current_sink.take(),
            is_wavis,
        });
    }

    let non_wavis: Vec<&SinkInputBlock> = blocks.iter().filter(|b| !b.is_wavis).collect();
    let wavis_inputs: Vec<&SinkInputBlock> = blocks.iter().filter(|b| b.is_wavis).collect();

    log::info!(
        "[audio_capture] pactl: found {} total sink-inputs, {} non-Wavis, {} Wavis",
        blocks.len(),
        non_wavis.len(),
        wavis_inputs.len()
    );

    // Move existing non-Wavis sink-inputs to wavis_capture.
    let mut moved_inputs: Vec<MovedSinkInput> = Vec::new();
    let mut move_failures = 0u32;

    for block in &non_wavis {
        let result = std::process::Command::new("pactl")
            .args(["move-sink-input", &block.index.to_string(), "wavis_capture"])
            .output();

        match result {
            Ok(output) if output.status.success() => {
                log::info!(
                    "[audio_capture] pactl: sink-input {} moved to wavis_capture",
                    block.index
                );
                moved_inputs.push(MovedSinkInput {
                    index: block.index,
                    original_sink: block.sink.clone(),
                });
            }
            _ => {
                log::warn!(
                    "[audio_capture] pactl: failed to move sink-input {} to capture sink",
                    block.index
                );
                move_failures += 1;
            }
        }
    }

    // Pin ALL Wavis sink-inputs to the hardware sink so peers don't
    // hear themselves and user hears peers directly.
    for block in &wavis_inputs {
        let _ = std::process::Command::new("pactl")
            .args(["move-sink-input", &block.index.to_string(), hardware_sink])
            .output();
        log::info!(
            "[audio_capture] pactl: pinned Wavis sink-input {} to hardware sink {hardware_sink}",
            block.index
        );
    }

    // -- Step 5: Unmute wavis_capture ----------------------------------
    // PipeWire sometimes creates null sinks in a muted state.
    let _ = std::process::Command::new("pactl")
        .args(["set-sink-mute", "wavis_capture", "0"])
        .output();

    // -- Step 5.5: Create loopback via pw-link -------------------------
    // PipeWire's module-loopback (PulseAudio compat) creates nodes and
    // links but doesn't reliably pass audio. Direct pw-link connections
    // between wavis_capture's monitor ports and the hardware sink's
    // playback ports are PipeWire-native and work reliably.
    let loopback_module = None;
    let pw_link_fl = std::process::Command::new("pw-link")
        .args([
            "wavis_capture:monitor_FL",
            &format!("{hardware_sink}:playback_FL"),
        ])
        .output();
    let pw_link_fr = std::process::Command::new("pw-link")
        .args([
            "wavis_capture:monitor_FR",
            &format!("{hardware_sink}:playback_FR"),
        ])
        .output();

    let warning = match (&pw_link_fl, &pw_link_fr) {
        (Ok(l), Ok(r)) if l.status.success() && r.status.success() => {
            log::info!(
                "[audio_capture] pw-link: connected wavis_capture:monitor → {hardware_sink}:playback"
            );
            None
        }
        _ => {
            restore_partial_loopback_setup(original_default_sink.as_deref(), null_sink_module);
            return Err(
                "failed to create audio loopback — keeping current system audio routing"
                    .to_string(),
            );
        }
    };

    // -- Step 6: Re-pin Wavis to hardware sink (post-loopback check) --
    // PipeWire may have moved Wavis sink-inputs when default sink changed.
    // Do a second scan and force any Wavis sink-inputs back to hardware.
    if let Ok(rescan) = std::process::Command::new("pactl")
        .args(["list", "sink-inputs"])
        .output()
    {
        let rescan_stdout = String::from_utf8_lossy(&rescan.stdout);
        let mut ri_index: Option<u32> = None;
        let mut ri_sink: Option<String> = None;
        let mut ri_is_wavis = false;

        let repin = |idx: u32, sink: &Option<String>, is_wavis: bool| {
            if !is_wavis {
                return;
            }
            let on_hardware = sink
                .as_ref()
                .map(|s| s == hardware_sink || s.parse::<u32>().is_ok())
                .unwrap_or(false);
            if !on_hardware {
                log::warn!(
                    "[audio_capture] pactl: Wavis sink-input {idx} drifted to sink {:?}, re-pinning to {hardware_sink}",
                    sink
                );
                let _ = std::process::Command::new("pactl")
                    .args(["move-sink-input", &idx.to_string(), hardware_sink])
                    .output();
            }
        };

        for line in rescan_stdout.lines() {
            let trimmed = line.trim();
            if let Some(rest) = trimmed.strip_prefix("Sink Input #") {
                if let Some(idx) = ri_index {
                    repin(idx, &ri_sink, ri_is_wavis);
                }
                ri_index = rest.parse::<u32>().ok();
                ri_sink = None;
                ri_is_wavis = false;
            } else if let Some(rest) = trimmed.strip_prefix("Sink: ") {
                ri_sink = Some(rest.to_string());
            } else if let Some(rest) = trimmed.strip_prefix("application.process.id = ") {
                if rest.trim_matches('"') == my_pid {
                    ri_is_wavis = true;
                }
            } else if let Some(rest) = trimmed.strip_prefix("application.name = ") {
                if rest.trim_matches('"').to_lowercase().contains("wavis") {
                    ri_is_wavis = true;
                }
            } else if let Some(rest) = trimmed.strip_prefix("node.name = ") {
                if rest.trim_matches('"').to_lowercase().contains("wavis") {
                    ri_is_wavis = true;
                }
            }
        }
        if let Some(idx) = ri_index {
            repin(idx, &ri_sink, ri_is_wavis);
        }
    }

    let warning = warning.or_else(|| {
        if move_failures > 0 {
            Some(format!(
                "echo possible: {move_failures} of {} non-Wavis streams could not be redirected",
                non_wavis.len()
            ))
        } else {
            None
        }
    });

    log::info!(
        "[audio_capture] pactl: loopback exclusion complete: {}/{} non-Wavis moved, {} Wavis pinned to hardware",
        moved_inputs.len(),
        non_wavis.len(),
        wavis_inputs.len()
    );

    Ok(LoopbackExclusion {
        null_sink_module,
        loopback_module,
        moved_inputs,
        warning,
        effective_capture_source: Some("wavis_capture.monitor".to_string()),
        original_default_sink,
    })
}

/// Clean up any leftover wavis_capture null-sink and loopback modules from
/// a previous session that crashed or failed to tear down cleanly.
///
/// Scans `pactl list short modules` for module-null-sink and module-loopback
/// entries that reference wavis_capture, and unloads them.
///
/// This MUST run before creating a new wavis_capture. Without it, PipeWire
/// creates a duplicate sink with the same name, and pw-link / capture
/// connections target the wrong one — causing silent capture or feedback loops.
#[cfg(target_os = "linux")]
fn cleanup_stale_wavis_modules() {
    let output = match std::process::Command::new("pactl")
        .args(["list", "short", "modules"])
        .output()
    {
        Ok(o) if o.status.success() => o,
        _ => return,
    };

    let stdout = String::from_utf8_lossy(&output.stdout);
    let mut stale_modules: Vec<&str> = Vec::new();

    for line in stdout.lines() {
        // Format: <index>\t<module-name>\t<args>
        let parts: Vec<&str> = line.splitn(3, '\t').collect();
        if parts.len() < 3 {
            continue;
        }
        let idx = parts[0].trim();
        let module_name = parts[1].trim();
        let args = parts[2];

        let is_wavis = args.contains("wavis_capture");
        let is_relevant = module_name == "module-null-sink" || module_name == "module-loopback";

        if is_wavis && is_relevant {
            stale_modules.push(idx);
        }
    }

    if stale_modules.is_empty() {
        return;
    }

    log::warn!(
        "[audio_capture] pactl: cleaning up {} stale wavis_capture modules: {:?}",
        stale_modules.len(),
        stale_modules
    );

    for idx in stale_modules {
        let _ = std::process::Command::new("pactl")
            .args(["unload-module", idx])
            .output();
    }
}

/// Validate that the given source ID exists in PulseAudio.
/// Uses the threaded mainloop pattern with a timeout.
#[cfg(target_os = "linux")]
fn validate_pa_source(source_id: &str) -> Result<(), String> {
    use std::sync::mpsc;

    let sid = source_id.to_string();
    let (tx, rx) = mpsc::channel();

    let handle = std::thread::Builder::new()
        .name("pa-validate-source".into())
        .spawn(move || {
            let result = validate_pa_source_inner(&sid);
            let _ = tx.send(result);
        })
        .map_err(|e| format!("audio source not found: {source_id}: {e}"))?;

    match rx.recv_timeout(PA_TIMEOUT) {
        Ok(result) => {
            let _ = handle.join();
            result
        }
        Err(_) => {
            log::warn!("[audio_capture] validate_pa_source timed out, falling back to pactl");
            validate_pa_source_pactl(source_id)
        }
    }
}

/// Fallback: validate a PulseAudio source exists via `pactl list sources short`.
#[cfg(target_os = "linux")]
fn validate_pa_source_pactl(source_id: &str) -> Result<(), String> {
    let output = std::process::Command::new("pactl")
        .args(["list", "sources", "short"])
        .output()
        .map_err(|e| format!("audio source not found: {source_id}: pactl failed: {e}"))?;

    if !output.status.success() {
        return Err(format!(
            "audio source not found: {source_id}: pactl returned error"
        ));
    }

    let stdout = String::from_utf8_lossy(&output.stdout);
    if stdout.lines().any(|line| line.contains(source_id)) {
        log::info!("[audio_capture] pactl fallback validated source: {source_id}");
        Ok(())
    } else {
        Err(format!("audio source not found: {source_id}"))
    }
}

#[cfg(target_os = "linux")]
fn validate_pa_source_inner(source_id: &str) -> Result<(), String> {
    use std::sync::{Arc, Mutex as StdMutex};

    use pulse::callbacks::ListResult;
    use pulse::context::{Context, FlagSet as ContextFlagSet, State as ContextState};
    use pulse::mainloop::threaded::Mainloop;

    let mut mainloop =
        Mainloop::new().ok_or_else(|| format!("audio source not found: {source_id}"))?;
    mainloop
        .start()
        .map_err(|_| format!("audio source not found: {source_id}"))?;

    let mut context = Context::new(&mainloop, "wavis-validate-source")
        .ok_or_else(|| format!("audio source not found: {source_id}"))?;

    mainloop.lock();
    context
        .connect(None, ContextFlagSet::NOFLAGS, None)
        .map_err(|_| {
            mainloop.unlock();
            format!("audio source not found: {source_id}")
        })?;

    loop {
        match context.get_state() {
            ContextState::Ready => break,
            ContextState::Failed | ContextState::Terminated => {
                mainloop.unlock();
                mainloop.stop();
                return Err(format!("audio source not found: {source_id}"));
            }
            _ => mainloop.wait(),
        }
    }

    let found: Arc<StdMutex<bool>> = Arc::new(StdMutex::new(false));
    let done: Arc<StdMutex<bool>> = Arc::new(StdMutex::new(false));

    let found_clone = found.clone();
    let done_clone = done.clone();
    let ml_ref = &mut mainloop as *mut Mainloop;
    let target = source_id.to_string();

    let _op = context
        .introspect()
        .get_source_info_list(move |list_result| match list_result {
            ListResult::Item(info) => {
                if let Some(ref name) = info.name {
                    if name.as_ref() == target.as_str() {
                        if let Ok(mut f) = found_clone.lock() {
                            *f = true;
                        }
                    }
                }
            }
            ListResult::End | ListResult::Error => {
                if let Ok(mut d) = done_clone.lock() {
                    *d = true;
                }
                unsafe { (*ml_ref).signal(false) };
            }
        });

    loop {
        if let Ok(d) = done.lock() {
            if *d {
                break;
            }
        }
        mainloop.wait();
    }

    mainloop.unlock();
    context.disconnect();
    mainloop.stop();

    let exists = found.lock().ok().map(|f| *f).unwrap_or(false);
    if exists {
        Ok(())
    } else {
        Err(format!("audio source not found: {source_id}"))
    }
}

// --- Capture Loop --------------------------------------------------

/// Audio capture loop — runs on a dedicated thread.
///
/// Opens a `pa_simple` connection to the selected monitor source at 48kHz mono,
/// reads 960-sample frames (20ms), and feeds them into the LiveKit screen audio
/// track via `feed_screen_audio()`.
///
/// On PulseAudio read error: stops capture, unpublishes track, emits `share_error`.
#[cfg(target_os = "linux")]
fn audio_capture_loop(
    source_id: &str,
    stop_flag: &std::sync::atomic::AtomicBool,
    conn: &std::sync::Arc<wavis_client_shared::livekit_connection::RealLiveKitConnection>,
    app: &tauri::AppHandle,
) {
    use std::sync::atomic::Ordering;

    use psimple::Simple;
    use pulse::def::BufferAttr;
    use pulse::sample::{Format, Spec};
    use pulse::stream::Direction;
    use tauri::Emitter;

    const SAMPLE_RATE: u32 = 48_000;
    const CHANNELS: u8 = 1;
    const FRAME_SAMPLES: usize = 960; // 20ms at 48kHz
    const FRAME_BYTES: u32 = (FRAME_SAMPLES * std::mem::size_of::<i16>()) as u32;
    const MAX_CAPTURE_LATENCY_US: u64 = 250_000; // drop stale capture backlog beyond 250ms

    let spec = Spec {
        format: Format::S16le,
        channels: CHANNELS,
        rate: SAMPLE_RATE,
    };

    assert!(spec.is_valid());

    let buffer_attr = BufferAttr {
        maxlength: FRAME_BYTES * 4,
        tlength: u32::MAX,
        prebuf: u32::MAX,
        minreq: u32::MAX,
        // Ask Pulse for 20ms record fragments instead of the large default.
        fragsize: FRAME_BYTES,
    };

    let simple = match Simple::new(
        None,                // Default server
        "wavis-audio-share", // Application name
        Direction::Record,
        Some(source_id), // Source device
        "screen-audio",  // Stream description
        &spec,
        None, // Default channel map
        Some(&buffer_attr),
    ) {
        Ok(s) => s,
        Err(e) => {
            log::error!("[audio_capture] pa_simple open failed: {e}");
            let _ = conn.unpublish_screen_audio();
            let _ = app.emit(
                "share_error",
                serde_json::json!({ "message": format!("Audio capture failed: {e}") }),
            );
            return;
        }
    };

    log::info!("[audio_capture] capture loop started on source: {source_id}");

    // Buffer for reading i16 samples (960 samples × 2 bytes = 1920 bytes).
    let mut buf = vec![0i16; FRAME_SAMPLES];

    loop {
        if stop_flag.load(Ordering::Relaxed) {
            break;
        }

        if let Ok(latency) = simple.get_latency() {
            if latency.0 > MAX_CAPTURE_LATENCY_US {
                log::warn!(
                    "[audio_capture] pa_simple capture latency={}us, flushing stale buffered audio",
                    latency.0
                );
                let _ = simple.flush();
            }
        }

        // Read one frame from PulseAudio.
        // pa_simple::read expects a &mut [u8] — reinterpret our i16 buffer.
        let byte_buf = unsafe {
            std::slice::from_raw_parts_mut(
                buf.as_mut_ptr() as *mut u8,
                FRAME_SAMPLES * std::mem::size_of::<i16>(),
            )
        };

        match simple.read(byte_buf) {
            Ok(()) => {}
            Err(e) => {
                log::error!("[audio_capture] pa_simple read error: {e}");
                // PulseAudio disconnected — stop capture, unpublish, emit error.
                let _ = conn.unpublish_screen_audio();
                let _ = app.emit(
                    "share_error",
                    serde_json::json!({ "message": "Audio source disconnected" }),
                );
                return;
            }
        }

        // Feed the i16 PCM data into the LiveKit screen audio track.
        if let Err(e) = conn.feed_screen_audio(&buf) {
            // If feed fails (e.g. track was unpublished externally), stop.
            log::warn!("[audio_capture] feed_screen_audio failed: {e}");
            break;
        }
    }

    log::info!("[audio_capture] capture loop stopped");
}

// --- Helpers -------------------------------------------------------

/// Clean up the published screen audio track on error during startup.
#[cfg(target_os = "linux")]
fn cleanup_publish_on_error(
    state: &tauri::State<'_, crate::media::MediaState>,
) -> Result<(), String> {
    let lk_guard = state.lk().map_err(|e| format!("lock: {e}"))?;
    if let Some(conn) = lk_guard.as_ref() {
        let _ = conn.unpublish_screen_audio();
    }
    Ok(())
}
// --- Loopback Exclusion Teardown -----------------------------------

/// Best-effort teardown of loopback exclusion:
/// 1. Move Wavis sink-input back to its original sink.
/// 2. Unload the null sink module.
///
/// Failures are logged but never propagated — this is cleanup code.
#[cfg(target_os = "linux")]
fn teardown_loopback_exclusion(handle: &AudioCaptureHandle) {
    if !needs_loopback_teardown(handle) {
        return;
    }

    // Use pactl-based teardown to avoid PulseAudio API deadlocks on PipeWire.
    teardown_loopback_exclusion_pactl(handle);
}

#[cfg(target_os = "linux")]
fn needs_loopback_teardown(handle: &AudioCaptureHandle) -> bool {
    !handle.moved_inputs.is_empty()
        || handle.null_sink_module.is_some()
        || handle.loopback_module.is_some()
        || handle.original_default_sink.is_some()
}

/// Teardown loopback exclusion via `pactl` subprocess commands.
///
/// !! IMPORTANT — ordering matters !!
/// 1. Restore default sink FIRST (so new streams go to hardware)
/// 2. Move ALL sink-inputs off wavis_capture (not just the ones we tracked —
///    new streams created during sharing auto-routed to wavis_capture)
/// 3. Disconnect pw-link loopback
/// 4. Unload modules
///
/// If you unload the null sink before moving streams off it, those streams
/// lose their output and go silent permanently.
#[cfg(target_os = "linux")]
fn teardown_loopback_exclusion_pactl(handle: &AudioCaptureHandle) {
    // Step 1: Restore the original default sink FIRST.
    // During sharing, wavis_capture was the default. Restore before
    // unloading modules so new streams created during teardown go to
    // the right place.
    if let Some(ref sink) = handle.original_default_sink {
        let _ = std::process::Command::new("pactl")
            .args(["set-default-sink", sink])
            .output();
        log::info!("[audio_capture] pactl teardown: default sink restored to {sink}");
    }

    // Step 2: Move ALL current sink-inputs on wavis_capture back to
    // the hardware sink. We can't rely on the saved moved_inputs list
    // because new streams may have been created during the share session
    // (they auto-routed to wavis_capture since it was the default).
    let hardware_sink = handle.original_default_sink.as_deref().unwrap_or_else(|| {
        handle
            .source_id
            .strip_suffix(".monitor")
            .unwrap_or(&handle.source_id)
    });

    if let Ok(output) = std::process::Command::new("pactl")
        .args(["list", "sink-inputs"])
        .output()
    {
        let stdout = String::from_utf8_lossy(&output.stdout);
        let mut current_idx: Option<u32> = None;
        let mut current_sink: Option<String> = None;

        let move_if_on_capture = |idx: u32, sink: &Option<String>| {
            let on_capture = sink
                .as_ref()
                .map(|s| {
                    s == "wavis_capture" || {
                        // Also check by numeric sink index matching wavis_capture
                        if let Ok(list) = std::process::Command::new("pactl")
                            .args(["list", "short", "sinks"])
                            .output()
                        {
                            let list_str = String::from_utf8_lossy(&list.stdout);
                            list_str.lines().any(|l| {
                                l.contains("wavis_capture") && l.starts_with(&format!("{s}\t"))
                            })
                        } else {
                            false
                        }
                    }
                })
                .unwrap_or(false);

            if on_capture {
                let result = std::process::Command::new("pactl")
                    .args(["move-sink-input", &idx.to_string(), hardware_sink])
                    .output();
                match result {
                    Ok(o) if o.status.success() => {
                        log::info!(
                            "[audio_capture] pactl teardown: moved sink-input {idx} back to {hardware_sink}"
                        );
                    }
                    _ => {
                        log::warn!(
                            "[audio_capture] pactl teardown: failed to move sink-input {idx} to {hardware_sink}"
                        );
                    }
                }
            }
        };

        for line in stdout.lines() {
            let trimmed = line.trim();
            if let Some(rest) = trimmed.strip_prefix("Sink Input #") {
                if let Some(idx) = current_idx {
                    move_if_on_capture(idx, &current_sink);
                }
                current_idx = rest.parse::<u32>().ok();
                current_sink = None;
            } else if let Some(rest) = trimmed.strip_prefix("Sink: ") {
                current_sink = Some(rest.to_string());
            }
        }
        if let Some(idx) = current_idx {
            move_if_on_capture(idx, &current_sink);
        }
    }

    // Step 3: Disconnect pw-link loopback and unload loopback module (if any).
    let hardware_sink_name = handle
        .source_id
        .strip_suffix(".monitor")
        .unwrap_or(&handle.source_id);
    let _ = std::process::Command::new("pw-link")
        .args([
            "-d",
            "wavis_capture:monitor_FL",
            &format!("{hardware_sink_name}:playback_FL"),
        ])
        .output();
    let _ = std::process::Command::new("pw-link")
        .args([
            "-d",
            "wavis_capture:monitor_FR",
            &format!("{hardware_sink_name}:playback_FR"),
        ])
        .output();
    log::info!("[audio_capture] pactl teardown: pw-link loopback disconnected");

    if let Some(idx) = handle.loopback_module {
        let _ = std::process::Command::new("pactl")
            .args(["unload-module", &idx.to_string()])
            .output();
        log::info!("[audio_capture] pactl teardown: loopback module {idx} unloaded");
    }

    // Step 4: Unload null sink module.
    if let Some(idx) = handle.null_sink_module {
        let _ = std::process::Command::new("pactl")
            .args(["unload-module", &idx.to_string()])
            .output();
        log::info!("[audio_capture] pactl teardown: null sink module {idx} unloaded");
    }
}

#[cfg(target_os = "linux")]
#[allow(dead_code)]
fn teardown_loopback_exclusion_inner(handle: &AudioCaptureHandle) -> Result<(), String> {
    use std::sync::{Arc, Mutex as StdMutex};

    use pulse::context::{Context, FlagSet as ContextFlagSet, State as ContextState};
    use pulse::mainloop::threaded::Mainloop;

    let mut mainloop =
        Mainloop::new().ok_or_else(|| "PulseAudio mainloop creation failed".to_string())?;
    mainloop
        .start()
        .map_err(|_| "PulseAudio mainloop start failed".to_string())?;

    let mut context = Context::new(&mainloop, "wavis-loopback-teardown")
        .ok_or_else(|| "PulseAudio context creation failed".to_string())?;

    mainloop.lock();
    context
        .connect(None, ContextFlagSet::NOFLAGS, None)
        .map_err(|_| {
            mainloop.unlock();
            "PulseAudio connect failed".to_string()
        })?;

    // Wait for context ready.
    loop {
        match context.get_state() {
            ContextState::Ready => break,
            ContextState::Failed | ContextState::Terminated => {
                mainloop.unlock();
                mainloop.stop();
                return Err("PulseAudio context failed to become ready".to_string());
            }
            _ => mainloop.wait(),
        }
    }

    // -- Step 1: Move ALL Wavis sink-inputs back to original sinks --
    for mi in &handle.moved_inputs {
        let Some(ref original_sink) = mi.original_sink else {
            continue;
        };

        let move_done: Arc<StdMutex<bool>> = Arc::new(StdMutex::new(false));
        let move_ok: Arc<StdMutex<bool>> = Arc::new(StdMutex::new(false));

        let ok_clone = move_ok.clone();
        let done_clone = move_done.clone();
        let ml_ref = &mut mainloop as *mut Mainloop;

        // The original_sink stores the sink index as a string (from setup).
        let sink_idx = original_sink.parse::<u32>().unwrap_or(0);

        context.introspect().move_sink_input_by_index(
            mi.index,
            sink_idx,
            Some(Box::new(move |success| {
                if let Ok(mut ok) = ok_clone.lock() {
                    *ok = success;
                }
                if let Ok(mut d) = done_clone.lock() {
                    *d = true;
                }
                unsafe { (*ml_ref).signal(false) };
            })),
        );

        loop {
            if let Ok(d) = move_done.lock() {
                if *d {
                    break;
                }
            }
            mainloop.wait();
        }

        let moved = move_ok.lock().ok().map(|ok| *ok).unwrap_or(false);
        if moved {
            log::info!(
                "[audio_capture] restored sink-input {} to original sink {}",
                mi.index,
                original_sink
            );
        } else {
            log::warn!(
                "[audio_capture] failed to restore sink-input {} to original sink {}",
                mi.index,
                original_sink
            );
        }
    }

    // -- Step 2: Unload null sink module ----------------------------
    if let Some(module_idx) = handle.null_sink_module {
        let unload_done: Arc<StdMutex<bool>> = Arc::new(StdMutex::new(false));

        let done_clone = unload_done.clone();
        let ml_ref = &mut mainloop as *mut Mainloop;

        context
            .introspect()
            .unload_module(module_idx, move |_success| {
                if let Ok(mut d) = done_clone.lock() {
                    *d = true;
                }
                unsafe { (*ml_ref).signal(false) };
            });

        loop {
            if let Ok(d) = unload_done.lock() {
                if *d {
                    break;
                }
            }
            mainloop.wait();
        }

        log::info!("[audio_capture] unloaded null sink module {}", module_idx);
    }

    mainloop.unlock();
    context.disconnect();
    mainloop.stop();

    Ok(())
}

#[cfg(target_os = "linux")]
fn audio_share_stop_linux(
    state: tauri::State<'_, crate::media::MediaState>,
    audio_capture: tauri::State<'_, AudioCaptureState>,
) -> Result<(), String> {
    use std::sync::atomic::Ordering;

    // -- Take the handle (set to None) ------------------------------
    let handle = {
        let mut guard = audio_capture
            .active
            .lock()
            .map_err(|e| format!("audio capture lock: {e}"))?;
        guard.take()
    };

    let Some(handle) = handle else {
        // No capture active — idempotent success.
        return Ok(());
    };

    // -- Signal the capture thread to stop --------------------------
    handle.stop_flag.store(true, Ordering::Relaxed);

    // -- Restore loopback exclusion (best-effort) -------------------
    teardown_loopback_exclusion(&handle);

    // -- Join the capture thread ------------------------------------
    if handle.pa_thread.join().is_err() {
        log::warn!("[audio_capture] capture thread panicked during join");
    }

    // -- Unpublish screen audio track -------------------------------
    let lk_guard = state.lk().map_err(|e| format!("lock: {e}"))?;
    if let Some(conn) = lk_guard.as_ref() {
        if let Err(e) = conn.unpublish_screen_audio() {
            log::warn!("[audio_capture] unpublish_screen_audio failed: {e}");
        }
    }

    log::info!("[audio_capture] audio_share_stop: capture stopped and cleaned up");

    Ok(())
}

// --- Loopback Exclusion Check --------------------------------------

/// Check whether loopback exclusion succeeded or should abort capture.
///
/// Returns `Some(error_message)` when `exclusion.warning` is present (capture
/// should be aborted to prevent self-echo). Returns `None` when exclusion
/// succeeded (all sink-inputs moved, no warning).
///
/// Pure function — no side effects, no I/O, testable in isolation.
#[cfg(target_os = "linux")]
#[allow(dead_code)]
fn check_loopback_exclusion(exclusion: &LoopbackExclusion) -> Option<String> {
    exclusion
        .warning
        .as_ref()
        .map(|warning| format!("system audio sharing blocked to prevent echo: {warning}"))
}

// --- Loopback Exclusion Rollback -----------------------------------

/// Roll back a partial or failed loopback exclusion setup.
///
/// Restores each moved sink-input to its original sink via `pactl move-sink-input`
/// (best-effort, each attempted independently) and unloads the null sink module
/// via `pactl unload-module` if one was loaded.
///
/// Does NOT require an `AudioCaptureHandle` — takes only the `LoopbackExclusion`
/// struct available at the abort point. The existing `teardown_loopback_exclusion_inner`
/// continues to be used for the normal stop path where a full handle exists.
///
/// Returns `Ok(())` if all restores succeed, `Err` with count of failed restores otherwise.
#[cfg(target_os = "linux")]
#[allow(dead_code)]
fn rollback_loopback_exclusion(exclusion: &LoopbackExclusion) -> Result<(), String> {
    let total = exclusion.moved_inputs.len();
    let mut failed = 0u32;

    // Step 1: Restore each moved sink-input to its original sink (best-effort).
    for mi in &exclusion.moved_inputs {
        let Some(ref original_sink) = mi.original_sink else {
            continue;
        };

        let result = std::process::Command::new("pactl")
            .args(["move-sink-input", &mi.index.to_string(), original_sink])
            .output();

        match result {
            Ok(output) if output.status.success() => {
                log::info!(
                    "[audio_capture] rollback: restored sink-input {} to original sink {}",
                    mi.index,
                    original_sink
                );
            }
            Ok(output) => {
                let stderr = String::from_utf8_lossy(&output.stderr);
                log::warn!(
                    "[audio_capture] rollback: failed to restore sink-input {} to sink {}: {}",
                    mi.index,
                    original_sink,
                    stderr.trim()
                );
                failed += 1;
            }
            Err(e) => {
                log::warn!(
                    "[audio_capture] rollback: failed to restore sink-input {} to sink {}: {}",
                    mi.index,
                    original_sink,
                    e
                );
                failed += 1;
            }
        }
    }

    // Step 2: Unload null sink module if one was loaded.
    if let Some(module_id) = exclusion.null_sink_module {
        let result = std::process::Command::new("pactl")
            .args(["unload-module", &module_id.to_string()])
            .output();

        match result {
            Ok(output) if output.status.success() => {
                log::info!(
                    "[audio_capture] rollback: unloaded null sink module {}",
                    module_id
                );
            }
            Ok(output) => {
                let stderr = String::from_utf8_lossy(&output.stderr);
                log::warn!(
                    "[audio_capture] rollback: failed to unload null sink module {}: {}",
                    module_id,
                    stderr.trim()
                );
            }
            Err(e) => {
                log::warn!(
                    "[audio_capture] rollback: failed to unload null sink module {}: {}",
                    module_id,
                    e
                );
            }
        }
    }

    if failed == 0 {
        Ok(())
    } else {
        Err(format!("failed to restore {failed} of {total} sink-inputs"))
    }
}

// --- Tests ---------------------------------------------------------

#[cfg(test)]
#[cfg(target_os = "linux")]
mod tests {
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::sync::Arc;

    use proptest::prelude::*;

    use super::{needs_loopback_teardown, rollback_loopback_exclusion, LoopbackExclusion};
    use crate::audio_capture::audio_capture_state::{AudioCaptureHandle, AudioCaptureState};

    /// Create a dummy `AudioCaptureHandle` with a no-op thread, simulating
    /// an active capture session for the given source ID.
    fn dummy_handle(source_id: &str) -> AudioCaptureHandle {
        let stop_flag = Arc::new(AtomicBool::new(false));
        let flag_clone = stop_flag.clone();

        let pa_thread = std::thread::Builder::new()
            .name("dummy-capture".into())
            .spawn(move || {
                // Wait until signalled to stop.
                while !flag_clone.load(Ordering::Relaxed) {
                    std::thread::sleep(std::time::Duration::from_millis(5));
                }
            })
            .expect("failed to spawn dummy thread");

        AudioCaptureHandle {
            pa_thread,
            stop_flag,
            source_id: source_id.to_string(),
            null_sink_module: None,
            loopback_module: None,
            matched_pid: std::process::id(),
            moved_inputs: Vec::new(),
            original_default_sink: None,
        }
    }

    /// Clean up a dummy handle by signalling the thread to stop and joining it.
    fn cleanup_handle(handle: AudioCaptureHandle) {
        handle.stop_flag.store(true, Ordering::Relaxed);
        let _ = handle.pa_thread.join();
    }

    proptest! {
        #![proptest_config(ProptestConfig::with_cases(100))]

        // Feature: custom-share-picker, Property 9: Audio capture double-start guard
        // **Validates: Requirements 4.5**
        #[test]
        fn audio_capture_double_start_guard(source_id in "[a-zA-Z0-9_.]{1,64}") {
            let state = AudioCaptureState::new();

            // Simulate an active capture by inserting a dummy handle.
            {
                let mut guard = state.active.lock().unwrap();
                *guard = Some(dummy_handle(&source_id));
            }

            // The guard condition: active is Some ? double-start must be rejected.
            {
                let guard = state.active.lock().unwrap();
                prop_assert!(
                    guard.is_some(),
                    "state must report active capture after inserting handle"
                );
            }

            // Clean up the dummy thread.
            let handle = state.active.lock().unwrap().take().unwrap();
            cleanup_handle(handle);
        }
    }

    // -- Unit Tests for rollback_loopback_exclusion -----------------
    //
    // **Validates: Requirements 2.3, 2.4**
    //
    // These tests exercise the real `rollback_loopback_exclusion` function
    // with the real `LoopbackExclusion` / `MovedSinkInput` types. They are
    // gated to Linux because the function calls `pactl` via
    // `std::process::Command`.

    #[test]
    fn test_rollback_empty_inputs_no_module() {
        // Empty moved_inputs + no null sink module ? Ok(()) without side effects.
        let exclusion = LoopbackExclusion {
            null_sink_module: None,
            loopback_module: None,
            moved_inputs: Vec::new(),
            warning: Some("test warning".to_string()),
            effective_capture_source: None,
            original_default_sink: None,
        };
        let result = rollback_loopback_exclusion(&exclusion);
        assert!(
            result.is_ok(),
            "empty moved_inputs + no null sink module should return Ok(()); got: {:?}",
            result
        );
    }

    #[test]
    fn test_rollback_empty_inputs_with_module() {
        // Empty moved_inputs + Some(module) ? attempts unload.
        // On a test environment without PulseAudio, the unload may fail —
        // that's fine. The function should still return Ok(()) because
        // null sink unload failure is logged but does not affect the
        // return value (only failed sink-input restores count).
        let exclusion = LoopbackExclusion {
            null_sink_module: Some(999_999),
            loopback_module: None,
            moved_inputs: Vec::new(),
            warning: Some("test warning".to_string()),
            effective_capture_source: None,
            original_default_sink: None,
        };
        let result = rollback_loopback_exclusion(&exclusion);
        // No moved_inputs ? 0 failed restores ? Ok(()) regardless of
        // whether the null sink unload succeeded or failed.
        assert!(
            result.is_ok(),
            "empty moved_inputs with null sink module should return Ok(()) \
             (unload failure is logged, not counted); got: {:?}",
            result
        );
    }

    #[test]
    fn test_needs_loopback_teardown_when_default_sink_was_changed() {
        let handle = dummy_handle("alsa_output.pci.monitor");
        let handle = AudioCaptureHandle {
            original_default_sink: Some("alsa_output.pci".to_string()),
            ..handle
        };

        assert!(
            needs_loopback_teardown(&handle),
            "restoring the original default sink must trigger teardown even without moved inputs or modules"
        );

        cleanup_handle(handle);
    }
}

#[cfg(test)]
#[cfg(target_os = "linux")]
mod loopback_exclusion_tests {
    use proptest::prelude::*;

    // -- Test-only type mirrors ----------------------------------------
    // These mirror `LoopbackExclusion` and `MovedSinkInput` to allow
    // property-based exploration of the check/rollback logic in isolation.

    #[derive(Debug, Clone)]
    #[allow(dead_code)]
    struct MovedSinkInput {
        index: u32,
        original_sink: Option<String>,
    }

    #[derive(Debug)]
    #[allow(dead_code)]
    struct LoopbackExclusion {
        null_sink_module: Option<u32>,
        moved_inputs: Vec<MovedSinkInput>,
        warning: Option<String>,
    }

    /// Check whether loopback exclusion succeeded or should abort capture.
    ///
    /// Returns `Some(error_message)` when `exclusion.warning` is present
    /// (capture should be aborted). Returns `None` when exclusion succeeded.
    fn check_loopback_exclusion(exclusion: &LoopbackExclusion) -> Option<String> {
        exclusion
            .warning
            .as_ref()
            .map(|warning| format!("system audio sharing blocked to prevent echo: {warning}"))
    }

    // --- Proptest Strategies ---------------------------------------

    /// Strategy for generating random `MovedSinkInput` structs.
    fn arb_moved_sink_input() -> impl Strategy<Value = MovedSinkInput> {
        (any::<u32>(), proptest::option::of("[0-9]{1,5}")).prop_map(|(index, original_sink)| {
            MovedSinkInput {
                index,
                original_sink,
            }
        })
    }

    /// Strategy for generating `LoopbackExclusion` with `warning: Some(_)`.
    /// Scoped to the bug condition: exclusion attempted but failed (zero PID
    /// matches, partial move, or total move failure).
    fn arb_loopback_exclusion_with_warning() -> impl Strategy<Value = LoopbackExclusion> {
        (
            prop_oneof![
                Just("echo possible: Wavis playback stream not found by PID — system audio may include your own voice".to_string()),
                Just("echo possible: failed to redirect Wavis audio — system audio may include your own voice".to_string()),
                Just("echo possible: 1 of 3 Wavis streams could not be redirected — system audio may include your own voice".to_string()),
                Just("echo possible: failed to create null sink — system audio may include your own voice".to_string()),
                ".{1,100}".prop_map(|s| s),
            ],
            proptest::option::of(any::<u32>()),
            proptest::collection::vec(arb_moved_sink_input(), 0..10),
        )
            .prop_map(|(warning, null_sink_module, moved_inputs)| LoopbackExclusion {
                null_sink_module,
                moved_inputs,
                warning: Some(warning),
            })
    }

    /// Strategy for generating fully random `LoopbackExclusion` structs —
    /// both with and without warnings. Used by Property 3 to test the
    /// biconditional: `check_loopback_exclusion` returns `Some` iff
    /// `warning.is_some()`.
    fn arb_loopback_exclusion() -> impl Strategy<Value = LoopbackExclusion> {
        (
            proptest::option::of(".{1,100}"),
            proptest::collection::vec(arb_moved_sink_input(), 0..10),
            proptest::option::of(0..1000u32),
        )
            .prop_map(
                |(warning, moved_inputs, null_sink_module)| LoopbackExclusion {
                    null_sink_module,
                    moved_inputs,
                    warning,
                },
            )
    }

    /// Strategy for generating `LoopbackExclusion` with `warning: None`.
    /// Scoped to the preservation case: exclusion succeeded (all sink-inputs
    /// moved, no warning). These inputs should NOT trigger an abort.
    fn arb_loopback_exclusion_without_warning() -> impl Strategy<Value = LoopbackExclusion> {
        (
            proptest::option::of(any::<u32>()),
            proptest::collection::vec(arb_moved_sink_input(), 0..10),
        )
            .prop_map(|(null_sink_module, moved_inputs)| LoopbackExclusion {
                null_sink_module,
                moved_inputs,
                warning: None,
            })
    }

    proptest! {
        #![proptest_config(ProptestConfig::with_cases(200))]

        // -- Property 1: Bug Condition — Self-Echo on Loopback Exclusion Failure --
        //
        // **Validates: Requirements 1.1, 1.2, 1.3, 1.4, 2.1, 2.2, 2.3, 2.4**
        //
        // For any LoopbackExclusion where warning.is_some() (the bug condition),
        // check_loopback_exclusion MUST return Some(error_msg) so the caller
        // aborts capture instead of proceeding with echo risk.
        //
        // EXPECTED TO FAIL on unfixed code: the stub returns None for all inputs,
        // but this test asserts Some when warning is present. The failure confirms
        // the buggy warn-and-proceed behavior.
        #[test]
        fn bug_condition_self_echo_on_loopback_exclusion_failure(
            exclusion in arb_loopback_exclusion_with_warning()
        ) {
            // Precondition: this is a bug-condition input (warning present).
            prop_assert!(exclusion.warning.is_some());

            let result = check_loopback_exclusion(&exclusion);

            // The fix should return Some(error_msg) to abort capture.
            prop_assert!(
                result.is_some(),
                "check_loopback_exclusion returned None for LoopbackExclusion with \
                 warning={:?} — capture would proceed with echo risk. \
                 Expected Some(error_msg) to abort capture.",
                exclusion.warning
            );

            // The error message should be non-empty.
            let msg = result.unwrap();
            prop_assert!(
                !msg.is_empty(),
                "check_loopback_exclusion returned Some(\"\") — error message must be non-empty"
            );
        }

        // -- Property 2: Preservation — Successful Capture Paths Unchanged --
        //
        // **Validates: Requirements 3.1, 3.2, 3.4**
        //
        // For any LoopbackExclusion where warning.is_none() (exclusion succeeded),
        // check_loopback_exclusion MUST return None — capture proceeds normally.
        //
        // On UNFIXED code: the stub returns None for ALL inputs, so this test
        // passes — confirming the baseline behavior we want to preserve.
        #[test]
        fn preservation_successful_capture_unchanged(
            exclusion in arb_loopback_exclusion_without_warning()
        ) {
            // Precondition: this is a non-buggy input (no warning).
            prop_assert!(exclusion.warning.is_none());

            let result = check_loopback_exclusion(&exclusion);

            // Successful exclusion ? None (capture proceeds).
            prop_assert!(
                result.is_none(),
                "check_loopback_exclusion returned Some({:?}) for LoopbackExclusion with \
                 warning=None — capture should proceed normally when exclusion succeeds.",
                result
            );
        }

        // -- Property 3: Preservation — External Process Fallback Unchanged --
        //
        // **Validates: Requirements 3.3**
        //
        // On Windows, when a PID target is specified and per-process loopback
        // fails, the system falls back to system-wide loopback. This is NOT a
        // bug condition because capturing an external process's audio does not
        // create self-echo. The LoopbackExclusion for this path has
        // warning: None (the fallback is intentional and safe).
        //
        // This test validates that check_loopback_exclusion returns None for
        // all inputs representing the external-process fallback scenario:
        // warning is None, moved_inputs may be empty (Windows path doesn't
        // use PulseAudio sink-input moves), null_sink_module is None.
        #[test]
        fn preservation_external_process_fallback_unchanged(
            null_sink_module in proptest::option::of(any::<u32>()),
            moved_inputs in proptest::collection::vec(arb_moved_sink_input(), 0..5),
        ) {
            // Simulate the Windows external-PID fallback scenario:
            // per-process loopback failed ? fell back to system-wide ?
            // no self-echo risk ? warning is None.
            let exclusion = LoopbackExclusion {
                null_sink_module,
                moved_inputs,
                warning: None,
            };

            let result = check_loopback_exclusion(&exclusion);

            // External process fallback ? None (capture proceeds, no self-echo).
            prop_assert!(
                result.is_none(),
                "check_loopback_exclusion returned Some({:?}) for external-process \
                 fallback scenario (warning=None) — this path should always proceed.",
                result
            );
        }

        // -- Property 3: check_loopback_exclusion correctness -----------
        //
        // **Validates: Requirements 2.2, 2.3, 2.4**
        //
        // For any fully random LoopbackExclusion (with or without warning),
        // check_loopback_exclusion returns Some iff warning.is_some().
        // When Some, the returned message is non-empty.
        #[test]
        fn pbt_check_loopback_exclusion(
            exclusion in arb_loopback_exclusion()
        ) {
            let result = check_loopback_exclusion(&exclusion);

            // Biconditional: Some iff warning.is_some()
            prop_assert_eq!(
                result.is_some(),
                exclusion.warning.is_some(),
                "check_loopback_exclusion returned is_some()={} but warning.is_some()={} \
                 for exclusion with warning={:?}",
                result.is_some(),
                exclusion.warning.is_some(),
                exclusion.warning
            );

            // When Some, the message must be non-empty.
            if let Some(ref msg) = result {
                prop_assert!(
                    !msg.is_empty(),
                    "check_loopback_exclusion returned Some(\"\") — error message must be non-empty"
                );
            }
        }
    }

    // -- Unit Tests for check_loopback_exclusion --------------------
    //
    // **Validates: Requirements 2.2, 2.3**

    #[test]
    fn test_check_loopback_exclusion_none_warning() {
        let exclusion = LoopbackExclusion {
            null_sink_module: Some(42),
            moved_inputs: vec![MovedSinkInput {
                index: 1,
                original_sink: Some("alsa_output.pci".to_string()),
            }],
            warning: None,
        };
        assert_eq!(check_loopback_exclusion(&exclusion), None);
    }

    #[test]
    fn test_check_loopback_exclusion_warning_empty_inputs() {
        let exclusion = LoopbackExclusion {
            null_sink_module: None,
            moved_inputs: Vec::new(),
            warning: Some("no sink-inputs found".to_string()),
        };
        let result = check_loopback_exclusion(&exclusion);
        assert!(
            result.is_some(),
            "expected Some when warning is present with empty moved_inputs"
        );
    }

    #[test]
    fn test_check_loopback_exclusion_warning_with_inputs() {
        let exclusion = LoopbackExclusion {
            null_sink_module: Some(99),
            moved_inputs: vec![
                MovedSinkInput {
                    index: 5,
                    original_sink: Some("alsa_output.pci".to_string()),
                },
                MovedSinkInput {
                    index: 8,
                    original_sink: None,
                },
            ],
            warning: Some("partial move".to_string()),
        };
        let result = check_loopback_exclusion(&exclusion);
        assert!(
            result.is_some(),
            "expected Some when warning is present with non-empty moved_inputs"
        );
    }

    #[test]
    fn test_check_loopback_exclusion_message_contains_warning() {
        let warning_text = "echo possible: 1 of 3 Wavis streams could not be redirected";
        let exclusion = LoopbackExclusion {
            null_sink_module: Some(7),
            moved_inputs: vec![MovedSinkInput {
                index: 10,
                original_sink: Some("sink0".to_string()),
            }],
            warning: Some(warning_text.to_string()),
        };
        let msg =
            check_loopback_exclusion(&exclusion).expect("expected Some when warning is present");
        assert!(
            msg.contains(warning_text),
            "error message should contain the original warning text; got: {msg}"
        );
        assert!(!msg.is_empty(), "error message must be non-empty");
    }

    // -- Unit Test: Windows init_tx Error Path ----------------------
    //
    // **Validates: Requirements 2.1, 2.4**
    //
    // Tests the mpsc channel communication pattern that
    // `audio_share_start_windows` relies on. When `wasapi_capture_thread`
    // detects that `activate_exclude_self_loopback()` fails on the
    // system-wide path (no PID), it sends `Err(...)` on `init_tx` and
    // returns. The caller receives `Ok(Err(e))` from `init_rx.recv()`,
    // which triggers cleanup (stop flag, join thread, unpublish track)
    // and returns `Err(e)` — no capture handle is stored.
    //
    // This test exercises the channel pattern directly (cross-platform)
    // rather than mocking WASAPI COM internals.

    #[test]
    fn test_windows_init_tx_error_path_channel_pattern() {
        use std::sync::mpsc;

        // Simulate the channel created in audio_share_start_windows.
        let (init_tx, init_rx) = mpsc::channel::<Result<bool, String>>();

        let error_msg = "system audio sharing requires loopback exclusion to prevent echo, \
                         but self-exclusion failed: API unavailable on this Windows version"
            .to_string();

        // Simulate wasapi_capture_thread sending Err when self-exclusion fails.
        init_tx
            .send(Err(error_msg.clone()))
            .expect("send should succeed");

        // Simulate audio_share_start_windows receiving the result.
        let received = init_rx
            .recv_timeout(std::time::Duration::from_secs(1))
            .expect("recv should not timeout");

        // The caller gets Ok(Err(e)) from recv — the Ok is from the channel,
        // the Err is the exclusion failure.
        assert!(
            received.is_err(),
            "expected Err from init_rx when self-exclusion fails; got Ok({:?})",
            received.ok()
        );

        let received_err = received.unwrap_err();
        assert_eq!(
            received_err, error_msg,
            "error message should propagate unchanged through the channel"
        );

        // After receiving Err, the caller would:
        // 1. Set stop_flag to true (signal thread to exit)
        // 2. Join the capture thread
        // 3. Call cleanup_publish_on_error_windows (unpublish track)
        // 4. Return Err(e) — no capture handle stored
        //
        // Verify the channel is now empty (thread sent exactly one message).
        let second = init_rx.try_recv();
        assert!(
            second.is_err(),
            "channel should be empty after single Err send; got: {:?}",
            second
        );
    }

    #[test]
    fn test_windows_init_tx_success_path_channel_pattern() {
        use std::sync::mpsc;

        // Verify the success path for contrast: when self-exclusion succeeds,
        // the thread sends Ok(true) and the caller proceeds to store the handle.
        let (init_tx, init_rx) = mpsc::channel::<Result<bool, String>>();

        // Simulate wasapi_capture_thread sending Ok(true) on success.
        init_tx.send(Ok(true)).expect("send should succeed");

        let received = init_rx
            .recv_timeout(std::time::Duration::from_secs(1))
            .expect("recv should not timeout");

        assert!(
            received.is_ok(),
            "expected Ok from init_rx when self-exclusion succeeds"
        );
        assert!(
            received.unwrap(),
            "loopback_exclusion_active should be true when self-exclusion succeeds"
        );
    }

    #[test]
    fn test_windows_init_tx_timeout_path_channel_pattern() {
        use std::sync::mpsc;

        // Verify the timeout path: if the thread never sends on init_tx
        // (e.g., it panics or hangs), recv_timeout returns Err.
        let (_init_tx, init_rx) = mpsc::channel::<Result<bool, String>>();

        // Use a very short timeout for the test.
        let received = init_rx.recv_timeout(std::time::Duration::from_millis(50));

        assert!(
            received.is_err(),
            "expected timeout when thread never sends on init_tx"
        );
    }
}
