//! Native media bridge — exposes RealLiveKitConnection to the frontend via Tauri IPC.
//!
//! On platforms where the webview lacks WebRTC (Linux/WebKitGTK), the React
//! frontend delegates media to this Rust module instead of the livekit-client
//! JS SDK. The interface mirrors LiveKitModule's shape so voice-room.ts can
//! swap transparently.

use serde::Serialize;
#[cfg(target_os = "windows")]
use std::os::windows::process::CommandExt;
#[cfg(target_os = "linux")]
use std::process::{Child, Command, Stdio};
#[cfg(any(target_os = "linux", target_os = "windows"))]
use std::sync::atomic::{AtomicBool, Ordering};
#[cfg(target_os = "linux")]
use std::sync::atomic::{AtomicU32, AtomicU64};
use std::sync::{Arc, Mutex};
#[cfg(target_os = "linux")]
use std::thread::JoinHandle as StdJoinHandle;
#[cfg(any(target_os = "linux", target_os = "windows"))]
use std::{
    io::{Read, Write},
    net::{TcpListener, TcpStream},
    time::Duration,
};
use tauri::{AppHandle, Emitter, State};
use tokio::runtime::{Builder, Runtime};
use tokio::task::JoinHandle;

use wavis_client_shared::audio::{AudioBackend, AudioTrack};
use wavis_client_shared::cpal_audio::{AudioBuffer, CpalAudioBackend, PeerVolumes};
use wavis_client_shared::denoise_filter::DenoiseFilter;
use wavis_client_shared::livekit_connection::RealLiveKitConnection;
use wavis_client_shared::passthrough_filter::PassthroughFilters;
use wavis_client_shared::room_session::LiveKitConnection;

#[cfg(target_os = "linux")]
use crate::screen_capture;
#[cfg(target_os = "windows")]
use crate::screen_capture;

const LOG: &str = "[wavis:native-media]";
#[cfg(target_os = "windows")]
const CREATE_NO_WINDOW: u32 = 0x08000000;

#[cfg(target_os = "windows")]
fn unix_now_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

#[cfg(target_os = "linux")]
#[allow(dead_code)]
fn is_hyprland_wayland() -> bool {
    let has_wayland = std::env::var("WAYLAND_DISPLAY").is_ok();
    let has_hypr_sig = std::env::var("HYPRLAND_INSTANCE_SIGNATURE").is_ok();
    let xdg_desktop = std::env::var("XDG_CURRENT_DESKTOP").unwrap_or_default();

    has_wayland && (has_hypr_sig || xdg_desktop.to_ascii_lowercase().contains("hypr"))
}

#[cfg(target_os = "linux")]
#[derive(Clone, Serialize)]
struct LinuxCaptureFallbackEvent {
    from: String,
    to: String,
    reason: String,
}

#[cfg(target_os = "linux")]
fn emit_linux_capture_fallback(app: &AppHandle, from: &str, reason: impl Into<String>) {
    let reason = reason.into();
    if let Err(err) = app.emit(
        "linux-capture-fallback-activated",
        LinuxCaptureFallbackEvent {
            from: from.to_string(),
            to: "none".to_string(),
            reason,
        },
    ) {
        log::warn!("{LOG} failed to emit linux capture fallback event: {err}");
    }
}

// ─── Event Types (Rust → JS via Tauri events) ─────────────────────

#[derive(Clone, Serialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum MediaEvent {
    Connected,
    Failed {
        reason: String,
    },
    Disconnected,
    AudioLevels {
        levels: Vec<AudioLevelEntry>,
    },
    LocalAudioLevel {
        rms_level: f32,
        is_speaking: bool,
    },
    Stats {
        rtt_ms: f64,
        packet_loss_percent: f64,
        jitter_ms: f64,
    },
    #[cfg(target_os = "linux")]
    ScreenShareStats {
        bitrate_kbps: u32,
        fps: f64,
        quality_limitation_reason: String,
        packet_loss_percent: f64,
        frame_width: u32,
        frame_height: u32,
        pli_count: u32,
        nack_count: u32,
        available_bandwidth_kbps: u32,
    },
}

#[derive(Clone, Serialize)]
pub struct AudioLevelEntry {
    pub identity: String,
    pub rms_level: f32,
    pub is_speaking: bool,
}

#[cfg(target_os = "linux")]
struct NativeScreenShareMetrics {
    active: AtomicBool,
    frame_width: AtomicU32,
    frame_height: AtomicU32,
    emitted_frames: AtomicU64,
}

#[cfg(target_os = "linux")]
impl NativeScreenShareMetrics {
    fn new() -> Self {
        Self {
            active: AtomicBool::new(false),
            frame_width: AtomicU32::new(0),
            frame_height: AtomicU32::new(0),
            emitted_frames: AtomicU64::new(0),
        }
    }

    fn start(&self) {
        self.frame_width.store(0, Ordering::Relaxed);
        self.frame_height.store(0, Ordering::Relaxed);
        self.emitted_frames.store(0, Ordering::Relaxed);
        self.active.store(true, Ordering::Relaxed);
    }

    fn record_frame(&self, width: u32, height: u32) {
        self.frame_width.store(width, Ordering::Relaxed);
        self.frame_height.store(height, Ordering::Relaxed);
        self.emitted_frames.fetch_add(1, Ordering::Relaxed);
    }

    fn stop(&self) {
        self.active.store(false, Ordering::Relaxed);
        self.frame_width.store(0, Ordering::Relaxed);
        self.frame_height.store(0, Ordering::Relaxed);
        self.emitted_frames.store(0, Ordering::Relaxed);
    }
}

#[cfg(target_os = "linux")]
#[derive(Clone)]
struct HeartbeatFrame {
    data: Arc<Vec<u8>>,
    width: u32,
    height: u32,
}

#[cfg(target_os = "linux")]
#[derive(Clone)]
struct LatestRemoteScreenShareFrame {
    data: Arc<Vec<u8>>,
    width: u32,
    height: u32,
    seq: u64,
}

#[cfg(target_os = "linux")]
struct LocalCameraCapture {
    active: Arc<AtomicBool>,
    child: Arc<Mutex<Option<Child>>>,
    thread: Option<StdJoinHandle<()>>,
}

#[cfg(target_os = "linux")]
impl LocalCameraCapture {
    fn stop(mut self) {
        self.active.store(false, Ordering::Relaxed);
        if let Ok(mut child_guard) = self.child.lock() {
            if let Some(child) = child_guard.as_mut() {
                let _ = child.kill();
                let _ = child.wait();
            }
        }
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}

#[cfg(target_os = "linux")]
#[derive(Clone, Serialize)]
pub struct PolledScreenShareFrame {
    pub identity: String,
    pub frame: String,
    pub width: u32,
    pub height: u32,
    pub seq: u64,
}

#[cfg(target_os = "linux")]
#[derive(Clone, Serialize)]
struct ScreenShareAvailablePayload {
    identity: String,
}

#[cfg(target_os = "linux")]
#[derive(Clone, Serialize)]
struct ScreenShareAudioPayload {
    identity: String,
}

#[cfg(target_os = "linux")]
#[derive(Clone, Serialize)]
pub struct PolledCameraFrame {
    pub identity: String,
    pub frame: String,
    pub width: u32,
    pub height: u32,
    pub seq: u64,
}

#[cfg(target_os = "linux")]
#[derive(Clone, Serialize)]
struct CameraAvailablePayload {
    identity: String,
}

#[cfg(target_os = "linux")]
fn start_remote_screen_share_stream_server(
    frames: Arc<Mutex<std::collections::HashMap<String, LatestRemoteScreenShareFrame>>>,
    config: Arc<screen_capture::frame_processor::ScreenShareConfig>,
) -> Option<u16> {
    let listener = match TcpListener::bind(("127.0.0.1", 0)) {
        Ok(listener) => listener,
        Err(err) => {
            log::warn!("{LOG} failed to bind remote screen share stream server: {err}");
            return None;
        }
    };
    let port = match listener.local_addr() {
        Ok(addr) => addr.port(),
        Err(err) => {
            log::warn!("{LOG} failed to read remote screen share stream server port: {err}");
            return None;
        }
    };

    std::thread::Builder::new()
        .name("remote-share-mjpeg".into())
        .spawn(move || {
            log::info!("{LOG} remote screen share stream server listening on 127.0.0.1:{port}");
            for stream in listener.incoming() {
                let Ok(stream) = stream else {
                    continue;
                };
                let frames = Arc::clone(&frames);
                let config = Arc::clone(&config);
                let _ = std::thread::Builder::new()
                    .name("remote-share-mjpeg-client".into())
                    .spawn(move || {
                        if let Err(err) = handle_remote_screen_share_stream(stream, frames, config)
                        {
                            log::debug!("{LOG} remote screen share stream ended: {err}");
                        }
                    });
            }
        })
        .ok()?;

    Some(port)
}

#[cfg(target_os = "windows")]
fn start_windows_i420_stream_server(
    latest_i420_frame: Arc<Mutex<Option<LatestI420Frame>>>,
    diagnostics: Arc<Mutex<Option<screen_capture::win_capture::SharedWindowsCaptureDiagnostics>>>,
) -> Result<u16, String> {
    let listener = TcpListener::bind(("127.0.0.1", 0))
        .map_err(|e| format!("failed to bind Windows I420 stream server: {e}"))?;
    let port = listener
        .local_addr()
        .map_err(|e| format!("failed to read Windows I420 stream server port: {e}"))?
        .port();

    std::thread::Builder::new()
        .name("wavis-windows-i420-stream".to_string())
        .spawn(move || {
            log::info!("{LOG} Windows I420 stream server listening on 127.0.0.1:{port}");
            for stream in listener.incoming() {
                let Ok(stream) = stream else {
                    continue;
                };
                let frames = Arc::clone(&latest_i420_frame);
                let diag = Arc::clone(&diagnostics);
                let _ = std::thread::Builder::new()
                    .name("wavis-windows-i420-stream-client".to_string())
                    .spawn(move || {
                        if let Err(err) = handle_windows_i420_stream(stream, frames, diag) {
                            log::debug!("{LOG} Windows I420 stream ended: {err}");
                        }
                    });
            }
        })
        .map_err(|e| format!("failed to spawn Windows I420 stream server: {e}"))?;

    Ok(port)
}

#[cfg(target_os = "windows")]
fn windows_i420_stream_frame_header(frame: &LatestI420Frame, byte_len: u32) -> [u8; 28] {
    let mut frame_header = [0u8; 28];
    frame_header[0..4].copy_from_slice(&frame.width.to_le_bytes());
    frame_header[4..8].copy_from_slice(&frame.height.to_le_bytes());
    frame_header[8..16].copy_from_slice(&frame.seq.to_le_bytes());
    frame_header[16..24].copy_from_slice(&frame.timestamp_us.to_le_bytes());
    frame_header[24..28].copy_from_slice(&byte_len.to_le_bytes());
    frame_header
}

#[cfg(target_os = "windows")]
fn handle_windows_i420_stream(
    mut stream: TcpStream,
    latest_i420_frame: Arc<Mutex<Option<LatestI420Frame>>>,
    diagnostics: Arc<Mutex<Option<screen_capture::win_capture::SharedWindowsCaptureDiagnostics>>>,
) -> Result<(), String> {
    stream
        .set_nodelay(true)
        .map_err(|e| format!("I420 stream set_nodelay failed: {e}"))?;
    let mut req = [0u8; 1024];
    let _ = stream
        .read(&mut req)
        .map_err(|e| format!("I420 stream request read failed: {e}"))?;
    let header = concat!(
        "HTTP/1.1 200 OK\r\n",
        "Content-Type: application/octet-stream\r\n",
        "Cache-Control: no-store, no-cache, must-revalidate, max-age=0\r\n",
        "Pragma: no-cache\r\n",
        "Connection: close\r\n",
        "Access-Control-Allow-Origin: *\r\n",
        "\r\n"
    );
    stream
        .write_all(header.as_bytes())
        .map_err(|e| format!("I420 stream header write failed: {e}"))?;

    let mut last_seq = 0u64;
    loop {
        let latest = latest_i420_frame
            .lock()
            .map_err(|e| format!("latest_i420_frame lock: {e}"))?
            .clone();

        let Some(latest) = latest else {
            record_windows_i420_stream_read(&diagnostics, None);
            std::thread::sleep(Duration::from_millis(2));
            continue;
        };
        if latest.seq == last_seq {
            std::thread::sleep(Duration::from_millis(1));
            continue;
        }

        last_seq = latest.seq;
        let len = u32::try_from(latest.frame.len())
            .map_err(|_| "I420 frame too large to stream".to_string())?;
        let frame_header = windows_i420_stream_frame_header(&latest, len);

        stream
            .write_all(&frame_header)
            .and_then(|_| stream.write_all(latest.frame.as_slice()))
            .map_err(|e| format!("I420 stream frame write failed: {e}"))?;
        record_windows_i420_stream_read(&diagnostics, Some((&latest, len)));
    }
}

#[cfg(target_os = "windows")]
fn record_windows_i420_stream_read(
    diagnostics: &Arc<Mutex<Option<screen_capture::win_capture::SharedWindowsCaptureDiagnostics>>>,
    frame: Option<(&LatestI420Frame, u32)>,
) {
    if let Ok(diag_guard) = diagnostics.lock() {
        if let Some(diagnostics) = diag_guard.as_ref() {
            if let Ok(mut diagnostics) = diagnostics.lock() {
                diagnostics.poll_calls = diagnostics.poll_calls.saturating_add(1);
                if let Some((frame, _len)) = frame {
                    diagnostics.poll_hits = diagnostics.poll_hits.saturating_add(1);
                    diagnostics.latest_polled_seq = Some(frame.seq);
                } else {
                    diagnostics.poll_misses = diagnostics.poll_misses.saturating_add(1);
                }
            }
        }
    }
}

#[cfg(target_os = "linux")]
fn handle_remote_screen_share_stream(
    mut stream: TcpStream,
    frames: Arc<Mutex<std::collections::HashMap<String, LatestRemoteScreenShareFrame>>>,
    config: Arc<screen_capture::frame_processor::ScreenShareConfig>,
) -> Result<(), String> {
    stream
        .set_nodelay(true)
        .map_err(|e| format!("set_nodelay failed: {e}"))?;

    let mut req = Vec::with_capacity(4096);
    while !req.windows(4).any(|window| window == b"\r\n\r\n") && req.len() < 16 * 1024 {
        let mut chunk = [0u8; 1024];
        let read = stream
            .read(&mut chunk)
            .map_err(|e| format!("stream request read failed: {e}"))?;
        if read == 0 {
            return Ok(());
        }
        req.extend_from_slice(&chunk[..read]);
    }

    let request = String::from_utf8_lossy(&req);
    let target = request
        .lines()
        .next()
        .and_then(|line| line.split_whitespace().nth(1))
        .unwrap_or("/");
    let identity = target
        .split_once('?')
        .and_then(|(_, query)| query_param(query, "identity"))
        .and_then(|value| percent_decode(&value))
        .ok_or_else(|| "missing identity query".to_string())?;
    let use_raw_stream = target
        .split_once('?')
        .and_then(|(_, query)| query_param(query, "format"))
        .is_some_and(|value| value == "raw");

    if use_raw_stream {
        return handle_remote_screen_share_raw_stream(stream, frames, identity);
    }

    let header = concat!(
        "HTTP/1.1 200 OK\r\n",
        "Content-Type: multipart/x-mixed-replace; boundary=wavisframe\r\n",
        "Cache-Control: no-store, no-cache, must-revalidate, max-age=0\r\n",
        "Pragma: no-cache\r\n",
        "Connection: close\r\n",
        "Access-Control-Allow-Origin: *\r\n",
        "\r\n"
    );
    stream
        .write_all(header.as_bytes())
        .map_err(|e| format!("stream header write failed: {e}"))?;

    let mut last_seq = 0u64;
    loop {
        let latest = {
            let frames = frames
                .lock()
                .map_err(|e| format!("remote_screen_share_frames lock: {e}"))?;
            frames.get(&identity).cloned()
        };

        let Some(latest) = latest else {
            std::thread::sleep(Duration::from_millis(10));
            continue;
        };

        if latest.seq == last_seq {
            std::thread::sleep(Duration::from_millis(5));
            continue;
        }
        last_seq = latest.seq;

        let jpeg = encode_remote_frame_jpeg(&latest, config.jpeg_quality())?;
        let part_header = format!(
            "--wavisframe\r\nContent-Type: image/jpeg\r\nContent-Length: {}\r\n\r\n",
            jpeg.len()
        );
        stream
            .write_all(part_header.as_bytes())
            .and_then(|_| stream.write_all(&jpeg))
            .and_then(|_| stream.write_all(b"\r\n"))
            .map_err(|e| format!("stream frame write failed: {e}"))?;
    }
}

#[cfg(target_os = "linux")]
fn handle_remote_screen_share_raw_stream(
    mut stream: TcpStream,
    frames: Arc<Mutex<std::collections::HashMap<String, LatestRemoteScreenShareFrame>>>,
    identity: String,
) -> Result<(), String> {
    let header = concat!(
        "HTTP/1.1 200 OK\r\n",
        "Content-Type: application/octet-stream\r\n",
        "Cache-Control: no-store, no-cache, must-revalidate, max-age=0\r\n",
        "Pragma: no-cache\r\n",
        "Connection: close\r\n",
        "Access-Control-Allow-Origin: *\r\n",
        "\r\n"
    );
    stream
        .write_all(header.as_bytes())
        .map_err(|e| format!("raw stream header write failed: {e}"))?;

    let mut last_seq = 0u64;
    loop {
        let latest = {
            let frames = frames
                .lock()
                .map_err(|e| format!("remote_screen_share_frames lock: {e}"))?;
            frames.get(&identity).cloned()
        };

        let Some(latest) = latest else {
            std::thread::sleep(Duration::from_millis(10));
            continue;
        };

        if latest.seq == last_seq {
            std::thread::sleep(Duration::from_millis(1));
            continue;
        }
        last_seq = latest.seq;

        let len = u32::try_from(latest.data.len())
            .map_err(|_| "raw frame too large to stream".to_string())?;
        let mut frame_header = [0u8; 20];
        frame_header[0..4].copy_from_slice(&latest.width.to_le_bytes());
        frame_header[4..8].copy_from_slice(&latest.height.to_le_bytes());
        frame_header[8..16].copy_from_slice(&latest.seq.to_le_bytes());
        frame_header[16..20].copy_from_slice(&len.to_le_bytes());

        stream
            .write_all(&frame_header)
            .and_then(|_| stream.write_all(latest.data.as_slice()))
            .map_err(|e| format!("raw stream frame write failed: {e}"))?;
    }
}

#[cfg(target_os = "linux")]
fn encode_remote_frame_jpeg(
    frame: &LatestRemoteScreenShareFrame,
    jpeg_quality: u8,
) -> Result<Vec<u8>, String> {
    use image::codecs::jpeg::JpegEncoder;
    use std::io::Cursor;

    let pixel_count = (frame.width * frame.height) as usize;
    let mut rgb_data = Vec::with_capacity(pixel_count * 3);
    for rgba in frame.data.chunks_exact(4) {
        rgb_data.push(rgba[0]);
        rgb_data.push(rgba[1]);
        rgb_data.push(rgba[2]);
    }

    let mut jpeg_buf = Cursor::new(Vec::with_capacity(256 * 1024));
    let mut encoder = JpegEncoder::new_with_quality(&mut jpeg_buf, jpeg_quality);
    encoder
        .encode(
            &rgb_data,
            frame.width,
            frame.height,
            image::ExtendedColorType::Rgb8,
        )
        .map_err(|e| format!("remote JPEG encode failed: {e}"))?;
    Ok(jpeg_buf.into_inner())
}

#[cfg(target_os = "linux")]
fn query_param(query: &str, key: &str) -> Option<String> {
    query.split('&').find_map(|pair| {
        let (k, v) = pair.split_once('=')?;
        if k == key {
            Some(v.to_string())
        } else {
            None
        }
    })
}

#[cfg(target_os = "linux")]
fn percent_encode(value: &str) -> String {
    let mut out = String::with_capacity(value.len());
    for byte in value.bytes() {
        if byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b'.' | b'~') {
            out.push(byte as char);
        } else {
            out.push_str(&format!("%{byte:02X}"));
        }
    }
    out
}

