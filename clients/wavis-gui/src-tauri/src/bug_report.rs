//! Bug report log buffer — captures Rust-side log lines for the in-app bug report feature.
//!
//! `RustLogBuffer` is a bounded `VecDeque<String>` behind a `Mutex`, registered as Tauri
//! managed state (`RustLogBufferState`). A `BugReportLogLayer` (implemented as a
//! `fern::Dispatch` target integrated with `tauri_plugin_log`) formats and pushes log
//! lines into the buffer. The `get_rust_log_buffer` IPC command returns a snapshot
//! without draining.

use std::collections::VecDeque;
use std::io::Write;
use std::sync::{Arc, Mutex};

/// Bounded ring buffer for Rust log lines.
///
/// Wraps a `VecDeque<String>` with a fixed capacity. When the buffer is full,
/// the oldest entry is discarded on each new push.
pub struct RustLogBuffer {
    lines: VecDeque<String>,
    capacity: usize,
}

impl RustLogBuffer {
    /// Create a new buffer with the given maximum capacity.
    pub fn new(capacity: usize) -> Self {
        Self {
            lines: VecDeque::with_capacity(capacity),
            capacity,
        }
    }

    /// Push a formatted log line, discarding the oldest if at capacity.
    pub fn push(&mut self, line: String) {
        if self.lines.len() >= self.capacity {
            self.lines.pop_front();
        }
        self.lines.push_back(line);
    }

    /// Return all buffered lines in insertion order without clearing.
    pub fn snapshot(&self) -> Vec<String> {
        self.lines.iter().cloned().collect()
    }
}

/// Shared handle to the log buffer.
pub type SharedLogBuffer = Arc<Mutex<RustLogBuffer>>;

/// Create a new shared log buffer with the given capacity.
pub fn new_shared_buffer(capacity: usize) -> SharedLogBuffer {
    Arc::new(Mutex::new(RustLogBuffer::new(capacity)))
}

/// Tauri managed state wrapper holding an `Arc` to the shared buffer.
pub struct RustLogBufferState(pub SharedLogBuffer);

impl RustLogBufferState {
    pub fn new(buffer: SharedLogBuffer) -> Self {
        Self(buffer)
    }
}

// ─── BugReportLogLayer ─────────────────────────────────────────────
//
// Implemented as a `fern::Dispatch` target that integrates with
// `tauri_plugin_log` via `TargetKind::Dispatch`. Each formatted log
// record is written into the shared `RustLogBuffer`.

/// A `Write` adapter that pushes each written line into the shared log buffer.
///
/// `fern::Dispatch::chain` accepts `Box<dyn Write + Send>`. Each `write()` call
/// from fern contains one fully-formatted log line (with trailing newline).
/// We strip the trailing newline and push the line into the ring buffer.
struct BugReportLogWriter(SharedLogBuffer);

impl Write for BugReportLogWriter {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        let line = String::from_utf8_lossy(buf);
        let trimmed = line.trim_end_matches('\n').trim_end_matches('\r');
        if !trimmed.is_empty() {
            if let Ok(mut buffer) = self.0.lock() {
                buffer.push(trimmed.to_string());
            }
        }
        Ok(buf.len())
    }

    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

/// Build the `BugReportLogLayer` — a `fern::Dispatch` that forwards formatted
/// log records into the shared buffer.
///
/// Pass the returned dispatch to `tauri_plugin_log::Builder` via
/// `Target::new(TargetKind::Dispatch(dispatch))`.
pub fn build_bug_report_log_layer(buffer: SharedLogBuffer) -> fern::Dispatch {
    let writer = BugReportLogWriter(buffer);
    fern::Dispatch::new()
        .format(|out, message, record| {
            out.finish(format_args!(
                "[{}][{}] {}",
                record.level(),
                record.target(),
                message
            ))
        })
        .chain(Box::new(writer) as Box<dyn Write + Send>)
}

/// IPC command — returns buffered Rust log lines without clearing the buffer.
#[tauri::command]
pub fn get_rust_log_buffer(state: tauri::State<'_, RustLogBufferState>) -> Vec<String> {
    state.0.lock().map(|buf| buf.snapshot()).unwrap_or_default()
}

// ─── Window Screenshot Capture ─────────────────────────────────────

