//! GDI polling capture backend for Windows game cursor compatibility.
//!
//! Uses `PrintWindow(PW_RENDERFULLCONTENT)` to capture window content without
//! creating a WGC `GraphicsCaptureSession`. WGC sessions unconditionally disable
//! DWM Multi-Plane Overlay (MPO) on session creation, breaking the software cursor
//! rendered by OpenGL/SDL2 games (Project Zomboid, Battle Brothers). This backend
//! never creates a WGC session, leaving MPO intact and preserving in-game cursors.

use std::mem;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

use tauri::AppHandle;

use windows::Win32::Foundation::{HWND, POINT, RECT};
use windows::Win32::Graphics::Gdi::{
    ClientToScreen, CreateCompatibleBitmap, CreateCompatibleDC, DeleteDC, DeleteObject, GetDC,
    GetDIBits, ReleaseDC, SelectObject, BITMAPINFO, BITMAPINFOHEADER, BI_RGB, DIB_RGB_COLORS,
    HBITMAP, HDC, HGDIOBJ,
};
use windows::Win32::Storage::Xps::{PrintWindow, PRINT_WINDOW_FLAGS, PW_CLIENTONLY};
use windows::Win32::UI::WindowsAndMessaging::{
    CURSOR_SHOWING, CURSORINFO, DI_NORMAL, DrawIconEx, GetClientRect, GetCursorInfo, GetIconInfo,
    HICON, ICONINFO, IsIconic, PW_RENDERFULLCONTENT,
};

/// HWND is a raw-pointer newtype in the windows crate and does not auto-derive Send.
/// Window handles in Windows are safe to pass between threads for the operations
/// used here (PrintWindow, GetClientRect, IsIconic, ClientToScreen).
struct SendHwnd(HWND);
unsafe impl Send for SendHwnd {}

use super::{CaptureError, CapturedFrame, ScreenCapture};

const LOG: &str = "[wavis:gdi-capture]";

type FrameCallback = Arc<Mutex<Option<Box<dyn Fn(CapturedFrame) + Send + 'static>>>>;

pub struct GdiCapture {
    stop_flag: Arc<AtomicBool>,
    frame_callback: FrameCallback,
    capture_thread: Mutex<Option<JoinHandle<()>>>,
}

pub struct GdiCaptureConfig {
    pub hwnd: HWND,
    #[allow(dead_code)]
    pub app_handle: AppHandle,
    pub target_fps: u32,
}

struct GdiResources {
    mem_dc: HDC,
    bitmap: HBITMAP,
    width: u32,
    height: u32,
}

/// Allocate a memory DC + compatible bitmap of the given dimensions.
unsafe fn alloc_gdi_resources(w: u32, h: u32) -> Option<GdiResources> {
    // GetDC(NULL) returns the screen DC — needed so CreateCompatibleBitmap
    // produces a color (not monochrome) bitmap matching the display format.
    let screen_dc = GetDC(HWND(std::ptr::null_mut()));
    if screen_dc.0.is_null() {
        log::error!("{LOG} GetDC(NULL) failed");
        return None;
    }
    let mem_dc = CreateCompatibleDC(screen_dc);
    let bitmap = CreateCompatibleBitmap(screen_dc, w as i32, h as i32);
    ReleaseDC(HWND(std::ptr::null_mut()), screen_dc);

    if mem_dc.0.is_null() {
        log::error!("{LOG} CreateCompatibleDC failed");
        if !bitmap.0.is_null() {
            let _ = DeleteObject(HGDIOBJ(bitmap.0));
        }
        return None;
    }
    if bitmap.0.is_null() {
        log::error!("{LOG} CreateCompatibleBitmap failed for {}x{}", w, h);
        let _ = DeleteDC(mem_dc);
        return None;
    }
    Some(GdiResources { mem_dc, bitmap, width: w, height: h })
}

unsafe fn free_gdi_resources(r: GdiResources) {
    let _ = DeleteObject(HGDIOBJ(r.bitmap.0));
    let _ = DeleteDC(r.mem_dc);
}

