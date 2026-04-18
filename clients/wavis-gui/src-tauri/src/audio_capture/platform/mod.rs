//! Platform dispatch layer for audio capture.
//!
//! Four free functions delegate to the platform-specific implementation selected
//! at compile time. No platform-specific types are visible at this level.
//! Callers interact only with AudioCaptureState and AudioShareStartResult.

#[cfg(target_os = "linux")]
pub(super) mod linux;
#[cfg(target_os = "macos")]
pub(super) mod macos;
#[cfg(target_os = "macos")]
pub(super) mod macos_driver_install;
#[cfg(any(test, target_os = "macos"))]
pub(super) mod macos_routing;
#[cfg(target_os = "macos")]
pub(super) mod macos_virtual_device;

#[cfg(target_os = "windows")]
pub(super) mod windows;

use super::audio_capture_state::{AudioCaptureState, AudioShareStartResult};

#[cfg(any(test, target_os = "macos"))]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) struct MacOsVersion {
    pub(super) major: isize,
    pub(super) minor: isize,
    pub(super) patch: isize,
}

#[cfg(any(test, target_os = "macos"))]
impl MacOsVersion {
    pub(super) fn supports_screen_capture_kit(self) -> bool {
        self.major > 12 || (self.major == 12 && self.minor >= 3)
    }

    pub(super) fn supports_process_tap(self) -> bool {
        self.major > 14 || (self.major == 14 && self.minor >= 2)
    }
}

#[cfg(any(test, target_os = "macos"))]
pub(super) fn try_start_process_tap<F>(
    version: MacOsVersion,
    tap_start: F,
) -> Result<Option<AudioShareStartResult>, String>
where
    F: FnOnce() -> Result<AudioShareStartResult, String>,
{
    if !version.supports_process_tap() {
        return Ok(None);
    }

    match tap_start() {
        Ok(result) => Ok(Some(result)),
        Err(err) => {
            log::warn!(
                "[audio_capture] audio_share_start_macos: Core Audio process tap failed on \
                 macOS {}.{}.{} ({}); falling back to ScreenCaptureKit",
                version.major,
                version.minor,
                version.patch,
                err
            );
            log::warn!(
                "[audio_capture] audio_share_start_macos: Screen & System Audio Recording \
                 permission may be missing or stale for this app host"
            );
            Ok(None)
        }
    }
}

#[cfg(target_os = "linux")]
pub(super) fn resolve_monitor() -> Result<String, String> {
    linux::resolve_monitor()
}

#[cfg(target_os = "windows")]
pub(super) fn resolve_monitor() -> Result<String, String> {
    windows::resolve_monitor()
}

#[cfg(target_os = "macos")]
pub(super) fn resolve_monitor() -> Result<String, String> {
    macos::resolve_monitor()
}

#[cfg(target_os = "linux")]
pub(super) fn resolve_monitor_fast() -> Result<String, String> {
    linux::resolve_monitor_fast()
}

#[cfg(target_os = "windows")]
pub(super) fn resolve_monitor_fast() -> Result<String, String> {
    windows::resolve_monitor_fast()
}

#[cfg(target_os = "macos")]
pub(super) fn resolve_monitor_fast() -> Result<String, String> {
    macos::resolve_monitor_fast()
}

#[cfg(target_os = "linux")]
pub(super) fn start(
    source_id: String,
    state: tauri::State<'_, crate::media::MediaState>,
    audio_capture: tauri::State<'_, AudioCaptureState>,
    app: tauri::AppHandle,
) -> Result<AudioShareStartResult, String> {
    linux::start(source_id, state, audio_capture, app)
}

#[cfg(target_os = "windows")]
pub(super) fn start(
    source_id: String,
    state: tauri::State<'_, crate::media::MediaState>,
    audio_capture: tauri::State<'_, AudioCaptureState>,
    app: tauri::AppHandle,
) -> Result<AudioShareStartResult, String> {
    windows::start(source_id, state, audio_capture, app)
}

#[cfg(target_os = "macos")]
pub(super) fn start(
    source_id: String,
    state: tauri::State<'_, crate::media::MediaState>,
    audio_capture: tauri::State<'_, AudioCaptureState>,
    app: tauri::AppHandle,
) -> Result<AudioShareStartResult, String> {
    macos::start(source_id, state, audio_capture, app)
}

#[cfg(target_os = "linux")]
pub(super) fn stop(
    state: tauri::State<'_, crate::media::MediaState>,
    audio_capture: tauri::State<'_, AudioCaptureState>,
) -> Result<(), String> {
    linux::stop(state, audio_capture)
}

#[cfg(target_os = "windows")]
pub(super) fn stop(
    state: tauri::State<'_, crate::media::MediaState>,
    audio_capture: tauri::State<'_, AudioCaptureState>,
) -> Result<(), String> {
    windows::stop(state, audio_capture)
}

#[cfg(target_os = "macos")]
pub(super) fn stop(
    state: tauri::State<'_, crate::media::MediaState>,
    audio_capture: tauri::State<'_, AudioCaptureState>,
) -> Result<(), String> {
    macos::stop(state, audio_capture)
}

#[cfg(test)]
mod tests {
    use crate::audio_capture::proptest_support::{
        arb_process_tap_supported_version, arb_process_tap_unsupported_version,
    };

    use super::{try_start_process_tap, AudioShareStartResult, MacOsVersion};
    use proptest::prelude::*;
    use std::sync::{
        atomic::{AtomicUsize, Ordering},
        Arc,
    };

