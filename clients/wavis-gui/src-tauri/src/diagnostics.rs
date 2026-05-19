use std::collections::{HashMap, HashSet, VecDeque};
use std::io::Write;
use std::sync::Mutex;
use sysinfo::{Pid, ProcessesToUpdate, System};

/* ─── Types ─────────────────────────────────────────────────────── */

/// Env-var configuration for the diagnostics window.
/// All fields are read from environment variables at call time.
#[derive(Debug, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DiagnosticsConfig {
    pub enabled: bool,
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

/// A single structured measurement sample emitted during a codec-policy shootout run.
/// Written by `write_shootout_sample` to per-cell JSONL artifact files consumed by
/// the runner at `scripts/codec-shootout/run-matrix.mjs`. See design.md §4.4.
#[derive(Debug, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ShootoutSample {
    /// Identifies the run (matches `runId` in the runner manifest).
    pub run_id: String,
    /// Identifies the matrix cell (e.g. `vp9vp8-p4-detail-detail`).
    pub cell_id: String,
    /// Unix milliseconds at sample time.
    pub timestamp_ms: u64,
    /// Primary codec in use for this cell.
    pub codec_primary: String,
    /// Share profile in use (`detail` or `motion`).
    pub profile: String,
    /// Total participant count including the publisher.
    pub participant_count: u32,
    /// Content source replay type (`detail` or `motion`).
    pub content_source: String,
    /// RSS of the full Wavis process tree in MB at sample time.
    pub rss_mb: f64,
    /// Publisher CPU usage percentage at sample time (0–100).
    pub cpu_usage_percent: f64,
    /// Discriminates publisher-side resource samples from WebRTC event samples.
    /// Values: `resource` (periodic RSS/CPU), `share_started`, `frame_received`,
    /// `downgrade`, `floor_hit`, `cell_start`, `cell_end`.
    pub sample_kind: String,
    /// Optional extra fields as a JSON string (WebRTC stats, event details, etc.).
    /// Parsed by the runner when aggregating the CSV summary.
    pub extra: Option<String>,
}

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

/// BFS traversal of the Wavis process tree rooted at `own_pid`.
/// Returns `(rss_mb, cpu_usage_percent, child_count)`.
/// Caller must call `sys.refresh_cpu_all()` and `sys.refresh_processes()` before invoking.
fn collect_process_tree_stats(sys: &System) -> (f64, f64, usize) {
    let own_pid = Pid::from_u32(std::process::id());

    let mut children_map: HashMap<Pid, Vec<Pid>> = HashMap::new();
    for (pid, process) in sys.processes() {
        if let Some(parent) = process.parent() {
            children_map.entry(parent).or_default().push(*pid);
        }
    }

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

    let child_count = tree_pids.len().saturating_sub(1);
    let total_rss: u64 = tree_pids
        .iter()
        .filter_map(|pid| sys.process(*pid))
        .map(|p| p.memory())
        .sum();
    let rss_mb = total_rss as f64 / 1024.0 / 1024.0;
    let cpu_usage_percent = f64::from(sys.global_cpu_usage());
    (rss_mb, cpu_usage_percent, child_count)
}

/* ─── Commands ──────────────────────────────────────────────────── */

/// Returns the diagnostics configuration derived from environment variables.
/// Called once by the frontend on startup to decide whether to open the window.
#[tauri::command]
pub fn get_diagnostics_config() -> DiagnosticsConfig {
    DiagnosticsConfig {
        enabled: crate::debug_env::diagnostics_window_enabled(),
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
    let mut sys = sys_state.0.lock().unwrap_or_else(|e| e.into_inner());
    // Refresh CPU first so the delta is measured over the longest possible window
    // (before the process refresh, which takes non-trivial time on Windows).
    sys.refresh_cpu_all();
    sys.refresh_processes(ProcessesToUpdate::All, false);

    // BFS from own_pid collects the full descendant tree.
    // On Windows, WebView2 spawns: wavis.exe → msedgewebview2.exe (browser) →
    //   msedgewebview2.exe (renderer / GPU / utility …). Counting only direct
    //   children misses the renderer/GPU processes which hold the bulk of the RAM.
    let (rss_mb, cpu_usage_percent, child_count) = collect_process_tree_stats(&sys);

    DiagnosticsSnapshot {
        rss_mb,
        child_count,
        timestamp_ms: unix_now_ms(),
        cpu_usage_percent,
    }
}

/// Runtime configuration injected by the W4 shootout runner via environment variables.
/// Returned by `get_shootout_env`; `None` when `WAVIS_SHOOTOUT_ENABLED != 1`.
#[derive(Debug, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ShootoutEnvConfig {
    pub run_id: String,
    pub cell_id: String,
    pub codec_primary: String,
    pub codec_backup: String,
    pub simulcast: bool,
    pub profile: String,
    pub content_source: String,
    pub participant_count: u32,
    pub artifact_dir: String,
}

