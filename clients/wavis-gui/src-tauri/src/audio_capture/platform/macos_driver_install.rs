//! macOS audio driver check and install helper.
//!
//! Detection: checks CoreAudio for any supported virtual loopback device
//! (BlackHole, Loopback Audio, or Wavis Audio Tap).
//!
//! Install: opens the BlackHole download page in the default browser.
//! BlackHole must be installed manually by the user; Wavis cannot install it
//! automatically since it ships as a signed installer package.

use tauri::AppHandle;

/// Returns true when a supported virtual audio loopback device (BlackHole,
/// Loopback, or Wavis Audio Tap) is present in the CoreAudio device list.
pub(in super::super) fn is_driver_installed() -> bool {
    super::macos_virtual_device::detect_virtual_audio_device().is_some()
}

/// Opens the BlackHole download page in the default browser so the user can
/// install it manually.
///
/// Errors:
/// - `"manual_install_required"` — always returned; the user must install
///                                  BlackHole themselves and restart Wavis.
pub(in super::super) fn install_driver(_app: &AppHandle) -> Result<(), String> {
    let _ = std::process::Command::new("open")
        .arg("https://existential.audio/blackhole/")
        .spawn();
    Err("manual_install_required".to_string())
}
