//! Windows screen capture backend using the Windows Graphics Capture API.
//!
//! Parallel to `pipewire_capture.rs` and `x11_capture.rs` (Linux backends).
//! Creates a `GraphicsCaptureItem` for a specified monitor (HMONITOR) or
//! window (HWND) handle, captures frames via `Direct3D11CaptureFramePool`,
//! and delivers them as `CapturedFrame` structs through the `ScreenCapture` trait.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Condvar, Mutex};

use serde::Serialize;
use tauri::{AppHandle, Emitter};

use super::{CaptureError, CapturedFrame, ScreenCapture};

const LOG: &str = "[wavis:win-capture]";

/// Alias for the frame callback type to satisfy clippy::type_complexity.
type FrameCallback = Arc<Mutex<Option<Box<dyn Fn(CapturedFrame) + Send + 'static>>>>;
pub type SharedWindowsCaptureDiagnostics = Arc<Mutex<WindowsNativeCaptureDiagnostics>>;

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct WindowsNativeCaptureDiagnostics {
    pub backend: String,
    pub source_kind: String,
    pub startup_stage: String,
    pub current_operation: String,
    pub startup_elapsed_ms: Option<u64>,
    pub startup_step_timings: Vec<WindowsNativeStartupStepTiming>,
    pub capture_thread_started_at_ms: Option<u64>,
    pub last_stage_update_at_ms: Option<u64>,
    pub capture_thread_alive: bool,
    pub item_width: u32,
    pub item_height: u32,
    pub adapter_description: Option<String>,
    pub adapter_vendor_id: Option<u32>,
    pub adapter_device_id: Option<u32>,
    pub adapter_dedicated_video_memory_mb: Option<u64>,
    pub adapter_dedicated_system_memory_mb: Option<u64>,
    pub adapter_shared_system_memory_mb: Option<u64>,
    pub border_required_disabled: Option<bool>,
    pub border_required_error: Option<String>,
    pub capture_indicator_note: Option<String>,
    pub environment: WindowsNativeCaptureEnvironment,
    pub timing: WindowsNativeCaptureTiming,
    pub frame_arrived_callbacks: u64,
    pub raw_backend_callback_fps: f64,
    pub usable_frames: u64,
    pub emitted_pollable_frames: u64,
    pub emitted_pollable_frame_fps: f64,
    pub throttle_drop_count: u64,
    pub throttle_drop_fps: f64,
    pub cap_downscale_avg_ms: f64,
    pub i420_convert_avg_ms: f64,
    pub rgba_to_rgb_avg_ms: f64,
    pub jpeg_encode_avg_ms: f64,
    pub base64_encode_avg_ms: f64,
    pub latest_frame_write_avg_ms: f64,
    pub raw_to_pollable_frame_avg_ms: f64,
    pub active_backend: String,
    pub previous_backend: Option<String>,
    pub retry_reason: Option<String>,
    pub retry_at_ms: Option<u64>,
    pub try_get_next_frame_failures: u64,
    pub surface_failures: u64,
    pub surface_cast_failures: u64,
    pub texture_interface_failures: u64,
    pub zero_dimension_frames: u64,
    pub staging_texture_failures: u64,
    pub map_failures: u64,
    pub first_error: Option<String>,
    pub first_raw_frame_latency_ms: Option<u64>,
    pub first_buffered_jpeg_latency_ms: Option<u64>,
    pub poll_calls: u64,
    pub poll_hits: u64,
    pub poll_misses: u64,
    pub first_poll_hit_latency_ms: Option<u64>,
    pub latest_polled_seq: Option<u64>,
    #[serde(skip)]
    interval_started_at_ms: u64,
    #[serde(skip)]
    interval_frame_arrived_callbacks: u64,
    #[serde(skip)]
    interval_emitted_pollable_frames: u64,
    #[serde(skip)]
    interval_throttle_drop_count: u64,
}

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct WindowsNativeStartupStepTiming {
    pub stage: String,
    pub elapsed_ms: u64,
}

#[derive(Clone, Debug, Default, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct WindowsNativeCaptureEnvironment {
    pub process_id: u32,
    pub global_cpu_percent: Option<f64>,
    pub process_tree_rss_mb: Option<f64>,
    pub process_tree_cpu_percent: Option<f64>,
    pub child_process_count: Option<usize>,
    pub webview_process_count: Option<usize>,
    pub monitor_count: Option<usize>,
    pub selected_source_width: u32,
    pub selected_source_height: u32,
    pub target_fps: u32,
    pub jpeg_quality: u8,
    pub video_driver_provider: Option<String>,
    pub video_driver_version: Option<String>,
    pub video_driver_date: Option<String>,
    pub video_driver_query_error: Option<String>,
    pub overlay_processes: Vec<WindowsOverlayProcess>,
}

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct WindowsOverlayProcess {
    pub name: String,
    pub pid: u32,
}

#[derive(Clone, Debug, Default, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct WindowsNativeCaptureTiming {
    pub first_raw_frame_latency_ms: Option<u64>,
    pub first_cap_downscale_ms: Option<u64>,
    pub first_i420_convert_ms: Option<u64>,
    pub first_rgba_to_rgb_ms: Option<u64>,
    pub first_jpeg_encode_ms: Option<u64>,
    pub first_base64_encode_ms: Option<u64>,
    pub first_latest_frame_write_ms: Option<u64>,
    pub first_raw_to_pollable_frame_ms: Option<u64>,
}

impl WindowsNativeCaptureDiagnostics {
    pub fn new(backend: &str, source_kind: &str, item_width: u32, item_height: u32) -> Self {
        Self {
            backend: backend.to_string(),
            source_kind: source_kind.to_string(),
            startup_stage: "backend_start".to_string(),
            current_operation: "backend_start".to_string(),
            startup_elapsed_ms: None,
            startup_step_timings: Vec::new(),
            capture_thread_started_at_ms: None,
            last_stage_update_at_ms: None,
            capture_thread_alive: false,
            item_width,
            item_height,
            adapter_description: None,
            adapter_vendor_id: None,
            adapter_device_id: None,
            adapter_dedicated_video_memory_mb: None,
            adapter_dedicated_system_memory_mb: None,
            adapter_shared_system_memory_mb: None,
            border_required_disabled: None,
            border_required_error: None,
            capture_indicator_note: None,
            environment: WindowsNativeCaptureEnvironment {
                process_id: std::process::id(),
                selected_source_width: item_width,
                selected_source_height: item_height,
                ..Default::default()
            },
            timing: WindowsNativeCaptureTiming::default(),
            frame_arrived_callbacks: 0,
            raw_backend_callback_fps: 0.0,
            usable_frames: 0,
            emitted_pollable_frames: 0,
            emitted_pollable_frame_fps: 0.0,
            throttle_drop_count: 0,
            throttle_drop_fps: 0.0,
            cap_downscale_avg_ms: 0.0,
            i420_convert_avg_ms: 0.0,
            rgba_to_rgb_avg_ms: 0.0,
            jpeg_encode_avg_ms: 0.0,
            base64_encode_avg_ms: 0.0,
            latest_frame_write_avg_ms: 0.0,
            raw_to_pollable_frame_avg_ms: 0.0,
            active_backend: backend.to_string(),
            previous_backend: None,
            retry_reason: None,
            retry_at_ms: None,
            try_get_next_frame_failures: 0,
            surface_failures: 0,
            surface_cast_failures: 0,
            texture_interface_failures: 0,
            zero_dimension_frames: 0,
            staging_texture_failures: 0,
            map_failures: 0,
            first_error: None,
            first_raw_frame_latency_ms: None,
            first_buffered_jpeg_latency_ms: None,
            poll_calls: 0,
            poll_hits: 0,
            poll_misses: 0,
            first_poll_hit_latency_ms: None,
            latest_polled_seq: None,
            interval_started_at_ms: unix_now_ms(),
            interval_frame_arrived_callbacks: 0,
            interval_emitted_pollable_frames: 0,
            interval_throttle_drop_count: 0,
        }
    }

