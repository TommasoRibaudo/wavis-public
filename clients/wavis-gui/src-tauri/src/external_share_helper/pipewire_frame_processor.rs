//! Owns external share frame decoding, resolution capping, and LiveKit video
//! track publication. The parent module owns HTTP/session coordination and
//! calls into this module once the browser frame has already arrived.

use std::sync::atomic::{AtomicU64, Ordering as AtomicOrdering};
use std::sync::{Arc, Mutex};

use image::{ImageFormat, RgbaImage};
use tauri::{AppHandle, Manager};
use wavis_client_shared::room_session::LiveKitConnection;

use super::{Inner, LOG};
use crate::media::MediaState;
use crate::screen_capture::frame_processor::cap_resolution;
use crate::screen_capture::CapturedFrame;

static FRAME_SEQ: AtomicU64 = AtomicU64::new(0);

pub(super) fn process_helper_frame(
    session_id: &str,
    body: Vec<u8>,
    inner: &Arc<Mutex<Inner>>,
    app: &AppHandle,
) -> Result<(), String> {
    let frame_seq = FRAME_SEQ.fetch_add(1, AtomicOrdering::Relaxed) + 1;

    if body.is_empty() {
        return Err("empty frame body".to_string());
    }

    log_incoming_frame(&body, frame_seq);

    let needs_publish = {
        let mut guard = inner
            .lock()
            .map_err(|e| format!("external helper lock: {e}"))?;
        let session = guard
            .active_session
            .as_mut()
            .ok_or_else(|| "helper session not active".to_string())?;
        if session.id != session_id {
            return Err("helper session mismatch".to_string());
        }
        if session.stop_requested {
            return Err("helper session stopping".to_string());
        }
        if session.publishing_in_progress {
            // Another thread is currently running publish_video. Drop this
            // frame because the browser will send another one shortly.
            log::debug!("{LOG} frame #{frame_seq}: dropped (publish in progress)");
            return Ok(());
        }
        if session.published {
            false
        } else {
            session.publishing_in_progress = true;
            true
        }
    };

    let media_state = app
        .try_state::<MediaState>()
        .ok_or_else(|| "media state unavailable".to_string())?;
    let decoded = decode_helper_frame(&body, frame_seq)?;
    let (width, height) = decoded.dimensions();
    if needs_publish {
        log_first_publish_frame(&body, &decoded, &media_state);
    }
    let frame = cap_resolution(
        CapturedFrame {
            width,
            height,
            data: decoded.into_raw(),
            timestamp_ms: 0,
        },
        media_state.screen_share_config.max_width(),
        media_state.screen_share_config.max_height(),
    );

    let lk_guard = media_state.lk().map_err(|e| format!("lock: {e}"))?;
    let conn = lk_guard
        .as_ref()
        .ok_or_else(|| "not connected to a room".to_string())?;

    if !conn.is_available() {
        return Err("not connected to a room".to_string());
    }

    if needs_publish {
        let pub_w = media_state.screen_share_config.max_width().max(1);
        let pub_h = media_state.screen_share_config.max_height().max(1);
        log::info!("{LOG} publishing video track at {pub_w}x{pub_h}");
        let publish_result = media_state
            .runtime
            .block_on(async { conn.publish_video(pub_w, pub_h) });

        // Update session state after publish completes so later frames see a
        // deterministic published/in-progress view.
        {
            let mut guard = inner
                .lock()
                .map_err(|e| format!("external helper lock: {e}"))?;
            if let Some(session) = guard.active_session.as_mut() {
                if session.id == session_id {
                    session.publishing_in_progress = false;
                    if publish_result.is_ok() {
                        session.published = true;
                    }
                }
            }
        }

        if let Err(err) = publish_result {
            return Err(format!(
                "failed to publish external share video track: {err}"
            ));
        }
    }

    conn.feed_video_frame(&frame.data, frame.width, frame.height)
        .map_err(|e| format!("failed to feed external share frame: {e}"))?;
    Ok(())
}

