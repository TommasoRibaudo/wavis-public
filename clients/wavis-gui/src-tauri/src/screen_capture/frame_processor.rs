//! Frame processing utilities for the screen capture pipeline.
//!
//! Provides configurable resolution capping, frame rate throttling
//! (timestamp-based, thread-safe), and a runtime-adjustable quality
//! configuration shared between the capture pipeline and the frontend.

use std::sync::atomic::{AtomicU32, AtomicU8, Ordering};
use std::sync::Mutex;
use std::time::{Duration, Instant};

use super::CapturedFrame;

/// Default maximum output width after resolution capping.
const DEFAULT_MAX_WIDTH: u32 = 1920;
/// Default maximum output height after resolution capping.
const DEFAULT_MAX_HEIGHT: u32 = 1080;
/// Default maximum frame rate for the capture pipeline.
const DEFAULT_MAX_FPS: u32 = 15;
/// Default JPEG quality for viewer-side frame encoding (0–100).
const DEFAULT_JPEG_QUALITY: u8 = 75;

// ─── Runtime Screen Share Quality Configuration ────────────────────

/// Shared, runtime-adjustable screen share quality configuration.
///
/// All fields use atomics so the capture callback (hot path) can read
/// without locking, and the Tauri command thread can write without
/// blocking frame delivery.
pub struct ScreenShareConfig {
    max_width: AtomicU32,
    max_height: AtomicU32,
    max_fps: AtomicU32,
    jpeg_quality: AtomicU8,
}

impl ScreenShareConfig {
    /// Create a new config with default values (1920×1080 @ 15fps, JPEG 75).
    pub fn new() -> Self {
        Self {
            max_width: AtomicU32::new(DEFAULT_MAX_WIDTH),
            max_height: AtomicU32::new(DEFAULT_MAX_HEIGHT),
            max_fps: AtomicU32::new(DEFAULT_MAX_FPS),
            jpeg_quality: AtomicU8::new(DEFAULT_JPEG_QUALITY),
        }
    }

    pub fn max_width(&self) -> u32 {
        self.max_width.load(Ordering::Relaxed)
    }

    pub fn max_height(&self) -> u32 {
        self.max_height.load(Ordering::Relaxed)
    }

    pub fn max_fps(&self) -> u32 {
        self.max_fps.load(Ordering::Relaxed)
    }

    pub fn jpeg_quality(&self) -> u8 {
        self.jpeg_quality.load(Ordering::Relaxed)
    }

    /// Apply a named quality preset. Returns `Err` if the name is unknown.
    pub fn apply_preset(&self, preset: &str) -> Result<(), String> {
        match preset {
            "low" => {
                self.max_width.store(1920, Ordering::Relaxed);
                self.max_height.store(1080, Ordering::Relaxed);
                self.max_fps.store(30, Ordering::Relaxed);
                self.jpeg_quality.store(85, Ordering::Relaxed);
            }
            "high" => {
                self.max_width.store(2560, Ordering::Relaxed);
                self.max_height.store(1440, Ordering::Relaxed);
                self.max_fps.store(30, Ordering::Relaxed);
                self.jpeg_quality.store(92, Ordering::Relaxed);
            }
            "max" => {
                self.max_width.store(2560, Ordering::Relaxed);
                self.max_height.store(1440, Ordering::Relaxed);
                self.max_fps.store(60, Ordering::Relaxed);
                self.jpeg_quality.store(95, Ordering::Relaxed);
            }
            _ => return Err(format!("unknown quality preset: {preset}")),
        }
        log::info!(
            "screen share quality preset applied: {preset} ({}x{} @ {}fps, jpeg={})",
            self.max_width(),
            self.max_height(),
            self.max_fps(),
            self.jpeg_quality(),
        );
        Ok(())
    }
}

impl Default for ScreenShareConfig {
    fn default() -> Self {
        Self::new()
    }
}

