//! Video track management and frame processing for the LiveKit SFU path.
//!
//! Owns [`VideoState`] (grouped video fields held by value on `RealLiveKitConnection`),
//! `run_video_receiver_task` (per-participant video decoder), [`rgba_to_i420`]
//! (native RGBA→I420 conversion for local publishing), and codec detection
//! helpers. All items are `pub(super)`.

use livekit::track::LocalVideoTrack;
use livekit::webrtc::video_source::native::NativeVideoSource;
use livekit::webrtc::video_stream::native::NativeVideoStream;
use std::sync::atomic::{AtomicBool, AtomicI64};
use std::sync::{Arc, Mutex};
use tokio_stream::StreamExt;

// ---------------------------------------------------------------------------
// VideoState
// ---------------------------------------------------------------------------

/// Grouped video fields for `RealLiveKitConnection`.
///
/// Held by value (not Arc) — `RealLiveKitConnection` is already behind Arc at
/// the call site. Each inner field is `Arc<Mutex<...>>` so handles can be
/// cloned into task closures without an outer Arc wrapper.
pub(super) struct VideoState {
    /// Published local video track handle (for cleanup on unpublish/disconnect).
    pub(super) published_video_track: Arc<Mutex<Option<LocalVideoTrack>>>,
    /// Video source handle for feeding captured RGBA frames into the LiveKit SDK.
    pub(super) video_source: Arc<Mutex<Option<NativeVideoSource>>>,
    /// Monotonic timestamp for locally fed video frames. The Rust LiveKit path
    /// must advance timestamps itself; constant timestamps can stall outbound
    /// screen-share delivery for repeated frames.
    pub(super) next_timestamp_us: Arc<AtomicI64>,
    /// Callback for receiving decoded video frames from remote screen shares.
    /// Receives (identity, rgba_data, width, height).
    #[allow(clippy::type_complexity)]
    pub(super) video_frame_cb:
        Arc<Mutex<Option<Box<dyn Fn(&str, &[u8], u32, u32) + Send + 'static>>>>,
    /// Callback for when a remote screen share video track ends.
    /// Receives (identity).
    #[allow(clippy::type_complexity)]
    pub(super) video_track_ended_cb: Arc<Mutex<Option<Box<dyn Fn(&str) + Send + 'static>>>>,
}

impl VideoState {
    pub(super) fn new() -> Self {
        Self {
            published_video_track: Arc::new(Mutex::new(None)),
            video_source: Arc::new(Mutex::new(None)),
            next_timestamp_us: Arc::new(AtomicI64::new(1)),
            video_frame_cb: Arc::new(Mutex::new(None)),
            video_track_ended_cb: Arc::new(Mutex::new(None)),
        }
    }
}

// ---------------------------------------------------------------------------
// Video receiver task
// ---------------------------------------------------------------------------

/// Per-participant video receiver task — reads frames from `stream`, converts
/// to RGBA via `to_argb`, and fires `video_cb`. Uses a watch channel for
/// keep-latest frame dropping so slow consumers don't accumulate a backlog.
///
/// Spawned once per `TrackSubscribed` (Video) event in
/// `livekit_connection::connect()`.
#[allow(clippy::type_complexity)]
pub(super) async fn run_video_receiver_task(
    mut stream: NativeVideoStream,
    participant_id: String,
    video_cb: Arc<Mutex<Option<Box<dyn Fn(&str, &[u8], u32, u32) + Send + 'static>>>>,
    ended_cb: Arc<Mutex<Option<Box<dyn Fn(&str) + Send + 'static>>>>,
    closing: Arc<AtomicBool>,
) {
    use livekit::webrtc::video_frame::VideoFormatType;

    // Keep-latest frame dropping: we use a watch channel so
    // only the most recent frame is retained. The emitter
    // task reads at its own pace, dropping intermediates.
    let (frame_tx, mut frame_rx) = tokio::sync::watch::channel::<Option<(Vec<u8>, u32, u32)>>(None);

    let participant_id_emit = participant_id.clone();
    let closing_emit = Arc::clone(&closing);
    let emit_task = tokio::spawn(async move {
        loop {
            if closing_emit.load(std::sync::atomic::Ordering::SeqCst) {
                break;
            }
            // Wait for a new frame to arrive.
            if frame_rx.changed().await.is_err() {
                break; // sender dropped
            }
            let frame_data = {
                let borrowed = frame_rx.borrow_and_update();
                borrowed.clone()
            };
            if let Some((rgba, w, h)) = frame_data {
                if let Some(cb) = video_cb.lock().unwrap().as_ref() {
                    cb(&participant_id_emit, &rgba, w, h);
                }
            }
        }
    });

    let mut frames_received: u64 = 0;

    while let Some(frame) = stream.next().await {
        if closing.load(std::sync::atomic::Ordering::SeqCst) {
            break;
        }

        let buffer = frame.buffer.as_ref();
        let width = buffer.width();
        let height = buffer.height();

        if width == 0 || height == 0 {
            continue;
        }

        // Request ABGR from libyuv/libwebrtc.
        // On little-endian targets this yields RGBA byte layout
        // in memory, which matches downstream expectations.
        let dst_stride = width * 4;
        let mut rgba = vec![0u8; (dst_stride * height) as usize];
        buffer.to_argb(
            VideoFormatType::ABGR,
            &mut rgba,
            dst_stride,
            width as i32,
            height as i32,
        );

        // Send to the keep-latest channel (drops old frame).
        let _ = frame_tx.send(Some((rgba, width, height)));

        frames_received += 1;
        if frames_received.is_multiple_of(150) {
            log::info!(
                "livekit_video: participant={participant_id} frames_received={frames_received}"
            );
        }
    }

    emit_task.abort();
    log::info!(
        "livekit_video: video stream ended for {participant_id}, total frames={frames_received}"
    );

    // Fire track-ended callback when the stream ends.
    if let Some(cb) = ended_cb.lock().unwrap().as_ref() {
        cb(&participant_id);
    }
}