pub(super) fn stop_published_video(app: &AppHandle, guard: &mut Inner) -> Result<(), String> {
    let Some(session) = guard.active_session.as_mut() else {
        return Ok(());
    };
    if !session.published {
        return Ok(());
    }

    let media_state = app
        .try_state::<MediaState>()
        .ok_or_else(|| "media state unavailable".to_string())?;
    let lk_guard = media_state.lk().map_err(|e| format!("lock: {e}"))?;
    if let Some(conn) = lk_guard.as_ref() {
        media_state
            .runtime
            .block_on(async { conn.unpublish_video() })
            .map_err(|e| format!("failed to unpublish external share video track: {e}"))?;
    }
    session.published = false;
    Ok(())
}

fn log_incoming_frame(body: &[u8], frame_seq: u64) {
    if frame_seq > 5 {
        return;
    }

    let magic: Vec<u8> = body.iter().take(8).copied().collect();
    let is_png = body.len() >= 8 && body[..8] == [0x89, 0x50, 0x4E, 0x47, 0x0D, 0x0A, 0x1A, 0x0A];
    let is_jpeg = body.len() >= 2 && body[0] == 0xFF && body[1] == 0xD8;
    log::debug!(
        "{LOG} frame #{frame_seq}: magic={magic:02x?} is_png={is_png} is_jpeg={is_jpeg} body_len={}",
        body.len()
    );
}

fn decode_helper_frame(body: &[u8], frame_seq: u64) -> Result<RgbaImage, String> {
    image::load_from_memory_with_format(body, ImageFormat::Png)
        .or_else(|_| {
            // Fall back to auto-detect because some browsers may ignore the
            // PNG request despite the helper asking for PNG frames.
            log::debug!("{LOG} PNG decode failed, trying auto-detect for frame #{frame_seq}");
            image::load_from_memory(body)
        })
        .map_err(|e| format!("failed to decode helper frame: {e}"))
        .map(|decoded| decoded.to_rgba8())
}

fn log_first_publish_frame(body: &[u8], decoded: &RgbaImage, media_state: &MediaState) {
    let (width, height) = decoded.dimensions();
    log::debug!(
        "{LOG} FIRST FRAME: png_len={} decoded={}x{} config_max={}x{}",
        body.len(),
        width,
        height,
        media_state.screen_share_config.max_width(),
        media_state.screen_share_config.max_height(),
    );
    let raw = decoded.as_raw();
    let mid = (height / 2 * width + width / 2) as usize * 4;
    let (r, g, b, a) = if mid + 3 < raw.len() {
        (raw[mid], raw[mid + 1], raw[mid + 2], raw[mid + 3])
    } else {
        (0, 0, 0, 0)
    };
    log::debug!(
        "{LOG} FIRST FRAME center pixel RGBA=({r},{g},{b},{a}) total_bytes={}",
        raw.len()
    );
}

#[cfg(test)]
mod tests {
    use super::*;

    fn make_test_jpeg(width: u32, height: u32, r: u8, g: u8, b: u8) -> Vec<u8> {
        let img = image::RgbImage::from_fn(width, height, |_, _| image::Rgb([r, g, b]));
        let mut buf = std::io::Cursor::new(Vec::new());
        img.write_to(&mut buf, ImageFormat::Jpeg).unwrap();
        buf.into_inner()
    }

    fn make_gradient_jpeg(width: u32, height: u32) -> Vec<u8> {
        let img = image::RgbImage::from_fn(width, height, |x, y| {
            let r = ((x * 255) / width.max(1)) as u8;
            let g = ((y * 255) / height.max(1)) as u8;
            let b = 128u8;
            image::Rgb([r, g, b])
        });
        let mut buf = std::io::Cursor::new(Vec::new());
        img.write_to(&mut buf, ImageFormat::Jpeg).unwrap();
        buf.into_inner()
    }

