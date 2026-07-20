//! PipeWire/xdg-desktop-portal screen capture backend (primary path).
//!
//! Spawns `pw_capture_helper` as a subprocess to avoid PipeWire/WebKitGTK
//! conflicts in the main Tauri process. The helper runs the portal session
//! (native source picker) and PipeWire capture loop, streaming JPEG-encoded
//! frames back via stdout.
//!
//! Portal availability is detected at runtime via D-Bus — no compile-time gating.
//! When the portal is unavailable (e.g., Wayland compositor without
//! xdg-desktop-portal), the helper exits with an error code and we return
//! [`CaptureError::PortalUnavailable`] so the fallback chain can try X11.

use std::collections::VecDeque;
use std::io::Read;
use std::process::{Child, Command, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Condvar, Mutex};

use super::{CaptureError, CapturedFrame, ScreenCapture};

type FrameCallback = Arc<Mutex<Option<Box<dyn Fn(CapturedFrame) + Send + 'static>>>>;

struct EncodedFrame {
    width: u32,
    height: u32,
    jpeg_data: Vec<u8>,
}

/// Maximum accepted JPEG payload size in a `pw_capture_helper` frame header —
/// guards against a corrupted stream (desynced framing) causing an
/// unbounded allocation.
const MAX_JPEG_LEN: u32 = 50_000_000;

/// Decoded fields of the `pw_capture_helper` wire protocol's 12-byte frame
/// header: `u32 LE width | u32 LE height | u32 LE jpeg_len` (see
/// `bin/pw_capture_helper.rs`, which writes this exact layout to stdout).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct FrameHeader {
    width: u32,
    height: u32,
    jpeg_len: u32,
}

impl FrameHeader {
    /// A header is valid only if every dimension/length is non-zero and the
    /// JPEG length is within the sanity ceiling. All-zero or oversized
    /// values indicate a desynced stream, not a real frame.
    fn is_valid(&self) -> bool {
        self.width != 0 && self.height != 0 && self.jpeg_len != 0 && self.jpeg_len <= MAX_JPEG_LEN
    }
}

/// Decode (without validating) the 12-byte frame header.
fn decode_frame_header(header: &[u8; 12]) -> FrameHeader {
    FrameHeader {
        width: u32::from_le_bytes([header[0], header[1], header[2], header[3]]),
        height: u32::from_le_bytes([header[4], header[5], header[6], header[7]]),
        jpeg_len: u32::from_le_bytes([header[8], header[9], header[10], header[11]]),
    }
}

/// PipeWire/portal-based screen capture backend.
///
/// Runs the PipeWire capture in a subprocess (`pw_capture_helper`) to avoid
/// segfaults caused by PipeWire/WebKitGTK library conflicts in the main
/// Tauri process.
pub struct PipeWireCapture {
    active: Arc<AtomicBool>,
    frame_callback: FrameCallback,
    requested_max_width: u32,
    requested_max_height: u32,
    requested_max_fps: u32,
    pending_frames: Arc<(Mutex<VecDeque<EncodedFrame>>, Condvar)>,
    /// Handle to the frame reader thread.
    reader_thread: Mutex<Option<std::thread::JoinHandle<()>>>,
    /// Handle to the frame processor thread.
    processor_thread: Mutex<Option<std::thread::JoinHandle<()>>>,
    /// Handle to the helper subprocess — kept alive while capturing.
    child: Mutex<Option<Child>>,
}

impl PipeWireCapture {
    pub fn new(
        requested_max_width: u32,
        requested_max_height: u32,
        requested_max_fps: u32,
    ) -> Self {
        Self {
            active: Arc::new(AtomicBool::new(false)),
            frame_callback: Arc::new(Mutex::new(None)),
            requested_max_width: requested_max_width.max(1),
            requested_max_height: requested_max_height.max(1),
            requested_max_fps: requested_max_fps.max(1),
            pending_frames: Arc::new((Mutex::new(VecDeque::with_capacity(1)), Condvar::new())),
            reader_thread: Mutex::new(None),
            processor_thread: Mutex::new(None),
            child: Mutex::new(None),
        }
    }

    /// Find the pw_capture_helper binary next to the current executable.
    fn helper_path() -> Result<std::path::PathBuf, CaptureError> {
        let exe = std::env::current_exe().map_err(|e| {
            CaptureError::CaptureStartFailed(format!("cannot determine current exe: {e}"))
        })?;
        let dir = exe.parent().ok_or_else(|| {
            CaptureError::CaptureStartFailed("cannot determine exe directory".into())
        })?;
        let helper = dir.join("pw_capture_helper");
        if helper.exists() {
            Ok(helper)
        } else {
            Err(CaptureError::CaptureStartFailed(format!(
                "pw_capture_helper not found at {}",
                helper.display()
            )))
        }
    }

