/// Collects RSS and CPU stats for a process.
///
/// - In-process mode (Linux): reads `/proc/self/status` and `/proc/self/stat`
/// - External mode: reads `/proc/<pid>/status` and `/proc/<pid>/stat` (pid from `BACKEND_PID` env var)
/// - Non-Linux or missing procfs: all methods return `None` with a warning logged once
pub struct ProcessStatsCollector {
    /// `None` means "self" (in-process mode).
    #[allow(dead_code)]
    pid: Option<u32>,
    baseline_rss_kb: Option<u64>,
    peak_rss_kb: Option<u64>,
}

impl ProcessStatsCollector {
    /// Create a collector for the current process (in-process mode).
    pub fn for_self() -> Self {
        Self {
            pid: None,
            baseline_rss_kb: None,
            peak_rss_kb: None,
        }
    }

    /// Create a collector for an external process by PID.
    pub fn for_pid(pid: u32) -> Self {
        Self {
            pid: Some(pid),
            baseline_rss_kb: None,
            peak_rss_kb: None,
        }
    }

    /// Try to create from the `BACKEND_PID` environment variable.
    /// Returns `None` if the variable is not set or cannot be parsed.
    pub fn from_env() -> Option<Self> {
        let raw = std::env::var("BACKEND_PID").ok()?;
        let pid: u32 = raw.trim().parse().ok()?;
        Some(Self::for_pid(pid))
    }

    /// Sample current RSS in KB.
    /// Returns `None` on non-Linux platforms or if procfs is unavailable.
    pub fn sample_rss_kb(&self) -> Option<u64> {
        #[cfg(target_os = "linux")]
        {
            let path = match self.pid {
                None => "/proc/self/status".to_string(),
                Some(pid) => format!("/proc/{pid}/status"),
            };
            read_vmrss_kb(&path)
        }
        #[cfg(not(target_os = "linux"))]
        {
            static WARNED: std::sync::OnceLock<()> = std::sync::OnceLock::new();
            WARNED.get_or_init(|| {
                eprintln!("SKIPPED: procfs not available");
            });
            None
        }
    }

    /// Record the baseline RSS (call during the 5-second idle period before a scenario).
    pub fn record_baseline(&mut self) {
        self.baseline_rss_kb = self.sample_rss_kb();
    }

    /// Update the peak RSS if the current sample exceeds the stored peak.
    pub fn update_peak(&mut self) {
        if let Some(current) = self.sample_rss_kb() {
            self.peak_rss_kb = Some(match self.peak_rss_kb {
                None => current,
                Some(prev) => prev.max(current),
            });
        }
    }

    /// Compute RSS growth percentage: `(peak - baseline) / baseline * 100`.
    /// Returns `None` if the baseline has not been recorded or procfs is unavailable.
    pub fn rss_growth_pct(&self) -> Option<f64> {
        let baseline = self.baseline_rss_kb?;
        let peak = self.peak_rss_kb?;
        if baseline == 0 {
            return None;
        }
        Some((peak.saturating_sub(baseline) as f64) / baseline as f64 * 100.0)
    }
}

/// Parse `VmRSS:` from a `/proc/<pid>/status` file and return the value in KB.
#[cfg(target_os = "linux")]
fn read_vmrss_kb(path: &str) -> Option<u64> {
    let content = std::fs::read_to_string(path).ok()?;
    for line in content.lines() {
        if line.starts_with("VmRSS:") {
            // Format: "VmRSS:    12345 kB"
            let kb: u64 = line.split_whitespace().nth(1)?.parse().ok()?;
            return Some(kb);
        }
    }
    None
}