/// Composite the hardware cursor onto `mem_dc` relative to `hwnd`'s client area.
///
/// Handle ownership contract (Windows API):
/// - `ci.hCursor`   — UNOWNED (shared system resource). Do NOT `DeleteObject` it.
/// - `info.hbmMask` — OWNED by the caller after `GetIconInfo`. Must `DeleteObject`.
/// - `info.hbmColor`— OWNED by the caller after `GetIconInfo`. Must `DeleteObject` if non-null.
/// - `ci.hCursor` passed to `DrawIconEx` — still UNOWNED; `DrawIconEx` does not take ownership.
unsafe fn composite_cursor(mem_dc: HDC, hwnd: HWND) {
    let mut ci: CURSORINFO = mem::zeroed();
    ci.cbSize = mem::size_of::<CURSORINFO>() as u32;
    if GetCursorInfo(&mut ci).is_err() || ci.flags != CURSOR_SHOWING {
        return;
    }

    // ClientToScreen with (0,0) gives the client area top-left in screen coords.
    let mut origin = POINT { x: 0, y: 0 };
    let _ = ClientToScreen(hwnd, &mut origin);

    let mut info: ICONINFO = mem::zeroed();
    if GetIconInfo(ci.hCursor, &mut info).is_err() {
        return;
    }
    // Free owned bitmaps immediately — we only need the hotspot coordinates.
    let _ = DeleteObject(HGDIOBJ(info.hbmMask.0));
    if !info.hbmColor.0.is_null() {
        let _ = DeleteObject(HGDIOBJ(info.hbmColor.0));
    }

    let (draw_x, draw_y) = compute_cursor_draw_pos(
        (origin.x, origin.y),
        (ci.ptScreenPos.x, ci.ptScreenPos.y),
        (info.xHotspot, info.yHotspot),
    );

    // ci.hCursor is UNOWNED — DrawIconEx borrows it, does not take ownership.
    let _ = DrawIconEx(
        mem_dc,
        draw_x,
        draw_y,
        HICON(ci.hCursor.0),
        0,
        0,
        0,
        None,
        DI_NORMAL,
    );
}

/// Map cursor screen position to draw coordinates within the window's client area.
///
/// Pure function — no Windows API calls, safe to unit-test in CI.
pub(crate) fn compute_cursor_draw_pos(
    client_origin: (i32, i32),
    screen_pos: (i32, i32),
    hotspot: (u32, u32),
) -> (i32, i32) {
    let x = screen_pos.0 - client_origin.0 - hotspot.0 as i32;
    let y = screen_pos.1 - client_origin.1 - hotspot.1 as i32;
    (x, y)
}