/// Returns the W4 shootout configuration read from `WAVIS_SHOOTOUT_*` environment
/// variables, or `None` if `WAVIS_SHOOTOUT_ENABLED` is not set to `"1"`.
#[tauri::command]
pub fn get_shootout_env() -> Option<ShootoutEnvConfig> {
    if std::env::var("WAVIS_SHOOTOUT_ENABLED").as_deref() != Ok("1") {
        return None;
    }
    Some(ShootoutEnvConfig {
        run_id: std::env::var("WAVIS_SHOOTOUT_RUN_ID").unwrap_or_default(),
        cell_id: std::env::var("WAVIS_SHOOTOUT_CELL_ID").unwrap_or_default(),
        codec_primary: std::env::var("WAVIS_SHOOTOUT_CODEC_PRIMARY").unwrap_or_default(),
        codec_backup: std::env::var("WAVIS_SHOOTOUT_CODEC_BACKUP").unwrap_or_default(),
        simulcast: std::env::var("WAVIS_SHOOTOUT_SIMULCAST").as_deref() == Ok("true"),
        profile: std::env::var("WAVIS_SHOOTOUT_PROFILE").unwrap_or_default(),
        content_source: std::env::var("WAVIS_SHOOTOUT_CONTENT_SOURCE").unwrap_or_default(),
        participant_count: std::env::var("WAVIS_SHOOTOUT_PARTICIPANT_COUNT")
            .ok()
            .and_then(|v| v.parse().ok())
            .unwrap_or(1),
        artifact_dir: std::env::var("WAVIS_SHOOTOUT_ARTIFACT_DIR").unwrap_or_default(),
    })
}

/// Appends a `ShootoutSample` line to the per-cell JSONL artifact file for the
/// W4 codec-policy shootout. Called by the GUI JavaScript during a shootout run
/// (detected via `WAVIS_SHOOTOUT_ENABLED` env var). The runner reads the JSONL
/// file to collect publisher-side RSS/CPU and event samples.
///
/// `artifact_dir` is the run directory (e.g. `artifacts/codec-shootout/runs/{runId}`).
/// The file is written to `{artifact_dir}/{cell_id}/{cell_id}.jsonl`.
#[tauri::command]
#[allow(clippy::too_many_arguments)]
pub fn write_shootout_sample(
    sys_state: tauri::State<'_, DiagnosticsSystemState>,
    run_id: String,
    cell_id: String,
    codec_primary: String,
    profile: String,
    participant_count: u32,
    content_source: String,
    artifact_dir: String,
    sample_kind: String,
    extra: Option<String>,
) -> Result<(), String> {
    let (rss_mb, cpu_usage_percent) = {
        let mut sys = sys_state.0.lock().unwrap_or_else(|e| e.into_inner());
        sys.refresh_cpu_all();
        sys.refresh_processes(ProcessesToUpdate::All, false);
        let (rss, cpu, _) = collect_process_tree_stats(&sys);
        (rss, cpu)
    };

    let sample = ShootoutSample {
        run_id,
        cell_id: cell_id.clone(),
        timestamp_ms: unix_now_ms(),
        codec_primary,
        profile,
        participant_count,
        content_source,
        rss_mb,
        cpu_usage_percent,
        sample_kind,
        extra,
    };

    let line = serde_json::to_string(&sample).map_err(|e| e.to_string())?;
    let dir = std::path::Path::new(&artifact_dir).join(&cell_id);
    std::fs::create_dir_all(&dir).map_err(|e| e.to_string())?;
    let path = dir.join(format!("{cell_id}.jsonl"));
    let mut file = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&path)
        .map_err(|e| e.to_string())?;
    writeln!(file, "{line}").map_err(|e| e.to_string())
}
