//! Tauri desktop application entry point and command-registration boundary.
//!
//! This module owns process startup, plugin and tray wiring, global shortcut
//! setup, platform-specific initialization for screen capture permissions and
//! external share helpers, state registration, and the full `invoke_handler!`
//! surface exposed to the frontend. It also defines the keyring cache and the
//! small DTOs and commands that belong to the app shell itself. It does not own
//! audio capture internals (`audio_capture.rs`), native media transport
//! (`media.rs`), share-source enumeration (`share_sources.rs`), or backend and
//! shared signaling rules.
//!
//! Invariants:
//! - Every frontend command exposed from Rust must be registered in
//!   `invoke_handler!`.
//! - Every window label used with Tauri `listen`/`emit` paths must stay aligned
//!   with `capabilities/default.json`, or event delivery can fail silently.
//! - Platform-specific modules such as portal auth, screen-recording auth, and
//!   the external share helper must compile to real implementations on supported
//!   targets and safe stubs elsewhere.
// Prevents additional console window on Windows in release
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod audio_capture;
#[cfg(target_os = "linux")]
mod external_share_helper;
#[cfg(not(target_os = "linux"))]
mod external_share_helper {
    pub struct ExternalShareHelperState;
    impl ExternalShareHelperState {
        pub fn new() -> Self {
            Self
        }
    }

    #[tauri::command]
    pub fn external_share_start() -> Result<(), String> {
        Err("external share helper is only available on Linux".to_string())
    }

    #[tauri::command]
    pub fn external_share_stop() -> Result<(), String> {
        Ok(())
    }
}
#[cfg(any(target_os = "linux", target_os = "windows"))]
mod media;
#[cfg(not(any(target_os = "linux", target_os = "windows")))]
mod media {
    /// Stub for macOS — the LiveKit JS SDK handles media directly.
    pub struct MediaState;
    impl MediaState {
        pub fn new() -> Self {
            Self
        }
        pub fn ensure_audio_streams(&self) -> Result<(), String> {
            Ok(())
        }
        pub fn set_selected_device(&self, _kind: &str, _raw_name: &str) {}
        pub fn set_input_gain(&self, _gain: f32) {}
    }

    #[tauri::command]
    pub fn media_connect(
        _url: String,
        _token: String,
        _denoise_enabled: bool,
    ) -> Result<(), String> {
        Ok(())
    }

    #[tauri::command]
    pub fn media_disconnect() -> Result<(), String> {
        Ok(())
    }

    #[tauri::command]
    pub fn media_set_mic_enabled(_enabled: bool) -> Result<(), String> {
        Ok(())
    }

    #[tauri::command]
    pub fn media_set_denoise_enabled(_enabled: bool) {}

    #[tauri::command]
    pub fn media_set_participant_volume(_identity: String, _volume: u8) -> Result<(), String> {
        Ok(())
    }

    #[tauri::command]
    pub fn media_set_screen_share_audio_volume(
        _identity: String,
        _volume: u8,
    ) -> Result<(), String> {
        Ok(())
    }

    #[tauri::command]
    pub fn media_attach_screen_share_audio(_identity: String) -> Result<(), String> {
        Ok(())
    }

    #[tauri::command]
    pub fn media_detach_screen_share_audio(_identity: String) -> Result<(), String> {
        Ok(())
    }

    #[tauri::command]
    pub fn media_set_master_volume(_volume: u8) -> Result<(), String> {
        Ok(())
    }

    #[tauri::command]
    pub fn media_is_connected() -> Result<bool, String> {
        Ok(false)
    }

    #[tauri::command]
    pub fn screen_share_start() -> Result<(), String> {
        Err("screen share is only available on Linux/Windows".to_string())
    }

    #[tauri::command]
    pub fn screen_share_start_source(_source_id: String) -> Result<(), String> {
        Err("screen share is only available on Linux/Windows".to_string())
    }

    #[tauri::command]
    pub fn screen_share_stop() -> Result<(), String> {
        Ok(())
    }

    #[tauri::command]
    pub fn screen_share_poll_frame() -> Result<Option<String>, String> {
        Ok(None)
    }

    #[tauri::command]
    pub fn media_set_screen_share_quality(_quality: u8) -> Result<(), String> {
        Ok(())
    }
}
#[cfg(target_os = "linux")]
mod portal_auth;
#[cfg(not(target_os = "linux"))]
mod portal_auth {
    /// Stub for non-Linux platforms — portal auth is Linux-only.
    pub struct PortalAuthState;
    impl PortalAuthState {
        pub fn new() -> Self {
            Self
        }
    }