#[cfg(target_os = "linux")]
fn percent_decode(value: &str) -> Option<String> {
    let bytes = value.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        match bytes[i] {
            b'%' if i + 2 < bytes.len() => {
                let hi = (bytes[i + 1] as char).to_digit(16)?;
                let lo = (bytes[i + 2] as char).to_digit(16)?;
                out.push(((hi << 4) | lo) as u8);
                i += 3;
            }
            b'+' => {
                out.push(b' ');
                i += 1;
            }
            byte => {
                out.push(byte);
                i += 1;
            }
        }
    }
    String::from_utf8(out).ok()
}

// ─── Managed State ─────────────────────────────────────────────────

#[cfg(target_os = "linux")]
/// Alias for the LiveKit connection guard to satisfy clippy::type_complexity.
type LkGuard<'a> = std::sync::MutexGuard<'a, Option<Arc<RealLiveKitConnection>>>;

fn screen_share_audio_volume_key(identity: &str) -> String {
    format!("{identity}:screen-share")
}

/// A single encoded frame buffered for JS polling (Windows JS SDK path).
#[cfg(target_os = "windows")]
#[derive(Clone, Serialize)]
pub struct LatestFrame {
    pub frame: String,
    pub width: u32,
    pub height: u32,
    /// Monotonic sequence number so JS can detect duplicates.
    pub seq: u64,
}

/// A single raw I420 frame buffered for JS polling (Windows JS SDK path).
#[cfg(target_os = "windows")]
#[derive(Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct LatestI420Frame {
    pub frame: Vec<u8>,
    pub width: u32,
    pub height: u32,
    pub timestamp_us: u64,
    /// Monotonic sequence number so JS can detect duplicates.
    pub seq: u64,
}

#[cfg(target_os = "windows")]
fn clamp_u8(value: i32) -> u8 {
    value.clamp(0, 255) as u8
}

#[cfg(target_os = "windows")]
fn rgba_to_i420(data: &[u8], width: u32, height: u32) -> Option<(Vec<u8>, u32, u32)> {
    let src_width = width;
    let src_height = height;
    let out_width = src_width & !1;
    let out_height = src_height & !1;
    if out_width == 0 || out_height == 0 {
        return None;
    }
    let required_len = (src_width as usize)
        .checked_mul(src_height as usize)?
        .checked_mul(4)?;
    if data.len() < required_len {
        return None;
    }

    let y_len = (out_width as usize).checked_mul(out_height as usize)?;
    let chroma_len = y_len / 4;
    let mut out = vec![0u8; y_len + chroma_len * 2];
    let (y_plane, chroma) = out.split_at_mut(y_len);
    let (u_plane, v_plane) = chroma.split_at_mut(chroma_len);
    let src_stride = src_width as usize * 4;
    let out_width_usize = out_width as usize;

    for y in 0..out_height as usize {
        for x in 0..out_width as usize {
            let idx = y * src_stride + x * 4;
            let r = data[idx] as i32;
            let g = data[idx + 1] as i32;
            let b = data[idx + 2] as i32;
            y_plane[y * out_width_usize + x] =
                clamp_u8(((66 * r + 129 * g + 25 * b + 128) >> 8) + 16);
        }
    }

    for y in (0..out_height as usize).step_by(2) {
        for x in (0..out_width as usize).step_by(2) {
            let mut r_sum = 0i32;
            let mut g_sum = 0i32;
            let mut b_sum = 0i32;
            for dy in 0..2 {
                for dx in 0..2 {
                    let idx = (y + dy) * src_stride + (x + dx) * 4;
                    r_sum += data[idx] as i32;
                    g_sum += data[idx + 1] as i32;
                    b_sum += data[idx + 2] as i32;
                }
            }
            let r = r_sum / 4;
            let g = g_sum / 4;
            let b = b_sum / 4;
            let chroma_idx = (y / 2) * (out_width_usize / 2) + (x / 2);
            u_plane[chroma_idx] = clamp_u8(((-38 * r - 74 * g + 112 * b + 128) >> 8) + 128);
            v_plane[chroma_idx] = clamp_u8(((112 * r - 94 * g - 18 * b + 128) >> 8) + 128);
        }
    }

    Some((out, out_width, out_height))
}

#[cfg(target_os = "windows")]
struct NativeShareLeakSession {
    share_session_id: String,
    source_id: String,
    source_kind: WindowsSourceKind,
    capture_backend: WindowsCaptureBackend,
    started_at_ms: u64,
    first_rust_frame_at_ms: Option<u64>,
    frames_buffered: u64,
    diagnostics: screen_capture::win_capture::SharedWindowsCaptureDiagnostics,
}

#[cfg(target_os = "windows")]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum WindowsSourceKind {
    Screen,
    Window,
}

#[cfg(target_os = "windows")]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum WindowsCaptureBackend {
    Wgc,
    GdiPoll,
}

#[cfg(target_os = "windows")]
fn parse_windows_source_kind(value: Option<&str>) -> Result<WindowsSourceKind, String> {
    match value {
        Some("screen") => Ok(WindowsSourceKind::Screen),
        Some("window") => Ok(WindowsSourceKind::Window),
        _ => Err("screen_share_start_source requires sourceKind='screen' or 'window'".to_string()),
    }
}

#[cfg(target_os = "windows")]
fn select_windows_capture_backend(
    source_kind: WindowsSourceKind,
    compatibility_mode: bool,
    requested_backend: Option<&str>,
) -> Result<WindowsCaptureBackend, String> {
    match requested_backend {
        Some("wgc") => Ok(WindowsCaptureBackend::Wgc),
        Some("gdi_poll") => Ok(WindowsCaptureBackend::GdiPoll),
        Some(other) => Err(format!("unsupported Windows capture backend '{other}'")),
        None if compatibility_mode && source_kind == WindowsSourceKind::Window => {
            Ok(WindowsCaptureBackend::GdiPoll)
        }
        None => Ok(WindowsCaptureBackend::Wgc),
    }
}

#[cfg(target_os = "windows")]
fn windows_bridge_transport_fps(
    config: &screen_capture::frame_processor::ScreenShareConfig,
) -> u32 {
    config.max_fps()
}

#[cfg(target_os = "windows")]
fn collect_windows_startup_environment(
    diagnostics: &screen_capture::win_capture::SharedWindowsCaptureDiagnostics,
    selected_width: u32,
    selected_height: u32,
    target_fps: u32,
    jpeg_quality: u8,
) {
    use std::collections::{HashMap, HashSet, VecDeque};
    use sysinfo::{Pid, ProcessesToUpdate, System};

    let mut sys = System::new();
    sys.refresh_cpu_all();
    sys.refresh_processes(ProcessesToUpdate::All, false);
    let own_pid = Pid::from_u32(std::process::id());
    let mut children: HashMap<Pid, Vec<Pid>> = HashMap::new();
    for (pid, process) in sys.processes() {
        if let Some(parent) = process.parent() {
            children.entry(parent).or_default().push(*pid);
        }
    }
    let mut tree = HashSet::new();
    let mut queue = VecDeque::new();
    tree.insert(own_pid);
    queue.push_back(own_pid);
    while let Some(pid) = queue.pop_front() {
        if let Some(kids) = children.get(&pid) {
            for child in kids {
                if tree.insert(*child) {
                    queue.push_back(*child);
                }
            }
        }
    }

    let process_tree_rss_mb = tree
        .iter()
        .filter_map(|pid| sys.process(*pid))
        .map(|process| process.memory())
        .sum::<u64>() as f64
        / 1024.0
        / 1024.0;
    let process_tree_cpu_percent = tree
        .iter()
        .filter_map(|pid| sys.process(*pid))
        .map(|process| f64::from(process.cpu_usage()))
        .sum::<f64>();
    let webview_process_count = sys
        .processes()
        .values()
        .filter(|process| {
            process
                .name()
                .to_string_lossy()
                .to_ascii_lowercase()
                .contains("webview")
        })
        .count();
    let overlay_names = [
        "discord",
        "obs",
        "gamebar",
        "xbox",
        "nvidia",
        "amd",
        "radeon",
        "rtss",
        "rivatuner",
        "msiafterburner",
    ];
    let overlay_processes = sys
        .processes()
        .iter()
        .filter_map(|(pid, process)| {
            let name = process.name().to_string_lossy().to_string();
            let lower = name.to_ascii_lowercase();
            overlay_names
                .iter()
                .any(|needle| lower.contains(needle))
                .then(|| screen_capture::win_capture::WindowsOverlayProcess {
                    name,
                    pid: pid.as_u32(),
                })
        })
        .collect::<Vec<_>>();

    if let Ok(mut diag) = diagnostics.lock() {
        diag.environment.global_cpu_percent = Some(f64::from(sys.global_cpu_usage()));
        diag.environment.process_tree_rss_mb = Some(process_tree_rss_mb);
        diag.environment.process_tree_cpu_percent = Some(process_tree_cpu_percent);
        diag.environment.child_process_count = Some(tree.len().saturating_sub(1));
        diag.environment.webview_process_count = Some(webview_process_count);
        diag.environment.monitor_count = None;
        diag.environment.selected_source_width = selected_width;
        diag.environment.selected_source_height = selected_height;
        diag.environment.target_fps = target_fps;
        diag.environment.jpeg_quality = jpeg_quality;
        diag.environment.overlay_processes = overlay_processes;
    }
    collect_windows_video_driver_info(diagnostics);
}

#[cfg(target_os = "windows")]
fn collect_windows_video_driver_info(
    diagnostics: &screen_capture::win_capture::SharedWindowsCaptureDiagnostics,
) {
    let mut child = match std::process::Command::new("powershell.exe")
        .creation_flags(CREATE_NO_WINDOW)
        .args([
            "-NoProfile",
            "-Command",
            "Get-CimInstance Win32_VideoController | Select-Object -First 1 DriverProviderName,DriverVersion,DriverDate | ConvertTo-Json -Compress",
        ])
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()
    {
        Ok(child) => child,
        Err(error) => {
            if let Ok(mut diag) = diagnostics.lock() {
                diag.environment.video_driver_query_error = Some(error.to_string());
            }
            return;
        }
    };
    let started = std::time::Instant::now();
    loop {
        match child.try_wait() {
            Ok(Some(_)) => break,
            Ok(None) if started.elapsed() < std::time::Duration::from_millis(900) => {
                std::thread::sleep(std::time::Duration::from_millis(25));
            }
            Ok(None) => {
                let _ = child.kill();
                if let Ok(mut diag) = diagnostics.lock() {
                    diag.environment.video_driver_query_error =
                        Some("video driver query timed out".to_string());
                }
                return;
            }
            Err(error) => {
                let _ = child.kill();
                if let Ok(mut diag) = diagnostics.lock() {
                    diag.environment.video_driver_query_error = Some(error.to_string());
                }
                return;
            }
        }
    }

    let output = child.wait_with_output();
    match output {
        Ok(output) if output.status.success() => {
            let text = String::from_utf8_lossy(&output.stdout);
            let parsed = serde_json::from_str::<serde_json::Value>(&text);
            if let (Ok(value), Ok(mut diag)) = (parsed, diagnostics.lock()) {
                diag.environment.video_driver_provider = value
                    .get("DriverProviderName")
                    .and_then(|v| v.as_str())
                    .map(ToString::to_string);
                diag.environment.video_driver_version = value
                    .get("DriverVersion")
                    .and_then(|v| v.as_str())
                    .map(ToString::to_string);
                diag.environment.video_driver_date = value
                    .get("DriverDate")
                    .and_then(|v| v.as_str())
                    .map(ToString::to_string);
            }
        }
        Ok(output) => {
            if let Ok(mut diag) = diagnostics.lock() {
                diag.environment.video_driver_query_error =
                    Some(format!("powershell exited with {}", output.status));
            }
        }
        Err(error) => {
            if let Ok(mut diag) = diagnostics.lock() {
                diag.environment.video_driver_query_error = Some(error.to_string());
            }
        }
    }
}

#[cfg(target_os = "windows")]
fn is_windows_capture_setup_error(stage: &str) -> bool {
    stage.ends_with("_error")
        && stage != "cursor_border_config_error"
        && stage != "closed_handler_register_error"
}

#[cfg(target_os = "windows")]
fn wait_for_first_pollable_frame(
    latest_i420_frame: &Arc<Mutex<Option<LatestI420Frame>>>,
    latest_frame: &Arc<Mutex<Option<LatestFrame>>>,
    diagnostics: &screen_capture::win_capture::SharedWindowsCaptureDiagnostics,
    raw_frame_timeout_ms: u64,
    first_pollable_processing_timeout_ms: u64,
) -> Result<(), String> {
    let started = std::time::Instant::now();
    let mut first_frame_processing_started_at = None;
    loop {
        let has_i420 = latest_i420_frame
            .lock()
            .map_err(|e| format!("latest_i420_frame lock: {e}"))?
            .is_some();
        let has_jpeg = latest_frame
            .lock()
            .map_err(|e| format!("latest_frame lock: {e}"))?
            .is_some();
        if has_i420 || has_jpeg {
            return Ok(());
        }
        let setup_error = diagnostics.lock().ok().and_then(|diag| {
            if is_windows_capture_setup_error(&diag.startup_stage) {
                Some(diag.compact_summary())
            } else {
                None
            }
        });
        if let Some(summary) = setup_error {
            return Err(format!(
                "native capture setup failed before a pollable first frame was produced; {summary}"
            ));
        }
        let first_frame_processing_started = diagnostics
            .lock()
            .map(|diag| {
                diag.frame_arrived_callbacks > 0
                    || diag.usable_frames > 0
                    || diag.timing.first_raw_frame_latency_ms.is_some()
                    || diag.timing.first_raw_to_pollable_frame_ms.is_some()
                    || diag.first_raw_frame_latency_ms.is_some()
                    || diag.first_buffered_jpeg_latency_ms.is_some()
                    || diag.startup_step_timings.iter().any(|timing| {
                        timing.stage == "first_frame_arrived"
                            || timing.stage == "first_buffered_jpeg"
                            || timing.stage == "first_pollable_frame_written"
                    })
            })
            .unwrap_or(false);
        if first_frame_processing_started && first_frame_processing_started_at.is_none() {
            first_frame_processing_started_at = Some(std::time::Instant::now());
        }
        let phase_elapsed = first_frame_processing_started_at
            .unwrap_or(started)
            .elapsed();
        let timeout_ms = if first_frame_processing_started {
            first_pollable_processing_timeout_ms
        } else {
            raw_frame_timeout_ms
        };
        if phase_elapsed >= std::time::Duration::from_millis(timeout_ms) {
            let summary = diagnostics
                .lock()
                .map(|mut diag| {
                    let timeout_elapsed_ms = diag
                        .startup_elapsed_ms
                        .unwrap_or(0)
                        .saturating_add(phase_elapsed.as_millis() as u64);
                    let post_start_no_frame = diag.backend == "wgc"
                        && diag.frame_arrived_callbacks == 0
                        && diag
                            .startup_step_timings
                            .iter()
                            .any(|timing| timing.stage == "start_capture_done");
                    if post_start_no_frame {
                        diag.record_startup_stage(
                            "post_start_no_frame_timeout",
                            "awaiting first FrameArrived callback",
                            timeout_elapsed_ms,
                        );
                        format!(
                            "post_start_no_frame=true stalled_after=start_capture_done awaiting_first_frame {}",
                            diag.compact_summary()
                        )
                    } else {
                        let stage = if first_frame_processing_started {
                            "first_pollable_processing_timeout"
                        } else {
                            "first_pollable_frame_timeout"
                        };
                        diag.record_startup_stage(
                            stage,
                            "wait for first pollable frame",
                            timeout_elapsed_ms,
                        );
                        diag.compact_summary()
                    }
                })
                .unwrap_or_else(|_| "diagnostics unavailable".to_string());
            return Err(format!(
                "native capture did not produce a pollable first frame within {timeout_ms}ms; {summary}"
            ));
        }
        std::thread::sleep(std::time::Duration::from_millis(10));
    }
}

