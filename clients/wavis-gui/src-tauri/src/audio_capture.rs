//! Audio capture subsystem - public API and Tauri command surface.
//!
//! Stable entry point for all audio capture operations. Owns the four Tauri
//! commands registered in `main.rs` and re-exports `AudioCaptureState` for
//! `.manage()`. Platform implementations are entirely contained in
//! `audio_capture/platform/`. Callers see no platform-specific types or cfg branches.
//!
//! Invariants:
//! - Only one active capture session per process (enforced by `AudioCaptureState`).
//! - `audio_share_stop` is idempotent and safe to call when no capture is active.
//! - Platform dispatch is compile-time only; no runtime platform checks here.

mod audio_capture_state;
mod platform;
#[cfg(test)]
pub(crate) mod proptest_support;

pub use audio_capture_state::{AudioCaptureState, AudioShareStartResult};

/// Returns which audio-share features the current OS supports.
/// Used by the frontend to gate UI before attempting a Rust call that would fail.
#[tauri::command]
pub fn get_platform_capabilities() -> PlatformCapabilities {
    let (has_screen_capture_kit, has_process_tap) = platform::platform_capabilities();
    PlatformCapabilities { has_screen_capture_kit, has_process_tap }
}

#[derive(serde::Serialize)]
pub struct PlatformCapabilities {
    pub has_screen_capture_kit: bool,
    pub has_process_tap: bool,
}

/// Resolve the default system audio monitor source name.
///
/// Used by Screen+Audio and Window+Audio modes where the audio source is implicit.
#[tauri::command]
pub fn get_default_audio_monitor() -> Result<String, String> {
    platform::resolve_monitor()
}

/// Lightweight variant that resolves the default monitor source via the platform
/// fallback path rather than the heavier native API path.
#[tauri::command]
pub fn get_default_audio_monitor_fast() -> Result<String, String> {
    platform::resolve_monitor_fast()
}

/// Start capturing system audio from the given platform source identifier.
#[tauri::command]
pub fn audio_share_start(
    source_id: String,
    state: tauri::State<'_, crate::media::MediaState>,
    audio_capture: tauri::State<'_, AudioCaptureState>,
    app: tauri::AppHandle,
) -> Result<AudioShareStartResult, String> {
    platform::start(source_id, state, audio_capture, app)
}

/// Stop the active audio capture session.
#[tauri::command]
pub fn audio_share_stop(
    state: tauri::State<'_, crate::media::MediaState>,
    audio_capture: tauri::State<'_, AudioCaptureState>,
) -> Result<(), String> {
    platform::stop(state, audio_capture)
}

/// Check whether the WavisAudioTap HAL driver is installed.
/// Returns true if the .driver bundle is present in /Library/Audio/Plug-Ins/HAL/.
/// Always returns true on non-macOS platforms.
#[cfg(target_os = "macos")]
#[tauri::command]
pub fn check_audio_driver() -> bool {
    platform::macos_driver_install::is_driver_installed()
}

#[cfg(not(target_os = "macos"))]
#[tauri::command]
pub fn check_audio_driver() -> bool {
    true
}

/// Install the bundled WavisAudioTap HAL driver.
/// Shows a native macOS admin password dialog. Blocks until the device
/// appears in CoreAudio (up to 10 s) or returns an error string:
/// `"resource_not_found"`, `"user_cancelled"`, `"copy_failed:<detail>"`,
/// or `"device_not_ready"`. No-op on non-macOS platforms.
#[cfg(target_os = "macos")]
#[tauri::command]
pub fn install_audio_driver(app: tauri::AppHandle) -> Result<(), String> {
    platform::macos_driver_install::install_driver(&app)
}

#[cfg(not(target_os = "macos"))]
#[tauri::command]
pub fn install_audio_driver(_app: tauri::AppHandle) -> Result<(), String> {
    Ok(())
}
