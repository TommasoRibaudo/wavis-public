use log::LevelFilter;

fn env_flag(name: &str) -> bool {
    matches!(
        std::env::var(name).as_deref(),
        Ok("1" | "true" | "TRUE" | "True")
    )
}

pub fn rust_debug_logs_enabled() -> bool {
    env_flag("WAVIS_DEBUG_LOGS")
}

pub fn screen_capture_debug_enabled() -> bool {
    env_flag("WAVIS_DEBUG_SCREEN_CAPTURE")
}

pub fn diagnostics_window_enabled() -> bool {
    env_flag("WAVIS_DIAGNOSTICS_WINDOW")
}

pub fn diagnostics_notifications_enabled() -> bool {
    env_flag("WAVIS_DIAGNOSTICS_NOTIFICATIONS")
}

pub fn debug_share_audio_enabled() -> bool {
    env_flag("WAVIS_DEBUG_SHARE_AUDIO")
}

pub fn tauri_log_level() -> LevelFilter {
    if rust_debug_logs_enabled() || screen_capture_debug_enabled() || debug_share_audio_enabled() {
        LevelFilter::Info
    } else {
        LevelFilter::Warn
    }
}

pub fn debug_stderr_enabled() -> bool {
    rust_debug_logs_enabled() || screen_capture_debug_enabled() || debug_share_audio_enabled()
}

#[macro_export]
macro_rules! debug_eprintln {
    ($($arg:tt)*) => {
        if $crate::debug_env::debug_stderr_enabled() {
            eprintln!($($arg)*);
        }
    };
}