pub struct MediaState {
    pub(crate) runtime: Runtime,
    audio: CpalAudioBackend,
    lk: Mutex<Option<Arc<RealLiveKitConnection>>>,
    local_meter_task: Mutex<Option<JoinHandle<()>>>,
    /// CPAL playback buffer for remote audio output.
    playback_buffer: Mutex<Option<AudioBuffer>>,
    /// CPAL capture buffer for mic input.
    capture_buffer: Mutex<Option<AudioBuffer>>,
    /// Shared per-peer volume map — wired to the connection on connect.
    peer_volumes: PeerVolumes,
    /// Shared playback-only passthrough muffle state — wired to the connection on connect.
    passthrough_filters: PassthroughFilters,
    /// Shared denoise filter — wired to LiveKit connection (and future P2P backend) at session start.
    denoise: Mutex<Option<Arc<DenoiseFilter>>>,
    /// Active screen capture backend (Linux only).
    #[cfg(target_os = "linux")]
    pub(crate) screen_capture: Mutex<Option<Box<dyn screen_capture::ScreenCapture>>>,
    #[cfg(target_os = "linux")]
    native_screen_share_metrics: Arc<NativeScreenShareMetrics>,
    #[cfg(target_os = "linux")]
    native_screen_share_stats_task: Mutex<Option<JoinHandle<()>>>,
    #[cfg(target_os = "linux")]
    native_screen_share_heartbeat_active: Arc<AtomicBool>,
    #[cfg(target_os = "linux")]
    native_screen_share_heartbeat_thread: Mutex<Option<StdJoinHandle<()>>>,
    /// Latest raw remote screen-share frame per participant. The LiveKit
    /// callback writes here; the webview polls and encodes only the newest
    /// frame it is ready to render.
    #[cfg(target_os = "linux")]
    remote_screen_share_frames:
        Arc<Mutex<std::collections::HashMap<String, LatestRemoteScreenShareFrame>>>,
    #[cfg(target_os = "linux")]
    remote_screen_share_seq: Arc<AtomicU64>,
    /// Latest raw remote camera frame per participant. Camera frames are kept
    /// separate from screen-share frames because a participant may publish both.
    #[cfg(target_os = "linux")]
    remote_camera_frames:
        Arc<Mutex<std::collections::HashMap<String, LatestRemoteScreenShareFrame>>>,
    #[cfg(target_os = "linux")]
    remote_camera_seq: Arc<AtomicU64>,
    #[cfg(target_os = "linux")]
    local_camera_frame: Arc<Mutex<Option<LatestRemoteScreenShareFrame>>>,
    #[cfg(target_os = "linux")]
    local_camera_seq: Arc<AtomicU64>,
    #[cfg(target_os = "linux")]
    local_camera_capture: Mutex<Option<LocalCameraCapture>>,
    #[cfg(target_os = "linux")]
    linux_mic_handle: Mutex<Option<crate::linux_mic::LinuxMicHandle>>,
    #[cfg(target_os = "linux")]
    linux_playback_handle: Mutex<Option<crate::linux_mic::LinuxPlaybackHandle>>,
    #[cfg(target_os = "linux")]
    remote_screen_share_stream_port: u16,
    #[cfg(target_os = "linux")]
    native_screen_share_viewers: Arc<Mutex<std::collections::HashMap<String, std::process::Child>>>,
    /// Active screen capture backend (Windows).
    #[cfg(target_os = "windows")]
    pub(crate) screen_capture: Mutex<Option<Box<dyn screen_capture::ScreenCapture>>>,
    /// Runtime-adjustable screen share quality configuration.
    /// Shared with capture callbacks via Arc so preset changes take effect
    /// on the next frame without restarting the capture pipeline.
    #[cfg(any(target_os = "linux", target_os = "windows"))]
    pub screen_share_config: Arc<screen_capture::frame_processor::ScreenShareConfig>,
    /// Latest encoded frame for JS polling (Windows JS SDK path only).
    /// The capture thread writes here; `screen_share_poll_frame` reads.
    /// Using a parking_lot Mutex for low-contention fast lock.
    #[cfg(target_os = "windows")]
    pub latest_frame: Arc<Mutex<Option<LatestFrame>>>,
    /// Latest raw I420 frame for JS polling (Windows JS SDK path only).
    #[cfg(target_os = "windows")]
    pub latest_i420_frame: Arc<Mutex<Option<LatestI420Frame>>>,
    #[cfg(target_os = "windows")]
    windows_i420_stream_port: u16,
    /// Enables the temporary JPEG/base64 polling fallback for JS runtimes that
    /// cannot construct VideoFrame directly from I420 bytes.
    #[cfg(target_os = "windows")]
    pub windows_jpeg_fallback_enabled: Arc<AtomicBool>,
    #[cfg(target_os = "windows")]
    native_share_leak_session: Arc<Mutex<Option<NativeShareLeakSession>>>,
    #[cfg(target_os = "windows")]
    windows_capture_diagnostics:
        Arc<Mutex<Option<screen_capture::win_capture::SharedWindowsCaptureDiagnostics>>>,
}

impl MediaState {
    pub fn new() -> Self {
        #[cfg(any(target_os = "linux", target_os = "windows"))]
        let screen_share_config =
            Arc::new(screen_capture::frame_processor::ScreenShareConfig::new());
        #[cfg(target_os = "linux")]
        let remote_screen_share_frames = Arc::new(Mutex::new(std::collections::HashMap::<
            String,
            LatestRemoteScreenShareFrame,
        >::new()));
        #[cfg(target_os = "linux")]
        let remote_screen_share_stream_port = start_remote_screen_share_stream_server(
            Arc::clone(&remote_screen_share_frames),
            Arc::clone(&screen_share_config),
        )
        .unwrap_or(0);
        #[cfg(target_os = "linux")]
        let remote_camera_frames = Arc::new(Mutex::new(std::collections::HashMap::<
            String,
            LatestRemoteScreenShareFrame,
        >::new()));
        #[cfg(target_os = "linux")]
        let local_camera_frame = Arc::new(Mutex::new(None));

        #[cfg(target_os = "windows")]
        let latest_i420_frame = Arc::new(Mutex::new(None));
        #[cfg(target_os = "windows")]
        let windows_capture_diagnostics = Arc::new(Mutex::new(None));
        #[cfg(target_os = "windows")]
        let windows_i420_stream_port = start_windows_i420_stream_server(
            Arc::clone(&latest_i420_frame),
            Arc::clone(&windows_capture_diagnostics),
        )
        .unwrap_or(0);

        Self {
            runtime: Builder::new_multi_thread()
                .enable_all()
                .build()
                .expect("failed to create media runtime"),
            audio: CpalAudioBackend::new(),
            lk: Mutex::new(None),
            local_meter_task: Mutex::new(None),
            playback_buffer: Mutex::new(None),
            capture_buffer: Mutex::new(None),
            peer_volumes: PeerVolumes::new(),
            passthrough_filters: PassthroughFilters::new(),
            denoise: Mutex::new(None),
            #[cfg(target_os = "linux")]
            screen_capture: Mutex::new(None),
            #[cfg(target_os = "linux")]
            native_screen_share_metrics: Arc::new(NativeScreenShareMetrics::new()),
            #[cfg(target_os = "linux")]
            native_screen_share_stats_task: Mutex::new(None),
            #[cfg(target_os = "linux")]
            native_screen_share_heartbeat_active: Arc::new(AtomicBool::new(false)),
            #[cfg(target_os = "linux")]
            native_screen_share_heartbeat_thread: Mutex::new(None),
            #[cfg(target_os = "linux")]
            remote_screen_share_frames,
            #[cfg(target_os = "linux")]
            remote_screen_share_seq: Arc::new(AtomicU64::new(0)),
            #[cfg(target_os = "linux")]
            remote_camera_frames,
            #[cfg(target_os = "linux")]
            remote_camera_seq: Arc::new(AtomicU64::new(0)),
            #[cfg(target_os = "linux")]
            local_camera_frame,
            #[cfg(target_os = "linux")]
            local_camera_seq: Arc::new(AtomicU64::new(0)),
            #[cfg(target_os = "linux")]
            local_camera_capture: Mutex::new(None),
            #[cfg(target_os = "linux")]
            linux_mic_handle: Mutex::new(None),
            #[cfg(target_os = "linux")]
            linux_playback_handle: Mutex::new(None),
            #[cfg(target_os = "linux")]
            remote_screen_share_stream_port,
            #[cfg(target_os = "linux")]
            native_screen_share_viewers: Arc::new(Mutex::new(std::collections::HashMap::new())),
            #[cfg(target_os = "windows")]
            screen_capture: Mutex::new(None),
            #[cfg(any(target_os = "linux", target_os = "windows"))]
            screen_share_config,
            #[cfg(target_os = "windows")]
            latest_frame: Arc::new(Mutex::new(None)),
            #[cfg(target_os = "windows")]
            latest_i420_frame,
            #[cfg(target_os = "windows")]
            windows_i420_stream_port,
            #[cfg(target_os = "windows")]
            windows_jpeg_fallback_enabled: Arc::new(AtomicBool::new(false)),
            #[cfg(target_os = "windows")]
            native_share_leak_session: Arc::new(Mutex::new(None)),
            #[cfg(target_os = "windows")]
            windows_capture_diagnostics,
        }
    }

    #[cfg(target_os = "linux")]
    /// Access the LiveKit connection mutex (for use by other modules like audio_capture).
    pub fn lk(&self) -> Result<LkGuard<'_>, std::sync::PoisonError<LkGuard<'_>>> {
        self.lk.lock()
    }

    /// Returns the shared DenoiseFilter if a media session is active.
    pub fn denoise_filter(&self) -> Option<Arc<DenoiseFilter>> {
        self.denoise.lock().ok()?.clone()
    }

    /// Store the user's preferred device name so the next `ensure_audio_streams()`
    /// call opens the correct CPAL device.  `raw_name` is the device name without
    /// the "input:" / "output:" prefix.
    pub fn set_selected_device(&self, kind: &str, raw_name: &str) {
        if kind == "input" {
            self.audio.set_input_device_name(Some(raw_name.to_string()));
        } else {
            self.audio
                .set_output_device_name(Some(raw_name.to_string()));
        }
    }

    /// Update the microphone input gain (0.0–1.0). Takes effect immediately
    /// without restarting the CPAL streams.
    pub fn set_input_gain(&self, gain: f32) {
        self.audio.set_input_gain(gain);
    }

    pub fn ensure_audio_streams(&self) -> Result<(), String> {
        // Stop any existing Linux mic thread before clearing CPAL streams.
        #[cfg(target_os = "linux")]
        {
            if let Ok(mut guard) = self.linux_mic_handle.lock() {
                if let Some(handle) = guard.take() {
                    handle.stop();
                }
            }
            if let Ok(mut guard) = self.linux_playback_handle.lock() {
                if let Some(handle) = guard.take() {
                    handle.stop();
                }
            }
        }

        self.audio
            .stop()
            .map_err(|err| format!("failed to stop audio backend: {err}"))?;

        // On Linux, use pa_simple mic capture to bypass the bundled libasound.so.2
        // in AppImage builds (which lacks the PipeWire ALSA plugin).
        #[cfg(not(target_os = "linux"))]
        self.audio
            .capture_mic()
            .map_err(|err| format!("failed to start input device: {err}"))?;

        #[cfg(target_os = "linux")]
        {
            let handle = crate::linux_mic::start_linux_mic_capture(
                self.audio.capture_buffer.clone(),
                self.audio.input_gain_arc(),
            )
            .map_err(|err| format!("failed to start input device: {err}"))?;
            if let Ok(mut guard) = self.linux_mic_handle.lock() {
                *guard = Some(handle);
            }
        }

        #[cfg(not(target_os = "linux"))]
        self.audio
            .play_remote(AudioTrack {
                id: "native-playback".to_string(),
            })
            .map_err(|err| format!("failed to start output device: {err}"))?;

        #[cfg(target_os = "linux")]
        {
            let handle = crate::linux_mic::start_linux_playback(self.audio.playback_buffer.clone())
                .map_err(|err| format!("failed to start output device: {err}"))?;
            if let Ok(mut guard) = self.linux_playback_handle.lock() {
                *guard = Some(handle);
            }
        }

        let mut capture_guard = self
            .capture_buffer
            .lock()
            .map_err(|err| format!("capture lock: {err}"))?;
        *capture_guard = Some(self.audio.capture_buffer.clone());
        drop(capture_guard);

        let mut playback_guard = self
            .playback_buffer
            .lock()
            .map_err(|err| format!("playback lock: {err}"))?;
        *playback_guard = Some(self.audio.playback_buffer.clone());
        drop(playback_guard);

        let lk_guard = self
            .lk
            .lock()
            .map_err(|err| format!("livekit lock: {err}"))?;
        if let Some(conn) = lk_guard.as_ref() {
            // LiveKit SDK setters internally call `tokio::task::spawn`, which
            // panics if invoked outside a Tokio runtime context. This method
            // is reached from sync Tauri commands on the main thread, which
            // doesn't have one in TLS — enter the media runtime first.
            let _enter = self.runtime.enter();
            conn.set_capture_buffer(self.audio.capture_buffer.clone());
            conn.set_playback_buffer(self.audio.playback_buffer.clone());
            conn.set_peer_volumes(self.peer_volumes.clone());
            conn.set_passthrough_filters(self.passthrough_filters.clone());
        }

        Ok(())
    }

    pub fn start_local_meter(&self, app: AppHandle) -> Result<(), String> {
        if let Ok(mut task_guard) = self.local_meter_task.lock() {
            if let Some(task) = task_guard.take() {
                task.abort();
            }
        }

        let capture_buffer = self.audio.capture_buffer.clone();
        let handle = self.runtime.spawn(async move {
            use tokio::time::{interval, Duration, MissedTickBehavior};

            let mut ticker = interval(Duration::from_millis(50));
            ticker.set_missed_tick_behavior(MissedTickBehavior::Skip);
            let mut samples = vec![0.0f32; 960];

            loop {
                ticker.tick().await;

                let copied = capture_buffer.peek_recent(&mut samples);
                let window = &samples[..copied];
                let rms = if window.is_empty() {
                    0.0
                } else {
                    let sum_sq: f32 = window.iter().map(|s| s * s).sum();
                    (sum_sq / window.len() as f32).sqrt()
                };

                let _ = app.emit(
                    "media-event",
                    MediaEvent::LocalAudioLevel {
                        rms_level: rms,
                        is_speaking: rms > 0.02,
                    },
                );
            }
        });

        let mut task_guard = self
            .local_meter_task
            .lock()
            .map_err(|err| format!("local meter lock: {err}"))?;
        *task_guard = Some(handle);
        Ok(())
    }

    #[cfg(target_os = "linux")]
    fn start_native_screen_share_stats(&self, app: AppHandle) -> Result<(), String> {
        self.native_screen_share_metrics.start();

        if let Ok(mut task_guard) = self.native_screen_share_stats_task.lock() {
            if let Some(task) = task_guard.take() {
                task.abort();
            }
        }

        let metrics = Arc::clone(&self.native_screen_share_metrics);
        let handle = self.runtime.spawn(async move {
            use tokio::time::{interval, Duration, MissedTickBehavior};

            let mut ticker = interval(Duration::from_secs(1));
            ticker.set_missed_tick_behavior(MissedTickBehavior::Skip);
            let mut prev_frames = 0u64;

            loop {
                ticker.tick().await;

                if !metrics.active.load(Ordering::Relaxed) {
                    break;
                }

                let total_frames = metrics.emitted_frames.load(Ordering::Relaxed);
                let fps = total_frames.saturating_sub(prev_frames) as f64;
                prev_frames = total_frames;

                let _ = app.emit(
                    "media-event",
                    MediaEvent::ScreenShareStats {
                        bitrate_kbps: 0,
                        fps,
                        quality_limitation_reason: "native".to_string(),
                        packet_loss_percent: 0.0,
                        frame_width: metrics.frame_width.load(Ordering::Relaxed),
                        frame_height: metrics.frame_height.load(Ordering::Relaxed),
                        pli_count: 0,
                        nack_count: 0,
                        available_bandwidth_kbps: 0,
                    },
                );
            }
        });

        let mut task_guard = self
            .native_screen_share_stats_task
            .lock()
            .map_err(|err| format!("native screen share stats lock: {err}"))?;
        *task_guard = Some(handle);
        Ok(())
    }

    #[cfg(target_os = "linux")]
    fn stop_native_screen_share_stats(&self) {
        self.native_screen_share_metrics.stop();
        if let Ok(mut task_guard) = self.native_screen_share_stats_task.lock() {
            if let Some(task) = task_guard.take() {
                task.abort();
            }
        }
        self.stop_native_screen_share_heartbeat();
    }

    #[cfg(target_os = "linux")]
    fn start_native_screen_share_heartbeat(
        &self,
        conn_weak: std::sync::Weak<RealLiveKitConnection>,
        latest_frame: Arc<Mutex<Option<HeartbeatFrame>>>,
        last_capture_ms: Arc<AtomicU64>,
        config: Arc<screen_capture::frame_processor::ScreenShareConfig>,
    ) {
        self.stop_native_screen_share_heartbeat();
        self.native_screen_share_heartbeat_active
            .store(true, Ordering::Relaxed);

        let active = Arc::clone(&self.native_screen_share_heartbeat_active);
        let metrics = Arc::clone(&self.native_screen_share_metrics);
        let handle = std::thread::Builder::new()
            .name("native-screen-share-heartbeat".into())
            .spawn(move || {
                while active.load(Ordering::Relaxed) {
                    let frame_interval_ms = (1000u64 / config.max_fps().max(1) as u64).max(1);
                    std::thread::sleep(std::time::Duration::from_millis(frame_interval_ms));
                    if !active.load(Ordering::Relaxed) {
                        break;
                    }

                    if last_capture_ms.load(Ordering::Relaxed) == 0 {
                        continue;
                    }

                    let Some(conn) = conn_weak.upgrade() else {
                        break;
                    };
                    let frame = latest_frame.lock().ok().and_then(|guard| guard.clone());
                    let Some(frame) = frame else {
                        continue;
                    };

                    if let Err(e) =
                        conn.feed_video_frame(frame.data.as_slice(), frame.width, frame.height)
                    {
                        log::warn!("{LOG} screen share heartbeat feed failed: {e}");
                    } else {
                        metrics.record_frame(frame.width, frame.height);
                    }
                }
            });

        match handle {
            Ok(handle) => {
                if let Ok(mut slot) = self.native_screen_share_heartbeat_thread.lock() {
                    *slot = Some(handle);
                }
            }
            Err(e) => {
                self.native_screen_share_heartbeat_active
                    .store(false, Ordering::Relaxed);
                log::warn!("{LOG} failed to start screen share heartbeat thread: {e}");
            }
        }
    }