    #[tauri::command]
    pub fn authorize_screen_capture() -> Result<String, String> {
        Ok("not_needed".to_string())
    }

    #[tauri::command]
    pub fn get_capture_auth_status() -> Result<CaptureAuthStatus, String> {
        Ok(CaptureAuthStatus {
            display_server: "unsupported".to_string(),
            authorized: false,
            needs_auth: false,
            was_attempted: false,
        })
    }

    #[derive(Debug, Clone, serde::Serialize)]
    pub struct CaptureAuthStatus {
        pub display_server: String,
        pub authorized: bool,
        pub needs_auth: bool,
        pub was_attempted: bool,
    }
}
#[cfg(target_os = "macos")]
mod screen_recording_auth {
    #[derive(Debug, Clone, serde::Serialize)]
    #[serde(rename_all = "camelCase")]
    pub struct ScreenRecordingAccessStatus {
        pub authorized: bool,
        pub prompt_shown: bool,
        pub restart_required: bool,
    }

    #[link(name = "CoreGraphics", kind = "framework")]
    unsafe extern "C" {
        fn CGPreflightScreenCaptureAccess() -> bool;
        fn CGRequestScreenCaptureAccess() -> bool;
    }

    #[tauri::command]
    pub fn ensure_screen_recording_access() -> Result<ScreenRecordingAccessStatus, String> {
        let already_authorized = unsafe { CGPreflightScreenCaptureAccess() };
        if already_authorized {
            return Ok(ScreenRecordingAccessStatus {
                authorized: true,
                prompt_shown: false,
                restart_required: false,
            });
        }

        let granted = unsafe { CGRequestScreenCaptureAccess() };

        Ok(ScreenRecordingAccessStatus {
            authorized: granted,
            prompt_shown: true,
            // macOS commonly requires the process to relaunch before a newly granted
            // Screen Recording permission becomes usable by capture APIs.
            restart_required: granted,
        })
    }
}
#[cfg(not(target_os = "macos"))]
mod screen_recording_auth {
    #[derive(Debug, Clone, serde::Serialize)]
    #[serde(rename_all = "camelCase")]
    pub struct ScreenRecordingAccessStatus {
        pub authorized: bool,
        pub prompt_shown: bool,
        pub restart_required: bool,
    }

    #[tauri::command]
    pub fn ensure_screen_recording_access() -> Result<ScreenRecordingAccessStatus, String> {
        Ok(ScreenRecordingAccessStatus {
            authorized: true,
            prompt_shown: false,
            restart_required: false,
        })
    }
}
mod bug_report;
mod debug_env;
mod diagnostics;
#[cfg(target_os = "windows")]
mod native_mic;
#[cfg(not(target_os = "windows"))]
mod native_mic {
    pub struct NativeMicState;
    impl NativeMicState {
        pub fn new() -> Self {
            Self
        }
    }

    #[tauri::command]
    pub fn native_mic_start(
        _denoise_enabled: bool,
        _device_id: Option<String>,
    ) -> Result<(), String> {
        Ok(())
    }

    #[tauri::command]
    pub fn native_mic_stop() -> Result<(), String> {
        Ok(())
    }

    #[tauri::command]
    pub fn native_mic_set_denoise_enabled(_enabled: bool) -> Result<(), String> {
        Ok(())
    }

    #[tauri::command]
    pub fn native_mic_set_input_device(_device_id: String) -> Result<(), String> {
        Ok(())
    }
}
#[cfg(any(target_os = "linux", target_os = "windows", test))]
mod screen_capture;
mod share_sources;
mod tray;

use cpal::traits::{DeviceTrait, HostTrait};
#[cfg(target_os = "linux")]
use serde_json::Value;
#[cfg(target_os = "linux")]
use std::process::Command;
use tauri::{Emitter, Manager};