    #[test]
    fn jpeg_decode_produces_nonzero_rgba() {
        let jpeg = make_test_jpeg(320, 240, 200, 100, 50);
        let decoded = image::load_from_memory_with_format(&jpeg, ImageFormat::Jpeg)
            .expect("JPEG decode failed")
            .to_rgba8();

        let (w, h) = decoded.dimensions();
        assert_eq!(w, 320);
        assert_eq!(h, 240);

        let raw = decoded.as_raw();
        let mid = (120 * 320 + 160) * 4;
        let (r, g, b, a) = (raw[mid], raw[mid + 1], raw[mid + 2], raw[mid + 3]);
        assert!(r > 150, "red channel too low: {r}");
        assert!(g > 60, "green channel too low: {g}");
        assert!(b > 20, "blue channel too low: {b}");
        assert_eq!(a, 255, "alpha should be 255");

        let nonzero = raw.iter().filter(|&&v| v != 0).count();
        assert!(
            nonzero > raw.len() / 2,
            "decoded image is mostly zeros ({nonzero}/{} nonzero)",
            raw.len()
        );
    }

    #[test]
    fn jpeg_decode_1920x1080_produces_correct_dimensions() {
        let jpeg = make_test_jpeg(1920, 1080, 100, 150, 200);
        let decoded = image::load_from_memory_with_format(&jpeg, ImageFormat::Jpeg)
            .expect("JPEG decode failed")
            .to_rgba8();

        assert_eq!(decoded.dimensions(), (1920, 1080));
        assert_eq!(decoded.as_raw().len(), 1920 * 1080 * 4);
    }

    #[test]
    fn cap_resolution_passthrough_at_1920x1080() {
        let jpeg = make_gradient_jpeg(1920, 1080);
        let decoded = image::load_from_memory_with_format(&jpeg, ImageFormat::Jpeg)
            .expect("JPEG decode failed")
            .to_rgba8();

        let raw = decoded.into_raw();
        let frame = CapturedFrame {
            width: 1920,
            height: 1080,
            data: raw.clone(),
            timestamp_ms: 0,
        };

        let capped = cap_resolution(frame, 1920, 1080);
        assert_eq!(capped.width, 1920);
        assert_eq!(capped.height, 1080);
        assert_eq!(capped.data.len(), raw.len());
        assert_eq!(
            capped.data, raw,
            "cap_resolution modified the data when it shouldn't have"
        );
    }

    #[test]
    fn cap_resolution_downscales_larger_frame() {
        let jpeg = make_gradient_jpeg(2560, 1440);
        let decoded = image::load_from_memory_with_format(&jpeg, ImageFormat::Jpeg)
            .expect("JPEG decode failed")
            .to_rgba8();

        let frame = CapturedFrame {
            width: 2560,
            height: 1440,
            data: decoded.into_raw(),
            timestamp_ms: 0,
        };

        let capped = cap_resolution(frame, 1920, 1080);
        assert!(capped.width <= 1920, "width {} > 1920", capped.width);
        assert!(capped.height <= 1080, "height {} > 1080", capped.height);
        assert_eq!(
            capped.data.len(),
            (capped.width * capped.height * 4) as usize
        );

        let nonzero = capped.data.iter().filter(|&&v| v != 0).count();
        assert!(
            nonzero > capped.data.len() / 4,
            "downscaled data is mostly zeros"
        );
    }

    #[test]
    fn full_pipeline_jpeg_to_rgba_frame() {
        let jpeg = make_gradient_jpeg(1920, 1080);
        let decoded = image::load_from_memory_with_format(&jpeg, ImageFormat::Jpeg)
            .expect("JPEG decode failed")
            .to_rgba8();
        let (width, height) = decoded.dimensions();

        let frame = cap_resolution(
            CapturedFrame {
                width,
                height,
                data: decoded.into_raw(),
                timestamp_ms: 0,
            },
            1920,
            1080,
        );

        assert_eq!(frame.width, 1920);
        assert_eq!(frame.height, 1080);
        assert_eq!(frame.data.len(), 1920 * 1080 * 4);

        let mid = (540 * 1920 + 960) * 4;
        let a = frame.data[mid + 3];
        assert_eq!(a, 255, "alpha must be 255");

        let rgb_nonzero = frame
            .data
            .chunks_exact(4)
            .filter(|px| px[0] != 0 || px[1] != 0 || px[2] != 0)
            .count();
        let total_pixels = (frame.width * frame.height) as usize;
        assert!(
            rgb_nonzero > total_pixels / 2,
            "too few nonzero pixels: {rgb_nonzero}/{total_pixels}"
        );
    }