    /// Spawn the helper subprocess, wait for the READY signal (portal
    /// session completed), then start reading frames.
    fn spawn_helper(&self) -> Result<(), CaptureError> {
        let helper_path = Self::helper_path()?;
        eprintln!(
            "wavis: pipewire_capture: spawning helper: {}",
            helper_path.display()
        );
        log::info!(
            "pipewire_capture: spawning helper subprocess: {}",
            helper_path.display()
        );

        let mut child = Command::new(&helper_path)
            .arg(self.requested_max_fps.to_string())
            .arg(self.requested_max_width.to_string())
            .arg(self.requested_max_height.to_string())
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::inherit()) // helper logs go to app stderr
            .spawn()
            .map_err(|e| {
                CaptureError::CaptureStartFailed(format!("failed to spawn pw_capture_helper: {e}"))
            })?;

        let mut stdout = child.stdout.take().ok_or_else(|| {
            CaptureError::CaptureStartFailed("failed to capture helper stdout".into())
        })?;

        // Wait for the READY signal from the helper. This blocks until the
        // user selects a source in the portal picker or cancels.
        let mut ready_buf = [0u8; 6]; // "READY\n"
        match stdout.read_exact(&mut ready_buf) {
            Ok(()) if &ready_buf == b"READY\n" => {
                eprintln!("wavis: pipewire_capture: helper signalled READY");
            }
            Ok(()) => {
                // Unexpected data — helper might have crashed.
                let _ = child.kill();
                let _ = child.wait();
                return Err(CaptureError::CaptureStartFailed(
                    "unexpected data from helper (expected READY)".into(),
                ));
            }
            Err(_) => {
                // Helper exited before READY — check exit code.
                let status = child.wait().ok();
                let code = status.and_then(|s| s.code());
                if code == Some(2) {
                    return Err(CaptureError::UserCancelled);
                }
                return Err(CaptureError::PortalUnavailable(format!(
                    "pw_capture_helper exited before ready (code: {code:?})"
                )));
            }
        }

        // Store child process handle.
        *self
            .child
            .lock()
            .map_err(|e| CaptureError::CaptureStartFailed(format!("child lock poisoned: {e}")))? =
            Some(child);

        // Spawn reader thread that reads frames from the helper's stdout.
        let active = self.active.clone();
        let frame_cb = self.frame_callback.clone();
        let pending_frames = Arc::clone(&self.pending_frames);

        let handle = std::thread::Builder::new()
            .name("pw-capture-reader".into())
            .spawn(move || {
                Self::read_frames(stdout, active, pending_frames);
            })
            .map_err(|e| {
                CaptureError::CaptureStartFailed(format!("failed to spawn reader thread: {e}"))
            })?;

        *self.reader_thread.lock().map_err(|e| {
            CaptureError::CaptureStartFailed(format!("reader_thread lock poisoned: {e}"))
        })? = Some(handle);

        let active = self.active.clone();
        let pending_frames = Arc::clone(&self.pending_frames);
        let handle = std::thread::Builder::new()
            .name("pw-capture-processor".into())
            .spawn(move || {
                Self::process_frames(active, pending_frames, frame_cb);
            })
            .map_err(|e| {
                CaptureError::CaptureStartFailed(format!("failed to spawn processor thread: {e}"))
            })?;

        *self.processor_thread.lock().map_err(|e| {
            CaptureError::CaptureStartFailed(format!("processor_thread lock poisoned: {e}"))
        })? = Some(handle);