/// Cap frame resolution to the configured maximum, maintaining aspect ratio.
///
/// If the frame is already within bounds, it is returned unchanged (no copy).
/// Downscaling uses bilinear interpolation over RGBA pixels.
pub fn cap_resolution(frame: CapturedFrame, max_width: u32, max_height: u32) -> CapturedFrame {
    if frame.width <= max_width && frame.height <= max_height {
        return frame;
    }

    // Compute the scale factor that fits both dimensions within the cap.
    let scale_x = max_width as f64 / frame.width as f64;
    let scale_y = max_height as f64 / frame.height as f64;
    let scale = scale_x.min(scale_y);

    let new_width = ((frame.width as f64 * scale).round() as u32).max(1);
    let new_height = ((frame.height as f64 * scale).round() as u32).max(1);

    let src_w = frame.width as usize;
    let src_h = frame.height as usize;
    let dst_w = new_width as usize;
    let dst_h = new_height as usize;

    let mut out = vec![0u8; dst_w * dst_h * 4];

    for dst_y in 0..dst_h {
        // Map destination pixel center back to source coordinates.
        let src_yf = (dst_y as f64 + 0.5) * (src_h as f64 / dst_h as f64) - 0.5;
        let sy0 = (src_yf.floor() as isize).max(0) as usize;
        let sy1 = (sy0 + 1).min(src_h - 1);
        let fy = (src_yf - sy0 as f64).max(0.0);

        for dst_x in 0..dst_w {
            let src_xf = (dst_x as f64 + 0.5) * (src_w as f64 / dst_w as f64) - 0.5;
            let sx0 = (src_xf.floor() as isize).max(0) as usize;
            let sx1 = (sx0 + 1).min(src_w - 1);
            let fx = (src_xf - sx0 as f64).max(0.0);

            // Bilinear interpolation over 4 source pixels.
            let idx = |x: usize, y: usize, c: usize| -> u8 { frame.data[(y * src_w + x) * 4 + c] };

            let dst_idx = (dst_y * dst_w + dst_x) * 4;
            for c in 0..4 {
                let top = idx(sx0, sy0, c) as f64 * (1.0 - fx) + idx(sx1, sy0, c) as f64 * fx;
                let bot = idx(sx0, sy1, c) as f64 * (1.0 - fx) + idx(sx1, sy1, c) as f64 * fx;
                let val = top * (1.0 - fy) + bot * fy;
                out[dst_idx + c] = val.round().clamp(0.0, 255.0) as u8;
            }
        }
    }

    CapturedFrame {
        width: new_width,
        height: new_height,
        data: out,
        timestamp_ms: frame.timestamp_ms,
    }
}

/// Returns `true` if the `WAVIS_DEBUG_SCREEN_CAPTURE` environment variable
/// is set to `"true"` (case-sensitive). Intended to be called once at capture
/// start and stored for the duration of the session.
#[allow(dead_code)]
pub fn is_debug_capture_enabled() -> bool {
    std::env::var("WAVIS_DEBUG_SCREEN_CAPTURE")
        .map(|v| v == "true")
        .unwrap_or(false)
}

/// Frame rate throttler that enforces a minimum interval between emitted frames.
///
/// Thread-safe — designed to be called from capture callback threads.
/// Uses timestamp-based throttling: tracks the last emitted frame time and
/// skips frames that arrive too soon.
///
/// Supports runtime FPS changes via `set_fps()` — the new interval takes
/// effect on the next `should_emit()` call without dropping frames.
pub struct FrameThrottler {
    /// Current minimum interval between frames, stored as nanoseconds in an
    /// atomic so `set_fps()` can update it without locking.
    min_interval_ns: AtomicU64,
    last_frame: Mutex<Option<Instant>>,
}

use std::sync::atomic::AtomicU64;

impl FrameThrottler {
    /// Create a new throttler with the given maximum frame rate.
    ///
    /// `max_fps` is clamped to at least 1 to avoid division by zero.
    pub fn new(max_fps: u32) -> Self {
        let fps = max_fps.max(1);
        Self {
            min_interval_ns: AtomicU64::new(1_000_000_000 / fps as u64),
            last_frame: Mutex::new(None),
        }
    }

    /// Update the target frame rate at runtime.
    ///
    /// `max_fps` is clamped to at least 1. Takes effect on the next
    /// `should_emit()` call.
    pub fn set_fps(&self, max_fps: u32) {
        let fps = max_fps.max(1);
        self.min_interval_ns
            .store(1_000_000_000 / fps as u64, Ordering::Relaxed);
    }