    #[test]
    fn solid_red_survives_jpeg_roundtrip() {
        let jpeg = make_test_jpeg(640, 480, 255, 0, 0);
        let decoded = image::load_from_memory_with_format(&jpeg, ImageFormat::Jpeg)
            .unwrap()
            .to_rgba8();

        let raw = decoded.as_raw();
        let mid = (240 * 640 + 320) * 4;
        assert!(raw[mid] > 200, "R={} too low for red", raw[mid]);
        assert!(raw[mid + 1] < 50, "G={} too high for red", raw[mid + 1]);
        assert!(raw[mid + 2] < 50, "B={} too high for red", raw[mid + 2]);
    }

    #[test]
    fn solid_black_jpeg_is_all_zero_rgb() {
        let jpeg = make_test_jpeg(640, 480, 0, 0, 0);
        let decoded = image::load_from_memory_with_format(&jpeg, ImageFormat::Jpeg)
            .unwrap()
            .to_rgba8();
        let raw = decoded.as_raw();

        for pixel in raw.chunks_exact(4) {
            assert!(pixel[0] < 5, "R={} for black pixel", pixel[0]);
            assert!(pixel[1] < 5, "G={} for black pixel", pixel[1]);
            assert!(pixel[2] < 5, "B={} for black pixel", pixel[2]);
            assert_eq!(pixel[3], 255, "alpha must be 255");
        }
    }

    #[test]
    fn empty_body_returns_error() {
        let result = image::load_from_memory_with_format(&[], ImageFormat::Jpeg);
        assert!(result.is_err(), "empty JPEG should fail to decode");
    }

    fn make_test_png(width: u32, height: u32, r: u8, g: u8, b: u8) -> Vec<u8> {
        let img = image::RgbaImage::from_fn(width, height, |_, _| image::Rgba([r, g, b, 255]));
        let mut buf = std::io::Cursor::new(Vec::new());
        img.write_to(&mut buf, ImageFormat::Png).unwrap();
        buf.into_inner()
    }

    fn make_gradient_png(width: u32, height: u32) -> Vec<u8> {
        let img = image::RgbaImage::from_fn(width, height, |x, y| {
            let r = ((x * 255) / width.max(1)) as u8;
            let g = ((y * 255) / height.max(1)) as u8;
            let b = 128u8;
            image::Rgba([r, g, b, 255])
        });
        let mut buf = std::io::Cursor::new(Vec::new());
        img.write_to(&mut buf, ImageFormat::Png).unwrap();
        buf.into_inner()
    }

    #[test]
    fn png_decode_produces_nonzero_rgba() {
        let png = make_test_png(320, 240, 200, 100, 50);
        let decoded = image::load_from_memory_with_format(&png, ImageFormat::Png)
            .expect("PNG decode failed")
            .to_rgba8();

        let (w, h) = decoded.dimensions();
        assert_eq!(w, 320);
        assert_eq!(h, 240);

        let raw = decoded.as_raw();
        let mid = (120 * 320 + 160) * 4;
        assert_eq!(raw[mid], 200, "R mismatch");
        assert_eq!(raw[mid + 1], 100, "G mismatch");
        assert_eq!(raw[mid + 2], 50, "B mismatch");
        assert_eq!(raw[mid + 3], 255, "A mismatch");
    }

    #[test]
    fn png_decode_1920x1080_produces_correct_dimensions() {
        let png = make_test_png(1920, 1080, 100, 150, 200);
        let decoded = image::load_from_memory_with_format(&png, ImageFormat::Png)
            .expect("PNG decode failed")
            .to_rgba8();

        assert_eq!(decoded.dimensions(), (1920, 1080));
        assert_eq!(decoded.as_raw().len(), 1920 * 1080 * 4);
    }

