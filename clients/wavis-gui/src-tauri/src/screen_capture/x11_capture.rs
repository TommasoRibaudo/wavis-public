//! X11/XWayland fallback screen capture backend.
//!
//! Uses `x11rb` for XGetImage-based full-screen capture of the primary display.
//! Activated only when PipeWire/portal is unavailable (runtime detection).
//! On Wayland with XWayland running, X11 capture via XWayland is attempted —
//! `DISPLAY` env var presence detects X11 availability regardless of whether
//! the session is native X11 or XWayland.
//!
//! Captures the full primary display (no source picker). Lower priority than
//! PipeWire/portal — this is the fallback path.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use x11rb::connection::Connection;
use x11rb::protocol::xproto::{self, ConnectionExt as _, ImageFormat};

use super::{CaptureError, CapturedFrame, ScreenCapture};

/// Default capture interval targeting ~30 fps.
const DEFAULT_FRAME_INTERVAL: Duration = Duration::from_millis(33);
type FrameCallback = Arc<Mutex<Option<Box<dyn Fn(CapturedFrame) + Send + 'static>>>>;

/// X11/XWayland fallback screen capture backend.
///
/// Connects to the X11 display (native or XWayland) and captures the full
/// primary screen via `GetImage` (ZPixmap format). Frames are converted from
/// the X11 pixel format (typically BGRx) to RGBA before delivery.
pub struct X11Capture {
    active: Arc<AtomicBool>,
    frame_callback: FrameCallback,
    /// Handle to the capture thread — kept alive while capturing.
    capture_thread: Mutex<Option<std::thread::JoinHandle<()>>>,
}

impl X11Capture {
    /// Create a new X11 capture backend.
    pub fn new() -> Self {
        Self {
            active: Arc::new(AtomicBool::new(false)),
            frame_callback: Arc::new(Mutex::new(None)),
            capture_thread: Mutex::new(None),
        }
    }

    /// Check whether X11 is available by inspecting the `DISPLAY` env var.
    fn check_display() -> Result<(), CaptureError> {
        match std::env::var("DISPLAY") {
            Ok(val) if !val.is_empty() => Ok(()),
            _ => Err(CaptureError::X11Unavailable(
                "DISPLAY environment variable is not set — X11/XWayland not available".into(),
            )),
        }
    }

    /// The capture loop body — runs on a dedicated thread.
    ///
    /// Connects to the X11 display, gets root window geometry, and captures
    /// frames at the configured interval until the active flag is cleared.
    fn capture_loop(active: Arc<AtomicBool>, frame_cb: FrameCallback) {
        // Connect to the X11 display.
        let (conn, screen_num) = match x11rb::connect(None) {
            Ok(result) => result,
            Err(e) => {
                log::error!("X11 connect failed: {e}");
                active.store(false, Ordering::SeqCst);
                return;
            }
        };

        let screen = &conn.setup().roots[screen_num];
        let root = screen.root;
        let width = screen.width_in_pixels;
        let height = screen.height_in_pixels;

        log::info!(
            "X11 capture: connected to display, root window {}x{}",
            width,
            height
        );

        // Capture loop — grab frames at the configured interval.
        while active.load(Ordering::SeqCst) {
            let frame_start = Instant::now();

            match Self::capture_frame(&conn, root, width, height) {
                Ok(frame) => {
                    if let Ok(guard) = frame_cb.lock() {
                        if let Some(cb) = guard.as_ref() {
                            cb(frame);
                        }
                    }
                }
                Err(e) => {
                    log::warn!("X11 frame capture failed: {e}");
                    // Continue trying — transient failures are possible.
                }
            }

            // Sleep for the remainder of the frame interval.
            let elapsed = frame_start.elapsed();
            if elapsed < DEFAULT_FRAME_INTERVAL {
                std::thread::sleep(DEFAULT_FRAME_INTERVAL - elapsed);
            }
        }

        log::info!("X11 capture loop exited");
    }