    /// Returns `true` if enough time has passed since the last emitted frame,
    /// and records the current instant as the last emission time.
    ///
    /// The first call always returns `true`.
    pub fn should_emit(&self) -> bool {
        let min_interval = Duration::from_nanos(self.min_interval_ns.load(Ordering::Relaxed));
        let now = Instant::now();
        let mut guard = self.last_frame.lock().unwrap();
        match *guard {
            None => {
                *guard = Some(now);
                true
            }
            Some(last) => {
                if now.duration_since(last) >= min_interval {
                    *guard = Some(now);
                    true
                } else {
                    false
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Default cap dimensions used by tests (matches the "high" preset).
    const TEST_MAX_W: u32 = 2560;
    const TEST_MAX_H: u32 = 1440;

    /// Helper: create a CapturedFrame with the given dimensions filled with
    /// a repeating RGBA pattern.
    fn make_frame(width: u32, height: u32) -> CapturedFrame {
        let pixel_count = (width * height) as usize;
        let mut data = Vec::with_capacity(pixel_count * 4);
        for i in 0..pixel_count {
            let v = (i % 256) as u8;
            data.extend_from_slice(&[v, v, v, 255]);
        }
        CapturedFrame {
            width,
            height,
            data,
            timestamp_ms: 1000,
        }
    }

    // ─── Resolution capping tests ──────────────────────────────────

    #[test]
    fn passthrough_when_within_bounds() {
        let frame = make_frame(1920, 1080);
        let out = cap_resolution(frame, TEST_MAX_W, TEST_MAX_H);
        assert_eq!(out.width, 1920);
        assert_eq!(out.height, 1080);
    }

    #[test]
    fn passthrough_at_max_bounds() {
        let frame = make_frame(2560, 1440);
        let out = cap_resolution(frame, TEST_MAX_W, TEST_MAX_H);
        assert_eq!(out.width, 2560);
        assert_eq!(out.height, 1440);
    }

    #[test]
    fn passthrough_small_frame() {
        let frame = make_frame(640, 480);
        let out = cap_resolution(frame, TEST_MAX_W, TEST_MAX_H);
        assert_eq!(out.width, 640);
        assert_eq!(out.height, 480);
    }

    #[test]
    fn downscale_4k_landscape() {
        // 3840×2160 (4K) should scale down to fit within 2560×1440.
        let frame = make_frame(3840, 2160);
        let out = cap_resolution(frame, TEST_MAX_W, TEST_MAX_H);
        assert!(
            out.width <= TEST_MAX_W,
            "width {} > {}",
            out.width,
            TEST_MAX_W
        );
        assert!(
            out.height <= TEST_MAX_H,
            "height {} > {}",
            out.height,
            TEST_MAX_H
        );
        assert_eq!(out.data.len(), (out.width * out.height * 4) as usize);
    }

    #[test]
    fn downscale_4k_with_1080p_cap() {
        // Test with the old 1080p cap to verify parameterization works.
        let frame = make_frame(3840, 2160);
        let out = cap_resolution(frame, 1920, 1080);
        assert_eq!(out.width, 1920);
        assert_eq!(out.height, 1080);
        assert_eq!(out.data.len(), (out.width * out.height * 4) as usize);
    }

    #[test]
    fn downscale_ultrawide() {
        // 3440×1440 ultrawide — width is the limiting factor.
        let frame = make_frame(3440, 1440);
        let out = cap_resolution(frame, TEST_MAX_W, TEST_MAX_H);
        assert!(out.width <= TEST_MAX_W);
        assert!(out.height <= TEST_MAX_H);
        assert_eq!(out.data.len(), (out.width * out.height * 4) as usize);
    }

    #[test]
    fn downscale_tall_portrait() {
        // 1080×2400 portrait — height is the limiting factor.
        let frame = make_frame(1080, 2400);
        let out = cap_resolution(frame, TEST_MAX_W, TEST_MAX_H);
        assert!(out.width <= TEST_MAX_W);
        assert!(out.height <= TEST_MAX_H);
        assert_eq!(out.data.len(), (out.width * out.height * 4) as usize);
    }

    #[test]
    fn downscale_only_width_exceeds() {
        // 3000×1080 — only width exceeds.
        let frame = make_frame(3000, 1080);
        let out = cap_resolution(frame, TEST_MAX_W, TEST_MAX_H);
        assert!(out.width <= TEST_MAX_W);
        assert!(out.height <= TEST_MAX_H);
        assert_eq!(out.data.len(), (out.width * out.height * 4) as usize);
    }

    #[test]
    fn downscale_only_height_exceeds() {
        // 2560×2000 — only height exceeds.
        let frame = make_frame(2560, 2000);
        let out = cap_resolution(frame, TEST_MAX_W, TEST_MAX_H);
        assert!(out.width <= TEST_MAX_W);
        assert!(out.height <= TEST_MAX_H);
        assert_eq!(out.data.len(), (out.width * out.height * 4) as usize);
    }

    #[test]
    fn preserves_timestamp() {
        let mut frame = make_frame(3840, 2160);
        frame.timestamp_ms = 42;
        let out = cap_resolution(frame, TEST_MAX_W, TEST_MAX_H);
        assert_eq!(out.timestamp_ms, 42);
    }

    #[test]
    fn minimum_1x1_output() {
        // Extremely large aspect ratio — ensure we never produce 0-dimension output.
        let frame = make_frame(100_000, 1);
        let out = cap_resolution(frame, TEST_MAX_W, TEST_MAX_H);
        assert!(out.width >= 1);
        assert!(out.height >= 1);
        assert!(out.width <= TEST_MAX_W);
        assert!(out.height <= TEST_MAX_H);
    }

    // ─── ScreenShareConfig tests ───────────────────────────────────

    #[test]
    fn config_defaults() {
        let cfg = ScreenShareConfig::new();
        assert_eq!(cfg.max_width(), 1920);
        assert_eq!(cfg.max_height(), 1080);
        assert_eq!(cfg.max_fps(), 15);
        assert_eq!(cfg.jpeg_quality(), 75);
    }

    #[test]
    fn config_apply_low_preset() {
        let cfg = ScreenShareConfig::new();
        cfg.apply_preset("low").unwrap();
        assert_eq!(cfg.max_width(), 1920);
        assert_eq!(cfg.max_height(), 1080);
        assert_eq!(cfg.max_fps(), 30);
        assert_eq!(cfg.jpeg_quality(), 85);
    }

    #[test]
    fn config_apply_max_preset() {
        let cfg = ScreenShareConfig::new();
        cfg.apply_preset("max").unwrap();
        assert_eq!(cfg.max_width(), 2560);
        assert_eq!(cfg.max_height(), 1440);
        assert_eq!(cfg.max_fps(), 60);
        assert_eq!(cfg.jpeg_quality(), 95);
    }

    #[test]
    fn config_unknown_preset_errors() {
        let cfg = ScreenShareConfig::new();
        assert!(cfg.apply_preset("ultra").is_err());
    }

    // ─── Frame throttler tests ─────────────────────────────────────

    #[test]
    fn first_frame_always_emitted() {
        let throttler = FrameThrottler::new(30);
        assert!(throttler.should_emit());
    }

    #[test]
    fn rapid_calls_throttled() {
        let throttler = FrameThrottler::new(30);
        assert!(throttler.should_emit()); // first frame
                                          // Immediately calling again should be throttled (< 33ms).
        assert!(!throttler.should_emit());
    }

    #[test]
    fn emits_after_interval() {
        let throttler = FrameThrottler::new(15); // ~66ms interval
        assert!(throttler.should_emit());
        // Sleep past the interval.
        std::thread::sleep(Duration::from_millis(70));
        assert!(throttler.should_emit());
    }

    #[test]
    fn zero_fps_clamped_to_one() {
        // 0 fps should not panic — clamped to 1 fps.
        let throttler = FrameThrottler::new(0);
        assert!(throttler.should_emit());
        // 1 fps = 1000ms interval, so immediate second call is throttled.
        assert!(!throttler.should_emit());
    }

    #[test]
    fn set_fps_changes_interval() {
        let throttler = FrameThrottler::new(1); // 1 fps = 1000ms interval
        assert!(throttler.should_emit());
        assert!(!throttler.should_emit()); // too soon

        // Switch to 1000 fps — effectively no throttle.
        throttler.set_fps(1000);
        std::thread::sleep(Duration::from_millis(2));
        assert!(throttler.should_emit());
    }
}