fn main() {
    // Load .env in debug builds so WAVIS_* vars work without manually exporting them.
    // dotenvy searches from CWD upward, so it finds clients/wavis-gui/.env from src-tauri/.
    // No-op in release builds or when no .env file is present.
    #[cfg(debug_assertions)]
    let _ = dotenvy::dotenv();

    // Work around WebKitGTK + Wayland compositing crash (GDK Protocol error 71).
    #[cfg(target_os = "linux")]
    if std::env::var("WEBKIT_DISABLE_COMPOSITING_MODE").is_err() {
        std::env::set_var("WEBKIT_DISABLE_COMPOSITING_MODE", "1");
    }

    // Create the shared Rust log buffer for bug report diagnostics.
    let log_buffer = bug_report::new_shared_buffer(200);
    let log_layer = bug_report::build_bug_report_log_layer(log_buffer.clone());

    tauri::Builder::default()
        .plugin(
            tauri_plugin_log::Builder::new()
                .level(debug_env::tauri_log_level())
                .target(tauri_plugin_log::Target::new(
                    tauri_plugin_log::TargetKind::Dispatch(log_layer),
                ))
                .build(),
        )
        .plugin(tauri_plugin_store::Builder::new().build())
        .plugin(tauri_plugin_http::init())
        .plugin(tauri_plugin_notification::init())
        .plugin(tauri_plugin_shell::init())
        .plugin(tauri_plugin_clipboard_manager::init())
        .manage(KeyringCache::new())
        .manage(media::MediaState::new())
        .manage(external_share_helper::ExternalShareHelperState::new())
        .manage(portal_auth::PortalAuthState::new())
        .manage(audio_capture::AudioCaptureState::new())
        .manage(native_mic::NativeMicState::new())
        .manage(bug_report::RustLogBufferState::new(log_buffer))
        .manage(diagnostics::DiagnosticsSystemState(std::sync::Mutex::new(
            sysinfo::System::new(),
        )))
        .setup(|app| {
            #[cfg(desktop)]
            {
                if let Err(err) = app
                    .handle()
                    .plugin(tauri_plugin_global_shortcut::Builder::new().build())
                {
                    eprintln!("wavis: global shortcut plugin unavailable: {err}");
                }

                if let Err(err) = tray::setup_tray(app) {
                    eprintln!("wavis: tray unavailable: {err}");
                }
            }
            Ok(())
        })
        .on_window_event(|window, event| {
            if let tauri::WindowEvent::CloseRequested { api, .. } = event {
                let label = window.label();
                let app = window.app_handle();

                // Only the main window gets minimize-to-tray behavior
                if label == "main" {
                    if let Some(webview_window) = app.get_webview_window(label) {
                        if let (Some(flag), Some(vis)) = (
                            app.try_state::<tray::MinimizeToTrayFlag>(),
                            app.try_state::<tray::WindowVisibility>(),
                        ) {
                            if tray::handle_close_requested(&webview_window, &flag, &vis) {
                                api.prevent_close();
                                return;
                            }
                        }
                    }
                    // Main window is actually closing (not minimized to tray).
                    // Notify the frontend so it can tear down the voice session
                    // and close child windows before the window is destroyed.
                    let _ = app.emit("main-window-closing", ());

                    // Also emit voice-session:ended directly so pop-out windows
                    // (ScreenSharePage) self-close even if the main window's JS
                    // listener doesn't execute in time (race condition on destroy).
                    let _ = app.emit("voice-session:ended", ());
                }
            }
        })
        .invoke_handler(tauri::generate_handler![
            list_audio_devices,
            store_token,
            get_token,
            delete_token,
            set_peer_volume,
            set_master_volume,
            set_audio_device,
            set_input_gain,
            bug_report::get_rust_log_buffer,
            bug_report::capture_window_screenshot,
            media::media_connect,
            media::media_disconnect,
            media::media_set_mic_enabled,
            media::media_set_denoise_enabled,
            media::media_set_participant_volume,
            media::media_set_screen_share_audio_volume,
            media::media_attach_screen_share_audio,
            media::media_detach_screen_share_audio,
            media::media_set_master_volume,
            media::media_is_connected,
            media::screen_share_start,
            media::screen_share_start_source,
            media::screen_share_stop,
            media::screen_share_poll_frame,
            media::media_set_screen_share_quality,
            is_window_visible,
            share_sources::list_share_sources,
            share_sources::fetch_source_thumbnail,
            share_sources::share_picker_select,
            share_sources::share_picker_cancel,
            portal_auth::authorize_screen_capture,
            portal_auth::get_capture_auth_status,
            screen_recording_auth::ensure_screen_recording_access,
            external_share_helper::external_share_start,
            external_share_helper::external_share_stop,
            audio_capture::get_default_audio_monitor,
            audio_capture::get_default_audio_monitor_fast,
            audio_capture::audio_share_start,
            audio_capture::audio_share_stop,
            audio_capture::check_audio_driver,
            audio_capture::install_audio_driver,
            diagnostics::get_diagnostics_config,
            diagnostics::get_diagnostics_snapshot,
            native_mic::native_mic_start,
            native_mic::native_mic_stop,
            native_mic::native_mic_set_denoise_enabled,
            native_mic::native_mic_set_input_device,
        ])
        .build(tauri::generate_context!())
        .expect("error while building wavis")
        .run(|_app, event| {
            if let tauri::RunEvent::Exit = event {
                // Stop any active screen capture so the OS overlay is removed
                // before the process exits.
                #[cfg(any(target_os = "linux", target_os = "windows"))]
                if let Some(state) = _app.try_state::<media::MediaState>() {
                    let mut sc_guard = state
                        .screen_capture
                        .lock()
                        .unwrap_or_else(|e| e.into_inner());
                    if let Some(cap) = sc_guard.take() {
                        cap.stop();
                        log::info!("[wavis] exit: screen capture stopped");
                    }
                }
            }
        });
}