        Ok(())
    }

    /// Read length-prefixed JPEG frames from the helper's stdout and deliver
    /// them via the registered frame callback.
    fn read_frames(
        mut stdout: std::process::ChildStdout,
        active: Arc<AtomicBool>,
        pending_frames: Arc<(Mutex<VecDeque<EncodedFrame>>, Condvar)>,
    ) {
        let mut header = [0u8; 12]; // width(4) + height(4) + jpeg_len(4)
        let mut frame_count: u64 = 0;

        loop {
            if !active.load(Ordering::SeqCst) {
                break;
            }

            // Read frame header.
            if stdout.read_exact(&mut header).is_err() {
                // Helper exited or pipe broken.
                eprintln!(
                    "wavis: pipewire_capture: helper stdout closed (after {frame_count} frames)"
                );
                active.store(false, Ordering::SeqCst);
                break;
            }

            frame_count += 1;
            let parsed = decode_frame_header(&header);

            // Sanity checks.
            if !parsed.is_valid() {
                eprintln!(
                    "wavis: pipewire_capture: frame #{frame_count}: invalid header: {}x{} jpeg_len={}",
                    parsed.width, parsed.height, parsed.jpeg_len
                );
                active.store(false, Ordering::SeqCst);
                break;
            }
            let FrameHeader {
                width,
                height,
                jpeg_len,
            } = parsed;

            if frame_count <= 3 || frame_count.is_multiple_of(300) {
                eprintln!(
                    "wavis: pipewire_capture: frame #{frame_count}: received {width}x{height} jpeg_len={jpeg_len}"
                );
            }

            // Read JPEG data.
            let mut jpeg_data = vec![0u8; jpeg_len as usize];
            if stdout.read_exact(&mut jpeg_data).is_err() {
                eprintln!(
                    "wavis: pipewire_capture: frame #{frame_count}: failed to read JPEG data"
                );
                active.store(false, Ordering::SeqCst);
                break;
            }

            let (queue_lock, queue_cv) = &*pending_frames;
            if let Ok(mut queue) = queue_lock.lock() {
                // Keep only the newest frame so helper stdout is drained even
                // if downstream decode/feed work is slower than capture.
                queue.clear();
                queue.push_back(EncodedFrame {
                    width,
                    height,
                    jpeg_data,
                });
                queue_cv.notify_one();
            }
        }

        eprintln!("wavis: pipewire_capture: reader thread exiting (total frames: {frame_count})");
    }

    fn process_frames(
        active: Arc<AtomicBool>,
        pending_frames: Arc<(Mutex<VecDeque<EncodedFrame>>, Condvar)>,
        frame_cb: FrameCallback,
    ) {
        let mut processed_count: u64 = 0;

        loop {
            let encoded = {
                let (queue_lock, queue_cv) = &*pending_frames;
                let mut queue = match queue_lock.lock() {
                    Ok(guard) => guard,
                    Err(_) => break,
                };

                while active.load(Ordering::SeqCst) && queue.is_empty() {
                    match queue_cv.wait(queue) {
                        Ok(guard) => queue = guard,
                        Err(_) => return,
                    }
                }

                if !active.load(Ordering::SeqCst) && queue.is_empty() {
                    break;
                }

                queue.pop_front()
            };

            let Some(encoded) = encoded else {
                continue;
            };

            processed_count += 1;
            let rgba = match decode_jpeg_to_rgba(&encoded.jpeg_data, encoded.width, encoded.height)
            {
                Some(data) => data,
                None => {
                    eprintln!(
                        "wavis: pipewire_capture: frame #{processed_count}: JPEG decode failed"
                    );
                    continue;
                }
            };

            let timestamp_ms = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_millis() as u64)
                .unwrap_or(0);

            let frame = CapturedFrame {
                width: encoded.width,
                height: encoded.height,
                data: rgba,
                timestamp_ms,
            };

            if let Ok(guard) = frame_cb.lock() {
                if let Some(cb) = guard.as_ref() {
                    cb(frame);
                }
            }
        }

        eprintln!(
            "wavis: pipewire_capture: processor thread exiting (total frames: {processed_count})"
        );
    }
}

/// Decode JPEG bytes to RGBA pixel data.
fn decode_jpeg_to_rgba(jpeg_data: &[u8], expected_w: u32, expected_h: u32) -> Option<Vec<u8>> {
    use image::ImageReader;
    use std::io::Cursor;

    let reader = ImageReader::new(Cursor::new(jpeg_data))
        .with_guessed_format()
        .ok()?;
    let img = reader.decode().ok()?;
    let rgba_img = img.to_rgba8();

    if rgba_img.width() != expected_w || rgba_img.height() != expected_h {
        eprintln!(
            "wavis: pipewire_capture: JPEG dimensions mismatch: expected {}x{}, got {}x{}",
            expected_w,
            expected_h,
            rgba_img.width(),
            rgba_img.height()
        );
    }

    Some(rgba_img.into_raw())
}

impl ScreenCapture for PipeWireCapture {
    fn start(&self) -> Result<(), CaptureError> {
        if self.active.load(Ordering::SeqCst) {
            return Ok(());
        }

        eprintln!("wavis: pipewire_capture: start (subprocess mode)");
        log::info!("pipewire_capture: starting via subprocess");

        self.active.store(true, Ordering::SeqCst);

        // spawn_helper() blocks until the portal picker completes (user
        // selects a source) or the helper exits (cancel/error).
        if let Err(e) = self.spawn_helper() {
            self.active.store(false, Ordering::SeqCst);
            return Err(e);
        }

        log::info!("pipewire_capture: helper subprocess ready, capture started");
        Ok(())
    }