// ---------------------------------------------------------------------------
// Video codec detection
// ---------------------------------------------------------------------------

/// Detect the preferred video codec for screen sharing.
///
/// On Linux, uses VA-API detection to prefer H.264 when a hardware encoder is
/// available. On other platforms, returns the default VP8 codec.
pub(super) fn detect_preferred_video_codec() -> livekit::options::VideoCodec {
    #[cfg(target_os = "linux")]
    {
        // codec_detect is in the wavis-gui crate, not in clients/shared.
        // We check for VA-API H.264 via the same lightweight probe approach:
        // look for /dev/dri/renderD* and vainfo output.
        // For now, use a simple env-var override + runtime probe.
        if std::env::var("WAVIS_PREFER_H264").is_ok() || probe_vaapi_h264_available() {
            log::info!("video_codec: H.264 preferred (VA-API hardware encoder detected)");
            return livekit::options::VideoCodec::H264;
        }
        log::info!("video_codec: VP8 (no VA-API H.264 hardware encoder)");
        livekit::options::VideoCodec::VP8
    }
    #[cfg(not(target_os = "linux"))]
    {
        livekit::options::VideoCodec::VP8
    }
}

/// Lightweight VA-API H.264 probe — checks for DRI render nodes and runs
/// `vainfo` to detect H.264 encode entrypoints. Mirrors the logic in
/// `screen_capture/codec_detect.rs` but lives here so `clients/shared`
/// doesn't depend on the GUI crate.
#[cfg(target_os = "linux")]
fn probe_vaapi_h264_available() -> bool {
    use std::sync::OnceLock;
    static CACHED: OnceLock<bool> = OnceLock::new();
    *CACHED.get_or_init(|| {
        // Check for DRI render nodes.
        let dri_path = std::path::Path::new("/dev/dri");
        let has_render_node = std::fs::read_dir(dri_path)
            .ok()
            .map(|entries| {
                entries.filter_map(|e| e.ok()).any(|e| {
                    e.file_name()
                        .to_str()
                        .is_some_and(|name| name.starts_with("renderD"))
                })
            })
            .unwrap_or(false);

        if !has_render_node {
            return false;
        }

        // Run vainfo and check for H.264 encode entrypoint.
        let output = std::process::Command::new("vainfo")
            .arg("--display")
            .arg("drm")
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped())
            .output();

        match output {
            Ok(out) if out.status.success() => {
                let stdout = String::from_utf8_lossy(&out.stdout);
                stdout.lines().any(|line| {
                    let lower = line.to_lowercase();
                    (lower.contains("h264") || lower.contains("h.264"))
                        && lower.contains("vaentrypointencslice")
                })
            }
            _ => false,
        }
    })
}

// ---------------------------------------------------------------------------
// I420 conversion
// ---------------------------------------------------------------------------