    #[cfg(target_os = "linux")]
    fn stop_native_screen_share_heartbeat(&self) {
        self.native_screen_share_heartbeat_active
            .store(false, Ordering::Relaxed);
        if let Ok(mut slot) = self.native_screen_share_heartbeat_thread.lock() {
            if let Some(handle) = slot.take() {
                let _ = handle.join();
            }
        }
    }
}

// ─── IPC Commands ──────────────────────────────────────────────────

/// Connect to a LiveKit SFU room via the native Rust SDK.
///
/// Creates a new RealLiveKitConnection, wires audio buffers and event
/// callbacks, then connects. Emits `media-event` Tauri events for
/// state transitions and audio levels.
#[tauri::command]
pub fn media_connect(
    url: String,
    token: String,
    denoise_enabled: bool,
    state: State<'_, MediaState>,
    app: AppHandle,
) -> Result<(), String> {
    log::info!("{LOG} connect requested: url={url}");

    // Tear down any existing connection
    {
        let mut lk_guard = state.lk.lock().map_err(|e| format!("lock: {e}"))?;
        if let Some(old) = lk_guard.take() {
            let _ = state.runtime.block_on(async move { old.disconnect() });
        }
    }

    state.ensure_audio_streams()?;
    state.start_local_meter(app.clone())?;

    // Create fresh connection (Arc-wrapped for safe sharing with screen capture callback)
    let conn = Arc::new(
        state
            .runtime
            .block_on(async { RealLiveKitConnection::new() }),
    );

    // Wire CPAL buffers and peer volumes
    if let Ok(pb) = state.playback_buffer.lock() {
        if let Some(buf) = pb.as_ref() {
            conn.set_playback_buffer(buf.clone());
        }
    }
    if let Ok(cb) = state.capture_buffer.lock() {
        if let Some(buf) = cb.as_ref() {
            conn.set_capture_buffer(buf.clone());
        }
    }
    conn.set_peer_volumes(state.peer_volumes.clone());
    conn.set_passthrough_filters(state.passthrough_filters.clone());

    // Create and wire denoise filter (from persisted preference)
    let denoise = Arc::new(DenoiseFilter::new(denoise_enabled));
    conn.set_denoise_filter(denoise.clone());
    {
        let mut dn_guard = state
            .denoise
            .lock()
            .map_err(|e| format!("denoise lock: {e}"))?;
        *dn_guard = Some(denoise);
    }

    // Wire audio frame callback → emit audio levels to frontend
    let app_levels = app.clone();
    conn.on_audio_frame(Box::new(move |identity, samples| {
        // Compute RMS from samples
        let rms = if samples.is_empty() {
            0.0
        } else {
            let sum_sq: f32 = samples.iter().map(|s| s * s).sum();
            (sum_sq / samples.len() as f32).sqrt()
        };
        let is_speaking = rms > 0.02;

        let entry = AudioLevelEntry {
            identity: identity.to_string(),
            rms_level: rms,
            is_speaking,
        };
        let _ = app_levels.emit(
            "media-event",
            MediaEvent::AudioLevels {
                levels: vec![entry],
            },
        );
    }));

    // Wire stats callback → emit transport stats (RTT, loss, jitter) to frontend
    let app_stats = app.clone();
    conn.on_stats(Box::new(move |rtt_ms, packet_loss_percent, jitter_ms| {
        let _ = app_stats.emit(
            "media-event",
            MediaEvent::Stats {
                rtt_ms,
                packet_loss_percent,
                jitter_ms,
            },
        );
    }));

    // Wire video frame callback → store the newest raw frame per participant.
    // The webview polls this buffer when it is ready to render. That preserves
    // smoothness when the machine can keep up, but prevents stale frame events
    // from piling up behind the UI thread.
    #[cfg(target_os = "linux")]
    {
        let app_video = app.clone();
        let latest_remote_frames = Arc::clone(&state.remote_screen_share_frames);
        let remote_seq = Arc::clone(&state.remote_screen_share_seq);
        conn.on_video_frame(Box::new(move |identity, rgba_data, width, height| {
            let seq = remote_seq.fetch_add(1, Ordering::Relaxed) + 1;
            let identity_owned = identity.to_string();
            let was_new_share = {
                let mut frames = match latest_remote_frames.lock() {
                    Ok(g) => g,
                    Err(_) => return,
                };
                let was_new = !frames.contains_key(identity);
                frames.insert(
                    identity_owned.clone(),
                    LatestRemoteScreenShareFrame {
                        data: Arc::new(rgba_data.to_vec()),
                        width,
                        height,
                        seq,
                    },
                );
                was_new
            };

            if was_new_share {
                let _ = app_video.emit_to(
                    "main",
                    "screen_share_available",
                    ScreenShareAvailablePayload {
                        identity: identity_owned,
                    },
                );
            }
        }));

        let app_ended = app.clone();
        let latest_remote_frames_cleanup = Arc::clone(&state.remote_screen_share_frames);
        conn.on_video_track_ended(Box::new(move |identity| {
            log::info!("{LOG} remote screen share ended: {identity}");
            if let Ok(mut map) = latest_remote_frames_cleanup.lock() {
                map.remove(identity);
            }
            let _ = app_ended.emit_to(
                "main",
                "screen_share_ended",
                serde_json::json!({
                    "identity": identity,
                }),
            );
        }));

        let app_audio_available = app.clone();
        conn.on_screen_share_audio_available(Box::new(move |identity| {
            log::info!("{LOG} remote screen share audio available: {identity}");
            let _ = app_audio_available.emit_to(
                "main",
                "screen_share_audio_available",
                ScreenShareAudioPayload {
                    identity: identity.to_string(),
                },
            );
        }));

        let app_audio_ended = app.clone();
        conn.on_screen_share_audio_ended(Box::new(move |identity| {
            log::info!("{LOG} remote screen share audio ended: {identity}");
            let _ = app_audio_ended.emit_to(
                "main",
                "screen_share_audio_ended",
                ScreenShareAudioPayload {
                    identity: identity.to_string(),
                },
            );
        }));

        let app_camera = app.clone();
        let latest_remote_camera_frames = Arc::clone(&state.remote_camera_frames);
        let remote_camera_seq = Arc::clone(&state.remote_camera_seq);
        conn.on_camera_frame(Box::new(move |identity, rgba_data, width, height| {
            let seq = remote_camera_seq.fetch_add(1, Ordering::Relaxed) + 1;
            let identity_owned = identity.to_string();
            let was_new_camera = {
                let mut frames = match latest_remote_camera_frames.lock() {
                    Ok(g) => g,
                    Err(_) => return,
                };
                let was_new = !frames.contains_key(identity);
                frames.insert(
                    identity_owned.clone(),
                    LatestRemoteScreenShareFrame {
                        data: Arc::new(rgba_data.to_vec()),
                        width,
                        height,
                        seq,
                    },
                );
                was_new
            };

            if was_new_camera {
                let _ = app_camera.emit_to(
                    "main",
                    "camera_available",
                    CameraAvailablePayload {
                        identity: identity_owned,
                    },
                );
            }
        }));

        let app_camera_ended = app.clone();
        let latest_remote_camera_cleanup = Arc::clone(&state.remote_camera_frames);
        conn.on_camera_track_ended(Box::new(move |identity| {
            log::info!("{LOG} remote camera ended: {identity}");
            if let Ok(mut map) = latest_remote_camera_cleanup.lock() {
                map.remove(identity);
            }
            let _ = app_camera_ended.emit_to(
                "main",
                "camera_ended",
                serde_json::json!({
                    "identity": identity,
                }),
            );
        }));
    }

    // Connect
    let app_connected = app.clone();
    let app_failed = app.clone();

    match state.runtime.block_on(async { conn.connect(&url, &token) }) {
        Ok(()) => {
            let publish_result = state.runtime.block_on(async {
                conn.set_mic_enabled(false)
                    .map_err(|err| format!("failed to disable mic before publish: {err}"))?;
                conn.publish_audio(&AudioTrack {
                    id: "native-mic".to_string(),
                })
                .map_err(|err| format!("{err}"))
            });
            if let Err(err) = publish_result {
                let reason = err.to_string();
                log::error!("{LOG} publish failed: {reason}");
                let _ = state.runtime.block_on(async { conn.disconnect() });
                let _ = app_failed.emit(
                    "media-event",
                    MediaEvent::Failed {
                        reason: reason.clone(),
                    },
                );
                return Err(reason);
            }
            log::info!("{LOG} connected to {url}");
            let _ = app_connected.emit("media-event", MediaEvent::Connected);
        }
        Err(e) => {
            let reason = format!("{e}");
            log::error!("{LOG} connect failed: {reason}");
            let _ = app_failed.emit(
                "media-event",
                MediaEvent::Failed {
                    reason: reason.clone(),
                },
            );
            return Err(reason);
        }
    }

    // Store the connection
    let mut lk_guard = state.lk.lock().map_err(|e| format!("lock: {e}"))?;
    *lk_guard = Some(conn);

    Ok(())
}

/// Disconnect from the LiveKit SFU room.
///
/// Stops screen capture (if active) before dropping the connection to
/// prevent use-after-free in the frame callback.
#[tauri::command]
pub fn media_disconnect(state: State<'_, MediaState>, app: AppHandle) -> Result<(), String> {
    log::info!("{LOG} disconnect requested");

    // Stop screen capture FIRST — the frame callback holds a Weak<> to the
    // connection, so we must stop it before dropping the Arc.
    #[cfg(target_os = "linux")]
    {
        state.stop_native_screen_share_stats();
        let camera_capture = {
            let mut camera_guard = state
                .local_camera_capture
                .lock()
                .map_err(|e| format!("local_camera_capture lock: {e}"))?;
            camera_guard.take()
        };
        if let Some(capture) = camera_capture {
            capture.stop();
            log::info!("{LOG} media_disconnect: stopped active camera capture");
        }
        let capture = {
            let mut sc_guard = state
                .screen_capture
                .lock()
                .map_err(|e| format!("screen_capture lock: {e}"))?;
            sc_guard.take()
        };
        if let Some(cap) = capture {
            cap.stop();
            log::info!("{LOG} media_disconnect: stopped active screen capture");
        }
    }

    let mut lk_guard = state.lk.lock().map_err(|e| format!("lock: {e}"))?;
    if let Some(conn) = lk_guard.take() {
        let _ = state.runtime.block_on(async move { conn.disconnect() });
    }
    // Clear the denoise filter — no session active.
    if let Ok(mut dn_guard) = state.denoise.lock() {
        *dn_guard = None;
    }
    #[cfg(target_os = "linux")]
    {
        if let Ok(mut frames) = state.remote_screen_share_frames.lock() {
            frames.clear();
        }
        if let Ok(mut frames) = state.remote_camera_frames.lock() {
            frames.clear();
        }
        if let Ok(mut frame) = state.local_camera_frame.lock() {
            *frame = None;
        }
    }
    if let Ok(mut task_guard) = state.local_meter_task.lock() {
        if let Some(task) = task_guard.take() {
            task.abort();
        }
    }
    #[cfg(target_os = "linux")]
    {
        if let Ok(mut guard) = state.linux_mic_handle.lock() {
            if let Some(handle) = guard.take() {
                handle.stop();
            }
        }
        if let Ok(mut guard) = state.linux_playback_handle.lock() {
            if let Some(handle) = guard.take() {
                handle.stop();
            }
        }
    }
    let _ = state.audio.stop();
    let _ = app.emit("media-event", MediaEvent::Disconnected);
    Ok(())
}

/// Enable or disable the local microphone.
///
/// Note: RealLiveKitConnection manages mic via the capture buffer and
/// NativeAudioSource. This command controls whether audio frames are
/// published (mute = stop pumping frames).
#[tauri::command]
pub fn media_set_mic_enabled(enabled: bool, state: State<'_, MediaState>) -> Result<(), String> {
    log::info!("{LOG} set mic enabled: {enabled}");
    // LiveKit SDK setters internally `tokio::task::spawn`, which panics when
    // called from a thread without a Tokio runtime in TLS. Enter the media
    // runtime for the duration of the call.
    let _enter = state.runtime.enter();
    let lk_guard = state.lk.lock().map_err(|e| format!("lock: {e}"))?;
    if let Some(conn) = lk_guard.as_ref() {
        conn.set_mic_enabled(enabled)
            .map_err(|e| format!("failed to set mic enabled: {e}"))?;
    }
    Ok(())
}

#[cfg(target_os = "linux")]
fn default_linux_camera_device() -> Result<String, String> {
    for idx in 0..10 {
        let path = format!("/dev/video{idx}");
        if std::path::Path::new(&path).exists() {
            return Ok(path);
        }
    }
    Err("no camera device found under /dev/video*".to_string())
}

#[cfg(target_os = "linux")]
fn resolve_linux_camera_device(device_id: Option<String>) -> Result<String, String> {
    match device_id.filter(|value| !value.trim().is_empty()) {
        Some(value) => {
            if value.starts_with("/dev/video") {
                Ok(value)
            } else {
                Err(format!("unsupported native camera device id: {value}"))
            }
        }
        None => default_linux_camera_device(),
    }
}

#[cfg(target_os = "linux")]
fn spawn_ffmpeg_camera_capture(
    device: &str,
    width: u32,
    height: u32,
    fps: u32,
    force_input_mode: bool,
) -> std::io::Result<Child> {
    let mut command = Command::new("ffmpeg");
    command.args(["-hide_banner", "-loglevel", "error", "-nostdin"]);
    command.args(["-fflags", "nobuffer", "-flags", "low_delay"]);
    command.args(["-f", "v4l2"]);
    if force_input_mode {
        command.args([
            "-framerate",
            &fps.to_string(),
            "-video_size",
            &format!("{width}x{height}"),
        ]);
    }
    command.args(["-i", device]);
    command.args([
        "-an",
        "-vf",
        &format!(
            "fps={fps},scale={width}:{height}:force_original_aspect_ratio=decrease,pad={width}:{height}:(ow-iw)/2:(oh-ih)/2,setsar=1,format=rgba"
        ),
        "-f",
        "rawvideo",
        "-pix_fmt",
        "rgba",
        "-s",
        &format!("{width}x{height}"),
        "-",
    ]);
    command
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
}

/// Start local Linux webcam capture and publish it as a LiveKit Camera track.
#[tauri::command]
#[cfg(target_os = "linux")]
pub fn media_camera_start(
    device_id: Option<String>,
    width: u32,
    height: u32,
    fps: u32,
    state: State<'_, MediaState>,
) -> Result<(), String> {
    let width = width.clamp(160, 1280);
    let height = height.clamp(120, 720);
    let fps = fps.clamp(5, 30);
    let device = resolve_linux_camera_device(device_id)?;

    if Command::new("ffmpeg").arg("-version").output().is_err() {
        return Err(
            "Linux native camera capture requires ffmpeg with v4l2 support in PATH".to_string(),
        );
    }

    let conn = {
        let lk_guard = state.lk.lock().map_err(|e| format!("lock: {e}"))?;
        lk_guard
            .as_ref()
            .cloned()
            .ok_or_else(|| "media is not connected".to_string())?
    };

    {
        let mut existing = state
            .local_camera_capture
            .lock()
            .map_err(|e| format!("local_camera_capture lock: {e}"))?;
        if let Some(capture) = existing.take() {
            capture.stop();
        }
    }
    if let Ok(mut frame) = state.local_camera_frame.lock() {
        *frame = None;
    }

    state
        .runtime
        .block_on(async { conn.publish_camera_video(width, height) })
        .map_err(|e| format!("{e}"))?;

    let active = Arc::new(AtomicBool::new(true));
    let child_slot: Arc<Mutex<Option<Child>>> = Arc::new(Mutex::new(None));
    let local_frame = Arc::clone(&state.local_camera_frame);
    let local_seq = Arc::clone(&state.local_camera_seq);
    let active_thread = Arc::clone(&active);
    let child_thread = Arc::clone(&child_slot);
    let conn_thread = Arc::clone(&conn);
    let frame_len = (width as usize) * (height as usize) * 4;

    let thread = std::thread::spawn(move || {
        let mut child = match spawn_ffmpeg_camera_capture(&device, width, height, fps, true)
            .or_else(|_| spawn_ffmpeg_camera_capture(&device, width, height, fps, false))
        {
            Ok(child) => child,
            Err(err) => {
                log::warn!("{LOG} failed to start ffmpeg camera capture: {err}");
                let _ = conn_thread.unpublish_camera_video();
                return;
            }
        };

        let mut stdout = match child.stdout.take() {
            Some(stdout) => stdout,
            None => {
                log::warn!("{LOG} ffmpeg camera capture had no stdout");
                let _ = child.kill();
                let _ = child.wait();
                let _ = conn_thread.unpublish_camera_video();
                return;
            }
        };

        if let Ok(mut slot) = child_thread.lock() {
            *slot = Some(child);
        }

        let mut frame = vec![0u8; frame_len];
        while active_thread.load(Ordering::Relaxed) {
            if let Err(err) = stdout.read_exact(&mut frame) {
                if active_thread.load(Ordering::Relaxed) {
                    log::warn!("{LOG} camera frame read failed: {err}");
                }
                break;
            }

            let rgba = frame.clone();
            let seq = local_seq.fetch_add(1, Ordering::Relaxed) + 1;
            if let Ok(mut latest) = local_frame.lock() {
                *latest = Some(LatestRemoteScreenShareFrame {
                    data: Arc::new(rgba.clone()),
                    width,
                    height,
                    seq,
                });
            }
            if let Err(err) = conn_thread.feed_camera_frame(&rgba, width, height) {
                log::warn!("{LOG} feed_camera_frame failed: {err}");
                break;
            }
        }

        if let Ok(mut slot) = child_thread.lock() {
            if let Some(mut child) = slot.take() {
                let _ = child.kill();
                let _ = child.wait();
            }
        }
        let _ = conn_thread.unpublish_camera_video();
    });

    *state
        .local_camera_capture
        .lock()
        .map_err(|e| format!("local_camera_capture lock: {e}"))? = Some(LocalCameraCapture {
        active,
        child: child_slot,
        thread: Some(thread),
    });

    log::info!("{LOG} linux camera capture started");
    Ok(())
}