/// IPC command — captures the application window as a PNG screenshot.
///
/// - Windows: Uses Win32 GDI APIs (GetWindowRect, BitBlt) to capture the window content.
/// - macOS: Uses CGWindowListCreateImage to capture the window.
/// - Linux: Returns an error — descoped to follow-up.
#[tauri::command]
pub fn capture_window_screenshot(window: tauri::Window) -> Result<Vec<u8>, String> {
    capture_window_screenshot_impl(window)
}

#[cfg(target_os = "windows")]
fn capture_window_screenshot_impl(window: tauri::Window) -> Result<Vec<u8>, String> {
    use image::ImageBuffer;
    use image::Rgba;
    use std::io::Cursor;
    use windows::Win32::Foundation::{HWND, RECT};
    use windows::Win32::Graphics::Gdi::{
        BitBlt, CreateCompatibleBitmap, CreateCompatibleDC, DeleteDC, DeleteObject, GetDIBits,
        GetWindowDC, ReleaseDC, SelectObject, BITMAPINFO, BITMAPINFOHEADER, BI_RGB, DIB_RGB_COLORS,
        SRCCOPY,
    };
    use windows::Win32::UI::WindowsAndMessaging::GetWindowRect;

    // Get the native HWND from the Tauri window.
    let hwnd = window
        .hwnd()
        .map_err(|e| format!("Failed to get HWND: {e}"))?;
    let hwnd = HWND(hwnd.0);

    unsafe {
        // Get window dimensions.
        let mut rect: RECT = std::mem::zeroed();
        GetWindowRect(hwnd, &mut rect).map_err(|e| format!("GetWindowRect failed: {e}"))?;

        let width = rect.right - rect.left;
        let height = rect.bottom - rect.top;
        if width <= 0 || height <= 0 {
            return Err("Window has zero or negative dimensions".to_string());
        }

        // Get the window DC (captures DWM-composed content).
        let hdc_window = GetWindowDC(hwnd);
        if hdc_window.is_invalid() {
            return Err("GetWindowDC failed".to_string());
        }

        let hdc_mem = CreateCompatibleDC(hdc_window);
        if hdc_mem.is_invalid() {
            ReleaseDC(hwnd, hdc_window);
            return Err("CreateCompatibleDC failed".to_string());
        }

        let hbm = CreateCompatibleBitmap(hdc_window, width, height);
        if hbm.is_invalid() {
            let _ = DeleteDC(hdc_mem);
            ReleaseDC(hwnd, hdc_window);
            return Err("CreateCompatibleBitmap failed".to_string());
        }

        let old_bm = SelectObject(hdc_mem, hbm);

        // BitBlt the window content into the memory DC.
        let blt_result = BitBlt(hdc_mem, 0, 0, width, height, hdc_window, 0, 0, SRCCOPY);

        if blt_result.is_err() {
            SelectObject(hdc_mem, old_bm);
            let _ = DeleteObject(hbm);
            let _ = DeleteDC(hdc_mem);
            ReleaseDC(hwnd, hdc_window);
            return Err("BitBlt failed".to_string());
        }

        // Read the bitmap pixel data via GetDIBits.
        let mut bmi = BITMAPINFO {
            bmiHeader: BITMAPINFOHEADER {
                biSize: std::mem::size_of::<BITMAPINFOHEADER>() as u32,
                biWidth: width,
                biHeight: -height, // top-down DIB
                biPlanes: 1,
                biBitCount: 32,
                biCompression: BI_RGB.0,
                ..std::mem::zeroed()
            },
            ..std::mem::zeroed()
        };

        let pixel_count = (width * height) as usize;
        let mut pixels: Vec<u8> = vec![0u8; pixel_count * 4];

        let lines = GetDIBits(
            hdc_window,
            hbm,
            0,
            height as u32,
            Some(pixels.as_mut_ptr() as *mut _),
            &mut bmi,
            DIB_RGB_COLORS,
        );

        // Clean up GDI resources.
        SelectObject(hdc_mem, old_bm);
        let _ = DeleteObject(hbm);
        let _ = DeleteDC(hdc_mem);
        ReleaseDC(hwnd, hdc_window);

        if lines == 0 {
            return Err("GetDIBits failed".to_string());
        }

        // Convert BGRA → RGBA.
        for chunk in pixels.chunks_exact_mut(4) {
            chunk.swap(0, 2); // B ↔ R
        }

        // Encode as PNG.
        let img: ImageBuffer<Rgba<u8>, Vec<u8>> =
            ImageBuffer::from_raw(width as u32, height as u32, pixels)
                .ok_or_else(|| "Failed to create image buffer".to_string())?;

        let mut png_bytes: Vec<u8> = Vec::new();
        img.write_to(&mut Cursor::new(&mut png_bytes), image::ImageFormat::Png)
            .map_err(|e| format!("PNG encoding failed: {e}"))?;

        Ok(png_bytes)
    }
}