// ─── IPC Commands ──────────────────────────────────────────────────

#[derive(serde::Serialize)]
struct AudioDevice {
    id: String,
    name: String,
    kind: String, // "input" | "output"
    is_default: bool,
}

#[tauri::command]
fn list_audio_devices() -> Result<Vec<AudioDevice>, String> {
    #[cfg(target_os = "linux")]
    if let Ok(devices) = list_linux_pulse_audio_devices() {
        if !devices.is_empty() {
            return Ok(devices);
        }
    }

    list_cpal_audio_devices()
}

fn list_cpal_audio_devices() -> Result<Vec<AudioDevice>, String> {
    let host = cpal::default_host();

    let default_input_name = host
        .default_input_device()
        .and_then(|device| device.name().ok());
    let default_output_name = host
        .default_output_device()
        .and_then(|device| device.name().ok());

    let mut devices = Vec::new();

    if let Ok(inputs) = host.input_devices() {
        for device in inputs {
            if let Ok(name) = device.name() {
                devices.push(AudioDevice {
                    id: format!("input:{name}"),
                    is_default: default_input_name.as_deref() == Some(name.as_str()),
                    kind: "input".into(),
                    name,
                });
            }
        }
    }

    if let Ok(outputs) = host.output_devices() {
        for device in outputs {
            if let Ok(name) = device.name() {
                devices.push(AudioDevice {
                    id: format!("output:{name}"),
                    is_default: default_output_name.as_deref() == Some(name.as_str()),
                    kind: "output".into(),
                    name,
                });
            }
        }
    }

    Ok(devices)
}

#[cfg(target_os = "linux")]
fn list_linux_pulse_audio_devices() -> Result<Vec<AudioDevice>, String> {
    let default_sink = pactl_default_name("Default Sink:");
    let default_source = pactl_default_name("Default Source:");

    let mut devices = Vec::new();
    devices.extend(parse_pactl_devices(
        "sinks",
        "output",
        default_sink.as_deref(),
    )?);
    devices.extend(parse_pactl_devices(
        "sources",
        "input",
        default_source.as_deref(),
    )?);

    Ok(devices)
}

#[cfg(target_os = "linux")]
fn pactl_default_name(prefix: &str) -> Option<String> {
    let output = Command::new("pactl").arg("info").output().ok()?;
    if !output.status.success() {
        return None;
    }

    let stdout = String::from_utf8(output.stdout).ok()?;
    stdout.lines().find_map(|line| {
        line.strip_prefix(prefix)
            .map(|value| value.trim().to_string())
    })
}

#[cfg(target_os = "linux")]
fn parse_pactl_devices(
    category: &str,
    kind: &str,
    default_name: Option<&str>,
) -> Result<Vec<AudioDevice>, String> {
    let output = Command::new("pactl")
        .args(["-f", "json", "list", category])
        .output()
        .map_err(|err| format!("failed to execute pactl: {err}"))?;

    if !output.status.success() {
        return Err(format!(
            "pactl list {category} failed with status {}",
            output.status
        ));
    }

    let entries: Value = serde_json::from_slice(&output.stdout)
        .map_err(|err| format!("failed to parse pactl {category} json: {err}"))?;
    let items = entries
        .as_array()
        .ok_or_else(|| format!("unexpected pactl {category} json shape"))?;

    let mut devices = Vec::new();
    for item in items {
        let name = item.get("name").and_then(Value::as_str).unwrap_or_default();
        if name.is_empty() {
            continue;
        }

        let description = item
            .get("description")
            .and_then(Value::as_str)
            .or_else(|| {
                item.get("properties")
                    .and_then(|props| props.get("device.description"))
                    .and_then(Value::as_str)
            })
            .or_else(|| {
                item.get("properties")
                    .and_then(|props| props.get("node.description"))
                    .and_then(Value::as_str)
            })
            .unwrap_or(name);

        devices.push(AudioDevice {
            id: format!("{kind}:{name}"),
            name: description.to_string(),
            kind: kind.to_string(),
            is_default: default_name == Some(name),
        });
    }

    Ok(devices)
}