#[tauri::command]
#[cfg(not(target_os = "linux"))]
pub fn media_camera_start(
    _device_id: Option<String>,
    _width: u32,
    _height: u32,
    _fps: u32,
) -> Result<(), String> {
    Err("native camera capture is only available on Linux".to_string())
}

#[tauri::command]
#[cfg(target_os = "linux")]
pub fn media_camera_stop(state: State<'_, MediaState>) -> Result<(), String> {
    let capture = {
        let mut guard = state
            .local_camera_capture
            .lock()
            .map_err(|e| format!("local_camera_capture lock: {e}"))?;
        guard.take()
    };
    if let Some(capture) = capture {
        capture.stop();
    } else if let Ok(lk_guard) = state.lk.lock() {
        if let Some(conn) = lk_guard.as_ref() {
            let _ = state
                .runtime
                .block_on(async { conn.unpublish_camera_video() });
        }
    }
    if let Ok(mut frame) = state.local_camera_frame.lock() {
        *frame = None;
    }
    Ok(())
}

#[tauri::command]
#[cfg(not(target_os = "linux"))]
pub fn media_camera_stop() -> Result<(), String> {
    Ok(())
}

/// Enable or disable nnnoiseless noise suppression at runtime.
///
/// If a media session is active, flips the shared `AtomicBool` on the
/// `DenoiseFilter` immediately. If no session is active, this is a no-op
/// (the persisted preference handles the startup case).
#[tauri::command]
pub fn media_set_denoise_enabled(enabled: bool, state: State<'_, MediaState>) {
    if let Some(denoise) = state.denoise_filter() {
        denoise.set_enabled(enabled);
    }
}

/// Set per-participant volume (0–100).
#[tauri::command]
pub fn media_set_participant_volume(
    id: String,
    level: u32,
    state: State<'_, MediaState>,
) -> Result<(), String> {
    let clamped = level.min(100) as u8;
    state.peer_volumes.set(&id, clamped);
    Ok(())
}

/// Mark whether a participant's mic audio should receive passthrough muffle filtering.
#[tauri::command]
pub fn media_set_participant_passthrough(
    id: String,
    enabled: bool,
    state: State<'_, MediaState>,
) -> Result<(), String> {
    state
        .passthrough_filters
        .set_participant_passthrough(&id, enabled);
    Ok(())
}

/// Set global passthrough muffle filter settings (host-controlled via signaling).
#[tauri::command]
pub fn media_set_passthrough_filter_settings(
    enabled: bool,
    strength: u32,
    state: State<'_, MediaState>,
) -> Result<(), String> {
    state
        .passthrough_filters
        .set_settings(enabled, strength.min(100) as u8);
    Ok(())
}

/// Set per-participant screen share audio volume (0–100).
#[tauri::command]
pub fn media_set_screen_share_audio_volume(
    id: String,
    level: u32,
    state: State<'_, MediaState>,
) -> Result<(), String> {
    let clamped = level.min(100) as u8;
    state
        .peer_volumes
        .set(&screen_share_audio_volume_key(&id), clamped);
    Ok(())
}

/// Allow a participant's remote screen share audio to enter the native mix.
#[tauri::command]
pub fn media_attach_screen_share_audio(
    id: String,
    state: State<'_, MediaState>,
) -> Result<(), String> {
    // LiveKit SDK setters internally `tokio::task::spawn`, which panics when
    // called from a thread without a Tokio runtime in TLS. This was the
    // root cause of "watch another stream → app freezes → crash" on Linux.
    let _enter = state.runtime.enter();
    let lk_guard = state.lk.lock().map_err(|e| format!("lock: {e}"))?;
    if let Some(conn) = lk_guard.as_ref() {
        conn.set_screen_share_audio_enabled(&id, true);
    }
    Ok(())
}

/// Block a participant's remote screen share audio from the native mix.
#[tauri::command]
pub fn media_detach_screen_share_audio(
    id: String,
    state: State<'_, MediaState>,
) -> Result<(), String> {
    let _enter = state.runtime.enter();
    let lk_guard = state.lk.lock().map_err(|e| format!("lock: {e}"))?;
    if let Some(conn) = lk_guard.as_ref() {
        conn.set_screen_share_audio_enabled(&id, false);
    }
    Ok(())
}

/// Set master volume (0–100).
#[tauri::command]
pub fn media_set_master_volume(level: u32, state: State<'_, MediaState>) -> Result<(), String> {
    let clamped = level.min(100) as u8;
    if let Ok(pb) = state.playback_buffer.lock() {
        if let Some(buf) = pb.as_ref() {
            buf.set_volume(clamped);
        }
    }
    Ok(())
}

/// Check if native media is currently connected.
#[tauri::command]
pub fn media_is_connected(state: State<'_, MediaState>) -> Result<bool, String> {
    let lk_guard = state.lk.lock().map_err(|e| format!("lock: {e}"))?;
    Ok(lk_guard.as_ref().is_some_and(|c| c.is_available()))
}

// ─── Screen Share IPC Commands (Linux only) ────────────────────────

/// Start screen sharing via the native capture pipeline (Linux only).
///
/// Returns `Ok(true)` if capture started, `Ok(false)` if the user cancelled
/// the portal picker, or `Err(msg)` on failure.
///
/// Double-start guard: if a capture session is already active, returns
/// `Ok(true)` immediately without starting a second pipeline (req 2.4.1).
///
/// On non-Linux platforms this command returns an error — screen sharing
/// is handled by the LiveKit JS SDK via `getDisplayMedia`.
#[tauri::command]
#[cfg(target_os = "linux")]
pub fn screen_share_start(state: State<'_, MediaState>, app: AppHandle) -> Result<bool, String> {
    let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        screen_share_start_impl(state, app)
    }));

    match result {
        Ok(result) => result,
        Err(payload) => {
            let panic_msg = if let Some(msg) = payload.downcast_ref::<&str>() {
                (*msg).to_string()
            } else if let Some(msg) = payload.downcast_ref::<String>() {
                msg.clone()
            } else {
                "unknown panic payload".to_string()
            };
            log::error!("{LOG} screen_share_start panicked: {panic_msg}");
            Err(format!("screen_share_start panicked: {panic_msg}"))
        }
    }
}

#[cfg(target_os = "linux")]
fn screen_share_start_impl(state: State<'_, MediaState>, app: AppHandle) -> Result<bool, String> {
    use screen_capture::frame_processor::{cap_resolution, FrameThrottler};

    crate::debug_eprintln!("wavis: native-media: screen_share_start entered");
    log::info!("{LOG} screen_share_start: entered command handler");

    // Double-start guard (req 2.4.1): if already capturing, return Ok(true).
    {
        let sc_guard = state
            .screen_capture
            .lock()
            .map_err(|e| format!("screen_capture lock: {e}"))?;
        if sc_guard.is_some() {
            log::info!("{LOG} screen_share_start: already active, returning Ok(true)");
            return Ok(true);
        }
    }

    // Must be connected to a LiveKit room.
    let lk_guard = state.lk.lock().map_err(|e| format!("lock: {e}"))?;
    let conn = lk_guard
        .as_ref()
        .ok_or_else(|| "not connected to a room".to_string())?;

    if !conn.is_available() {
        return Err("not connected to a room".to_string());
    }

    // Create and start the capture backend (PipeWire → X11 fallback chain).
    crate::debug_eprintln!("wavis: native-media: screen_share_start selecting backend");
    log::info!("{LOG} screen_share_start: selecting capture backend");
    let capture = match screen_capture::create_capture_backend(
        state.screen_share_config.max_width(),
        state.screen_share_config.max_height(),
        state.screen_share_config.max_fps(),
    ) {
        Ok(c) => c,
        Err(screen_capture::CaptureError::UserCancelled) => return Ok(false),
        Err(screen_capture::CaptureError::NoBackendAvailable(msg)) => {
            emit_linux_capture_fallback(&app, "pipewire_video", msg.clone());
            return Err(msg);
        }
        Err(screen_capture::CaptureError::CaptureStartFailed(msg)) => {
            emit_linux_capture_fallback(&app, "pipewire_video", msg.clone());
            return Err(msg);
        }
        Err(e) => {
            let reason = format!("{e}");
            emit_linux_capture_fallback(&app, "pipewire_video", reason.clone());
            return Err(reason);
        }
    };

    // Read current quality config for publish dimensions.
    let config = state.screen_share_config.clone();
    let pub_w = config.max_width().max(1);
    let pub_h = config.max_height().max(1);

    // Publish a video track via the LiveKit connection at the configured resolution.
    state
        .runtime
        .block_on(async { conn.publish_video(pub_w, pub_h) })
        .map_err(|e| format!("failed to publish video track: {e}"))?;

    // Wire the frame callback: cap_resolution → throttle → feed_video_frame.
    // Use Weak<> so the callback cannot prevent the connection from being
    // dropped, and gracefully stops feeding frames if the connection is gone.
    let conn_weak = Arc::downgrade(conn);
    drop(lk_guard);

    let throttler = Arc::new(FrameThrottler::new(config.max_fps()));
    let frame_seq = Arc::new(std::sync::atomic::AtomicU64::new(0));
    let latest_frame = Arc::new(Mutex::new(None::<HeartbeatFrame>));
    let last_capture_ms = Arc::new(AtomicU64::new(0));
    state.start_native_screen_share_heartbeat(
        conn_weak.clone(),
        Arc::clone(&latest_frame),
        Arc::clone(&last_capture_ms),
        Arc::clone(&config),
    );

    capture.on_frame(Box::new(move |frame| {
        let seq = frame_seq.fetch_add(1, std::sync::atomic::Ordering::Relaxed) + 1;
        crate::debug_eprintln!(
            "wavis: native-media: on_frame seq={} incoming={}x{} bytes={}",
            seq,
            frame.width,
            frame.height,
            frame.data.len()
        );

        // Read current config values (may change at runtime via preset).
        let max_w = config.max_width();
        let max_h = config.max_height();

        // Update throttler FPS from config (cheap atomic read + store).
        throttler.set_fps(config.max_fps());

        if !throttler.should_emit() {
            return;
        }

        // Cap resolution to configured maximum.
        let capped = cap_resolution(frame, max_w, max_h);
        let now_ms = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_millis() as u64)
            .unwrap_or(0);
        last_capture_ms.store(now_ms, Ordering::Relaxed);
        if let Ok(mut latest) = latest_frame.lock() {
            *latest = Some(HeartbeatFrame {
                data: Arc::new(capped.data.clone()),
                width: capped.width,
                height: capped.height,
            });
        }
        crate::debug_eprintln!(
            "wavis: native-media: on_frame seq={} capped={}x{} bytes={}",
            seq,
            capped.width,
            capped.height,
            capped.data.len()
        );

        crate::debug_eprintln!("wavis: native-media: on_frame seq={} cached frame", seq);
    }));

    // Store the capture backend in MediaState.
    crate::debug_eprintln!("wavis: native-media: screen_share_start storing capture");
    log::info!("{LOG} screen_share_start: capture backend initialized, storing state");
    let mut sc_guard = state
        .screen_capture
        .lock()
        .map_err(|e| format!("screen_capture lock: {e}"))?;
    *sc_guard = Some(capture);
    drop(sc_guard);
    state.start_native_screen_share_stats(app)?;

    log::info!(
        "{LOG} screen_share_start: capture started ({}x{} @ {}fps)",
        pub_w,
        pub_h,
        state.screen_share_config.max_fps()
    );
    Ok(true)
}

/// Start screen capture for a specific source (screen or window) by PipeWire node ID.
///
/// Creates a PipeWire stream targeting the specific node ID from direct enumeration,
/// bypassing the portal's interactive picker entirely.
/// On Wayland: uses the portal-authorized fd from `PortalAuthState`.
/// On X11: connects to PipeWire directly (no portal needed).
///
/// Returns `Ok(true)` if capture started, `Err(msg)` on failure.
/// Reuses `screen_share_stop` for stopping.
#[tauri::command]
#[cfg(target_os = "linux")]
pub fn screen_share_start_source(
    source_id: String,
    state: State<'_, MediaState>,
    portal_auth: State<'_, crate::portal_auth::PortalAuthState>,
    app: AppHandle,
) -> Result<bool, String> {
    use crate::portal_auth::DisplayServer;
    use screen_capture::frame_processor::{cap_resolution, FrameThrottler};
    use screen_capture::source_capture::{SourceCapture, SourceCaptureConfig};
    use screen_capture::ScreenCapture;

    // Parse the source_id as a PipeWire node ID.
    let node_id: u32 = source_id
        .parse()
        .map_err(|_| format!("invalid source id: {source_id}"))?;

    // Double-start guard: if already capturing, return Ok(true).
    {
        let sc_guard = state
            .screen_capture
            .lock()
            .map_err(|e| format!("screen_capture lock: {e}"))?;
        if sc_guard.is_some() {
            log::info!("{LOG} screen_share_start_source: already active, returning Ok(true)");
            return Ok(true);
        }
    }

    // Must be connected to a LiveKit room.
    let lk_guard = state.lk.lock().map_err(|e| format!("lock: {e}"))?;
    let conn = lk_guard
        .as_ref()
        .ok_or_else(|| "not connected to a room".to_string())?;

    if !conn.is_available() {
        return Err("not connected to a room".to_string());
    }

    // Resolve the PipeWire fd based on display server.
    let pw_fd = match portal_auth.display_server() {
        DisplayServer::Wayland => {
            // On Wayland, we need the portal-authorized fd for PipeWire access.
            let fd = portal_auth.try_clone_fd().ok_or_else(|| {
                "Wayland capture requires portal authorization — call authorize_screen_capture first"
                    .to_string()
            })?;
            Some(fd)
        }
        DisplayServer::X11 | DisplayServer::Unknown => {
            // On X11, direct PipeWire access works without portal authorization.
            None
        }
    };

    // Create and start the source capture.
    let capture = SourceCapture::start(SourceCaptureConfig {
        node_id,
        pw_fd,
        app_handle: app.clone(),
    })
    .map_err(|e| format!("{e}"))?;

    // Read current quality config for publish dimensions.
    let config = state.screen_share_config.clone();
    let pub_w = config.max_width();
    let pub_h = config.max_height();

    // Publish a video track via the LiveKit connection at the configured resolution.
    state
        .runtime
        .block_on(async { conn.publish_video(pub_w, pub_h) })
        .map_err(|e| format!("failed to publish video track: {e}"))?;

    // Wire the frame callback: cap_resolution → throttle → feed_video_frame.
    let conn_weak = Arc::downgrade(conn);
    drop(lk_guard);

    let throttler = Arc::new(FrameThrottler::new(config.max_fps()));
    let latest_frame = Arc::new(Mutex::new(None::<HeartbeatFrame>));
    let last_capture_ms = Arc::new(AtomicU64::new(0));
    state.start_native_screen_share_heartbeat(
        conn_weak.clone(),
        Arc::clone(&latest_frame),
        Arc::clone(&last_capture_ms),
        Arc::clone(&config),
    );

    capture.on_frame(Box::new(move |frame| {
        let max_w = config.max_width();
        let max_h = config.max_height();
        throttler.set_fps(config.max_fps());

        if !throttler.should_emit() {
            return;
        }

        let capped = cap_resolution(frame, max_w, max_h);
        let now_ms = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_millis() as u64)
            .unwrap_or(0);
        last_capture_ms.store(now_ms, Ordering::Relaxed);
        if let Ok(mut latest) = latest_frame.lock() {
            *latest = Some(HeartbeatFrame {
                data: Arc::new(capped.data.clone()),
                width: capped.width,
                height: capped.height,
            });
        }
    }));

    // Store the capture backend in MediaState so screen_share_stop can stop it.
    let mut sc_guard = state
        .screen_capture
        .lock()
        .map_err(|e| format!("screen_capture lock: {e}"))?;
    *sc_guard = Some(Box::new(capture));
    drop(sc_guard);
    state.start_native_screen_share_stats(app.clone())?;

    log::info!(
        "{LOG} screen_share_start_source: capturing node {node_id} ({}x{} @ {}fps)",
        pub_w,
        pub_h,
        state.screen_share_config.max_fps()
    );
    Ok(true)
}