    pub fn refresh_interval_rates(&mut self) {
        let now = unix_now_ms();
        let elapsed_ms = now.saturating_sub(self.interval_started_at_ms);
        if elapsed_ms < 1000 {
            return;
        }
        let elapsed_s = elapsed_ms as f64 / 1000.0;
        self.raw_backend_callback_fps = self
            .frame_arrived_callbacks
            .saturating_sub(self.interval_frame_arrived_callbacks) as f64
            / elapsed_s;
        self.emitted_pollable_frame_fps = self
            .emitted_pollable_frames
            .saturating_sub(self.interval_emitted_pollable_frames) as f64
            / elapsed_s;
        self.throttle_drop_fps = self
            .throttle_drop_count
            .saturating_sub(self.interval_throttle_drop_count) as f64
            / elapsed_s;
        self.interval_started_at_ms = now;
        self.interval_frame_arrived_callbacks = self.frame_arrived_callbacks;
        self.interval_emitted_pollable_frames = self.emitted_pollable_frames;
        self.interval_throttle_drop_count = self.throttle_drop_count;
    }

    pub fn record_backend_callback(&mut self) {
        self.frame_arrived_callbacks = self.frame_arrived_callbacks.saturating_add(1);
        self.refresh_interval_rates();
    }

    pub fn record_throttle_drop(&mut self) {
        self.throttle_drop_count = self.throttle_drop_count.saturating_add(1);
        self.refresh_interval_rates();
    }

    pub fn record_pollable_frame_timing(
        &mut self,
        cap_downscale_ms: u64,
        i420_convert_ms: u64,
        rgba_to_rgb_ms: u64,
        jpeg_encode_ms: u64,
        base64_encode_ms: u64,
        latest_frame_write_ms: u64,
        raw_to_pollable_frame_ms: u64,
    ) {
        self.emitted_pollable_frames = self.emitted_pollable_frames.saturating_add(1);
        let n = self.emitted_pollable_frames as f64;
        let update = |avg: &mut f64, value: u64| {
            *avg = ((*avg * (n - 1.0)) + value as f64) / n;
        };
        update(&mut self.cap_downscale_avg_ms, cap_downscale_ms);
        update(&mut self.i420_convert_avg_ms, i420_convert_ms);
        update(&mut self.rgba_to_rgb_avg_ms, rgba_to_rgb_ms);
        update(&mut self.jpeg_encode_avg_ms, jpeg_encode_ms);
        update(&mut self.base64_encode_avg_ms, base64_encode_ms);
        update(&mut self.latest_frame_write_avg_ms, latest_frame_write_ms);
        update(&mut self.raw_to_pollable_frame_avg_ms, raw_to_pollable_frame_ms);
        self.refresh_interval_rates();
    }

    pub fn record_border_required_disabled(&mut self) {
        self.border_required_disabled = Some(true);
        self.border_required_error = None;
        self.capture_indicator_note = Some(
            "SetIsBorderRequired(false) succeeded; if the yellow outline remains visible, Windows is enforcing the capture indicator, not rendering an app border"
                .to_string(),
        );
    }

    pub fn record_border_required_disable_error(&mut self, error: impl std::fmt::Display) {
        self.border_required_disabled = Some(false);
        self.border_required_error = Some(error.to_string());
        self.capture_indicator_note = Some(
            "SetIsBorderRequired(false) failed; Windows may keep showing the capture indicator"
                .to_string(),
        );
    }

    pub fn record_startup_stage(
        &mut self,
        stage: &str,
        current_operation: &str,
        elapsed_ms: u64,
    ) {
        self.startup_stage = stage.to_string();
        self.current_operation = current_operation.to_string();
        self.startup_elapsed_ms = Some(elapsed_ms);
        self.last_stage_update_at_ms = Some(unix_now_ms());
        self.startup_step_timings
            .push(WindowsNativeStartupStepTiming {
                stage: stage.to_string(),
                elapsed_ms,
            });
    }

    pub fn record_startup_error(
        &mut self,
        stage: &str,
        current_operation: &str,
        elapsed_ms: u64,
        error: impl std::fmt::Display,
    ) {
        let error = error.to_string();
        self.record_startup_stage(stage, current_operation, elapsed_ms);
        if self.first_error.is_none() {
            self.first_error = Some(format!("{stage}: {error}"));
        }
    }

    pub fn record_nonfatal_startup_error(
        &mut self,
        stage: &str,
        current_operation: &str,
        elapsed_ms: u64,
        _error: impl std::fmt::Display,
    ) {
        self.record_startup_stage(stage, current_operation, elapsed_ms);
    }

    pub fn startup_step_timing_summary(&self) -> String {
        if self.startup_step_timings.is_empty() {
            return "none".to_string();
        }
        self.startup_step_timings
            .iter()
            .map(|timing| format!("{}:{}ms", timing.stage, timing.elapsed_ms))
            .collect::<Vec<_>>()
            .join(",")
    }

    pub fn total_readback_failures(&self) -> u64 {
        self.try_get_next_frame_failures
            + self.surface_failures
            + self.surface_cast_failures
            + self.texture_interface_failures
            + self.zero_dimension_frames
            + self.staging_texture_failures
            + self.map_failures
    }

    pub fn compact_summary(&self) -> String {
        format!(
            "backend={} source_kind={} stalled_in={} current_operation={} startup_elapsed_ms={} last_stage_update_at_ms={} capture_thread_alive={} border_required_disabled={} border_required_error={} callbacks={} raw_callback_fps={:.1} usable_frames={} emitted_pollable_frames={} emitted_pollable_fps={:.1} throttle_drops={} throttle_drop_fps={:.1} readback_failures={} poll_calls={} poll_hits={} poll_misses={} latest_polled_seq={} startup_step_timings={} first_error={}",
            self.backend,
            self.source_kind,
            self.startup_stage,
            self.current_operation,
            self.startup_elapsed_ms
                .map(|elapsed| elapsed.to_string())
                .unwrap_or_else(|| "none".to_string()),
            self.last_stage_update_at_ms
                .map(|ts| ts.to_string())
                .unwrap_or_else(|| "none".to_string()),
            self.capture_thread_alive,
            self.border_required_disabled
                .map(|disabled| disabled.to_string())
                .unwrap_or_else(|| "unknown".to_string()),
            self.border_required_error.as_deref().unwrap_or("none"),
            self.frame_arrived_callbacks,
            self.raw_backend_callback_fps,
            self.usable_frames,
            self.emitted_pollable_frames,
            self.emitted_pollable_frame_fps,
            self.throttle_drop_count,
            self.throttle_drop_fps,
            self.total_readback_failures(),
            self.poll_calls,
            self.poll_hits,
            self.poll_misses,
            self.latest_polled_seq
                .map(|seq| seq.to_string())
                .unwrap_or_else(|| "none".to_string()),
            self.startup_step_timing_summary(),
            self.first_error.as_deref().unwrap_or("none"),
        )
    }
}

