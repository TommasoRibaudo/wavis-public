use std::collections::{HashMap, HashSet, VecDeque};
use std::sync::Mutex;
use sysinfo::{Pid, ProcessesToUpdate, System};

/* ─── Types ─────────────────────────────────────────────────────── */

/// Env-var configuration for the diagnostics window.
/// All fields are read from environment variables at call time.
#[derive(Debug, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DiagnosticsConfig {
    pub enabled: bool,
    pub notifications_enabled: bool,
    /// Polling interval in milliseconds (WAVIS_DIAGNOSTICS_POLL_MS, default 1000).
    pub poll_ms: u64,
    /// RSS warning threshold in MB (WAVIS_DIAGNOSTICS_MEMORY_WARN_MB, default 1200).
    pub memory_warn_mb: f64,
    /// Network send warning threshold in Mbps (WAVIS_DIAGNOSTICS_NETWORK_WARN_MBPS, default 20).
    pub network_warn_mbps: f64,
    /// Render time warning threshold in ms (WAVIS_DIAGNOSTICS_RENDER_WARN_MS, default 25).
    pub render_warn_ms: f64,
}

/// A single memory snapshot for the Wavis process tree.
/// Sums RSS for the main process and all descendants (full BFS traversal),
/// because WebView2 on Windows spawns multi-level trees (browser → renderer/GPU/utility).
#[derive(Debug, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DiagnosticsSnapshot {
    /// Total RSS of the full process tree in MB (main + all descendants).
    pub rss_mb: f64,
    /// Number of descendant processes found (informational).
    pub child_count: usize,
    /// Wall-clock timestamp (Unix milliseconds).
    pub timestamp_ms: u64,
    /// Average CPU usage across all logical cores as a percentage (0–100).
    /// Returns 0.0 on the very first call because sysinfo needs two samples to
    /// compute a delta. Subsequent calls reflect real usage.
    pub cpu_usage_percent: f64,
}

/// Shared sysinfo System instance managed by Tauri.
/// Wrapping in a newtype avoids conflicts if other modules manage a plain Mutex<System>.
pub struct DiagnosticsSystemState(pub Mutex<System>);

/* ─── Helpers ───────────────────────────────────────────────────── */

fn env_u64(name: &str, default: u64) -> u64 {
    std::env::var(name)
        .ok()
        .and_then(|v| v.parse::<u64>().ok())
        .unwrap_or(default)
}

fn env_f64(name: &str, default: f64) -> f64 {
    std::env::var(name)
        .ok()
        .and_then(|v| v.parse::<f64>().ok())
        .unwrap_or(default)
}

fn unix_now_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

/* ─── Commands ──────────────────────────────────────────────────── */

/// Returns the diagnostics configuration derived from environment variables.
/// Called once by the frontend on startup to decide whether to open the window.
#[tauri::command]
pub fn get_diagnostics_config() -> DiagnosticsConfig {
    DiagnosticsConfig {
        enabled: crate::debug_env::diagnostics_window_enabled(),
        notifications_enabled: crate::debug_env::diagnostics_notifications_enabled(),
        poll_ms: env_u64("WAVIS_DIAGNOSTICS_POLL_MS", 1000),
        memory_warn_mb: env_f64("WAVIS_DIAGNOSTICS_MEMORY_WARN_MB", 1200.0),
        network_warn_mbps: env_f64("WAVIS_DIAGNOSTICS_NETWORK_WARN_MBPS", 20.0),
        render_warn_ms: env_f64("WAVIS_DIAGNOSTICS_RENDER_WARN_MS", 25.0),
    }
}

/// Returns an RSS + CPU snapshot for the full Wavis process tree.
///
/// Tauri spawns separate webview processes per window, and on Windows WebView2
/// further spawns renderer/GPU/utility grandchildren. A BFS from own_pid
/// collects the entire tree so the reported RSS reflects actual allocation.
///
/// CPU % requires two samples to compute a delta, so the managed `System` is
/// reused across calls. The very first call returns 0.0 for CPU — this is
/// expected and displayed as such in the UI.
///
/// NOTE: `ProcessesToUpdate::All` does a full /proc scan (or WinAPI equivalent).
/// This is fine at ≥1s polling intervals.
#[tauri::command]
pub fn get_diagnostics_snapshot(
    sys_state: tauri::State<'_, DiagnosticsSystemState>,
) -> DiagnosticsSnapshot {
    let own_pid = Pid::from_u32(std::process::id());
    let mut sys = sys_state.0.lock().unwrap_or_else(|e| e.into_inner());
    // Refresh CPU first so the delta is measured over the longest possible window
    // (before the process refresh, which takes non-trivial time on Windows).
    sys.refresh_cpu_all();
    sys.refresh_processes(ProcessesToUpdate::All, false);

    // Build a parent→children map for O(n) BFS instead of O(depth * n) repeated scans.
    let mut children_map: HashMap<Pid, Vec<Pid>> = HashMap::new();
    for (pid, process) in sys.processes() {
        if let Some(parent) = process.parent() {
            children_map.entry(parent).or_default().push(*pid);
        }
    }

    // BFS from own_pid to collect the full descendant tree.
    // On Windows, WebView2 spawns: wavis.exe → msedgewebview2.exe (browser) →
    //   msedgewebview2.exe (renderer / GPU / utility …). Counting only direct
    //   children misses the renderer/GPU processes which hold the bulk of the RAM.
    let mut tree_pids: HashSet<Pid> = HashSet::new();
    let mut queue: VecDeque<Pid> = VecDeque::new();
    tree_pids.insert(own_pid);
    queue.push_back(own_pid);
    while let Some(pid) = queue.pop_front() {
        if let Some(kids) = children_map.get(&pid) {
            for &child in kids {
                if tree_pids.insert(child) {
                    queue.push_back(child);
                }
            }
        }
    }

    let child_count = tree_pids.len().saturating_sub(1); // exclude own process
    let total_rss: u64 = tree_pids
        .iter()
        .filter_map(|pid| sys.process(*pid))
        .map(|p| p.memory())
        .sum();
    let rss_mb = total_rss as f64 / 1024.0 / 1024.0;
    let cpu_usage_percent = f64::from(sys.global_cpu_usage());

    DiagnosticsSnapshot {
        rss_mb,
        child_count,
        timestamp_ms: unix_now_ms(),
        cpu_usage_percent,
    }
}