/// Start screen capture for a specific source (monitor or window) on Windows.
///
/// Uses the Windows Graphics Capture API to capture the specified source.
/// The `source_id` is an HMONITOR or HWND handle value as a string.
///
/// On the JS SDK path (no native LiveKit), frames are written to a shared
/// buffer (`MediaState::latest_frame`) that JS polls via `screen_share_poll_frame`.
/// This completely bypasses the Windows message queue / PostMessage / HWND,
/// avoiding the corruption caused by child windows (SharePicker) opening/closing.
///
/// Returns `Ok(true)` if capture started, `Err(msg)` on failure.
#[tauri::command]
#[cfg(target_os = "windows")]
pub fn screen_share_start_source(
    source_id: String,
    share_session_id: Option<String>,
    source_kind: Option<String>,
    capture_backend: Option<String>,
    compatibility_mode: Option<bool>,
    previous_backend: Option<String>,
    retry_reason: Option<String>,
    state: State<'_, MediaState>,
    app: AppHandle,
) -> Result<bool, String> {
    use screen_capture::frame_processor::{cap_resolution, FrameThrottler};
    use screen_capture::gdi_capture::{GdiCapture, GdiCaptureConfig, GdiCaptureTarget};
    use screen_capture::win_capture::{
        WinCapture, WinCaptureConfig, WinCaptureSourceKind, WindowsNativeCaptureDiagnostics,
    };
    use screen_capture::ScreenCapture;

    let source_kind = parse_windows_source_kind(source_kind.as_deref())?;
    let capture_backend = select_windows_capture_backend(
        source_kind,
        compatibility_mode.unwrap_or(false),
        capture_backend.as_deref(),
    )?;
    let source_kind_label = match source_kind {
        WindowsSourceKind::Screen => "screen",
        WindowsSourceKind::Window => "window",
    };
    let backend_label = match capture_backend {
        WindowsCaptureBackend::Wgc => "wgc",
        WindowsCaptureBackend::GdiPoll => "gdi_poll",
    };
    let diagnostics = Arc::new(Mutex::new(WindowsNativeCaptureDiagnostics::new(
        backend_label,
        source_kind_label,
        0,
        0,
    )));
    let retry_at_ms = retry_reason.as_ref().map(|_| unix_now_ms());
    if let Ok(mut diag) = diagnostics.lock() {
        diag.previous_backend = previous_backend;
        diag.retry_reason = retry_reason;
        diag.retry_at_ms = retry_at_ms;
    }
    {
        let mut guard = state
            .windows_capture_diagnostics
            .lock()
            .map_err(|e| format!("windows_capture_diagnostics lock: {e}"))?;
        *guard = Some(Arc::clone(&diagnostics));
    }
    log::info!(
        "{LOG} [diag] screen_share_start_source ENTERED, source_id={source_id}, source_kind={source_kind:?}, backend={capture_backend:?}, share_session_id={}",
        share_session_id.as_deref().unwrap_or("none")
    );

    // Double-start guard: if already capturing, return Ok(true).
    {
        let sc_guard = state
            .screen_capture
            .lock()
            .map_err(|e| format!("screen_capture lock: {e}"))?;
        if sc_guard.is_some() {
            log::info!("{LOG} screen_share_start_source: already active, returning Ok(true)");
            return Ok(true);
        }
    }

    log::info!("{LOG} [diag] past double-start guard");
    state
        .windows_jpeg_fallback_enabled
        .store(false, Ordering::Relaxed);

    // NOTE (cleanup 2026-03): the `has_native_lk` branch that published a video
    // track via the Rust LiveKit SDK and fed frames with `conn.feed_video_frame()`
    // was removed here. On Windows, LiveKit is always managed by the JS SDK inside
    // WebView2; `state.lk` is always `None`, making that branch unreachable.
    // The native-LK frame-feeding path lives in the Linux `screen_share_start()`
    // function (no `_source` suffix) which is #[cfg(target_os = "linux")].
    let bridge_transport_fps = windows_bridge_transport_fps(&state.screen_share_config);

    // Route to the requested native backend. GDI polling is source-kind-specific:
    // monitor shares use BitBlt and window shares use PrintWindow.
    let capture: Box<dyn ScreenCapture> = if capture_backend == WindowsCaptureBackend::GdiPoll {
        let handle_val: isize = source_id
            .parse()
            .map_err(|_| format!("GDI polling: invalid source id '{source_id}'"))?;
        let target = match source_kind {
            WindowsSourceKind::Screen => GdiCaptureTarget::Monitor(
                windows::Win32::Graphics::Gdi::HMONITOR(handle_val as *mut _),
            ),
            WindowsSourceKind::Window => {
                GdiCaptureTarget::Window(windows::Win32::Foundation::HWND(handle_val as *mut _))
            }
        };
        Box::new(
            GdiCapture::start(GdiCaptureConfig {
                target,
                app_handle: app.clone(),
                target_fps: bridge_transport_fps,
                diagnostics: Arc::clone(&diagnostics),
            })
            .map_err(|e| {
                log::warn!(
                    "{LOG} native capture startup failure: source_kind={source_kind:?} backend={capture_backend:?} last_stage=backend_start error={e}"
                );
                log::error!("{LOG} GdiCapture::start() FAILED: {e}");
                format!("{e}")
            })?,
        )
    } else {
        Box::new(
            WinCapture::start(WinCaptureConfig {
                source_id: source_id.clone(),
                source_kind: match source_kind {
                    WindowsSourceKind::Screen => WinCaptureSourceKind::Screen,
                    WindowsSourceKind::Window => WinCaptureSourceKind::Window,
                },
                app_handle: app.clone(),
                diagnostics: Arc::clone(&diagnostics),
            })
            .map_err(|e| {
                log::warn!(
                    "{LOG} native capture startup failure: source_kind={source_kind:?} backend={capture_backend:?} last_stage=backend_start error={e}"
                );
                log::error!("{LOG} [diag] WinCapture::start() FAILED: {e}");
                format!("{e}")
            })?,
        )
    };

    log::info!("{LOG} [diag] capture backend started");
    collect_windows_startup_environment(
        &diagnostics,
        diagnostics
            .lock()
            .ok()
            .map(|diag| diag.item_width)
            .unwrap_or(0),
        diagnostics
            .lock()
            .ok()
            .map(|diag| diag.item_height)
            .unwrap_or(0),
        bridge_transport_fps,
        state.screen_share_config.jpeg_quality(),
    );

    let config = state.screen_share_config.clone();
    let native_share_leak_session = Arc::clone(&state.native_share_leak_session);

    {
        let mut leak_guard = state
            .native_share_leak_session
            .lock()
            .map_err(|e| format!("native_share_leak_session lock: {e}"))?;
        *leak_guard = share_session_id
            .as_ref()
            .map(|session_id| NativeShareLeakSession {
                share_session_id: session_id.clone(),
                source_id: source_id.clone(),
                source_kind,
                capture_backend,
                started_at_ms: unix_now_ms(),
                first_rust_frame_at_ms: None,
                frames_buffered: 0,
                diagnostics: Arc::clone(&diagnostics),
            });
        if let Some(session) = leak_guard.as_ref() {
            log::info!(
                "{LOG} native capture: session={} stage=native_capture_start source_id={} source_kind={:?} backend={:?}",
                session.share_session_id,
                session.source_id,
                session.source_kind,
                session.capture_backend,
            );
        }
    }

    {
        // ── JS SDK path: write frames to shared buffer for JS polling ─────
        // JS polls via `screen_share_poll_frame` invoke command, which uses
        // the ipc:// custom protocol (HTTP-like), completely bypassing the
        // Windows message queue / PostMessage / HWND. This avoids the
        // ERROR_INVALID_WINDOW_HANDLE (0x80070578) corruption caused by
        // child windows (SharePicker) opening/closing.
        let debug_capture = screen_capture::frame_processor::is_debug_capture_enabled();
        let throttler = Arc::new(FrameThrottler::new(bridge_transport_fps));
        let frame_count = Arc::new(std::sync::atomic::AtomicU64::new(0));
        let latest_frame = Arc::clone(&state.latest_frame);
        let latest_i420_frame = Arc::clone(&state.latest_i420_frame);
        let jpeg_fallback_enabled = Arc::clone(&state.windows_jpeg_fallback_enabled);
        let diagnostics_for_frames = Arc::clone(&diagnostics);

        // Pre-allocate reusable buffers to avoid per-frame allocation.
        // RGB buffer: 1920*1080*3 ≈ 6MB initial capacity (grows if needed).
        let rgb_buf = Arc::new(std::sync::Mutex::new(Vec::<u8>::with_capacity(
            1920 * 1080 * 3,
        )));
        // JPEG output buffer: 256KB initial (JPEG is much smaller than raw).
        let jpeg_buf = Arc::new(std::sync::Mutex::new(Vec::<u8>::with_capacity(256 * 1024)));

        // Clear any stale frame from a previous capture session.
        {
            let mut lf = latest_frame
                .lock()
                .map_err(|e| format!("latest_frame lock: {e}"))?;
            *lf = None;
        }
        {
            let mut lf = latest_i420_frame
                .lock()
                .map_err(|e| format!("latest_i420_frame lock: {e}"))?;
            *lf = None;
        }

        capture.on_frame(Box::new(move |frame| {
            use base64::Engine;
            use image::codecs::jpeg::JpegEncoder;
            use std::io::Cursor;

            if let Ok(mut diag_guard) = diagnostics_for_frames.lock() {
                if diag_guard.backend == "gdi_poll" {
                    diag_guard.record_backend_callback();
                }
            }
            let max_w = config.max_width();
            let max_h = config.max_height();
            throttler.set_fps(bridge_transport_fps);

            // Throttle BEFORE downscaling — no point processing a frame we'll drop.
            if !throttler.should_emit() {
                if let Ok(mut diag_guard) = diagnostics_for_frames.lock() {
                    diag_guard.record_throttle_drop();
                }
                return;
            }

            let raw_to_pollable_start = std::time::Instant::now();
            let cap_start = std::time::Instant::now();
            let capped = cap_resolution(frame, max_w, max_h);
            let cap_downscale_ms = cap_start.elapsed().as_millis() as u64;

            let n = frame_count.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
            let seq = n + 1;
            if let Ok(mut leak_guard) = native_share_leak_session.lock() {
                if let Some(session) = leak_guard.as_mut() {
                    if session.first_rust_frame_at_ms.is_none() {
                        let first_frame_at_ms = unix_now_ms();
                        session.first_rust_frame_at_ms = Some(first_frame_at_ms);
                        log::info!(
                            "{LOG} native capture: session={} stage=first_rust_frame source_id={} source_kind={:?} backend={:?} elapsed_ms={}",
                            session.share_session_id,
                            session.source_id,
                            session.source_kind,
                            session.capture_backend,
                            first_frame_at_ms.saturating_sub(session.started_at_ms)
                        );
                    }
                }
            }
            if n == 0 {
                // Log first frame details for debugging black-screen issues
                let non_zero = capped.data.iter().filter(|&&b| b != 0).count();
                log::info!(
                    "{LOG} first frame: {}x{}, data_len={}, non_zero_bytes={}",
                    capped.width,
                    capped.height,
                    capped.data.len(),
                    non_zero
                );
            }

            let i420_start = std::time::Instant::now();
            let i420 = rgba_to_i420(&capped.data, capped.width, capped.height);
            let i420_convert_ms = i420_start.elapsed().as_millis() as u64;
            let mut i420_write_ms = 0;
            if let Some((i420_data, i420_width, i420_height)) = i420 {
                let timestamp_us = capped.timestamp_ms.saturating_mul(1000);
                let i420_write_start = std::time::Instant::now();
                if let Ok(mut lf) = latest_i420_frame.lock() {
                    *lf = Some(LatestI420Frame {
                        frame: i420_data,
                        width: i420_width,
                        height: i420_height,
                        timestamp_us,
                        seq,
                    });
                }
                i420_write_ms = i420_write_start.elapsed().as_millis() as u64;
            }

            if n > 0 && !jpeg_fallback_enabled.load(Ordering::Relaxed) {
                let raw_to_pollable_frame_ms = raw_to_pollable_start.elapsed().as_millis() as u64;
                if let Ok(mut diag_guard) = diagnostics_for_frames.lock() {
                    diag_guard.record_pollable_frame_timing(
                        cap_downscale_ms,
                        i420_convert_ms,
                        0,
                        0,
                        0,
                        i420_write_ms,
                        raw_to_pollable_frame_ms,
                    );
                }
                if let Ok(mut leak_guard) = native_share_leak_session.lock() {
                    if let Some(session) = leak_guard.as_mut() {
                        session.frames_buffered = seq;
                    }
                }
                if n == 0 {
                    let first_pollable_elapsed_ms = native_share_leak_session
                        .lock()
                        .ok()
                        .and_then(|guard| {
                            guard
                                .as_ref()
                                .map(|session| unix_now_ms().saturating_sub(session.started_at_ms))
                        })
                        .unwrap_or(raw_to_pollable_frame_ms);
                    if let Ok(mut diag_guard) = diagnostics_for_frames.lock() {
                        diag_guard.timing.first_cap_downscale_ms = Some(cap_downscale_ms);
                        diag_guard.timing.first_i420_convert_ms = Some(i420_convert_ms);
                        diag_guard.timing.first_latest_frame_write_ms = Some(i420_write_ms);
                        diag_guard.timing.first_raw_to_pollable_frame_ms =
                            Some(raw_to_pollable_frame_ms);
                        diag_guard.record_startup_stage(
                            "first_pollable_frame_written",
                            "write latest I420 pollable frame",
                            first_pollable_elapsed_ms,
                        );
                    }
                    log::info!(
                        "{LOG} native capture first I420 pollable frame timing: cap_downscale_ms={} i420_convert_ms={} latest_frame_write_ms={} raw_to_pollable_ms={}",
                        cap_downscale_ms,
                        i420_convert_ms,
                        i420_write_ms,
                        raw_to_pollable_frame_ms,
                    );
                }
                return;
            }

            // Strip alpha channel into reusable RGB buffer (JPEG only supports RGB).
            let pixel_count = (capped.width * capped.height) as usize;
            let rgb_len = pixel_count * 3;
            let rgb_start = std::time::Instant::now();
            let mut rgb_guard = match rgb_buf.lock() {
                Ok(g) => g,
                Err(_) => return,
            };
            rgb_guard.clear();
            let extra = rgb_len.saturating_sub(rgb_guard.capacity());
            rgb_guard.reserve(extra);
            for rgba in capped.data.chunks_exact(4) {
                rgb_guard.push(rgba[0]);
                rgb_guard.push(rgba[1]);
                rgb_guard.push(rgba[2]);
            }
            let rgba_to_rgb_ms = rgb_start.elapsed().as_millis() as u64;

            // Encode RGB → JPEG into reusable buffer.
            let jpeg_quality = config.jpeg_quality();
            let mut jpeg_guard = match jpeg_buf.lock() {
                Ok(g) => g,
                Err(_) => return,
            };
            jpeg_guard.clear();
            let jpeg_start = std::time::Instant::now();
            let mut cursor = Cursor::new(&mut *jpeg_guard);
            let mut encoder = JpegEncoder::new_with_quality(&mut cursor, jpeg_quality);
            if let Err(e) = encoder.encode(
                &rgb_guard,
                capped.width,
                capped.height,
                image::ExtendedColorType::Rgb8,
            ) {
                log::warn!("{LOG} JPEG encode failed: {e}");
                return;
            }
            // Drop encoder + cursor so we can read jpeg_guard directly.
            drop(encoder);
            #[allow(clippy::drop_non_drop)] // intentional: ends mutable borrow on jpeg_guard
            drop(cursor);
            let jpeg_encode_ms = jpeg_start.elapsed().as_millis() as u64;

            if n == 0 {
                log::info!("{LOG} first JPEG: {} bytes", jpeg_guard.len());
                if let Ok(leak_guard) = native_share_leak_session.lock() {
                    if let Some(session) = leak_guard.as_ref() {
                        if let Ok(mut diagnostics) = session.diagnostics.lock() {
                            let elapsed_ms = unix_now_ms().saturating_sub(session.started_at_ms);
                            diagnostics.first_buffered_jpeg_latency_ms = Some(elapsed_ms);
                            diagnostics.record_startup_stage(
                                "first_buffered_jpeg",
                                "JPEG/base64 bridge",
                                elapsed_ms,
                            );
                        }
                    }
                }
            }

            let base64_start = std::time::Instant::now();
            let b64 = base64::engine::general_purpose::STANDARD.encode(&*jpeg_guard);
            let base64_encode_ms = base64_start.elapsed().as_millis() as u64;

            // Drop buffers before taking the latest_frame lock to minimize hold time.
            drop(rgb_guard);
            drop(jpeg_guard);

            // Write to shared buffer — JS polls this via screen_share_poll_frame.
            // No PostMessage, no HWND, no window handle dependency.
            let write_start = std::time::Instant::now();
            if let Ok(mut lf) = latest_frame.lock() {
                *lf = Some(LatestFrame {
                    frame: b64,
                    width: capped.width,
                    height: capped.height,
                    seq,
                });
            }
            let latest_frame_write_ms = write_start.elapsed().as_millis() as u64;
            let raw_to_pollable_frame_ms = raw_to_pollable_start.elapsed().as_millis() as u64;
            if let Ok(mut diag_guard) = diagnostics_for_frames.lock() {
                diag_guard.record_pollable_frame_timing(
                    cap_downscale_ms,
                    i420_convert_ms,
                    rgba_to_rgb_ms,
                    jpeg_encode_ms,
                    base64_encode_ms,
                    latest_frame_write_ms,
                    raw_to_pollable_frame_ms,
                );
            }
            let mut first_pollable_elapsed_ms = raw_to_pollable_frame_ms;
            if let Ok(mut leak_guard) = native_share_leak_session.lock() {
                if let Some(session) = leak_guard.as_mut() {
                    session.frames_buffered = n + 1;
                    first_pollable_elapsed_ms = unix_now_ms().saturating_sub(session.started_at_ms);
                }
            }
            if n == 0 {
                if let Ok(mut diag_guard) = diagnostics_for_frames.lock() {
                    diag_guard.timing.first_cap_downscale_ms = Some(cap_downscale_ms);
                    diag_guard.timing.first_i420_convert_ms = Some(i420_convert_ms);
                    diag_guard.timing.first_rgba_to_rgb_ms = Some(rgba_to_rgb_ms);
                    diag_guard.timing.first_jpeg_encode_ms = Some(jpeg_encode_ms);
                    diag_guard.timing.first_base64_encode_ms = Some(base64_encode_ms);
                    diag_guard.timing.first_latest_frame_write_ms = Some(latest_frame_write_ms);
                    diag_guard.timing.first_raw_to_pollable_frame_ms =
                        Some(raw_to_pollable_frame_ms);
                    diag_guard.record_startup_stage(
                        "first_pollable_frame_written",
                        "write latest pollable frame",
                        first_pollable_elapsed_ms,
                    );
                }
                log::info!(
                    "{LOG} native capture first pollable frame timing: cap_downscale_ms={} i420_convert_ms={} rgba_to_rgb_ms={} jpeg_encode_ms={} base64_encode_ms={} latest_frame_write_ms={} raw_to_pollable_ms={}",
                    cap_downscale_ms,
                    i420_convert_ms,
                    rgba_to_rgb_ms,
                    jpeg_encode_ms,
                    base64_encode_ms,
                    latest_frame_write_ms,
                    raw_to_pollable_frame_ms,
                );
            }

            if debug_capture {
                log::debug!(
                    "{LOG} [debug-capture] buffered frame #{}: {}x{}, ts={}ms",
                    n + 1,
                    capped.width,
                    capped.height,
                    capped.timestamp_ms
                );
            }
        }));

        log::info!("{LOG} [diag] on_frame() callback registered (JS SDK path)");
        log::info!("{LOG} screen_share_start_source: JS SDK path (Channel IPC), capturing source {source_id}");
    }

    // Store the capture backend in MediaState so screen_share_stop can stop it.
    let mut sc_guard = state
        .screen_capture
        .lock()
        .map_err(|e| format!("screen_capture lock: {e}"))?;
    *sc_guard = Some(capture);

    log::info!("{LOG} [diag] capture stored in MediaState, waiting for first pollable frame");
    if let Err(error) = wait_for_first_pollable_frame(
        &state.latest_i420_frame,
        &state.latest_frame,
        &diagnostics,
        if capture_backend == WindowsCaptureBackend::GdiPoll {
            5000
        } else {
            2500
        },
        if capture_backend == WindowsCaptureBackend::GdiPoll {
            5000
        } else {
            7500
        },
    ) {
        if let Some(cap) = sc_guard.take() {
            cap.stop();
        }
        log::warn!("{LOG} native capture startup failure: {error}");
        return Err(error);
    }

    log::info!("{LOG} [diag] first pollable frame ready, function complete");
    Ok(true)
}