#[derive(Clone, Serialize)]
#[serde(rename_all = "camelCase")]
struct WindowsNativeCaptureFailureEvent {
    reason: String,
}

fn unix_now_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

fn startup_elapsed_ms(started: std::time::Instant) -> u64 {
    started.elapsed().as_millis() as u64
}

fn record_wgc_startup_stage(
    diagnostics: &SharedWindowsCaptureDiagnostics,
    startup_stage: &Arc<Mutex<&'static str>>,
    started: std::time::Instant,
    stage: &'static str,
    current_operation: &'static str,
) {
    if let Ok(mut stage_guard) = startup_stage.lock() {
        *stage_guard = stage;
    }
    if let Ok(mut diag) = diagnostics.lock() {
        diag.record_startup_stage(stage, current_operation, startup_elapsed_ms(started));
    }
}

fn record_wgc_startup_error(
    diagnostics: &SharedWindowsCaptureDiagnostics,
    startup_stage: &Arc<Mutex<&'static str>>,
    started: std::time::Instant,
    stage: &'static str,
    current_operation: &'static str,
    error: impl std::fmt::Display,
) {
    if let Ok(mut stage_guard) = startup_stage.lock() {
        *stage_guard = stage;
    }
    if let Ok(mut diag) = diagnostics.lock() {
        diag.record_startup_error(stage, current_operation, startup_elapsed_ms(started), error);
    }
}

fn record_wgc_startup_nonfatal_error(
    diagnostics: &SharedWindowsCaptureDiagnostics,
    startup_stage: &Arc<Mutex<&'static str>>,
    started: std::time::Instant,
    stage: &'static str,
    current_operation: &'static str,
    error: impl std::fmt::Display,
) {
    if let Ok(mut stage_guard) = startup_stage.lock() {
        *stage_guard = stage;
    }
    if let Ok(mut diag) = diagnostics.lock() {
        diag.record_nonfatal_startup_error(
            stage,
            current_operation,
            startup_elapsed_ms(started),
            error,
        );
    }
}

fn record_readback_failure(
    diagnostics: &SharedWindowsCaptureDiagnostics,
    app_handle: &AppHandle,
    field: fn(&mut WindowsNativeCaptureDiagnostics) -> &mut u64,
    stage: &str,
    error: impl std::fmt::Display,
) {
    let error = error.to_string();
    let emit_reason = diagnostics.lock().ok().and_then(|mut diag| {
        *field(&mut diag) += 1;
        if diag.first_error.is_none() {
            diag.first_error = Some(format!("{stage}: {error}"));
        }
        if diag.usable_frames == 0 && diag.total_readback_failures() == 3 {
            Some(format!(
                "WGC readback failed repeatedly before first frame: {}",
                diag.compact_summary()
            ))
        } else {
            None
        }
    });
    if let Some(reason) = emit_reason {
        log::warn!("{LOG} {reason}");
        let _ = app_handle.emit(
            "windows-native-capture-failed",
            WindowsNativeCaptureFailureEvent { reason },
        );
    }
}

fn staging_texture_matches(
    current: Option<(u32, u32, i32)>,
    width: u32,
    height: u32,
    format: i32,
) -> bool {
    current.is_some_and(|value| value == (width, height, format))
}

struct WgcStartupSummary {
    stage: Arc<Mutex<&'static str>>,
    first_frame_seen: Arc<AtomicBool>,
    diagnostics: SharedWindowsCaptureDiagnostics,
}

impl Drop for WgcStartupSummary {
    fn drop(&mut self) {
        if let Ok(mut diag) = self.diagnostics.lock() {
            diag.capture_thread_alive = false;
            diag.last_stage_update_at_ms = Some(unix_now_ms());
        }
        if !self.first_frame_seen.load(Ordering::SeqCst) {
            let stage = self.stage.lock().map(|stage| *stage).unwrap_or("unknown");
            log::warn!("{LOG} WGC exited before first frame; last_startup_stage={stage}");
        }
    }
}

/// Windows Graphics Capture backend for a specific source (monitor or window).
pub struct WinCapture {
    active: Arc<AtomicBool>,
    frame_callback: FrameCallback,
    /// Condvar signalled when `on_frame()` sets the callback, so the capture
    /// thread can defer `StartCapture()` until the consumer is ready.
    callback_ready: Arc<(Mutex<bool>, Condvar)>,
    capture_thread: Mutex<Option<std::thread::JoinHandle<()>>>,
    stop_flag: Arc<AtomicBool>,
}

/// Configuration for creating a Windows capture session.
pub struct WinCaptureConfig {
    /// Source ID — an HMONITOR or HWND handle value as a string.
    pub source_id: String,
    pub source_kind: WinCaptureSourceKind,
    /// Tauri app handle for emitting `share_error` events on source loss.
    pub app_handle: AppHandle,
    pub diagnostics: SharedWindowsCaptureDiagnostics,
}

struct CaptureLoopParams {
    handle_val: isize,
    source_kind: WinCaptureSourceKind,
    active: Arc<AtomicBool>,
    stop_flag: Arc<AtomicBool>,
    frame_callback: FrameCallback,
    callback_ready: Arc<(Mutex<bool>, Condvar)>,
    app_handle: AppHandle,
    diagnostics: SharedWindowsCaptureDiagnostics,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum WinCaptureSourceKind {
    Screen,
    Window,
}

/// Create a `GraphicsCaptureItem` from a handle value string.
///
/// Uses the picker-provided source kind to call `CreateForMonitor` or
/// `CreateForWindow` directly. Returns a descriptive error if the source is
/// no longer available.
fn create_capture_item(
    handle_val: isize,
    source_kind: WinCaptureSourceKind,
) -> Result<windows::Graphics::Capture::GraphicsCaptureItem, CaptureError> {
    use windows::Graphics::Capture::GraphicsCaptureItem;
    use windows::Win32::Foundation::HWND;
    use windows::Win32::Graphics::Gdi::HMONITOR;
    use windows::Win32::System::WinRT::Graphics::Capture::IGraphicsCaptureItemInterop;

    // Get the interop factory for creating capture items from raw handles.
    let interop: IGraphicsCaptureItemInterop = windows::core::factory::<
        GraphicsCaptureItem,
        IGraphicsCaptureItemInterop,
    >()
    .map_err(|e| {
        CaptureError::CaptureStartFailed(format!("Graphics Capture interop unavailable: {e}"))
    })?;

    let result: Result<GraphicsCaptureItem, _> = match source_kind {
        WinCaptureSourceKind::Screen => unsafe {
            interop.CreateForMonitor(HMONITOR(handle_val as *mut _))
        },
        WinCaptureSourceKind::Window => unsafe {
            interop.CreateForWindow(HWND(handle_val as *mut _))
        },
    };

    match result {
        Ok(item) => Ok(item),
        Err(e) => Err(CaptureError::CaptureStartFailed(format!(
            "The selected source is no longer available: {e}"
        ))),
    }
}

impl WinCapture {
    /// Create and start a capture session for the given source.
    ///
    /// Parses `source_id` as an isize handle, creates a `GraphicsCaptureItem`
    /// for the declared source kind, and starts the capture loop on a dedicated
    /// thread.
    ///
    /// Returns `CaptureError::CaptureStartFailed` with a descriptive message
    /// if the source is no longer available.
    pub fn start(config: WinCaptureConfig) -> Result<Self, CaptureError> {
        log::info!(
            "{LOG} [diag] start() ENTERED, source_id={}, source_kind={:?}, backend=wgc",
            config.source_id,
            config.source_kind,
        );

        let handle_val: isize = config.source_id.parse().map_err(|_| {
            CaptureError::CaptureStartFailed(format!("invalid source id: {}", config.source_id))
        })?;

        let active = Arc::new(AtomicBool::new(true));
        let stop_flag = Arc::new(AtomicBool::new(false));
        let frame_callback: FrameCallback = Arc::new(Mutex::new(None));
        let callback_ready = Arc::new((Mutex::new(false), Condvar::new()));

        let capture = Self {
            active: active.clone(),
            frame_callback: frame_callback.clone(),
            callback_ready: callback_ready.clone(),
            capture_thread: Mutex::new(None),
            stop_flag: stop_flag.clone(),
        };

        log::info!("{LOG} [diag] about to spawn capture thread");

        let handle = std::thread::Builder::new()
            .name("win-capture".into())
            .spawn(move || {
                Self::capture_loop(CaptureLoopParams {
                    handle_val,
                    source_kind: config.source_kind,
                    active,
                    stop_flag,
                    frame_callback,
                    callback_ready,
                    app_handle: config.app_handle,
                    diagnostics: config.diagnostics,
                });
            })
            .map_err(|e| {
                CaptureError::CaptureStartFailed(format!(
                    "failed to spawn Windows capture thread: {e}"
                ))
            })?;

        *capture.capture_thread.lock().unwrap() = Some(handle);
        log::info!("{LOG} [diag] capture thread spawned, returning WinCapture");
        Ok(capture)
    }