fn capture_loop(
    hwnd: SendHwnd,
    stop_flag: Arc<AtomicBool>,
    frame_callback: FrameCallback,
    target_fps: u32,
) {
    let hwnd = hwnd.0;
    let frame_interval = Duration::from_millis(1000 / target_fps.max(1) as u64);
    let mut next_frame_at = Instant::now() + frame_interval;

    // Initial allocation — get client rect to determine dimensions.
    let (init_w, init_h) = unsafe {
        let mut rect: RECT = mem::zeroed();
        if GetClientRect(hwnd, &mut rect).is_err() {
            log::error!("{LOG} initial GetClientRect failed for {hwnd:?}");
            return;
        }
        (
            (rect.right - rect.left).max(0) as u32,
            (rect.bottom - rect.top).max(0) as u32,
        )
    };

    let mut resources = match unsafe { alloc_gdi_resources(init_w.max(1), init_h.max(1)) } {
        Some(r) => r,
        None => {
            log::error!("{LOG} initial GDI resource allocation failed");
            return;
        }
    };

    let mut frame_count: u64 = 0;
    let mut consecutive_zero_frames: u32 = 0;
    let mut zero_start: Option<Instant> = None;
    let mut zero_warned = false;

    while !stop_flag.load(Ordering::Relaxed) {
        unsafe {
            // Skip minimized windows — no pixels to capture.
            if IsIconic(hwnd).as_bool() {
                thread::sleep(Duration::from_millis(16));
                next_frame_at = Instant::now() + frame_interval;
                continue;
            }

            // Get current client area size; abort if the window was destroyed.
            let mut rect: RECT = mem::zeroed();
            if GetClientRect(hwnd, &mut rect).is_err() {
                log::info!("{LOG} GetClientRect failed — window likely closed");
                break;
            }
            let w = (rect.right - rect.left).max(0) as u32;
            let h = (rect.bottom - rect.top).max(0) as u32;
            if w == 0 || h == 0 {
                thread::sleep(Duration::from_millis(16));
                next_frame_at = Instant::now() + frame_interval;
                continue;
            }

            // Reallocate if dimensions changed (e.g. window resize).
            if w != resources.width || h != resources.height {
                log::info!("{LOG} window resized to {}x{}, reallocating GDI resources", w, h);
                let old = mem::replace(&mut resources, {
                    match alloc_gdi_resources(w, h) {
                        Some(r) => r,
                        None => {
                            log::error!("{LOG} GDI resource reallocation failed");
                            break;
                        }
                    }
                });
                free_gdi_resources(old);
            }

            // Select bitmap into memory DC, capture, composite cursor, then deselect.
            let old_obj = SelectObject(resources.mem_dc, HGDIOBJ(resources.bitmap.0));
            // PW_CLIENTONLY is PRINT_WINDOW_FLAGS(1); PW_RENDERFULLCONTENT is u32(2) in a
            // different module — combine via the inner value.
            let _ = PrintWindow(
                hwnd,
                resources.mem_dc,
                PRINT_WINDOW_FLAGS(PW_CLIENTONLY.0 | PW_RENDERFULLCONTENT),
            );
            composite_cursor(resources.mem_dc, hwnd);
            // Deselect before GetDIBits — bitmap must not be selected when reading.
            SelectObject(resources.mem_dc, old_obj);

            // Read BGRA pixels from the bitmap and convert to RGBA (alpha forced to 255).
            // The bitmap must NOT be selected into any DC at this point (deselected above).
            let data = {
                let mut bmi = BITMAPINFO {
                    bmiHeader: BITMAPINFOHEADER {
                        biSize: mem::size_of::<BITMAPINFOHEADER>() as u32,
                        biWidth: w as i32,
                        biHeight: -(h as i32), // negative = top-down row order
                        biPlanes: 1,
                        biBitCount: 32,
                        biCompression: BI_RGB.0,
                        ..mem::zeroed()
                    },
                    ..mem::zeroed()
                };
                let mut bgra = vec![0u8; (w * h * 4) as usize];
                let rows = GetDIBits(
                    resources.mem_dc,
                    resources.bitmap,
                    0,
                    h,
                    Some(bgra.as_mut_ptr() as *mut _),
                    &mut bmi,
                    DIB_RGB_COLORS,
                );
                if rows == 0 {
                    crate::debug_eprintln!("{} GetDIBits returned 0 rows", LOG);
                    let now = Instant::now();
                    if now < next_frame_at {
                        thread::sleep(next_frame_at - now);
                    }
                    next_frame_at += frame_interval;
                    continue;
                }
                // Swap B↔R channels in-place; Windows GDI uses BGRA ordering.
                for pixel in bgra.chunks_exact_mut(4) {
                    pixel.swap(0, 2);
                    pixel[3] = 255;
                }
                bgra
            };

            // Black-frame detection: PrintWindow silently produces all-zero frames
            // for exclusive-fullscreen-windowed paths (e.g. DWM-excluded windows).
            let is_all_zero = data.iter().all(|&b| b == 0);
            if is_all_zero {
                consecutive_zero_frames += 1;
                if zero_start.is_none() {
                    zero_start = Some(Instant::now());
                }
                if !zero_warned
                    && zero_start
                        .is_some_and(|t| Instant::now().duration_since(t) > Duration::from_secs(2))
                {
                    log::warn!(
                        "{LOG} PrintWindow has produced all-zero frames for >2s on {hwnd:?} — \
                         window may be exclusive-fullscreen or DWM-excluded"
                    );
                    zero_warned = true;
                }
                if zero_warned {
                    // Don't forward useless black frames after the warning.
                    let now = Instant::now();
                    if now < next_frame_at {
                        thread::sleep(next_frame_at - now);
                    }
                    next_frame_at += frame_interval;
                    continue;
                }
            } else if consecutive_zero_frames > 0 {
                consecutive_zero_frames = 0;
                zero_start = None;
                zero_warned = false;
            }

            let non_zero = data.iter().filter(|&&b| b != 0).count();
            if frame_count == 0 {
                log::info!("{LOG} first frame: {}x{}, non_zero_bytes={}", w, h, non_zero);
            } else if frame_count.is_multiple_of(300) {
                log::info!("{LOG} frame #{} {}x{}", frame_count, w, h);
            }
            // DPI: PrintWindow uses the window's DPI context; GetClientRect returns physical
            // pixels on a per-monitor-V2-aware process (Tauri default). No scaling needed —
            // CapturedFrame dimensions match feed_video_frame and JPEG encode expectations.
            crate::debug_eprintln!("{} frame #{} {}x{} non_zero={}", LOG, frame_count, w, h, non_zero);

            let timestamp_ms = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap_or_default()
                .as_millis() as u64;

            let captured = CapturedFrame { width: w, height: h, data, timestamp_ms };

            if let Ok(guard) = frame_callback.lock() {
                if let Some(ref cb) = *guard {
                    cb(captured);
                }
            }

            frame_count += 1;
        }

        // Absolute-deadline pacer — drift-free fixed-interval advance.
        let now = Instant::now();
        if now < next_frame_at {
            thread::sleep(next_frame_at - now);
        }
        next_frame_at += frame_interval;
    }

    // Cleanup GDI resources on thread exit.
    unsafe { free_gdi_resources(resources) };
    log::info!("{LOG} capture thread exited");
}