/// Non-Linux/non-Windows stub for `screen_share_start_source`.
#[tauri::command]
#[cfg(not(any(target_os = "linux", target_os = "windows")))]
pub fn screen_share_start_source(_source_id: String) -> Result<bool, String> {
    Err("screen sharing via native capture is only available on Linux and Windows".to_string())
}

/// Stop screen sharing on Linux or Windows.
///
/// Stops the capture pipeline, unpublishes the video track, and cleans up.
#[tauri::command]
#[cfg(any(target_os = "linux", target_os = "windows"))]
pub fn screen_share_stop(state: State<'_, MediaState>) -> Result<(), String> {
    #[cfg(target_os = "linux")]
    state.stop_native_screen_share_stats();

    // Take the capture backend out of state.
    let capture = {
        let mut sc_guard = state
            .screen_capture
            .lock()
            .map_err(|e| format!("screen_capture lock: {e}"))?;
        sc_guard.take()
    };

    // Stop the capture pipeline if it was active.
    if let Some(cap) = capture {
        cap.stop();
        log::info!("{LOG} screen_share_stop: capture stopped");
    }

    // Unpublish the video track.
    let lk_guard = state.lk.lock().map_err(|e| format!("lock: {e}"))?;
    if let Some(conn) = lk_guard.as_ref() {
        if let Err(e) = state.runtime.block_on(async { conn.unpublish_video() }) {
            log::warn!("{LOG} screen_share_stop: unpublish_video failed: {e}");
        }
    }

    // Clear the shared frame buffer (Windows JS SDK polling path).
    #[cfg(target_os = "windows")]
    {
        if let Ok(mut lf) = state.latest_frame.lock() {
            *lf = None;
        }
        if let Ok(mut lf) = state.latest_i420_frame.lock() {
            *lf = None;
        }
        state
            .windows_jpeg_fallback_enabled
            .store(false, Ordering::Relaxed);
        if let Ok(mut leak_guard) = state.native_share_leak_session.lock() {
            if let Some(session) = leak_guard.take() {
                let ended_at_ms = unix_now_ms();
                let first_rust_frame_latency_ms = session
                    .first_rust_frame_at_ms
                    .map(|ts| ts.saturating_sub(session.started_at_ms));
                if session.frames_buffered == 0 {
                    let diagnostics_summary = session
                        .diagnostics
                        .lock()
                        .map(|diag| diag.compact_summary())
                        .unwrap_or_else(|_| "diagnostics unavailable".to_string());
                    log::warn!(
                        "{LOG} native capture first-frame failure: session={} source_kind={:?} backend={:?} {diagnostics_summary}",
                        session.share_session_id,
                        session.source_kind,
                        session.capture_backend,
                    );
                }
                log::info!(
                    "{LOG} native capture: session={} stage=session_closed source_id={} source_kind={:?} backend={:?} duration_ms={} frames_buffered={} first_rust_frame_latency_ms={}",
                    session.share_session_id,
                    session.source_id,
                    session.source_kind,
                    session.capture_backend,
                    ended_at_ms.saturating_sub(session.started_at_ms),
                    session.frames_buffered,
                    first_rust_frame_latency_ms
                        .map(|v| v.to_string())
                        .unwrap_or_else(|| "none".to_string())
                );
            }
        }
    }

    Ok(())
}

/// Poll the latest captured frame (Windows JS SDK path).
///
/// Returns the most recent JPEG frame as base64, or `null` if no frame
/// is available yet. Uses the `ipc://` custom protocol (HTTP request/response),
/// which does NOT use PostMessage or the Windows message queue — completely
/// immune to HWND corruption from child windows.
///
/// JS calls this in a tight `requestAnimationFrame` loop during native capture.
#[tauri::command]
#[cfg(target_os = "windows")]
pub fn screen_share_get_capture_diagnostics(
    state: State<'_, MediaState>,
) -> Result<Option<screen_capture::win_capture::WindowsNativeCaptureDiagnostics>, String> {
    let guard = state
        .windows_capture_diagnostics
        .lock()
        .map_err(|e| format!("windows_capture_diagnostics lock: {e}"))?;
    guard
        .as_ref()
        .map(|diagnostics| {
            diagnostics
                .lock()
                .map(|mut snapshot| {
                    snapshot.refresh_interval_rates();
                    snapshot.clone()
                })
                .map_err(|e| format!("capture diagnostics lock: {e}"))
        })
        .transpose()
}

#[tauri::command]
#[cfg(not(target_os = "windows"))]
pub fn screen_share_get_capture_diagnostics() -> Result<Option<()>, String> {
    Ok(None)
}

#[tauri::command]
#[cfg(target_os = "windows")]
pub fn screen_share_poll_frame(
    state: State<'_, MediaState>,
) -> Result<Option<LatestFrame>, String> {
    let latest = {
        let lf = state
            .latest_frame
            .lock()
            .map_err(|e| format!("latest_frame lock: {e}"))?;
        lf.clone()
    };

    if let Ok(diag_guard) = state.windows_capture_diagnostics.lock() {
        if let Some(diagnostics) = diag_guard.as_ref() {
            if let Ok(mut diagnostics) = diagnostics.lock() {
                diagnostics.poll_calls = diagnostics.poll_calls.saturating_add(1);
                if let Some(frame) = latest.as_ref() {
                    diagnostics.poll_hits = diagnostics.poll_hits.saturating_add(1);
                    diagnostics.latest_polled_seq = Some(frame.seq);
                    if diagnostics.first_poll_hit_latency_ms.is_none() {
                        if let Ok(leak_guard) = state.native_share_leak_session.lock() {
                            if let Some(session) = leak_guard.as_ref() {
                                diagnostics.first_poll_hit_latency_ms =
                                    Some(unix_now_ms().saturating_sub(session.started_at_ms));
                            }
                        }
                    }
                    if diagnostics.startup_stage == "first_buffered_jpeg"
                        || diagnostics.startup_stage == "first_pollable_frame_written"
                    {
                        let elapsed_ms = diagnostics.first_poll_hit_latency_ms.unwrap_or(0);
                        diagnostics.record_startup_stage(
                            "first_poll_hit",
                            "screen_share_poll_frame",
                            elapsed_ms,
                        );
                    }
                } else {
                    diagnostics.poll_misses = diagnostics.poll_misses.saturating_add(1);
                }
            }
        }
    }

    Ok(latest)
}

#[tauri::command]
#[cfg(target_os = "windows")]
pub fn screen_share_poll_i420_frame(
    state: State<'_, MediaState>,
) -> Result<Option<LatestI420Frame>, String> {
    let latest = {
        let lf = state
            .latest_i420_frame
            .lock()
            .map_err(|e| format!("latest_i420_frame lock: {e}"))?;
        lf.clone()
    };

    if let Ok(diag_guard) = state.windows_capture_diagnostics.lock() {
        if let Some(diagnostics) = diag_guard.as_ref() {
            if let Ok(mut diagnostics) = diagnostics.lock() {
                diagnostics.poll_calls = diagnostics.poll_calls.saturating_add(1);
                if let Some(frame) = latest.as_ref() {
                    diagnostics.poll_hits = diagnostics.poll_hits.saturating_add(1);
                    diagnostics.latest_polled_seq = Some(frame.seq);
                    if diagnostics.first_poll_hit_latency_ms.is_none() {
                        if let Ok(leak_guard) = state.native_share_leak_session.lock() {
                            if let Some(session) = leak_guard.as_ref() {
                                diagnostics.first_poll_hit_latency_ms =
                                    Some(unix_now_ms().saturating_sub(session.started_at_ms));
                            }
                        }
                    }
                    if diagnostics.startup_stage == "first_buffered_jpeg"
                        || diagnostics.startup_stage == "first_pollable_frame_written"
                    {
                        let elapsed_ms = diagnostics.first_poll_hit_latency_ms.unwrap_or(0);
                        diagnostics.record_startup_stage(
                            "first_poll_hit",
                            "screen_share_poll_i420_frame",
                            elapsed_ms,
                        );
                    }
                } else {
                    diagnostics.poll_misses = diagnostics.poll_misses.saturating_add(1);
                }
            }
        }
    }

    Ok(latest)
}

#[tauri::command]
#[cfg(target_os = "windows")]
pub fn screen_share_set_jpeg_fallback_enabled(
    enabled: bool,
    state: State<'_, MediaState>,
) -> Result<(), String> {
    state
        .windows_jpeg_fallback_enabled
        .store(enabled, Ordering::Relaxed);
    Ok(())
}

#[tauri::command]
#[cfg(target_os = "windows")]
pub fn screen_share_get_i420_stream_url(state: State<'_, MediaState>) -> Result<String, String> {
    if state.windows_i420_stream_port == 0 {
        return Err("Windows I420 stream server is unavailable".to_string());
    }
    Ok(format!(
        "http://127.0.0.1:{}/screen-share/i420",
        state.windows_i420_stream_port
    ))
}

/// Non-Windows stub for `screen_share_poll_frame`.
#[tauri::command]
#[cfg(not(target_os = "windows"))]
pub fn screen_share_poll_frame() -> Result<Option<()>, String> {
    Ok(None)
}

/// Non-Windows stub for `screen_share_poll_i420_frame`.
#[tauri::command]
#[cfg(not(target_os = "windows"))]
pub fn screen_share_poll_i420_frame() -> Result<Option<()>, String> {
    Ok(None)
}

/// Non-Windows stub for `screen_share_set_jpeg_fallback_enabled`.
#[tauri::command]
#[cfg(not(target_os = "windows"))]
pub fn screen_share_set_jpeg_fallback_enabled(_enabled: bool) -> Result<(), String> {
    Ok(())
}

#[tauri::command]
#[cfg(not(target_os = "windows"))]
pub fn screen_share_get_i420_stream_url() -> Result<String, String> {
    Err("Windows I420 stream is only available on Windows".to_string())
}

/// Poll the latest remote screen-share frame for a participant (Linux viewer path).
///
/// The LiveKit callback stores raw RGBA frames in `remote_screen_share_frames`.
/// This command encodes only the newest frame requested by the webview. If the
/// caller already rendered `last_seq`, returns `None` without encoding.
#[tauri::command]
#[cfg(target_os = "linux")]
pub fn media_poll_screen_share_frame(
    identity: String,
    last_seq: Option<u64>,
    state: State<'_, MediaState>,
) -> Result<Option<PolledScreenShareFrame>, String> {
    use base64::Engine;
    use image::codecs::jpeg::JpegEncoder;
    use std::io::Cursor;

    let latest = {
        let frames = state
            .remote_screen_share_frames
            .lock()
            .map_err(|e| format!("remote_screen_share_frames lock: {e}"))?;
        frames.get(&identity).cloned()
    };

    let Some(latest) = latest else {
        return Ok(None);
    };

    if last_seq == Some(latest.seq) {
        return Ok(None);
    }

    let pixel_count = (latest.width * latest.height) as usize;
    let mut rgb_data = Vec::with_capacity(pixel_count * 3);
    for rgba in latest.data.chunks_exact(4) {
        rgb_data.push(rgba[0]);
        rgb_data.push(rgba[1]);
        rgb_data.push(rgba[2]);
    }

    let jpeg_quality = state.screen_share_config.jpeg_quality();
    let mut jpeg_buf = Cursor::new(Vec::with_capacity(256 * 1024));
    let mut encoder = JpegEncoder::new_with_quality(&mut jpeg_buf, jpeg_quality);
    if let Err(e) = encoder.encode(
        &rgb_data,
        latest.width,
        latest.height,
        image::ExtendedColorType::Rgb8,
    ) {
        log::warn!("{LOG} remote JPEG encode failed: {e}");
        return Ok(None);
    }

    let frame = base64::engine::general_purpose::STANDARD.encode(jpeg_buf.into_inner());
    Ok(Some(PolledScreenShareFrame {
        identity,
        frame,
        width: latest.width,
        height: latest.height,
        seq: latest.seq,
    }))
}

/// Poll the latest remote camera frame for a participant (Linux native path).
///
/// Camera frames are decoded in Rust from LiveKit Camera tracks and exposed as
/// JPEGs so the webview can paint them to a canvas-backed MediaStreamTrack.
#[tauri::command]
#[cfg(target_os = "linux")]
pub fn media_poll_camera_frame(
    identity: String,
    last_seq: Option<u64>,
    state: State<'_, MediaState>,
) -> Result<Option<PolledCameraFrame>, String> {
    use base64::Engine;
    use image::codecs::jpeg::JpegEncoder;
    use std::io::Cursor;

    let latest = {
        let frames = state
            .remote_camera_frames
            .lock()
            .map_err(|e| format!("remote_camera_frames lock: {e}"))?;
        frames.get(&identity).cloned()
    };

    let Some(latest) = latest else {
        return Ok(None);
    };

    if last_seq == Some(latest.seq) {
        return Ok(None);
    }

    let pixel_count = (latest.width * latest.height) as usize;
    let mut rgb_data = Vec::with_capacity(pixel_count * 3);
    for rgba in latest.data.chunks_exact(4) {
        rgb_data.push(rgba[0]);
        rgb_data.push(rgba[1]);
        rgb_data.push(rgba[2]);
    }

    let jpeg_quality = state.screen_share_config.jpeg_quality();
    let mut jpeg_buf = Cursor::new(Vec::with_capacity(128 * 1024));
    let mut encoder = JpegEncoder::new_with_quality(&mut jpeg_buf, jpeg_quality);
    if let Err(e) = encoder.encode(
        &rgb_data,
        latest.width,
        latest.height,
        image::ExtendedColorType::Rgb8,
    ) {
        log::warn!("{LOG} remote camera JPEG encode failed: {e}");
        return Ok(None);
    }

    let frame = base64::engine::general_purpose::STANDARD.encode(jpeg_buf.into_inner());
    Ok(Some(PolledCameraFrame {
        identity,
        frame,
        width: latest.width,
        height: latest.height,
        seq: latest.seq,
    }))
}

/// Poll the latest local camera preview frame for Linux native publishing.
#[tauri::command]
#[cfg(target_os = "linux")]
pub fn media_poll_local_camera_frame(
    last_seq: Option<u64>,
    state: State<'_, MediaState>,
) -> Result<Option<PolledCameraFrame>, String> {
    use base64::Engine;
    use image::codecs::jpeg::JpegEncoder;
    use std::io::Cursor;

    let latest = {
        let frame = state
            .local_camera_frame
            .lock()
            .map_err(|e| format!("local_camera_frame lock: {e}"))?;
        frame.clone()
    };

    let Some(latest) = latest else {
        return Ok(None);
    };

    if last_seq == Some(latest.seq) {
        return Ok(None);
    }

    let pixel_count = (latest.width * latest.height) as usize;
    let mut rgb_data = Vec::with_capacity(pixel_count * 3);
    for rgba in latest.data.chunks_exact(4) {
        rgb_data.push(rgba[0]);
        rgb_data.push(rgba[1]);
        rgb_data.push(rgba[2]);
    }

    let mut jpeg_buf = Cursor::new(Vec::with_capacity(128 * 1024));
    let mut encoder = JpegEncoder::new_with_quality(&mut jpeg_buf, 80);
    if let Err(e) = encoder.encode(
        &rgb_data,
        latest.width,
        latest.height,
        image::ExtendedColorType::Rgb8,
    ) {
        log::warn!("{LOG} local camera JPEG encode failed: {e}");
        return Ok(None);
    }

    let frame = base64::engine::general_purpose::STANDARD.encode(jpeg_buf.into_inner());
    Ok(Some(PolledCameraFrame {
        identity: "self".to_string(),
        frame,
        width: latest.width,
        height: latest.height,
        seq: latest.seq,
    }))
}

/// Non-Linux stub for `media_poll_screen_share_frame`.
#[tauri::command]
#[cfg(not(target_os = "linux"))]
pub fn media_poll_screen_share_frame(
    _identity: String,
    _last_seq: Option<u64>,
) -> Result<Option<()>, String> {
    Ok(None)
}

/// Non-Linux stub for `media_poll_camera_frame`.
#[tauri::command]
#[cfg(not(target_os = "linux"))]
pub fn media_poll_camera_frame(
    _identity: String,
    _last_seq: Option<u64>,
) -> Result<Option<serde_json::Value>, String> {
    Ok(None)
}

/// Non-Linux stub for `media_poll_local_camera_frame`.
#[tauri::command]
#[cfg(not(target_os = "linux"))]
pub fn media_poll_local_camera_frame(
    _last_seq: Option<u64>,
) -> Result<Option<serde_json::Value>, String> {
    Ok(None)
}

/// Return a browser-native MJPEG stream URL for Linux remote screen-share viewing.
#[tauri::command]
#[cfg(target_os = "linux")]
pub fn media_get_screen_share_stream_url(
    identity: String,
    state: State<'_, MediaState>,
) -> Result<String, String> {
    if state.remote_screen_share_stream_port == 0 {
        return Err("remote screen share stream server is unavailable".to_string());
    }
    Ok(format!(
        "http://127.0.0.1:{}/screen-share.mjpeg?identity={}",
        state.remote_screen_share_stream_port,
        percent_encode(&identity)
    ))
}