    /// Capture a single frame from the root window via `GetImage` (ZPixmap).
    ///
    /// Returns the frame as RGBA pixel data. X11 typically delivers pixels in
    /// BGRx format (32 bits per pixel, ZPixmap) — we convert to RGBA.
    fn capture_frame(
        conn: &impl Connection,
        root: xproto::Window,
        width: u16,
        height: u16,
    ) -> Result<CapturedFrame, CaptureError> {
        let reply = conn
            .get_image(
                ImageFormat::Z_PIXMAP,
                root,
                0,
                0,
                width,
                height,
                !0, // all planes
            )
            .map_err(|e| CaptureError::CaptureStartFailed(format!("GetImage request failed: {e}")))?
            .reply()
            .map_err(|e| CaptureError::CaptureStartFailed(format!("GetImage reply failed: {e}")))?;

        let raw = &reply.data;
        let w = width as usize;
        let h = height as usize;
        let expected_size = w * h * 4;

        // Validate we got the expected amount of pixel data.
        if raw.len() < expected_size {
            return Err(CaptureError::CaptureStartFailed(format!(
                "GetImage returned {} bytes, expected {} ({}x{}x4)",
                raw.len(),
                expected_size,
                w,
                h
            )));
        }

        // Convert BGRx → RGBA. X11 ZPixmap with depth 24/32 is typically
        // laid out as B, G, R, X (or B, G, R, A) per pixel.
        let mut rgba = Vec::with_capacity(expected_size);
        for pixel in raw.chunks_exact(4) {
            rgba.push(pixel[2]); // R (was at offset 2 in BGRx)
            rgba.push(pixel[1]); // G
            rgba.push(pixel[0]); // B (was at offset 0 in BGRx)
            rgba.push(255); // A (force opaque)
        }

        let timestamp_ms = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_millis() as u64)
            .unwrap_or(0);

        Ok(CapturedFrame {
            width: width as u32,
            height: height as u32,
            data: rgba,
            timestamp_ms,
        })
    }
}

impl ScreenCapture for X11Capture {
    fn start(&self) -> Result<(), CaptureError> {
        if self.active.load(Ordering::SeqCst) {
            // Already capturing — idempotent.
            return Ok(());
        }

        // Check DISPLAY env var before attempting connection.
        Self::check_display()?;

        // Verify we can actually connect to the X11 display before spawning
        // the capture thread. This catches the case where DISPLAY is set but
        // the server is unreachable (e.g., stale env var).
        let (conn, screen_num) = x11rb::connect(None).map_err(|e| {
            CaptureError::X11Unavailable(format!("failed to connect to X11 display: {e}"))
        })?;

        // Verify root window geometry is sane.
        let screen = &conn.setup().roots[screen_num];
        if screen.width_in_pixels == 0 || screen.height_in_pixels == 0 {
            return Err(CaptureError::X11Unavailable(
                "X11 root window has zero dimensions".into(),
            ));
        }

        // Drop the validation connection — the capture thread opens its own.
        drop(conn);

        self.active.store(true, Ordering::SeqCst);

        let active = self.active.clone();
        let frame_cb = self.frame_callback.clone();

        let handle = std::thread::Builder::new()
            .name("x11-capture".into())
            .spawn(move || {
                Self::capture_loop(active, frame_cb);
            })
            .map_err(|e| {
                self.active.store(false, Ordering::SeqCst);
                CaptureError::CaptureStartFailed(format!("failed to spawn X11 capture thread: {e}"))
            })?;

        *self.capture_thread.lock().unwrap() = Some(handle);

        log::info!("X11 screen capture started");
        Ok(())
    }

    fn stop(&self) {
        if !self.active.load(Ordering::SeqCst) {
            return;
        }

        // Signal the capture loop to exit.
        self.active.store(false, Ordering::SeqCst);

        // Wait for the capture thread to finish.
        if let Ok(mut guard) = self.capture_thread.lock() {
            if let Some(handle) = guard.take() {
                let _ = handle.join();
            }
        }

        log::info!("X11 screen capture stopped");
    }

    fn is_active(&self) -> bool {
        self.active.load(Ordering::SeqCst)
    }

    fn on_frame(&self, cb: Box<dyn Fn(CapturedFrame) + Send + 'static>) {
        if let Ok(mut guard) = self.frame_callback.lock() {
            *guard = Some(cb);
        }
    }
}
