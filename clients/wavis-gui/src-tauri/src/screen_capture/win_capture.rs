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
    pub item_width: u32,
    pub item_height: u32,
    pub adapter_description: Option<String>,
    pub adapter_vendor_id: Option<u32>,
    pub adapter_device_id: Option<u32>,
    pub frame_arrived_callbacks: u64,
    pub usable_frames: u64,
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
}

impl WindowsNativeCaptureDiagnostics {
    pub fn new(backend: &str, source_kind: &str, item_width: u32, item_height: u32) -> Self {
        Self {
            backend: backend.to_string(),
            source_kind: source_kind.to_string(),
            startup_stage: "backend_start".to_string(),
            item_width,
            item_height,
            adapter_description: None,
            adapter_vendor_id: None,
            adapter_device_id: None,
            frame_arrived_callbacks: 0,
            usable_frames: 0,
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
        }
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
            "backend={} source_kind={} last_stage={} callbacks={} usable_frames={} readback_failures={} first_error={}",
            self.backend,
            self.source_kind,
            self.startup_stage,
            self.frame_arrived_callbacks,
            self.usable_frames,
            self.total_readback_failures(),
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
}

impl Drop for WgcStartupSummary {
    fn drop(&mut self) {
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
    capture_item: windows::Graphics::Capture::GraphicsCaptureItem,
    size: windows::Graphics::SizeInt32,
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

        let capture_item = create_capture_item(handle_val, config.source_kind)?;

        let size = capture_item.Size().map_err(|e| {
            CaptureError::CaptureStartFailed(format!("failed to get capture item size: {e}"))
        })?;

        if size.Width <= 0 || size.Height <= 0 {
            return Err(CaptureError::CaptureStartFailed(
                "The selected source has zero dimensions".to_string(),
            ));
        }
        if let Ok(mut diagnostics) = config.diagnostics.lock() {
            diagnostics.item_width = size.Width as u32;
            diagnostics.item_height = size.Height as u32;
            diagnostics.startup_stage = "capture_item_ready".to_string();
        }

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
                    capture_item,
                    size,
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
            capture_item,
            size,
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
        let startup_stage = Arc::new(Mutex::new("capture_thread_started"));
        let first_frame_seen = Arc::new(AtomicBool::new(false));
        let _startup_summary = WgcStartupSummary {
            stage: startup_stage.clone(),
            first_frame_seen: first_frame_seen.clone(),
        };
        if let Ok(mut diag) = diagnostics.lock() {
            diag.startup_stage = "capture_thread_started".to_string();
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
                    active.store(false, Ordering::SeqCst);
                    return;
                }
            }
        }
        *startup_stage.lock().unwrap() = "winrt_initialized";
        if let Ok(mut diag) = diagnostics.lock() {
            diag.startup_stage = "winrt_initialized".to_string();
        }