#[cfg(target_os = "linux")]
fn media_get_screen_share_raw_stream_url(
    identity: String,
    state: State<'_, MediaState>,
) -> Result<String, String> {
    if state.remote_screen_share_stream_port == 0 {
        return Err("remote screen share stream server is unavailable".to_string());
    }
    Ok(format!(
        "http://127.0.0.1:{}/screen-share.raw?identity={}&format=raw",
        state.remote_screen_share_stream_port,
        percent_encode(&identity)
    ))
}

/// Non-Linux stub for `media_get_screen_share_stream_url`.
#[tauri::command]
#[cfg(not(target_os = "linux"))]
pub fn media_get_screen_share_stream_url(_identity: String) -> Result<String, String> {
    Err("remote screen share stream URL is only available on Linux".to_string())
}

#[cfg(target_os = "linux")]
fn native_share_viewer_path() -> Result<std::path::PathBuf, String> {
    let exe = std::env::current_exe().map_err(|e| format!("cannot determine current exe: {e}"))?;
    let dir = exe
        .parent()
        .ok_or_else(|| "cannot determine exe directory".to_string())?;
    let viewer = dir.join("native_share_viewer");
    if viewer.exists() {
        Ok(viewer)
    } else {
        Err(format!(
            "native_share_viewer not found at {}",
            viewer.display()
        ))
    }
}

/// Open a native GTK viewer for a remote Linux screen share, bypassing WebKit rendering.
#[tauri::command]
#[cfg(target_os = "linux")]
pub fn media_open_native_screen_share_viewer(
    identity: String,
    title: String,
    state: State<'_, MediaState>,
) -> Result<(), String> {
    let url = media_get_screen_share_raw_stream_url(identity.clone(), state.clone())?;
    let viewer = native_share_viewer_path()?;

    let mut viewers = state
        .native_screen_share_viewers
        .lock()
        .map_err(|e| format!("native_screen_share_viewers lock: {e}"))?;
    if let Some(mut old) = viewers.remove(&identity) {
        let _ = old.kill();
        let _ = old.wait();
    }

    let child = std::process::Command::new(viewer)
        .arg(url)
        .arg(title)
        .spawn()
        .map_err(|e| format!("failed to spawn native screen share viewer: {e}"))?;
    viewers.insert(identity, child);
    Ok(())
}

/// Close a native GTK viewer opened by `media_open_native_screen_share_viewer`.
#[tauri::command]
#[cfg(target_os = "linux")]
pub fn media_close_native_screen_share_viewer(
    identity: String,
    state: State<'_, MediaState>,
) -> Result<(), String> {
    let mut viewers = state
        .native_screen_share_viewers
        .lock()
        .map_err(|e| format!("native_screen_share_viewers lock: {e}"))?;
    if let Some(mut child) = viewers.remove(&identity) {
        let _ = child.kill();
        let _ = child.wait();
    }
    Ok(())
}

#[tauri::command]
#[cfg(not(target_os = "linux"))]
pub fn media_open_native_screen_share_viewer(
    _identity: String,
    _title: String,
) -> Result<(), String> {
    Err("native screen share viewer is only available on Linux".to_string())
}

#[tauri::command]
#[cfg(not(target_os = "linux"))]
pub fn media_close_native_screen_share_viewer(_identity: String) -> Result<(), String> {
    Ok(())
}

/// Apply a screen share quality preset to the native capture pipeline.
///
/// Accepts `"low"`, `"high"`, or `"max"`. Takes effect immediately on the
/// next captured frame — no need to restart the capture pipeline.
///
/// Linux native preset values:
/// - `low`:  1920×1080 @ 60fps, JPEG 85
/// - `high`: 2560×1440 @ 30fps, JPEG 92
/// - `max`:  2560×1440 @ 60fps, JPEG 95
#[tauri::command]
#[cfg(target_os = "linux")]
pub fn media_set_screen_share_quality(
    quality: String,
    state: State<'_, MediaState>,
) -> Result<(), String> {
    state.screen_share_config.apply_preset(&quality)
}

#[tauri::command]
#[cfg(target_os = "windows")]
pub fn media_set_screen_share_quality(
    quality: String,
    state: State<'_, MediaState>,
) -> Result<(), String> {
    state.screen_share_config.apply_preset(&quality)
}

/// Non-Linux/non-Windows stub for `media_set_screen_share_quality`.
#[tauri::command]
#[cfg(not(any(target_os = "linux", target_os = "windows")))]
pub fn media_set_screen_share_quality(_quality: String) -> Result<(), String> {
    // On macOS, quality is controlled by the JS SDK — this is a no-op.
    Ok(())
}

/// Non-Linux stub — screen sharing is handled by LiveKit JS SDK on other platforms.
#[tauri::command]
#[cfg(not(target_os = "linux"))]
pub fn screen_share_start() -> Result<bool, String> {
    Err("screen sharing via native capture is only available on Linux".to_string())
}

#[cfg(all(test, target_os = "windows"))]
mod windows_capture_routing_tests {
    use super::{
        is_windows_capture_setup_error, parse_windows_source_kind, rgba_to_i420,
        select_windows_capture_backend, wait_for_first_pollable_frame,
        windows_bridge_transport_fps, windows_i420_stream_frame_header, LatestFrame,
        LatestI420Frame, WindowsCaptureBackend, WindowsSourceKind,
    };
    use crate::screen_capture::frame_processor::ScreenShareConfig;
    use crate::screen_capture::win_capture::WindowsNativeCaptureDiagnostics;
    use std::sync::{Arc, Mutex};
    use std::thread;
    use std::time::Duration;

    fn empty_i420_frame() -> Arc<Mutex<Option<LatestI420Frame>>> {
        Arc::new(Mutex::new(None))
    }

    #[test]
    fn rgba_to_i420_outputs_expected_length_for_even_dimensions() {
        let data = vec![255u8; 4 * 4 * 4];
        let (i420, width, height) = rgba_to_i420(&data, 4, 4).unwrap();

        assert_eq!(width, 4);
        assert_eq!(height, 4);
        assert_eq!(i420.len(), 4 * 4 * 3 / 2);
    }

    #[test]
    fn rgba_to_i420_crops_odd_dimensions_to_even_payload() {
        let data = vec![128u8; 5 * 3 * 4];
        let (i420, width, height) = rgba_to_i420(&data, 5, 3).unwrap();

        assert_eq!(width, 4);
        assert_eq!(height, 2);
        assert_eq!(i420.len(), 4 * 2 * 3 / 2);
    }

    #[test]
    fn i420_stream_header_encodes_fixed_wire_fields() {
        let frame = LatestI420Frame {
            frame: vec![0; 12],
            width: 4,
            height: 2,
            timestamp_us: 123_456,
            seq: 9,
        };
        let header = windows_i420_stream_frame_header(&frame, frame.frame.len() as u32);

        assert_eq!(u32::from_le_bytes(header[0..4].try_into().unwrap()), 4);
        assert_eq!(u32::from_le_bytes(header[4..8].try_into().unwrap()), 2);
        assert_eq!(u64::from_le_bytes(header[8..16].try_into().unwrap()), 9);
        assert_eq!(
            u64::from_le_bytes(header[16..24].try_into().unwrap()),
            123_456
        );
        assert_eq!(u32::from_le_bytes(header[24..28].try_into().unwrap()), 12);
    }

    #[test]
    fn source_kind_routes_without_handle_probing() {
        assert_eq!(
            parse_windows_source_kind(Some("screen")).unwrap(),
            WindowsSourceKind::Screen
        );
        assert_eq!(
            parse_windows_source_kind(Some("window")).unwrap(),
            WindowsSourceKind::Window
        );
        assert!(parse_windows_source_kind(None).is_err());
    }

    #[test]
    fn compatibility_mode_only_skips_wgc_for_windows() {
        assert_eq!(
            select_windows_capture_backend(WindowsSourceKind::Window, true, None).unwrap(),
            WindowsCaptureBackend::GdiPoll,
        );
        assert_eq!(
            select_windows_capture_backend(WindowsSourceKind::Screen, true, None).unwrap(),
            WindowsCaptureBackend::Wgc,
        );
    }

    #[test]
    fn bridge_transport_fps_follows_selected_preset() {
        let cfg = ScreenShareConfig::new();

        cfg.apply_preset("low").unwrap();
        assert_eq!(windows_bridge_transport_fps(&cfg), 60);

        cfg.apply_preset("high").unwrap();
        assert_eq!(windows_bridge_transport_fps(&cfg), 30);

        cfg.apply_preset("max").unwrap();
        assert_eq!(windows_bridge_transport_fps(&cfg), 60);
    }

    #[test]
    fn first_pollable_timeout_reports_current_operation_and_elapsed_time() {
        let latest_frame = Arc::new(Mutex::new(None::<LatestFrame>));
        let diagnostics = Arc::new(Mutex::new(WindowsNativeCaptureDiagnostics::new(
            "wgc", "screen", 1920, 1080,
        )));
        {
            let mut diag = diagnostics.lock().unwrap();
            diag.record_startup_stage("frame_pool_create_start", "CreateFreeThreaded", 12);
        }

        let error =
            wait_for_first_pollable_frame(&empty_i420_frame(), &latest_frame, &diagnostics, 0, 0)
                .unwrap_err();

        assert!(error.contains("stalled_in=first_pollable_frame_timeout"));
        assert!(error.contains("current_operation=wait for first pollable frame"));
        assert!(error.contains("startup_elapsed_ms="));
    }

    #[test]
    fn capture_session_create_error_reports_setup_failure() {
        let latest_frame = Arc::new(Mutex::new(None::<LatestFrame>));
        let diagnostics = Arc::new(Mutex::new(WindowsNativeCaptureDiagnostics::new(
            "wgc", "screen", 1920, 1080,
        )));
        {
            let mut diag = diagnostics.lock().unwrap();
            diag.record_startup_stage("frame_pool_create_done", "CreateFreeThreaded", 18);
            diag.record_startup_error(
                "capture_session_create_error",
                "CreateCaptureSession",
                20,
                "0x8001010E",
            );
        }

        let error = wait_for_first_pollable_frame(
            &empty_i420_frame(),
            &latest_frame,
            &diagnostics,
            5_000,
            7_500,
        )
        .unwrap_err();

        assert!(error.contains("native capture setup failed"));
        assert!(error.contains("stalled_in=capture_session_create_error"));
        assert!(error.contains("first_error=capture_session_create_error: 0x8001010E"));
        assert!(!error.contains("first_pollable_frame_timeout"));
    }

    #[test]
    fn first_pollable_timeout_remains_distinct_from_capture_session_error() {
        let latest_frame = Arc::new(Mutex::new(None::<LatestFrame>));
        let diagnostics = Arc::new(Mutex::new(WindowsNativeCaptureDiagnostics::new(
            "wgc", "screen", 1920, 1080,
        )));
        {
            let mut diag = diagnostics.lock().unwrap();
            diag.record_startup_stage("capture_session_create_done", "CreateCaptureSession", 20);
            diag.record_startup_stage("frame_arrived_handler_register_done", "FrameArrived", 25);
        }

        let error =
            wait_for_first_pollable_frame(&empty_i420_frame(), &latest_frame, &diagnostics, 0, 0)
                .unwrap_err();

        assert!(error.contains("stalled_in=first_pollable_frame_timeout"));
        assert!(!error.contains("native capture setup failed"));
        assert!(!error.contains("capture_session_create_error"));
    }

    #[test]
    fn startup_setup_error_classification_ignores_nonfatal_configuration_errors() {
        assert!(is_windows_capture_setup_error("capture_item_create_error"));
        assert!(is_windows_capture_setup_error(
            "capture_session_create_error"
        ));
        assert!(!is_windows_capture_setup_error(
            "cursor_border_config_error"
        ));
        assert!(!is_windows_capture_setup_error(
            "closed_handler_register_error"
        ));
        assert!(!is_windows_capture_setup_error(
            "first_pollable_frame_timeout"
        ));
    }

    #[test]
    fn cursor_border_unsupported_error_does_not_become_primary_first_error() {
        let mut diagnostics = WindowsNativeCaptureDiagnostics::new("wgc", "screen", 1920, 1080);

        diagnostics.record_nonfatal_startup_error(
            "cursor_border_config_error",
            "SetIsBorderRequired",
            22,
            "0x80004002",
        );
        diagnostics.record_startup_error(
            "capture_session_create_error",
            "CreateCaptureSession",
            30,
            "0x8001010E",
        );

        assert_eq!(
            diagnostics.first_error.as_deref(),
            Some("capture_session_create_error: 0x8001010E")
        );
        assert!(diagnostics
            .startup_step_timing_summary()
            .contains("cursor_border_config_error:22ms"));
    }

    #[test]
    fn raw_frame_arrived_keeps_waiting_past_raw_frame_deadline() {
        let latest_frame = Arc::new(Mutex::new(None::<LatestFrame>));
        let diagnostics = Arc::new(Mutex::new(WindowsNativeCaptureDiagnostics::new(
            "wgc", "screen", 1920, 1080,
        )));
        {
            let mut diag = diagnostics.lock().unwrap();
            diag.record_startup_stage("start_capture_done", "StartCapture", 20);
            diag.record_startup_stage("first_frame_arrived", "FrameArrived callback", 25);
            diag.frame_arrived_callbacks = 1;
        }
        let latest_frame_for_thread = latest_frame.clone();
        let writer = thread::spawn(move || {
            thread::sleep(Duration::from_millis(30));
            *latest_frame_for_thread.lock().unwrap() = Some(LatestFrame {
                frame: "jpeg".to_string(),
                width: 1920,
                height: 1080,
                seq: 1,
            });
        });

        let result =
            wait_for_first_pollable_frame(&empty_i420_frame(), &latest_frame, &diagnostics, 1, 250);

        writer.join().unwrap();
        assert!(result.is_ok());
    }

    #[test]
    fn raw_i420_pollable_frame_satisfies_first_frame_gate_without_jpeg() {
        let latest_i420_frame = Arc::new(Mutex::new(None::<LatestI420Frame>));
        let latest_frame = Arc::new(Mutex::new(None::<LatestFrame>));
        let diagnostics = Arc::new(Mutex::new(WindowsNativeCaptureDiagnostics::new(
            "wgc", "screen", 1920, 1080,
        )));
        let latest_i420_for_thread = latest_i420_frame.clone();
        let writer = thread::spawn(move || {
            thread::sleep(Duration::from_millis(15));
            *latest_i420_for_thread.lock().unwrap() = Some(LatestI420Frame {
                frame: vec![0; 1920 * 1080 * 3 / 2],
                width: 1920,
                height: 1080,
                timestamp_us: 16_667,
                seq: 1,
            });
        });

        let result =
            wait_for_first_pollable_frame(&latest_i420_frame, &latest_frame, &diagnostics, 1, 250);

        writer.join().unwrap();
        assert!(result.is_ok());
        assert!(latest_frame.lock().unwrap().is_none());
    }

    #[test]
    fn pollable_frame_after_old_wgc_boundary_succeeds() {
        let latest_frame = Arc::new(Mutex::new(None::<LatestFrame>));
        let diagnostics = Arc::new(Mutex::new(WindowsNativeCaptureDiagnostics::new(
            "wgc", "screen", 3440, 1440,
        )));
        {
            let mut diag = diagnostics.lock().unwrap();
            diag.record_startup_stage("start_capture_done", "StartCapture", 20);
            diag.record_startup_stage("first_frame_arrived", "FrameArrived callback", 25);
            diag.usable_frames = 1;
        }
        let latest_frame_for_thread = latest_frame.clone();
        let writer = thread::spawn(move || {
            thread::sleep(Duration::from_millis(35));
            *latest_frame_for_thread.lock().unwrap() = Some(LatestFrame {
                frame: "jpeg".to_string(),
                width: 2560,
                height: 1072,
                seq: 1,
            });
        });

        let result = wait_for_first_pollable_frame(
            &empty_i420_frame(),
            &latest_frame,
            &diagnostics,
            10,
            250,
        );

        writer.join().unwrap();
        assert!(result.is_ok());
    }

    #[test]
    fn first_pollable_processing_timeout_reports_processing_stage() {
        let latest_frame = Arc::new(Mutex::new(None::<LatestFrame>));
        let diagnostics = Arc::new(Mutex::new(WindowsNativeCaptureDiagnostics::new(
            "wgc", "screen", 3440, 1440,
        )));
        {
            let mut diag = diagnostics.lock().unwrap();
            diag.record_startup_stage("start_capture_done", "StartCapture", 20);
            diag.record_startup_stage("first_frame_arrived", "FrameArrived callback", 25);
            diag.frame_arrived_callbacks = 1;
        }

        let error =
            wait_for_first_pollable_frame(&empty_i420_frame(), &latest_frame, &diagnostics, 1, 0)
                .unwrap_err();

        assert!(error.contains("stalled_in=first_pollable_processing_timeout"));
        assert!(error.contains("current_operation=wait for first pollable frame"));
    }

    #[test]
    fn first_pollable_timeout_distinguishes_post_start_no_frame() {
        let latest_frame = Arc::new(Mutex::new(None::<LatestFrame>));
        let diagnostics = Arc::new(Mutex::new(WindowsNativeCaptureDiagnostics::new(
            "wgc", "screen", 1920, 1080,
        )));
        {
            let mut diag = diagnostics.lock().unwrap();
            diag.record_startup_stage("start_capture_done", "StartCapture", 20);
            diag.record_startup_stage("awaiting_first_frame", "FrameArrived callback", 21);
        }

        let error =
            wait_for_first_pollable_frame(&empty_i420_frame(), &latest_frame, &diagnostics, 0, 0)
                .unwrap_err();

        assert!(error.contains("post_start_no_frame=true"));
        assert!(error.contains("stalled_after=start_capture_done awaiting_first_frame"));
        assert!(error.contains("stalled_in=post_start_no_frame_timeout"));
    }
}

/// Non-Linux/non-Windows stub — screen sharing is handled by LiveKit JS SDK on other platforms.
#[tauri::command]
#[cfg(not(any(target_os = "linux", target_os = "windows")))]
pub fn screen_share_stop() -> Result<(), String> {
    Err("screen sharing via native capture is only available on Linux and Windows".to_string())
}