    /// Main capture loop — runs on a dedicated thread.
    fn capture_loop(params: CaptureLoopParams) {
        let CaptureLoopParams {
            handle_val,
            source_kind,
            active,
            stop_flag,
            frame_callback,
            callback_ready,
            app_handle,
            diagnostics,
        } = params;

        use windows::core::Interface;
        use windows::Graphics::Capture::Direct3D11CaptureFramePool;
        use windows::Graphics::DirectX::Direct3D11::IDirect3DDevice;
        use windows::Graphics::DirectX::DirectXPixelFormat;
        use windows::Win32::Graphics::Direct3D::{
            D3D_DRIVER_TYPE_HARDWARE, D3D_FEATURE_LEVEL_11_0,
        };
        use windows::Win32::Graphics::Direct3D11::{
            D3D11CreateDevice, ID3D11Texture2D, D3D11_CPU_ACCESS_READ,
            D3D11_CREATE_DEVICE_BGRA_SUPPORT, D3D11_MAPPED_SUBRESOURCE, D3D11_MAP_READ,
            D3D11_SDK_VERSION, D3D11_TEXTURE2D_DESC, D3D11_USAGE_STAGING,
        };
        use windows::Win32::Graphics::Dxgi::Common::DXGI_SAMPLE_DESC;
        use windows::Win32::Graphics::Dxgi::IDXGIDevice;
        use windows::Win32::System::WinRT::Direct3D11::{
            CreateDirect3D11DeviceFromDXGIDevice, IDirect3DDxgiInterfaceAccess,
        };

        // ── 0. Initialize WinRT on this thread ─────────────────────────
        let startup_started = std::time::Instant::now();
        let startup_stage = Arc::new(Mutex::new("capture_thread_started"));
        let first_frame_seen = Arc::new(AtomicBool::new(false));
        let _startup_summary = WgcStartupSummary {
            stage: startup_stage.clone(),
            first_frame_seen: first_frame_seen.clone(),
            diagnostics: diagnostics.clone(),
        };
        if let Ok(mut diag) = diagnostics.lock() {
            diag.capture_thread_started_at_ms = Some(unix_now_ms());
            diag.capture_thread_alive = true;
            diag.record_startup_stage(
                "capture_thread_started",
                "capture_thread_started",
                startup_elapsed_ms(startup_started),
            );
        }

        log::info!("{LOG} [diag] thread: step 0 — WinRT init");
        // The capture thread creates WinRT objects (frame pool, session).
        // Without RoInitialize the WinRT runtime may silently fail to
        // deliver FrameArrived callbacks.
        let did_ro_init;
        {
            use windows::Win32::System::WinRT::{RoInitialize, RO_INIT_TYPE};
            // RO_INIT_MULTITHREADED = 1
            match unsafe { RoInitialize(RO_INIT_TYPE(1)) } {
                Ok(()) => {
                    did_ro_init = true;
                    log::info!("{LOG} WinRT initialized on capture thread");
                }
                Err(e) if e.code().0 == 1 => {
                    // S_FALSE — already initialized, don't uninitialize later.
                    did_ro_init = false;
                    log::info!("{LOG} WinRT already initialized on capture thread");
                }
                Err(e) => {
                    log::error!("{LOG} RoInitialize failed: {e}");
                    record_wgc_startup_error(
                        &diagnostics,
                        &startup_stage,
                        startup_started,
                        "winrt_init_error",
                        "RoInitialize",
                        e,
                    );
                    active.store(false, Ordering::SeqCst);
                    return;
                }
            }
        }
        record_wgc_startup_stage(
            &diagnostics,
            &startup_stage,
            startup_started,
            "winrt_initialized",
            "RoInitialize",
        );

        // ── 1. Create D3D11 device ──────────────────────────────────────
        log::info!("{LOG} [diag] thread: step 1 — creating GraphicsCaptureItem");
        record_wgc_startup_stage(
            &diagnostics,
            &startup_stage,
            startup_started,
            "capture_item_create_start",
            "create_capture_item",
        );
        let capture_item = match create_capture_item(handle_val, source_kind) {
            Ok(item) => item,
            Err(e) => {
                log::error!("{LOG} failed to create capture item: {e}");
                record_wgc_startup_error(
                    &diagnostics,
                    &startup_stage,
                    startup_started,
                    "capture_item_create_error",
                    "create_capture_item",
                    e,
                );
                active.store(false, Ordering::SeqCst);
                return;
            }
        };
        record_wgc_startup_stage(
            &diagnostics,
            &startup_stage,
            startup_started,
            "capture_item_create_done",
            "create_capture_item",
        );

        record_wgc_startup_stage(
            &diagnostics,
            &startup_stage,
            startup_started,
            "capture_item_size_start",
            "GraphicsCaptureItem.Size",
        );
        let size = match capture_item.Size() {
            Ok(size) => size,
            Err(e) => {
                log::error!("{LOG} failed to get capture item size: {e}");
                record_wgc_startup_error(
                    &diagnostics,
                    &startup_stage,
                    startup_started,
                    "capture_item_size_error",
                    "GraphicsCaptureItem.Size",
                    e,
                );
                active.store(false, Ordering::SeqCst);
                return;
            }
        };
        if size.Width <= 0 || size.Height <= 0 {
            let error = "The selected source has zero dimensions";
            log::error!("{LOG} {error}");
            record_wgc_startup_error(
                &diagnostics,
                &startup_stage,
                startup_started,
                "capture_item_size_error",
                "GraphicsCaptureItem.Size",
                error,
            );
            active.store(false, Ordering::SeqCst);
            return;
        }
        if let Ok(mut diag) = diagnostics.lock() {
            diag.item_width = size.Width as u32;
            diag.item_height = size.Height as u32;
            diag.environment.selected_source_width = size.Width as u32;
            diag.environment.selected_source_height = size.Height as u32;
            diag.record_startup_stage(
                "capture_item_size_done",
                "GraphicsCaptureItem.Size",
                startup_elapsed_ms(startup_started),
            );
        }

        log::info!("{LOG} [diag] thread: step 2 — creating D3D11 device");
        let (d3d_device, d3d_context) = unsafe {
            let mut device = None;
            let mut context = None;
            let feature_levels = [D3D_FEATURE_LEVEL_11_0];

            let hr = D3D11CreateDevice(
                None,
                D3D_DRIVER_TYPE_HARDWARE,
                None,
                D3D11_CREATE_DEVICE_BGRA_SUPPORT,
                Some(&feature_levels),
                D3D11_SDK_VERSION,
                Some(&mut device),
                None,
                Some(&mut context),
            );

            if hr.is_err() {
                log::error!("{LOG} D3D11CreateDevice failed: {hr:?}");
                record_wgc_startup_error(
                    &diagnostics,
                    &startup_stage,
                    startup_started,
                    "d3d_device_create_error",
                    "D3D11CreateDevice",
                    format!("{hr:?}"),
                );
                active.store(false, Ordering::SeqCst);
                return;
            }

            match (device, context) {
                (Some(d), Some(c)) => (d, c),
                _ => {
                    log::error!("{LOG} D3D11CreateDevice returned None");
                    record_wgc_startup_error(
                        &diagnostics,
                        &startup_stage,
                        startup_started,
                        "d3d_device_create_error",
                        "D3D11CreateDevice",
                        "returned no device/context",
                    );
                    active.store(false, Ordering::SeqCst);
                    return;
                }
            }
        };

        // Enable multithread protection — FrameArrived fires on a thread-pool
        // thread but the immediate context is single-threaded by default.
        // Without this, CopyResource + Map from the callback races with the
        // device context and produces black (zeroed) frames.
        {
            use windows::Win32::Graphics::Direct3D11::ID3D11Multithread;
            if let Ok(mt) = d3d_device.cast::<ID3D11Multithread>() {
                let _ = unsafe { mt.SetMultithreadProtected(true) };
                log::info!("{LOG} D3D11 multithread protection enabled");
            } else {
                log::warn!("{LOG} failed to enable D3D11 multithread protection");
            }
        }

        log::info!("{LOG} [diag] thread: step 1 complete — D3D11 device ready");
        record_wgc_startup_stage(
            &diagnostics,
            &startup_stage,
            startup_started,
            "d3d_device_ready",
            "D3D11CreateDevice",
        );

        // ── 2. Create WinRT IDirect3DDevice ─────────────────────────────
        log::info!("{LOG} [diag] thread: step 2 — creating WinRT IDirect3DDevice");
        let dxgi_device: IDXGIDevice = match d3d_device.cast() {
            Ok(d) => d,
            Err(e) => {
                log::error!("{LOG} failed to get IDXGIDevice: {e}");
                record_wgc_startup_error(
                    &diagnostics,
                    &startup_stage,
                    startup_started,
                    "dxgi_device_cast_error",
                    "cast IDXGIDevice",
                    e,
                );
                active.store(false, Ordering::SeqCst);
                return;
            }
        };
        if let Ok(adapter) = unsafe { dxgi_device.GetAdapter() } {
            if let Ok(desc) = unsafe { adapter.GetDesc() } {
                let end = desc
                    .Description
                    .iter()
                    .position(|ch| *ch == 0)
                    .unwrap_or(desc.Description.len());
                if let Ok(mut diag) = diagnostics.lock() {
                    diag.adapter_description =
                        Some(String::from_utf16_lossy(&desc.Description[..end]));
                    diag.adapter_vendor_id = Some(desc.VendorId);
                    diag.adapter_device_id = Some(desc.DeviceId);
                    diag.adapter_dedicated_video_memory_mb =
                        Some((desc.DedicatedVideoMemory / 1024 / 1024) as u64);
                    diag.adapter_dedicated_system_memory_mb =
                        Some((desc.DedicatedSystemMemory / 1024 / 1024) as u64);
                    diag.adapter_shared_system_memory_mb =
                        Some((desc.SharedSystemMemory / 1024 / 1024) as u64);
                }
            }
        }

        let winrt_device = unsafe {
            match CreateDirect3D11DeviceFromDXGIDevice(&dxgi_device) {
                Ok(inspectable) => match inspectable.cast::<IDirect3DDevice>() {
                    Ok(device) => device,
                    Err(e) => {
                        log::error!("{LOG} failed to cast to IDirect3DDevice: {e}");
                        record_wgc_startup_error(
                            &diagnostics,
                            &startup_stage,
                            startup_started,
                            "winrt_d3d_device_error",
                            "cast IDirect3DDevice",
                            e,
                        );
                        active.store(false, Ordering::SeqCst);
                        return;
                    }
                },
                Err(e) => {
                    log::error!("{LOG} CreateDirect3D11DeviceFromDXGIDevice failed: {e}");
                    record_wgc_startup_error(
                        &diagnostics,
                        &startup_stage,
                        startup_started,
                        "winrt_d3d_device_error",
                        "CreateDirect3D11DeviceFromDXGIDevice",
                        e,
                    );
                    active.store(false, Ordering::SeqCst);
                    return;
                }
            }
        };
        record_wgc_startup_stage(
            &diagnostics,
            &startup_stage,
            startup_started,
            "winrt_d3d_device_ready",
            "CreateDirect3D11DeviceFromDXGIDevice",
        );

        // ── 3. Create frame pool + capture session ──────────────────────
        log::info!("{LOG} [diag] thread: step 3 — creating frame pool + capture session");
        record_wgc_startup_stage(
            &diagnostics,
            &startup_stage,
            startup_started,
            "frame_pool_create_start",
            "Direct3D11CaptureFramePool::CreateFreeThreaded",
        );
        let frame_pool = match Direct3D11CaptureFramePool::CreateFreeThreaded(
            &winrt_device,
            DirectXPixelFormat::B8G8R8A8UIntNormalized,
            2, // double-buffer for continuous capture
            size,
        ) {
            Ok(pool) => pool,
            Err(e) => {
                log::error!("{LOG} failed to create frame pool: {e}");
                record_wgc_startup_error(
                    &diagnostics,
                    &startup_stage,
                    startup_started,
                    "frame_pool_create_error",
                    "Direct3D11CaptureFramePool::CreateFreeThreaded",
                    e,
                );
                active.store(false, Ordering::SeqCst);
                return;
            }
        };
        record_wgc_startup_stage(
            &diagnostics,
            &startup_stage,
            startup_started,
            "frame_pool_create_done",
            "Direct3D11CaptureFramePool::CreateFreeThreaded",
        );

        record_wgc_startup_stage(
            &diagnostics,
            &startup_stage,
            startup_started,
            "capture_session_create_start",
            "CreateCaptureSession",
        );
        let session = match frame_pool.CreateCaptureSession(&capture_item) {
            Ok(s) => s,
            Err(e) => {
                log::error!("{LOG} failed to create capture session: {e}");
                record_wgc_startup_error(
                    &diagnostics,
                    &startup_stage,
                    startup_started,
                    "capture_session_create_error",
                    "CreateCaptureSession",
                    e,
                );
                active.store(false, Ordering::SeqCst);
                return;
            }
        };
        record_wgc_startup_stage(
            &diagnostics,
            &startup_stage,
            startup_started,
            "capture_session_create_done",
            "CreateCaptureSession",
        );

        // Disable the yellow capture border if the API supports it.
        // Cursor capture is enabled so viewers see the cursor in the stream.
        // Cursor visibility for the streamer is preserved by using monitor
        // capture (not window capture) in create_capture_item above.
        record_wgc_startup_stage(
            &diagnostics,
            &startup_stage,
            startup_started,
            "cursor_border_config_start",
            "SetIsCursorCaptureEnabled/SetIsBorderRequired",
        );
        if let Err(e) = session.SetIsCursorCaptureEnabled(true) {
            log::warn!("{LOG} SetIsCursorCaptureEnabled failed: {e}");
            record_wgc_startup_nonfatal_error(
                &diagnostics,
                &startup_stage,
                startup_started,
                "cursor_border_config_error",
                "SetIsCursorCaptureEnabled",
                e,
            );
        }
        match session.SetIsBorderRequired(false) {
            Ok(()) => {
                if let Ok(mut diag) = diagnostics.lock() {
                    diag.record_border_required_disabled();
                }
            }
            Err(e) => {
                let error = e.to_string();
                log::warn!("{LOG} SetIsBorderRequired failed: {error}");
                if let Ok(mut diag) = diagnostics.lock() {
                    diag.record_border_required_disable_error(&error);
                }
                record_wgc_startup_nonfatal_error(
                    &diagnostics,
                    &startup_stage,
                    startup_started,
                    "cursor_border_config_error",
                    "SetIsBorderRequired",
                    error,
                );
            }
        }
        record_wgc_startup_stage(
            &diagnostics,
            &startup_stage,
            startup_started,
            "cursor_border_config_done",
            "SetIsCursorCaptureEnabled/SetIsBorderRequired",
        );

        // ── 4. Wire up frame arrival handler ────────────────────────────
        log::info!("{LOG} [diag] thread: step 4 — wiring FrameArrived handler");
        record_wgc_startup_stage(
            &diagnostics,
            &startup_stage,
            startup_started,
            "frame_arrived_handler_register_start",
            "FrameArrived",
        );
        let stop_clone = stop_flag.clone();
        let cb_clone = frame_callback.clone();
        let d3d_device_clone = d3d_device.clone();
        let d3d_context_clone = d3d_context.clone();
        let capture_frame_count = Arc::new(std::sync::atomic::AtomicU64::new(0));
        let first_frame_seen_callback = first_frame_seen.clone();
        let diagnostics_callback = diagnostics.clone();
        let app_callback = app_handle.clone();
        let capture_started_at_ms = unix_now_ms();
        let startup_started_callback = startup_started;
        let staging_cache = Arc::new(Mutex::new(None::<(u32, u32, i32, ID3D11Texture2D)>));
        let staging_cache_callback = staging_cache.clone();

        let handler = windows::Foundation::TypedEventHandler::<
            Direct3D11CaptureFramePool,
            windows::core::IInspectable,
        >::new(move |pool, _| {
            if stop_clone.load(Ordering::SeqCst) {
                return Ok(());
            }
            if let Ok(mut diag) = diagnostics_callback.lock() {
                diag.record_backend_callback();
            }

            let pool = match pool {
                Some(p) => p,
                None => return Ok(()),
            };

            let frame = match pool.TryGetNextFrame() {
                Ok(f) => f,
                Err(e) => {
                    record_readback_failure(
                        &diagnostics_callback,
                        &app_callback,
                        |diag| &mut diag.try_get_next_frame_failures,
                        "try_get_next_frame",
                        e,
                    );
                    return Ok(());
                }
            };

            let surface = match frame.Surface() {
                Ok(s) => s,
                Err(e) => {
                    record_readback_failure(
                        &diagnostics_callback,
                        &app_callback,
                        |diag| &mut diag.surface_failures,
                        "surface",
                        e,
                    );
                    return Ok(());
                }
            };

            let access: IDirect3DDxgiInterfaceAccess = match surface.cast() {
                Ok(a) => a,
                Err(e) => {
                    record_readback_failure(
                        &diagnostics_callback,
                        &app_callback,
                        |diag| &mut diag.surface_cast_failures,
                        "surface_cast",
                        e,
                    );
                    return Ok(());
                }
            };

            let texture: ID3D11Texture2D = match unsafe { access.GetInterface() } {
                Ok(t) => t,
                Err(e) => {
                    record_readback_failure(
                        &diagnostics_callback,
                        &app_callback,
                        |diag| &mut diag.texture_interface_failures,
                        "texture_interface",
                        e,
                    );
                    return Ok(());
                }
            };

            let mut desc = D3D11_TEXTURE2D_DESC::default();
            unsafe { texture.GetDesc(&mut desc) };

            let w = desc.Width;
            let h = desc.Height;
            if w == 0 || h == 0 {
                record_readback_failure(
                    &diagnostics_callback,
                    &app_callback,
                    |diag| &mut diag.zero_dimension_frames,
                    "texture_dimensions",
                    format!("{}x{}", w, h),
                );
                return Ok(());
            }

            // Create staging texture for CPU read.
            let staging_desc = D3D11_TEXTURE2D_DESC {
                Width: w,
                Height: h,
                MipLevels: 1,
                ArraySize: 1,
                Format: desc.Format,
                SampleDesc: DXGI_SAMPLE_DESC {
                    Count: 1,
                    Quality: 0,
                },
                Usage: D3D11_USAGE_STAGING,
                BindFlags: 0,
                CPUAccessFlags: D3D11_CPU_ACCESS_READ.0 as u32,
                MiscFlags: 0,
            };

            let staging: ID3D11Texture2D = {
                let mut cache = match staging_cache_callback.lock() {
                    Ok(cache) => cache,
                    Err(_) => return Ok(()),
                };
                let current = cache
                    .as_ref()
                    .map(|(width, height, format, _)| (*width, *height, *format));
                if !staging_texture_matches(current, w, h, desc.Format.0) {
                    let created = unsafe {
                        let mut tex = None;
                        let hr =
                            d3d_device_clone.CreateTexture2D(&staging_desc, None, Some(&mut tex));
                        match (hr, tex) {
                            (Ok(()), Some(texture)) => Some(texture),
                            (Err(e), _) => {
                                record_readback_failure(
                                    &diagnostics_callback,
                                    &app_callback,
                                    |diag| &mut diag.staging_texture_failures,
                                    "staging_texture",
                                    e,
                                );
                                None
                            }
                            _ => None,
                        }
                    };
                    let Some(created) = created else {
                        return Ok(());
                    };
                    *cache = Some((w, h, desc.Format.0, created));
                }
                cache.as_ref().unwrap().3.clone()
            };

            unsafe {
                d3d_context_clone.CopyResource(&staging, &texture);
                // Flush ensures the GPU copy completes before we Map
                // the staging texture for CPU read.
                d3d_context_clone.Flush();
            }

            let mut mapped = D3D11_MAPPED_SUBRESOURCE::default();
            let map_result =
                unsafe { d3d_context_clone.Map(&staging, 0, D3D11_MAP_READ, 0, Some(&mut mapped)) };

            if let Err(error) = map_result {
                record_readback_failure(
                    &diagnostics_callback,
                    &app_callback,
                    |diag| &mut diag.map_failures,
                    "map",
                    error,
                );
                return Ok(());
            }

            // Convert BGRA → RGBA.
            let row_pitch = mapped.RowPitch as usize;
            let mut rgba = Vec::with_capacity((w * h * 4) as usize);

            unsafe {
                let src = mapped.pData as *const u8;
                for row in 0..h as usize {
                    let row_ptr = src.add(row * row_pitch);
                    for col in 0..w as usize {
                        let px = row_ptr.add(col * 4);
                        rgba.push(*px.add(2)); // R (was B)
                        rgba.push(*px.add(1)); // G
                        rgba.push(*px.add(0)); // B (was R)
                        rgba.push(255); // A (force opaque)
                    }
                }
            }

            unsafe {
                d3d_context_clone.Unmap(&staging, 0);
            }

            let n = capture_frame_count.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
            if let Ok(mut diag) = diagnostics_callback.lock() {
                diag.usable_frames += 1;
                if diag.first_raw_frame_latency_ms.is_none() {
                    let latency = unix_now_ms().saturating_sub(capture_started_at_ms);
                    diag.first_raw_frame_latency_ms = Some(latency);
                    diag.timing.first_raw_frame_latency_ms = Some(latency);
                }
            }
            if n == 0 {
                first_frame_seen_callback.store(true, Ordering::SeqCst);
                if let Ok(mut diag) = diagnostics_callback.lock() {
                    diag.record_startup_stage(
                        "first_frame_arrived",
                        "FrameArrived callback",
                        startup_elapsed_ms(startup_started_callback),
                    );
                }
                let non_zero = rgba.iter().filter(|&&b| b != 0).count();
                log::info!(
                        "{LOG} FrameArrived: first frame {}x{}, rgba_len={}, non_zero_bytes={}, row_pitch={}",
                        w, h, rgba.len(), non_zero, row_pitch
                    );
            } else if n.is_multiple_of(300) {
                log::info!("{LOG} FrameArrived: frame #{n}");
            }

            let now = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap_or_default()
                .as_millis() as u64;

            let captured = CapturedFrame {
                width: w,
                height: h,
                data: rgba,
                timestamp_ms: now,
            };

            if let Ok(guard) = cb_clone.lock() {
                if let Some(ref cb) = *guard {
                    cb(captured);
                } else if n == 0 {
                    log::warn!("{LOG} FrameArrived: callback not set yet, dropping frame #{n}");
                }
            }

            Ok(())
        });

        if let Err(e) = frame_pool.FrameArrived(&handler) {
            log::error!("{LOG} failed to register FrameArrived handler: {e}");
            record_wgc_startup_error(
                &diagnostics,
                &startup_stage,
                startup_started,
                "frame_arrived_handler_register_error",
                "FrameArrived",
                e,
            );
            active.store(false, Ordering::SeqCst);
            return;
        }

        log::info!("{LOG} [diag] thread: step 4 complete — FrameArrived handler registered");
        record_wgc_startup_stage(
            &diagnostics,
            &startup_stage,
            startup_started,
            "frame_arrived_handler_register_done",
            "FrameArrived",
        );

        // ── 5. Register Closed event for source loss detection ──────────
        log::info!("{LOG} [diag] thread: step 5 — registering Closed handler");
        record_wgc_startup_stage(
            &diagnostics,
            &startup_stage,
            startup_started,
            "closed_handler_register_start",
            "GraphicsCaptureItem.Closed",
        );
        let active_closed = active.clone();
        let stop_closed = stop_flag.clone();
        let app_closed = app_handle.clone();
        let closed_handler = windows::Foundation::TypedEventHandler::<
            windows::Graphics::Capture::GraphicsCaptureItem,
            windows::core::IInspectable,
        >::new(move |_, _| {
            log::info!("{LOG} GraphicsCaptureItem.Closed — source lost");
            active_closed.store(false, Ordering::SeqCst);
            stop_closed.store(true, Ordering::SeqCst);
            let _ = app_closed.emit("share_error", "Shared window was closed");
            Ok(())
        });
        if let Err(e) = capture_item.Closed(&closed_handler) {
            log::warn!("{LOG} failed to register GraphicsCaptureItem.Closed handler: {e}");
            record_wgc_startup_error(
                &diagnostics,
                &startup_stage,
                startup_started,
                "closed_handler_register_error",
                "GraphicsCaptureItem.Closed",
                e,
            );
        }
        record_wgc_startup_stage(
            &diagnostics,
            &startup_stage,
            startup_started,
            "closed_handler_register_done",
            "GraphicsCaptureItem.Closed",
        );

        // ── 6. Wait for on_frame() callback before starting capture ───
        log::info!("{LOG} [diag] thread: step 6 — waiting for on_frame() condvar signal");
        record_wgc_startup_stage(
            &diagnostics,
            &startup_stage,
            startup_started,
            "callback_wait_start",
            "wait for on_frame callback",
        );
        // The caller sets the frame callback via on_frame() after start()
        // returns. We wait here (up to 5s) so no early frames are lost.
        {
            let (lock, cvar) = &*callback_ready;
            let guard = lock.lock().unwrap();
            // Use wait_timeout_while so we skip the wait entirely if
            // on_frame() already set the flag before we got here.
            let result = cvar
                .wait_timeout_while(guard, std::time::Duration::from_secs(5), |ready| !*ready)
                .unwrap();
            if !*result.0 {
                log::warn!(
                    "{LOG} timed out waiting for on_frame() callback — starting capture anyway"
                );
                record_wgc_startup_stage(
                    &diagnostics,
                    &startup_stage,
                    startup_started,
                    "callback_wait_timeout",
                    "wait for on_frame callback",
                );
            } else {
                log::info!("{LOG} on_frame() callback set, starting capture");
                record_wgc_startup_stage(
                    &diagnostics,
                    &startup_stage,
                    startup_started,
                    "callback_wait_done",
                    "wait for on_frame callback",
                );
            }
        }

        // ── 7. Start capture ────────────────────────────────────────────
        log::info!("{LOG} [diag] thread: step 7 — calling StartCapture()");
        record_wgc_startup_stage(
            &diagnostics,
            &startup_stage,
            startup_started,
            "start_capture_start",
            "StartCapture",
        );
        if let Err(e) = session.StartCapture() {
            log::error!("{LOG} failed to start capture: {e}");
            record_wgc_startup_error(
                &diagnostics,
                &startup_stage,
                startup_started,
                "start_capture_error",
                "StartCapture",
                e,
            );
            active.store(false, Ordering::SeqCst);
            return;
        }
        record_wgc_startup_stage(
            &diagnostics,
            &startup_stage,
            startup_started,
            "start_capture_done",
            "StartCapture",
        );
        record_wgc_startup_stage(
            &diagnostics,
            &startup_stage,
            startup_started,
            "awaiting_first_frame",
            "FrameArrived callback",
        );

        log::info!("{LOG} capture started (session active)");

        // ── 8. Spin until stop_flag is set ──────────────────────────────
        while !stop_flag.load(Ordering::SeqCst) {
            std::thread::sleep(std::time::Duration::from_millis(50));
        }

        // ── 9. Cleanup ─────────────────────────────────────────────────
        let _ = session.Close();
        let _ = frame_pool.Close();
        active.store(false, Ordering::SeqCst);

        // Balance the RoInitialize call at the top of this function.
        if did_ro_init {
            unsafe { windows::Win32::System::WinRT::RoUninitialize() };
        }
        log::info!("{LOG} capture stopped");
        if let Ok(diag) = diagnostics.lock() {
            if diag.usable_frames == 0 {
                log::warn!("{LOG} WGC stopped before first frame; {}", diag.compact_summary());
            }
        };
    }
}

impl ScreenCapture for WinCapture {
    fn start(&self) -> Result<(), CaptureError> {
        // Capture is started in `WinCapture::start()` constructor.
        if self.active.load(Ordering::SeqCst) {
            Ok(())
        } else {
            Err(CaptureError::CaptureStartFailed(
                "Windows capture session is not active".to_string(),
            ))
        }
    }

