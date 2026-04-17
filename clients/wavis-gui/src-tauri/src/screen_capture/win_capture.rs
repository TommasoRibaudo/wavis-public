//! Windows screen capture backend using the Windows Graphics Capture API.
//!
//! Parallel to `pipewire_capture.rs` and `x11_capture.rs` (Linux backends).
//! Creates a `GraphicsCaptureItem` for a specified monitor (HMONITOR) or
//! window (HWND) handle, captures frames via `Direct3D11CaptureFramePool`,
//! and delivers them as `CapturedFrame` structs through the `ScreenCapture` trait.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Condvar, Mutex};

use tauri::{AppHandle, Emitter};

use super::{CaptureError, CapturedFrame, ScreenCapture};

const LOG: &str = "[wavis:win-capture]";

/// Alias for the frame callback type to satisfy clippy::type_complexity.
type FrameCallback = Arc<Mutex<Option<Box<dyn Fn(CapturedFrame) + Send + 'static>>>>;

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
    /// Tauri app handle for emitting `share_error` events on source loss.
    pub app_handle: AppHandle,
}

/// Create a `GraphicsCaptureItem` from a handle value string.
///
/// Tries monitor first (via `IGraphicsCaptureItemInterop::CreateForMonitor`),
/// then window (via `CreateForWindow`). Returns a descriptive error if the
/// source is no longer available.
fn create_capture_item(
    handle_val: isize,
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

    // Try as monitor first.
    let monitor_result: Result<GraphicsCaptureItem, _> =
        unsafe { interop.CreateForMonitor(HMONITOR(handle_val as *mut _)) };

    if let Ok(item) = monitor_result {
        return Ok(item);
    }

    // Try as window handle.
    let window_result: Result<GraphicsCaptureItem, _> =
        unsafe { interop.CreateForWindow(HWND(handle_val as *mut _)) };

    match window_result {
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
    /// (trying monitor first, then window), and starts the capture loop on a
    /// dedicated thread.
    ///
    /// Returns `CaptureError::CaptureStartFailed` with a descriptive message
    /// if the source is no longer available.
    pub fn start(config: WinCaptureConfig) -> Result<Self, CaptureError> {
        log::info!(
            "{LOG} [diag] start() ENTERED, source_id={}",
            config.source_id
        );

        let handle_val: isize = config.source_id.parse().map_err(|_| {
            CaptureError::CaptureStartFailed(format!("invalid source id: {}", config.source_id))
        })?;

        let capture_item = create_capture_item(handle_val)?;

        let size = capture_item.Size().map_err(|e| {
            CaptureError::CaptureStartFailed(format!("failed to get capture item size: {e}"))
        })?;

        if size.Width <= 0 || size.Height <= 0 {
            return Err(CaptureError::CaptureStartFailed(
                "The selected source has zero dimensions".to_string(),
            ));
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
                Self::capture_loop(
                    capture_item,
                    size,
                    active,
                    stop_flag,
                    frame_callback,
                    callback_ready,
                    config.app_handle,
                );
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
    fn capture_loop(
        capture_item: windows::Graphics::Capture::GraphicsCaptureItem,
        size: windows::Graphics::SizeInt32,
        active: Arc<AtomicBool>,
        stop_flag: Arc<AtomicBool>,
        frame_callback: FrameCallback,
        callback_ready: Arc<(Mutex<bool>, Condvar)>,
        app_handle: AppHandle,
    ) {
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

        // Disable the yellow capture border if the API supports it.
        let _ = session.SetIsCursorCaptureEnabled(true);
        let _ = session.SetIsBorderRequired(false);

        // ── 4. Wire up frame arrival handler ────────────────────────────
        log::info!("{LOG} [diag] thread: step 4 — wiring FrameArrived handler");
        let stop_clone = stop_flag.clone();
        let cb_clone = frame_callback.clone();
        let d3d_device_clone = d3d_device.clone();
        let d3d_context_clone = d3d_context.clone();
        let capture_frame_count = Arc::new(std::sync::atomic::AtomicU64::new(0));

        let handler = windows::Foundation::TypedEventHandler::<
            Direct3D11CaptureFramePool,
            windows::core::IInspectable,
        >::new(move |pool, _| {
            if stop_clone.load(Ordering::SeqCst) {
                return Ok(());
            }

            let pool = match pool {
                Some(p) => p,
                None => return Ok(()),
            };

            let frame = match pool.TryGetNextFrame() {
                Ok(f) => f,
                Err(_) => return Ok(()),
            };

            let surface = match frame.Surface() {
                Ok(s) => s,
                Err(_) => return Ok(()),
            };

            let access: IDirect3DDxgiInterfaceAccess = match surface.cast() {
                Ok(a) => a,
                Err(_) => return Ok(()),
            };

            let texture: ID3D11Texture2D = match unsafe { access.GetInterface() } {
                Ok(t) => t,
                Err(_) => return Ok(()),
            };

            let mut desc = D3D11_TEXTURE2D_DESC::default();
            unsafe { texture.GetDesc(&mut desc) };

            let w = desc.Width;
            let h = desc.Height;
            if w == 0 || h == 0 {
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

            let staging: ID3D11Texture2D = unsafe {
                let mut tex = None;
                let hr = d3d_device_clone.CreateTexture2D(&staging_desc, None, Some(&mut tex));
                match (hr, tex) {
                    (Ok(()), Some(t)) => t,
                    _ => return Ok(()),
                }
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

            if map_result.is_err() {
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
            if n == 0 {
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