    fn stop(&self) {
        if !self.active.load(Ordering::SeqCst) {
            return;
        }

        self.active.store(false, Ordering::SeqCst);
        {
            let (queue_lock, queue_cv) = &*self.pending_frames;
            if let Ok(mut queue) = queue_lock.lock() {
                queue.clear();
            }
            queue_cv.notify_all();
        }
        eprintln!("wavis: pipewire_capture: stopping helper subprocess");

        // Close the child's stdin — this signals the helper to exit.
        if let Ok(mut guard) = self.child.lock() {
            if let Some(mut child) = guard.take() {
                // Drop stdin to signal the helper.
                drop(child.stdin.take());

                // Give it a moment to exit gracefully.
                match child.wait_timeout(std::time::Duration::from_secs(3)) {
                    Ok(Some(_)) => {}
                    _ => {
                        eprintln!("wavis: pipewire_capture: killing helper");
                        let _ = child.kill();
                        let _ = child.wait();
                    }
                }
            }
        }

        // Wait for the reader thread.
        if let Ok(mut guard) = self.reader_thread.lock() {
            if let Some(handle) = guard.take() {
                let _ = handle.join();
            }
        }

        if let Ok(mut guard) = self.processor_thread.lock() {
            if let Some(handle) = guard.take() {
                let _ = handle.join();
            }
        }

        log::info!("pipewire_capture: helper subprocess stopped");
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

/// Extension trait for `Child` to add `wait_timeout`.
trait ChildExt {
    fn wait_timeout(
        &mut self,
        timeout: std::time::Duration,
    ) -> std::io::Result<Option<std::process::ExitStatus>>;
}

impl ChildExt for Child {
    fn wait_timeout(
        &mut self,
        timeout: std::time::Duration,
    ) -> std::io::Result<Option<std::process::ExitStatus>> {
        let start = std::time::Instant::now();
        loop {
            match self.try_wait()? {
                Some(status) => return Ok(Some(status)),
                None => {
                    if start.elapsed() >= timeout {
                        return Ok(None);
                    }
                    std::thread::sleep(std::time::Duration::from_millis(50));
                }
            }
        }
    }
}

// P2-7: pw_capture_helper wire protocol framing (see `bin/pw_capture_helper.rs`
// header comment: `u32 LE width | u32 LE height | u32 LE jpeg_len | jpeg bytes`).
#[cfg(test)]
mod tests {
    use super::*;

    /// Encode a header exactly as `pw_capture_helper` does: three little-endian
    /// u32s written back to back (see bin/pw_capture_helper.rs ~line 891:
    /// `w.to_le_bytes()` then `h.to_le_bytes()` then `len.to_le_bytes()`).
    fn encode_header(width: u32, height: u32, jpeg_len: u32) -> [u8; 12] {
        let mut buf = [0u8; 12];
        buf[0..4].copy_from_slice(&width.to_le_bytes());
        buf[4..8].copy_from_slice(&height.to_le_bytes());
        buf[8..12].copy_from_slice(&jpeg_len.to_le_bytes());
        buf
    }

    #[test]
    fn decode_frame_header_round_trips_pw_capture_helper_encoding() {
        let header_bytes = encode_header(1920, 1080, 123_456);
        let decoded = decode_frame_header(&header_bytes);
        assert_eq!(
            decoded,
            FrameHeader {
                width: 1920,
                height: 1080,
                jpeg_len: 123_456,
            }
        );
    }

    #[test]
    fn frame_header_rejects_zero_width() {
        assert!(!decode_frame_header(&encode_header(0, 1080, 1000)).is_valid());
    }

    #[test]
    fn frame_header_rejects_zero_height() {
        assert!(!decode_frame_header(&encode_header(1920, 0, 1000)).is_valid());
    }

    #[test]
    fn frame_header_rejects_zero_jpeg_len() {
        assert!(!decode_frame_header(&encode_header(1920, 1080, 0)).is_valid());
    }

    #[test]
    fn frame_header_rejects_jpeg_len_over_sanity_ceiling() {
        assert!(!decode_frame_header(&encode_header(1920, 1080, MAX_JPEG_LEN + 1)).is_valid());
    }

    #[test]
    fn frame_header_accepts_jpeg_len_at_sanity_ceiling() {
        assert!(decode_frame_header(&encode_header(1920, 1080, MAX_JPEG_LEN)).is_valid());
    }

    #[test]
    fn frame_header_accepts_typical_1080p_frame() {
        assert!(decode_frame_header(&encode_header(1920, 1080, 200_000)).is_valid());
    }
}