    fn stop(&self) {
        self.stop_flag.store(true, Ordering::SeqCst);
        // Wake the capture thread in case it's waiting for on_frame().
        let (lock, cvar) = &*self.callback_ready;
        if let Ok(mut ready) = lock.lock() {
            *ready = true;
            cvar.notify_one();
        }
        if let Ok(mut guard) = self.capture_thread.lock() {
            if let Some(handle) = guard.take() {
                let _ = handle.join();
            }
        }
        self.active.store(false, Ordering::SeqCst);
    }

    fn is_active(&self) -> bool {
        self.active.load(Ordering::SeqCst)
    }

    fn on_frame(&self, cb: Box<dyn Fn(CapturedFrame) + Send + 'static>) {
        if let Ok(mut guard) = self.frame_callback.lock() {
            *guard = Some(cb);
        }
        // Signal the capture thread that the callback is ready so it can
        // call StartCapture() without losing early frames.
        let (lock, cvar) = &*self.callback_ready;
        if let Ok(mut ready) = lock.lock() {
            *ready = true;
            cvar.notify_one();
        }
    }
}

impl Drop for WinCapture {
    fn drop(&mut self) {
        if self.active.load(Ordering::SeqCst) {
            log::info!("{LOG} Drop: stopping active capture session");
            self.stop_flag.store(true, Ordering::SeqCst);
            if let Ok(mut guard) = self.capture_thread.lock() {
                if let Some(handle) = guard.take() {
                    let _ = handle.join();
                }
            }
            self.active.store(false, Ordering::SeqCst);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{staging_texture_matches, WindowsNativeCaptureDiagnostics};

    #[test]
    fn staging_texture_is_reused_only_for_matching_dimensions_and_format() {
        assert!(staging_texture_matches(Some((1920, 1080, 87)), 1920, 1080, 87));
        assert!(!staging_texture_matches(Some((1920, 1080, 87)), 1280, 720, 87));
        assert!(!staging_texture_matches(Some((1920, 1080, 87)), 1920, 1080, 28));
        assert!(!staging_texture_matches(None, 1920, 1080, 87));
    }

    #[test]
    fn compact_summary_includes_failure_count_and_first_error() {
        let mut diag = WindowsNativeCaptureDiagnostics::new("wgc", "screen", 1920, 1080);
        diag.record_startup_stage("awaiting_first_frame", "FrameArrived callback", 42);
        diag.map_failures = 3;
        diag.first_error = Some("map: E_FAIL".to_string());
        let summary = diag.compact_summary();
        assert!(summary.contains("stalled_in=awaiting_first_frame"));
        assert!(summary.contains("current_operation=FrameArrived callback"));
        assert!(summary.contains("startup_elapsed_ms=42"));
        assert!(summary.contains("readback_failures=3"));
        assert!(summary.contains("first_error=map: E_FAIL"));
    }

    #[test]
    fn compact_summary_includes_startup_step_timings() {
        let mut diag = WindowsNativeCaptureDiagnostics::new("wgc", "screen", 1920, 1080);
        diag.record_startup_stage("capture_item_create_start", "create_capture_item", 4);
        diag.record_startup_stage("capture_item_create_done", "create_capture_item", 7);
        diag.record_startup_stage("frame_pool_create_start", "CreateFreeThreaded", 10);
        diag.record_startup_stage("frame_pool_create_done", "CreateFreeThreaded", 18);
        let summary = diag.compact_summary();
        assert!(summary.contains("startup_step_timings="));
        assert!(summary.contains("capture_item_create_start:4ms"));
        assert!(summary.contains("capture_item_create_done:7ms"));
        assert!(summary.contains("frame_pool_create_start:10ms"));
        assert!(summary.contains("frame_pool_create_done:18ms"));
    }

    #[test]
    fn compact_summary_identifies_window_capture_attempts() {
        let diag = WindowsNativeCaptureDiagnostics::new("gdi_poll", "window", 1280, 720);
        let summary = diag.compact_summary();
        assert!(summary.contains("backend=gdi_poll"));
        assert!(summary.contains("source_kind=window"));
    }

    #[test]
    fn border_required_result_is_recorded_for_diagnostics() {
        let mut diag = WindowsNativeCaptureDiagnostics::new("wgc", "screen", 1920, 1080);

        diag.record_border_required_disabled();
        assert_eq!(diag.border_required_disabled, Some(true));
        assert_eq!(diag.border_required_error, None);
        assert!(diag
            .capture_indicator_note
            .as_deref()
            .unwrap()
            .contains("Windows is enforcing the capture indicator"));
        assert!(diag
            .compact_summary()
            .contains("border_required_disabled=true"));

        diag.record_border_required_disable_error("E_ACCESSDENIED");
        assert_eq!(diag.border_required_disabled, Some(false));
        assert_eq!(
            diag.border_required_error.as_deref(),
            Some("E_ACCESSDENIED")
        );
        assert!(diag
            .compact_summary()
            .contains("border_required_error=E_ACCESSDENIED"));
    }

    #[test]
    fn cadence_diagnostics_count_callbacks_emitted_frames_throttle_and_timings() {
        let mut diag = WindowsNativeCaptureDiagnostics::new("wgc", "screen", 1920, 1080);
        diag.record_backend_callback();
        diag.record_backend_callback();
        diag.record_throttle_drop();
        diag.record_pollable_frame_timing(1, 2, 3, 4, 5, 6, 7);
        diag.record_pollable_frame_timing(3, 4, 5, 6, 7, 8, 9);

        assert_eq!(diag.frame_arrived_callbacks, 2);
        assert_eq!(diag.throttle_drop_count, 1);
        assert_eq!(diag.emitted_pollable_frames, 2);
        assert_eq!(diag.cap_downscale_avg_ms, 2.0);
        assert_eq!(diag.i420_convert_avg_ms, 3.0);
        assert_eq!(diag.rgba_to_rgb_avg_ms, 4.0);
        assert_eq!(diag.jpeg_encode_avg_ms, 5.0);
        assert_eq!(diag.base64_encode_avg_ms, 6.0);
        assert_eq!(diag.latest_frame_write_avg_ms, 7.0);
        assert_eq!(diag.raw_to_pollable_frame_avg_ms, 8.0);
    }
}