const KEYRING_SERVICE: &str = "com.wavis.gui";

/// In-memory cache for keyring values.
///
/// Each keyring read on macOS triggers a system "allow" prompt. By caching
/// after the first read we hit the keychain exactly once per key per session:
/// the cache is populated lazily on the first `get_token` and kept in sync
/// by `store_token` / `delete_token`. The cache lives only in process memory
/// and is never persisted to disk.
struct KeyringCache(std::sync::Mutex<std::collections::HashMap<String, String>>);

impl KeyringCache {
    fn new() -> Self {
        Self(std::sync::Mutex::new(std::collections::HashMap::new()))
    }
}

#[tauri::command]
fn store_token(
    key: String,
    value: String,
    cache: tauri::State<'_, KeyringCache>,
) -> Result<(), String> {
    let entry = keyring::Entry::new(KEYRING_SERVICE, &key).map_err(|e| e.to_string())?;
    entry.set_password(&value).map_err(|e| e.to_string())?;
    // Keep cache in sync so the next get_token is served from memory.
    cache.0.lock().unwrap().insert(key, value);
    Ok(())
}

#[tauri::command]
fn get_token(key: String, cache: tauri::State<'_, KeyringCache>) -> Result<Option<String>, String> {
    {
        let map = cache.0.lock().unwrap();
        if let Some(cached) = map.get(&key) {
            return Ok(Some(cached.clone()));
        }
    }
    // Cache miss — read from keychain once and populate the cache.
    let entry = keyring::Entry::new(KEYRING_SERVICE, &key).map_err(|e| e.to_string())?;
    match entry.get_password() {
        Ok(val) => {
            cache.0.lock().unwrap().insert(key, val.clone());
            Ok(Some(val))
        }
        Err(keyring::Error::NoEntry) => Ok(None),
        Err(e) => Err(e.to_string()),
    }
}

#[tauri::command]
fn delete_token(key: String, cache: tauri::State<'_, KeyringCache>) -> Result<(), String> {
    cache.0.lock().unwrap().remove(&key);
    let entry = keyring::Entry::new(KEYRING_SERVICE, &key).map_err(|e| e.to_string())?;
    match entry.delete_credential() {
        Ok(()) => Ok(()),
        Err(keyring::Error::NoEntry) => Ok(()), // already gone — idempotent
        Err(e) => Err(e.to_string()),
    }
}

#[tauri::command]
fn set_peer_volume(participant_id: String, level: u32) -> Result<(), String> {
    let _clamped = level.min(100) as u8;
    let _ = participant_id;
    // TODO: Wire to PeerVolumes (Arc<Mutex<HashMap<String, u8>>>)
    Ok(())
}

#[tauri::command]
fn set_master_volume(level: u32) -> Result<(), String> {
    let _clamped = level.min(100) as u8;
    // TODO: Wire to AudioBuffer::set_volume
    Ok(())
}

#[tauri::command]
fn set_audio_device(
    device_id: String,
    kind: String,
    state: tauri::State<'_, media::MediaState>,
) -> Result<(), String> {
    if kind != "input" && kind != "output" {
        return Err(format!(
            "invalid device kind: {kind}, expected \"input\" or \"output\""
        ));
    }

    let expected_prefix = if kind == "input" { "input:" } else { "output:" };
    let raw_name = device_id
        .strip_prefix(expected_prefix)
        .ok_or_else(|| format!("invalid {kind} device id: {device_id}"))?;

    #[cfg(target_os = "linux")]
    {
        let pactl_command = if kind == "input" {
            "set-default-source"
        } else {
            "set-default-sink"
        };

        let output = Command::new("pactl")
            .args([pactl_command, raw_name])
            .output()
            .map_err(|err| format!("failed to run pactl {pactl_command}: {err}"))?;

        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr);
            return Err(format!(
                "pactl {pactl_command} failed with status {}: {}",
                output.status,
                stderr.trim()
            ));
        }
    }

    // Store the selected device name so CPAL opens the right device on restart.
    state.set_selected_device(&kind, raw_name);

    state.ensure_audio_streams()?;

    Ok(())
}

#[tauri::command]
fn set_input_gain(gain: f32, state: tauri::State<'_, media::MediaState>) -> Result<(), String> {
    state.set_input_gain(gain.clamp(0.0, 1.0));
    Ok(())
}

#[tauri::command]
fn is_window_visible(state: tauri::State<'_, tray::WindowVisibility>) -> bool {
    !state.hidden.load(std::sync::atomic::Ordering::SeqCst)
}