#[cfg(target_os = "macos")]
#[allow(unexpected_cfgs)]
fn capture_window_screenshot_impl(window: tauri::Window) -> Result<Vec<u8>, String> {
    use core_graphics::display::{
        kCGWindowImageDefault, kCGWindowListOptionIncludingWindow, CGRect, CGWindowListCreateImage,
    };
    use core_graphics::geometry::{CGPoint, CGSize};

    // Get the NSWindow number from the Tauri window.
    // ns_window() returns *mut c_void on macOS.
    let ns_window = window
        .ns_window()
        .map_err(|e| format!("Failed to get NSWindow: {e}"))?;

    // The NSWindow pointer lets us get the windowNumber via Objective-C runtime.
    let window_number: u32 = unsafe {
        let ns_win: cocoa_id = ns_window as cocoa_id;
        let num: i64 = objc2::msg_send![ns_win, windowNumber];
        if num <= 0 {
            return Err("Invalid window number".to_string());
        }
        num as u32
    };

    // Capture the window using CGWindowListCreateImage.
    // A zero-origin, zero-size rect acts as CGRectNull — captures at the window's own bounds.
    let null_rect = CGRect::new(&CGPoint::new(0.0, 0.0), &CGSize::new(0.0, 0.0));
    let cg_image = unsafe {
        CGWindowListCreateImage(
            null_rect,
            kCGWindowListOptionIncludingWindow,
            window_number,
            kCGWindowImageDefault,
        )
    };

    if cg_image.is_null() {
        return Err("CGWindowListCreateImage returned null".to_string());
    }

    // Encode as PNG using CGImageDestination backed by a CFMutableData (raw CFData FFI).
    let png_data = unsafe {
        use core_foundation::base::TCFType;
        use core_foundation::string::CFString;

        let mutable_data = CFDataCreateMutable(std::ptr::null(), 0);
        if mutable_data.is_null() {
            return Err("Failed to create CFMutableData".to_string());
        }

        let png_uti = CFString::new("public.png");
        let dest = CGImageDestinationCreateWithData(
            mutable_data as *const _,
            png_uti.as_concrete_TypeRef() as *const _,
            1,
            std::ptr::null(),
        );

        if dest.is_null() {
            core_foundation::base::CFRelease(mutable_data as *const _);
            return Err("Failed to create image destination".to_string());
        }

        CGImageDestinationAddImage(dest, cg_image as *const _, std::ptr::null());

        let finalized = CGImageDestinationFinalize(dest);
        core_foundation::base::CFRelease(dest);

        if !finalized {
            core_foundation::base::CFRelease(mutable_data as *const _);
            return Err("Failed to finalize PNG image".to_string());
        }

        let len = CFDataGetLength(mutable_data as *const _) as usize;
        let ptr = CFDataGetBytePtr(mutable_data as *const _);
        let result = std::slice::from_raw_parts(ptr, len).to_vec();
        core_foundation::base::CFRelease(mutable_data as *const _);
        result
    };

    Ok(png_data)
}

#[cfg(target_os = "macos")]
#[allow(non_camel_case_types)]
type cocoa_id = *mut objc2::runtime::AnyObject;

#[cfg(target_os = "macos")]
extern "C" {
    fn CFDataCreateMutable(
        allocator: *const std::ffi::c_void,
        capacity: i64,
    ) -> *mut std::ffi::c_void;

    fn CFDataGetBytePtr(data: *const std::ffi::c_void) -> *const u8;

    fn CFDataGetLength(data: *const std::ffi::c_void) -> i64;

    fn CGImageDestinationCreateWithData(
        data: *const std::ffi::c_void,
        uti: *const std::ffi::c_void,
        count: usize,
        options: *const std::ffi::c_void,
    ) -> *const std::ffi::c_void;

    fn CGImageDestinationAddImage(
        dest: *const std::ffi::c_void,
        image: *const std::ffi::c_void,
        properties: *const std::ffi::c_void,
    );

    fn CGImageDestinationFinalize(dest: *const std::ffi::c_void) -> bool;
}

#[cfg(target_os = "linux")]
fn capture_window_screenshot_impl(_window: tauri::Window) -> Result<Vec<u8>, String> {
    Err("Screenshot capture not available on Linux".to_string())
}