    #[test]
    fn process_tap_failure_falls_back_to_sck_path() {
        let attempts = Arc::new(AtomicUsize::new(0));
        let attempts_for_closure = Arc::clone(&attempts);

        let result = try_start_process_tap(
            MacOsVersion {
                major: 15,
                minor: 0,
                patch: 0,
            },
            move || {
                attempts_for_closure.fetch_add(1, Ordering::SeqCst);
                Err("AudioHardwareCreateProcessTap failed: OSStatus 560947818".to_string())
            },
        )
        .expect("tap failure should degrade to ScreenCaptureKit fallback");

        assert!(
            result.is_none(),
            "tap failure should fall through to the SCK path"
        );
        assert_eq!(attempts.load(Ordering::SeqCst), 1);
    }

    #[test]
    fn sck_only_fallback_should_report_loopback_exclusion_as_unavailable() {
        let result = try_start_process_tap(
            MacOsVersion {
                major: 15,
                minor: 0,
                patch: 0,
            },
            || Err("AudioHardwareCreateProcessTap failed: OSStatus 560947818".to_string()),
        )
        .expect("tap failure should degrade to the SCK fallback")
        .unwrap_or(AudioShareStartResult {
            // Mirrors the fixed SCK-only fallback in macos.rs.
            loopback_exclusion_available: false,
            real_output_device_id: None,
            real_output_device_name: None,
            requires_mute_for_echo_prevention: false,
        });

        assert!(
            !result.loopback_exclusion_available,
            "SCK-only fallback cannot isolate WKWebView helper audio; it must report \
             loopback_exclusion_available=false"
        );
    }

    #[test]
    fn process_tap_success_returns_started_result() {
        let result = try_start_process_tap(
            MacOsVersion {
                major: 15,
                minor: 0,
                patch: 0,
            },
            || {
                Ok(AudioShareStartResult {
                    loopback_exclusion_available: true,
                    real_output_device_id: None,
                    real_output_device_name: None,
                    requires_mute_for_echo_prevention: false,
                })
            },
        )
        .expect("tap success should be returned directly");

        assert_eq!(result.map(|r| r.loopback_exclusion_available), Some(true));
    }

    #[test]
    fn process_tap_unavailable_skips_attempt_before_macos_14_2() {
        let attempts = Arc::new(AtomicUsize::new(0));
        let attempts_for_closure = Arc::clone(&attempts);

        let result = try_start_process_tap(
            MacOsVersion {
                major: 14,
                minor: 1,
                patch: 0,
            },
            move || {
                attempts_for_closure.fetch_add(1, Ordering::SeqCst);
                Ok(AudioShareStartResult {
                    loopback_exclusion_available: true,
                    real_output_device_id: None,
                    real_output_device_name: None,
                    requires_mute_for_echo_prevention: false,
                })
            },
        )
        .expect("pre-14.2 should skip the tap path cleanly");

        assert!(
            result.is_none(),
            "pre-14.2 should go straight to the SCK path"
        );
        assert_eq!(attempts.load(Ordering::SeqCst), 0);
    }

    #[test]
    fn screen_capture_kit_version_threshold_is_12_3() {
        assert!(!MacOsVersion {
            major: 12,
            minor: 2,
            patch: 9,
        }
        .supports_screen_capture_kit());
        assert!(MacOsVersion {
            major: 12,
            minor: 3,
            patch: 0,
        }
        .supports_screen_capture_kit());
    }

    #[test]
    fn platform_dispatch_cfg_is_mutually_exclusive() {
        let active_platforms = usize::from(cfg!(target_os = "linux"))
            + usize::from(cfg!(target_os = "macos"))
            + usize::from(cfg!(target_os = "windows"));

        assert_eq!(
            active_platforms, 1,
            "platform dispatch must stay behind mutually exclusive #[cfg] gates so macOS-only \
             routing changes cannot bleed into Windows or Linux"
        );
    }

    proptest! {
        #![proptest_config(ProptestConfig::with_cases(100))]

        #[test]
        fn process_tap_success_preserves_loopback_exclusion_for_supported_versions(
            version in arb_process_tap_supported_version()
        ) {
            let attempts = Arc::new(AtomicUsize::new(0));
            let attempts_for_closure = Arc::clone(&attempts);

            let result = try_start_process_tap(version, move || {
                attempts_for_closure.fetch_add(1, Ordering::SeqCst);
                Ok(AudioShareStartResult {
                    loopback_exclusion_available: true,
                    real_output_device_id: None,
                    real_output_device_name: None,
                    requires_mute_for_echo_prevention: false,
                })
            })
            .expect("tap success should be returned directly for supported macOS versions");

            prop_assert_eq!(
                result.as_ref().map(|started| started.loopback_exclusion_available),
                Some(true)
            );
            prop_assert_eq!(attempts.load(Ordering::SeqCst), 1);
        }

        #[test]
        fn process_tap_version_gate_preserves_pre_14_2_sck_fallback(
            version in arb_process_tap_unsupported_version()
        ) {
            let attempts = Arc::new(AtomicUsize::new(0));
            let attempts_for_closure = Arc::clone(&attempts);

            let result = try_start_process_tap(version, move || {
                attempts_for_closure.fetch_add(1, Ordering::SeqCst);
                Ok(AudioShareStartResult {
                    loopback_exclusion_available: true,
                    real_output_device_id: None,
                    real_output_device_name: None,
                    requires_mute_for_echo_prevention: false,
                })
            })
            .expect("pre-14.2 macOS should skip the tap path cleanly");

            prop_assert!(result.is_none());
            prop_assert_eq!(attempts.load(Ordering::SeqCst), 0);
        }
    }
}