    #[test]
    fn png_full_pipeline_to_rgba_frame() {
        let png = make_gradient_png(1920, 1080);
        let decoded = image::load_from_memory_with_format(&png, ImageFormat::Png)
            .expect("PNG decode failed")
            .to_rgba8();
        let (width, height) = decoded.dimensions();

        let frame = cap_resolution(
            CapturedFrame {
                width,
                height,
                data: decoded.into_raw(),
                timestamp_ms: 0,
            },
            1920,
            1080,
        );

        assert_eq!(frame.width, 1920);
        assert_eq!(frame.height, 1080);
        assert_eq!(frame.data.len(), 1920 * 1080 * 4);

        let mid = (540 * 1920 + 960) * 4;
        let expected_r = ((960 * 255) / 1920) as u8;
        let expected_g = ((540 * 255) / 1080) as u8;
        assert_eq!(frame.data[mid], expected_r, "R mismatch at center");
        assert_eq!(frame.data[mid + 1], expected_g, "G mismatch at center");
        assert_eq!(frame.data[mid + 2], 128, "B mismatch at center");
        assert_eq!(frame.data[mid + 3], 255, "A mismatch at center");
    }

    #[test]
    fn png_solid_red_exact() {
        let png = make_test_png(640, 480, 255, 0, 0);
        let decoded = image::load_from_memory_with_format(&png, ImageFormat::Png)
            .unwrap()
            .to_rgba8();

        let raw = decoded.as_raw();
        let mid = (240 * 640 + 320) * 4;
        assert_eq!(raw[mid], 255, "R");
        assert_eq!(raw[mid + 1], 0, "G");
        assert_eq!(raw[mid + 2], 0, "B");
        assert_eq!(raw[mid + 3], 255, "A");
    }

    #[test]
    fn png_cap_resolution_downscales() {
        let png = make_gradient_png(2560, 1440);
        let decoded = image::load_from_memory_with_format(&png, ImageFormat::Png)
            .expect("PNG decode failed")
            .to_rgba8();

        let frame = CapturedFrame {
            width: 2560,
            height: 1440,
            data: decoded.into_raw(),
            timestamp_ms: 0,
        };

        let capped = cap_resolution(frame, 1920, 1080);
        assert!(capped.width <= 1920, "width {} > 1920", capped.width);
        assert!(capped.height <= 1080, "height {} > 1080", capped.height);
        assert_eq!(
            capped.data.len(),
            (capped.width * capped.height * 4) as usize
        );

        let nonzero = capped.data.iter().filter(|&&v| v != 0).count();
        assert!(
            nonzero > capped.data.len() / 4,
            "downscaled data is mostly zeros"
        );
    }

    #[test]
    fn png_empty_body_returns_error() {
        let result = image::load_from_memory_with_format(&[], ImageFormat::Png);
        assert!(result.is_err(), "empty PNG should fail to decode");
    }

    #[test]
    fn png_with_alpha_false_canvas_simulation() {
        let png = make_test_png(640, 480, 100, 200, 50);
        let decoded = image::load_from_memory_with_format(&png, ImageFormat::Png)
            .unwrap()
            .to_rgba8();

        for (i, pixel) in decoded.as_raw().chunks_exact(4).enumerate() {
            assert_eq!(pixel[3], 255, "pixel {i} alpha={} != 255", pixel[3]);
        }
    }

    #[test]
    fn png_auto_detect_also_works() {
        let png = make_test_png(320, 240, 200, 100, 50);
        let decoded = image::load_from_memory(&png)
            .expect("auto-detect PNG decode failed")
            .to_rgba8();
        assert_eq!(decoded.dimensions(), (320, 240));
        let raw = decoded.as_raw();
        let mid = (120 * 320 + 160) * 4;
        assert_eq!(raw[mid], 200);
    }
}
