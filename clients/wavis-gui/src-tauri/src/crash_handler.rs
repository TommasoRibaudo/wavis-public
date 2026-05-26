//! Global panic hook for Wavis — captures backtraces and logs on crash.
//!
//! When a Rust panic occurs, this hook writes a `crash-report.txt` file
//! containing the panic message, location, backtrace, and a snapshot of
//! the recent in-memory logs.

use std::panic;
use std::fs;
use std::sync::Mutex;
use crate::bug_report::SharedLogBuffer;

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

        let backtrace = std::backtrace::Backtrace::capture();
        
        let mut report = String::new();
        report.push_str("Wavis Crash Report\n");
        report.push_str("==================\n\n");
        report.push_str(&format!("Message: {}\n", message));
        report.push_str(&format!("Location: {}\n\n", location));
        report.push_str("Backtrace:\n");
        report.push_str("----------\n");
        report.push_str(&format!("{:?}\n\n", backtrace));

        // Get log snapshot if possible
        if let Ok(buffer) = log_buffer.lock() {
            report.push_str("Recent Logs:\n");
            report.push_str("------------\n");
            for line in buffer.snapshot() {
                report.push_str(&line);
                report.push_str("\n");
            }
            report.push_str("\n");
        }

        // Collect potential paths to write the report to
        let mut paths = Vec::new();

        // 1. Try App Data directory if AppHandle is available
        if let Ok(guard) = APP_HANDLE.lock() {
            if let Some(app) = &*guard {
                use tauri::Manager;
                if let Ok(data_dir) = app.path().app_data_dir() {
                    paths.push(data_dir.join("crash-report.txt"));
                }
            }
        }

        // 2. Try Current Working Directory
        if let Ok(cwd) = std::env::current_dir() {
            paths.push(cwd.join("crash-report.txt"));
        }

        // 3. Fallback to a hardcoded temp path if everything else fails
        #[cfg(target_os = "windows")]
        paths.push(std::path::PathBuf::from("C:\\Temp\\wavis-crash.txt"));
        #[cfg(not(target_os = "windows"))]
        paths.push(std::path::PathBuf::from("/tmp/wavis-crash.txt"));

        let mut saved = false;
        for path in paths {
            if let Some(parent) = path.parent() {
                let _ = fs::create_dir_all(parent);
            }
            if fs::write(&path, &report).is_ok() {
                eprintln!("wavis: crash report saved to: {}", path.display());
                saved = true;
                // Only save to the first successful path
                break;
            }
        }

        if !saved {
            eprintln!("wavis: failed to save crash report to any known location.");
        }

        // Also print the crash info to stderr for console visibility
        eprintln!("wavis: application panicked at {}: {}", location, message);
    }));
}