impl GdiCapture {
    pub fn start(config: GdiCaptureConfig) -> Result<Self, CaptureError> {
        log::info!("{LOG} start() for {hwnd:?}, target_fps={fps}", hwnd = config.hwnd, fps = config.target_fps);

        let stop_flag = Arc::new(AtomicBool::new(false));
        let frame_callback: FrameCallback = Arc::new(Mutex::new(None));

        let capture = Self {
            stop_flag: stop_flag.clone(),
            frame_callback: frame_callback.clone(),
            capture_thread: Mutex::new(None),
        };

        let hwnd = SendHwnd(config.hwnd);
        let target_fps = config.target_fps;

        let handle = thread::Builder::new()
            .name("gdi-capture".into())
            .spawn(move || {
                capture_loop(hwnd, stop_flag, frame_callback, target_fps);
            })
            .map_err(|e| {
                CaptureError::CaptureStartFailed(format!("failed to spawn GDI capture thread: {e}"))
            })?;

        *capture.capture_thread.lock().unwrap() = Some(handle);
        log::info!("{LOG} capture thread spawned");
        Ok(capture)
    }
}

impl ScreenCapture for GdiCapture {
    fn start(&self) -> Result<(), CaptureError> {
        if !self.stop_flag.load(Ordering::Relaxed) {
            Ok(())
        } else {
            Err(CaptureError::CaptureStartFailed(
                "GDI capture session is not active".to_string(),
            ))
        }
    }

    fn stop(&self) {
        self.stop_flag.store(true, Ordering::Relaxed);
        if let Ok(mut guard) = self.capture_thread.lock() {
            if let Some(handle) = guard.take() {
                let _ = handle.join();
            }
        }
    }

    fn is_active(&self) -> bool {
        !self.stop_flag.load(Ordering::Relaxed)
    }

    fn on_frame(&self, cb: Box<dyn Fn(CapturedFrame) + Send + 'static>) {
        if let Ok(mut guard) = self.frame_callback.lock() {
            *guard = Some(cb);
        }
    }
}

impl Drop for GdiCapture {
    fn drop(&mut self) {
        if !self.stop_flag.load(Ordering::Relaxed) {
            log::info!("{LOG} Drop: stopping active capture");
            self.stop();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::compute_cursor_draw_pos;

    #[test]
    fn at_origin_zero_hotspot() {
        assert_eq!(compute_cursor_draw_pos((0, 0), (0, 0), (0, 0)), (0, 0));
    }

    #[test]
    fn hotspot_shifts_draw_left_and_up() {
        // Cursor at (10,10) screen, window origin (0,0), hotspot (3,5)
        assert_eq!(compute_cursor_draw_pos((0, 0), (10, 10), (3, 5)), (7, 5));
    }

    #[test]
    fn cursor_left_of_window_gives_negative_x() {
        // Cursor at (-5, 10) screen, window at (0,0), no hotspot — valid input,
        // DrawIconEx clips anything outside the DC bounds automatically.
        assert_eq!(compute_cursor_draw_pos((0, 0), (-5, 10), (0, 0)), (-5, 10));
    }

    #[test]
    fn window_offset_and_hotspot() {
        // Window origin (100,200), cursor at (150,250), hotspot (5,3)
        assert_eq!(compute_cursor_draw_pos((100, 200), (150, 250), (5, 3)), (45, 47));
    }
}