        // ── 1. Create D3D11 device ──────────────────────────────────────
        log::info!("{LOG} [diag] thread: step 1 — creating D3D11 device");
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
                active.store(false, Ordering::SeqCst);
                return;
            }

            match (device, context) {
                (Some(d), Some(c)) => (d, c),
                _ => {
                    log::error!("{LOG} D3D11CreateDevice returned None");
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
        *startup_stage.lock().unwrap() = "d3d_device_ready";
        if let Ok(mut diag) = diagnostics.lock() {
            diag.startup_stage = "d3d_device_ready".to_string();
        }

        // ── 2. Create WinRT IDirect3DDevice ─────────────────────────────
        log::info!("{LOG} [diag] thread: step 2 — creating WinRT IDirect3DDevice");
        let dxgi_device: IDXGIDevice = match d3d_device.cast() {
            Ok(d) => d,
            Err(e) => {
                log::error!("{LOG} failed to get IDXGIDevice: {e}");
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
                }
            }
        }

        let winrt_device = unsafe {
            match CreateDirect3D11DeviceFromDXGIDevice(&dxgi_device) {
                Ok(inspectable) => match inspectable.cast::<IDirect3DDevice>() {
                    Ok(device) => device,
                    Err(e) => {
                        log::error!("{LOG} failed to cast to IDirect3DDevice: {e}");
                        active.store(false, Ordering::SeqCst);
                        return;
                    }
                },
                Err(e) => {
                    log::error!("{LOG} CreateDirect3D11DeviceFromDXGIDevice failed: {e}");
                    active.store(false, Ordering::SeqCst);
                    return;
                }
            }
        };
        *startup_stage.lock().unwrap() = "winrt_d3d_device_ready";
        if let Ok(mut diag) = diagnostics.lock() {
            diag.startup_stage = "winrt_d3d_device_ready".to_string();
        }

        // ── 3. Create frame pool + capture session ──────────────────────
        log::info!("{LOG} [diag] thread: step 3 — creating frame pool + capture session");
        let frame_pool = match Direct3D11CaptureFramePool::CreateFreeThreaded(
            &winrt_device,
            DirectXPixelFormat::B8G8R8A8UIntNormalized,
            2, // double-buffer for continuous capture
            size,
        ) {
            Ok(pool) => pool,
            Err(e) => {
                log::error!("{LOG} failed to create frame pool: {e}");
                active.store(false, Ordering::SeqCst);
                return;
            }
        };

        let session = match frame_pool.CreateCaptureSession(&capture_item) {
            Ok(s) => s,
            Err(e) => {
                log::error!("{LOG} failed to create capture session: {e}");
                active.store(false, Ordering::SeqCst);
                return;
            }
        };
        *startup_stage.lock().unwrap() = "capture_session_ready";
        if let Ok(mut diag) = diagnostics.lock() {
            diag.startup_stage = "capture_session_ready".to_string();
        }

        // Disable the yellow capture border if the API supports it.
        // Cursor capture is enabled so viewers see the cursor in the stream.
        // Cursor visibility for the streamer is preserved by using monitor
        // capture (not window capture) in create_capture_item above.
        let _ = session.SetIsCursorCaptureEnabled(true);
        let _ = session.SetIsBorderRequired(false);

        // ── 4. Wire up frame arrival handler ────────────────────────────
        log::info!("{LOG} [diag] thread: step 4 — wiring FrameArrived handler");
        let stop_clone = stop_flag.clone();
        let cb_clone = frame_callback.clone();
        let d3d_device_clone = d3d_device.clone();
        let d3d_context_clone = d3d_context.clone();
        let capture_frame_count = Arc::new(std::sync::atomic::AtomicU64::new(0));
        let first_frame_seen_callback = first_frame_seen.clone();
        let diagnostics_callback = diagnostics.clone();
        let app_callback = app_handle.clone();
        let capture_started_at_ms = unix_now_ms();
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
                diag.frame_arrived_callbacks += 1;
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
                    diag.first_raw_frame_latency_ms =
                        Some(unix_now_ms().saturating_sub(capture_started_at_ms));
                }
            }
            if n == 0 {
                first_frame_seen_callback.store(true, Ordering::SeqCst);
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
            active.store(false, Ordering::SeqCst);
            return;
        }

        log::info!("{LOG} [diag] thread: step 4 complete — FrameArrived handler registered");
        *startup_stage.lock().unwrap() = "frame_handler_registered";
        if let Ok(mut diag) = diagnostics.lock() {
            diag.startup_stage = "frame_handler_registered".to_string();
        }

        // ── 5. Register Closed event for source loss detection ──────────
        log::info!("{LOG} [diag] thread: step 5 — registering Closed handler");
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
        let _ = capture_item.Closed(&closed_handler);

        // ── 6. Wait for on_frame() callback before starting capture ───
        log::info!("{LOG} [diag] thread: step 6 — waiting for on_frame() condvar signal");
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
            } else {
                log::info!("{LOG} on_frame() callback set, starting capture");
            }
        }

        // ── 7. Start capture ────────────────────────────────────────────
        log::info!("{LOG} [diag] thread: step 7 — calling StartCapture()");
        if let Err(e) = session.StartCapture() {
            log::error!("{LOG} failed to start capture: {e}");
            active.store(false, Ordering::SeqCst);
            return;
        }
        *startup_stage.lock().unwrap() = "capture_started";
        if let Ok(mut diag) = diagnostics.lock() {
            diag.startup_stage = "capture_started".to_string();
        }

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
        }
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
        diag.startup_stage = "capture_started".to_string();
        diag.map_failures = 3;
        diag.first_error = Some("map: E_FAIL".to_string());
        let summary = diag.compact_summary();
        assert!(summary.contains("readback_failures=3"));
        assert!(summary.contains("first_error=map: E_FAIL"));
    }

    #[test]
    fn compact_summary_identifies_window_capture_attempts() {
        let diag = WindowsNativeCaptureDiagnostics::new("gdi_poll", "window", 1280, 720);
        let summary = diag.compact_summary();
        assert!(summary.contains("backend=gdi_poll"));
        assert!(summary.contains("source_kind=window"));
    }
}
