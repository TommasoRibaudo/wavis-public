//! Global panic hook for Wavis — captures backtraces and logs on crash.
//!
//! When a Rust panic occurs, this hook writes a `crash-report-<unix>.txt`
//! file containing the timestamp, panic message, location, backtrace, and a
//! snapshot of the recent in-memory logs.
//!
//! `eprintln!` is used intentionally here: the logging infrastructure may
//! itself be in a broken state at crash time, so stderr is the only safe
//! output channel.

use std::panic;
use std::fs;
use std::sync::Mutex;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};
use crate::bug_report::SharedLogBuffer;

/// Guards against the hook firing twice for one crash (WebView2 background-thread
/// panic causes a second panic on the main event-loop thread ~1s later).
static REPORT_WRITTEN: AtomicBool = AtomicBool::new(false);

/// Shared state for the crash handler to access the AppHandle lazily.
/// Panics can happen before the app is fully initialized.
static APP_HANDLE: Mutex<Option<tauri::AppHandle>> = Mutex::new(None);

/// Register the global AppHandle so the crash handler can resolve app-specific paths.
pub fn register_app_handle(app: tauri::AppHandle) {
    if let Ok(mut guard) = APP_HANDLE.lock() {
        *guard = Some(app);
    }
}

/// Install the global panic hook.
pub fn install(log_buffer: SharedLogBuffer) {
    panic::set_hook(Box::new(move |panic_info| {
        // Only write a crash report once per process lifetime.
        if REPORT_WRITTEN.swap(true, Ordering::SeqCst) {
            return;
        }

        let message = if let Some(s) = panic_info.payload().downcast_ref::<&str>() {
            s.to_string()
        } else if let Some(s) = panic_info.payload().downcast_ref::<String>() {
            s.clone()
        } else {
            "Unknown panic message".to_string()
        };

        let location = panic_info.location().map(|loc| {
            format!("{}:{}:{}", loc.file(), loc.line(), loc.column())
        }).unwrap_or_else(|| "Unknown location".to_string());

        let backtrace = std::backtrace::Backtrace::force_capture();
        let timestamp = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs();

        let mut report = String::new();
        report.push_str("Wavis Crash Report\n");
        report.push_str("==================\n\n");
        report.push_str(&format!("Timestamp (UTC unix): {}\n", timestamp));
        report.push_str(&format!("Message: {}\n", message));
        report.push_str(&format!("Location: {}\n\n", location));
        report.push_str("Backtrace:\n");
        report.push_str("----------\n");
        report.push_str(&format!("{:?}\n\n", backtrace));

        // Get log snapshot if possible
        report.push_str("Recent Logs:\n");
        report.push_str("------------\n");
        if let Ok(buffer) = log_buffer.lock() {
            let lines = buffer.snapshot();
            if lines.is_empty() {
                report.push_str("(no log records captured — crash occurred before any voice/room activity)\n");
            } else {
                for line in lines {
                    report.push_str(&line);
                    report.push('\n');
                }
            }
        } else {
            report.push_str("(log buffer unavailable — mutex poisoned)\n");
        }
        report.push('\n');

        let filename = format!("crash-report-{}.txt", timestamp);

        // Collect candidate paths in priority order
        let mut paths = Vec::new();

        // 1. App Data directory (platform-specific, most user-friendly)
        if let Ok(guard) = APP_HANDLE.lock() {
            if let Some(app) = &*guard {
                use tauri::Manager;
                if let Ok(data_dir) = app.path().app_data_dir() {
                    paths.push(data_dir.join(&filename));
                }
            }
        }

        // 2. Current working directory
        if let Ok(cwd) = std::env::current_dir() {
            paths.push(cwd.join(&filename));
        }

        // 3. OS temp directory — cross-platform, always exists
        paths.push(std::env::temp_dir().join(&filename));

        let mut saved = false;
        for path in paths {
            if let Some(parent) = path.parent() {
                let _ = fs::create_dir_all(parent);
            }
            if fs::write(&path, &report).is_ok() {
                eprintln!("wavis: crash report saved to: {}", path.display());
                saved = true;
                break;
            }
        }

        if !saved {
            eprintln!("wavis: failed to save crash report to any known location.");
        }

        eprintln!("wavis: application panicked at {}: {}", location, message);
    }));
}