/// Convert RGBA pixel data to I420 (YUV 4:2:0) format.
///
/// This uses libwebrtc/libyuv's native converter instead of a handwritten
/// BT.601 implementation. That matters for two reasons:
/// 1. It matches the native sender path Wavis publishes through in production.
/// 2. The exact Y/U/V values are implementation-defined enough that tests
///    should lock down output invariants and regressions, not old handwritten
///    coefficient math.
///
/// The input slice is RGBA in memory order. On little-endian targets, libyuv's
/// `abgr_to_i420` entrypoint matches that byte layout.
pub(super) fn rgba_to_i420(
    rgba: &[u8],
    width: u32,
    height: u32,
) -> livekit::webrtc::video_frame::I420Buffer {
    use livekit::webrtc::native::yuv_helper;
    use livekit::webrtc::video_frame::I420Buffer;

    let mut i420 = I420Buffer::new(width, height);
    let (stride_y, stride_u, stride_v) = i420.strides();
    let (y_data, u_data, v_data) = i420.data_mut();

    // Keep this entrypoint in sync with the byte layout we publish. Swapping it
    // incorrectly can produce color corruption or solid green remote frames.
    yuv_helper::abgr_to_i420(
        rgba,
        width * 4,
        y_data,
        stride_y,
        u_data,
        stride_u,
        v_data,
        stride_v,
        width as i32,
        height as i32,
    );

    i420
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use proptest::prelude::*;

    fn rgba_solid(width: u32, height: u32, rgba: [u8; 4]) -> Vec<u8> {
        let mut data = Vec::with_capacity((width * height * 4) as usize);
        for _ in 0..(width * height) {
            data.extend_from_slice(&rgba);
        }
        data
    }

    fn grayscale_rgba(width: u32, height: u32, intensity: u8) -> Vec<u8> {
        rgba_solid(width, height, [intensity, intensity, intensity, 255])
    }

    fn assert_all_y_samples_equal(
        y_data: &[u8],
        stride_y: usize,
        width: usize,
        height: usize,
        expected: u8,
    ) {
        for row in 0..height {
            for col in 0..width {
                assert_eq!(
                    y_data[row * stride_y + col],
                    expected,
                    "Y mismatch at ({row}, {col})"
                );
            }
        }
    }

    fn assert_chroma_is_neutralish(u: u8, v: u8, tolerance: u8) {
        assert!(
            u.abs_diff(128) <= tolerance,
            "U should stay near neutral gray, got {u}"
        );
        assert!(
            v.abs_diff(128) <= tolerance,
            "V should stay near neutral gray, got {v}"
        );
    }

    // These tests intentionally avoid asserting exact legacy BT.601 values.
    // The production path now uses libwebrtc/libyuv's native converter, and
    // the regression we need to catch is "wrong layout / wrong ordering /
    // broken stride handling", not "a specific handwritten coefficient set".

    proptest! {
        #![proptest_config(ProptestConfig::with_cases(50))]
        #[test]
        fn prop_rgba_to_i420_stride_and_range_invariants(
            width in 1u32..=256u32,
            height in 1u32..=256u32,
            rgba_pixels in proptest::collection::vec(0u8..=255u8, 0..=(256 * 256 * 4))
        ) {
            let required_len = (width * height * 4) as usize;
            let mut rgba = rgba_pixels;
            rgba.resize(required_len, 0);

            let i420 = rgba_to_i420(&rgba, width, height);
            let (stride_y, stride_u, stride_v) = i420.strides();
            let stride_y = stride_y as usize;
            let stride_u = stride_u as usize;
            let stride_v = stride_v as usize;
            let (y_data, u_data, v_data) = i420.data();

            let w = width as usize;
            let h = height as usize;
            let chroma_w = w.div_ceil(2);
            let chroma_h = h.div_ceil(2);

            prop_assert!(stride_y >= w);
            prop_assert!(stride_u >= chroma_w);
            prop_assert!(stride_v >= chroma_w);
            prop_assert!(y_data.len() >= stride_y * h);
            prop_assert!(u_data.len() >= stride_u * chroma_h);
            prop_assert!(v_data.len() >= stride_v * chroma_h);

            for row in 0..h {
                for col in 0..w {
                    let _ = y_data[row * stride_y + col];
                }
            }
            for cy in 0..chroma_h {
                for cx in 0..chroma_w {
                    let _ = u_data[cy * stride_u + cx];
                    let _ = v_data[cy * stride_v + cx];
                }
            }
        }
    }

    #[test]
    fn test_rgba_to_i420_is_deterministic_for_known_input() {
        let rgba = vec![
            255u8, 0, 0, 255, 0, 255, 0, 255, 0, 0, 255, 255, 255, 255, 255, 255,
        ];

        let first = rgba_to_i420(&rgba, 2, 2);
        let second = rgba_to_i420(&rgba, 2, 2);
        assert_eq!(first.data(), second.data());
        assert_eq!(first.strides(), second.strides());
    }

    #[test]
    fn test_rgba_to_i420_grayscale_stays_neutral_in_chroma() {
        for intensity in [0u8, 32, 128, 200, 255] {
            let rgba = grayscale_rgba(2, 2, intensity);
            let i420 = rgba_to_i420(&rgba, 2, 2);
            let (stride_y, _, _) = i420.strides();
            let (y_data, u_data, v_data) = i420.data();

            assert_all_y_samples_equal(y_data, stride_y as usize, 2, 2, y_data[0]);
            assert_chroma_is_neutralish(u_data[0], v_data[0], 1);
        }
    }

    #[test]
    fn test_rgba_to_i420_grayscale_luma_is_monotonic() {
        let black = rgba_to_i420(&grayscale_rgba(2, 2, 0), 2, 2);
        let mid = rgba_to_i420(&grayscale_rgba(2, 2, 127), 2, 2);
        let white = rgba_to_i420(&grayscale_rgba(2, 2, 255), 2, 2);

        let (black_y, _, _) = black.data();
        let (mid_y, _, _) = mid.data();
        let (white_y, _, _) = white.data();

        assert!(
            black_y[0] < mid_y[0],
            "black should map to lower luma than gray"
        );
        assert!(
            mid_y[0] < white_y[0],
            "gray should map to lower luma than white"
        );
    }

    #[test]
    fn test_rgba_to_i420_primary_colors_have_distinct_chroma() {
        let red = rgba_to_i420(&rgba_solid(2, 2, [255, 0, 0, 255]), 2, 2);
        let green = rgba_to_i420(&rgba_solid(2, 2, [0, 255, 0, 255]), 2, 2);
        let blue = rgba_to_i420(&rgba_solid(2, 2, [0, 0, 255, 255]), 2, 2);

        let (_, red_u, red_v) = red.data();
        let (_, green_u, green_v) = green.data();
        let (_, blue_u, blue_v) = blue.data();

        assert_ne!((red_u[0], red_v[0]), (green_u[0], green_v[0]));
        assert_ne!((red_u[0], red_v[0]), (blue_u[0], blue_v[0]));
        assert_ne!((green_u[0], green_v[0]), (blue_u[0], blue_v[0]));
    }

    #[test]
    fn test_rgba_to_i420_solid_colors_are_uniform_across_planes() {
        for rgba in [
            [255u8, 0, 0, 255],
            [0u8, 255, 0, 255],
            [0u8, 0, 255, 255],
            [255u8, 255, 255, 255],
            [0u8, 0, 0, 255],
        ] {
            let i420 = rgba_to_i420(&rgba_solid(4, 4, rgba), 4, 4);
            let (stride_y, stride_u, stride_v) = i420.strides();
            let (y_data, u_data, v_data) = i420.data();

            assert_all_y_samples_equal(y_data, stride_y as usize, 4, 4, y_data[0]);

            for row in 0..2usize {
                for col in 0..2usize {
                    assert_eq!(u_data[row * stride_u as usize + col], u_data[0]);
                    assert_eq!(v_data[row * stride_v as usize + col], v_data[0]);
                }
            }
        }
    }

    #[test]
    fn test_rgba_to_i420_supports_minimum_1x1_input() {
        let i420 = rgba_to_i420(&vec![100u8, 150, 200, 255], 1, 1);
        let (stride_y, stride_u, stride_v) = i420.strides();
        let (y_data, u_data, v_data) = i420.data();

        assert!(stride_y >= 1);
        assert!(stride_u >= 1);
        assert!(stride_v >= 1);
        assert!(!y_data.is_empty());
        assert!(!u_data.is_empty());
        assert!(!v_data.is_empty());
    }

    #[test]
    fn test_rgba_to_i420_supports_odd_dimensions() {
        let width = 3u32;
        let height = 3u32;
        let rgba = rgba_solid(width, height, [42, 42, 42, 255]);
        let i420 = rgba_to_i420(&rgba, width, height);
        let (stride_y, stride_u, stride_v) = i420.strides();
        let (y_data, u_data, v_data) = i420.data();

        assert!(y_data.len() >= stride_y as usize * height as usize);
        assert!(u_data.len() >= stride_u as usize * (height as usize).div_ceil(2));
        assert!(v_data.len() >= stride_v as usize * (height as usize).div_ceil(2));
        assert_chroma_is_neutralish(u_data[0], v_data[0], 1);
    }
}
