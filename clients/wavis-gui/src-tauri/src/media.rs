//! Native media bridge — exposes RealLiveKitConnection to the frontend via Tauri IPC.
//!
//! On platforms where the webview lacks WebRTC (Linux/WebKitGTK), the React
//! frontend delegates media to this Rust module instead of the livekit-client
//! JS SDK. The interface mirrors LiveKitModule's shape so voice-room.ts can
//! swap transparently.

use serde::Serialize;
#[cfg(target_os = "linux")]
use std::sync::atomic::{AtomicBool, AtomicU32, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
#[cfg(target_os = "linux")]
use std::thread::JoinHandle as StdJoinHandle;
use tauri::{AppHandle, Emitter, State};
use tokio::runtime::{Builder, Runtime};
use tokio::task::JoinHandle;

use wavis_client_shared::audio::{AudioBackend, AudioTrack};
use wavis_client_shared::cpal_audio::{AudioBuffer, CpalAudioBackend, PeerVolumes};
use wavis_client_shared::denoise_filter::DenoiseFilter;
use wavis_client_shared::livekit_connection::RealLiveKitConnection;
use wavis_client_shared::room_session::LiveKitConnection;

#[cfg(target_os = "linux")]
use crate::screen_capture;
#[cfg(target_os = "windows")]
use crate::screen_capture;

const LOG: &str = "[wavis:native-media]";

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

// ─── Event Types (Rust → JS via Tauri events) ─────────────────────

#[derive(Clone, Serialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum MediaEvent {
    Connected,
    Failed {
        reason: String,
    },
    Disconnected,
    ParticipantMuteChanged {
        identity: String,
        is_muted: bool,
    },
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

#[cfg(target_os = "windows")]
struct NativeShareLeakSession {
    share_session_id: String,
    source_id: String,
    started_at_ms: u64,
    first_rust_frame_at_ms: Option<u64>,
    frames_buffered: u64,
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
    #[cfg(target_os = "windows")]
    native_share_leak_session: Arc<Mutex<Option<NativeShareLeakSession>>>,
}

impl MediaState {
    pub fn new() -> Self {
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
            #[cfg(target_os = "windows")]
            screen_capture: Mutex::new(None),
            #[cfg(any(target_os = "linux", target_os = "windows"))]
            screen_share_config: Arc::new(screen_capture::frame_processor::ScreenShareConfig::new()),
            #[cfg(target_os = "windows")]
            latest_frame: Arc::new(Mutex::new(None)),
            #[cfg(target_os = "windows")]
            native_share_leak_session: Arc::new(Mutex::new(None)),
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
        self.audio
            .stop()
            .map_err(|err| format!("failed to stop audio backend: {err}"))?;

        self.audio
            .capture_mic()
            .map_err(|err| format!("failed to start input device: {err}"))?;
        self.audio
            .play_remote(AudioTrack {
                id: "native-playback".to_string(),
            })
            .map_err(|err| format!("failed to start output device: {err}"))?;

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
            conn.set_capture_buffer(self.audio.capture_buffer.clone());
            conn.set_playback_buffer(self.audio.playback_buffer.clone());
            conn.set_peer_volumes(self.peer_volumes.clone());
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

    let app_mute = app.clone();
    conn.on_participant_mute_changed(Box::new(move |identity, is_muted| {
        let _ = app_mute.emit(
            "media-event",
            MediaEvent::ParticipantMuteChanged {
                identity: identity.to_string(),
                is_muted,
            },
        );
    }));

    // Wire video frame callback → encode JPEG, base64, emit screen_share_frame event.
    // Only compiled on Linux where the image + base64 crates are available.
    #[cfg(target_os = "linux")]
    {
        let app_video = app.clone();
        let viewer_config = state.screen_share_config.clone();
        conn.on_video_frame(Box::new(move |identity, rgba_data, width, height| {
            use base64::Engine;
            use image::codecs::jpeg::JpegEncoder;
            use std::io::Cursor;

            // Read JPEG quality from the shared config (runtime-adjustable).
            let jpeg_quality = viewer_config.jpeg_quality();

            // Encode RGBA → JPEG (strip alpha — JPEG only supports RGB).
            let rgb_data: Vec<u8> = rgba_data
                .chunks_exact(4)
                .flat_map(|rgba| [rgba[0], rgba[1], rgba[2]])
                .collect();
            let mut jpeg_buf = Cursor::new(Vec::with_capacity(128 * 1024));
            let mut encoder = JpegEncoder::new_with_quality(&mut jpeg_buf, jpeg_quality);
            if let Err(e) = encoder.encode(&rgb_data, width, height, image::ExtendedColorType::Rgb8)
            {
                log::warn!("{LOG} JPEG encode failed: {e}");
                return;
            }

            // Base64-encode the JPEG.
            let b64 = base64::engine::general_purpose::STANDARD.encode(jpeg_buf.into_inner());

            // Emit screen_share_frame Tauri event.
            let _ = app_video.emit_to(
                "main",
                "screen_share_frame",
                serde_json::json!({
                    "identity": identity,
                    "frame": b64,
                }),
            );
        }));

        let app_ended = app.clone();
        conn.on_video_track_ended(Box::new(move |identity| {
            log::info!("{LOG} remote screen share ended: {identity}");
            let _ = app_ended.emit_to(
                "main",
                "screen_share_ended",
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
            if let Err(err) = conn.publish_audio(&AudioTrack {
                id: "native-mic".to_string(),
            }) {
                let reason = format!("{err}");
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
    if let Ok(mut task_guard) = state.local_meter_task.lock() {
        if let Some(task) = task_guard.take() {
            task.abort();
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
    let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        log::info!("{LOG} set mic enabled: {enabled}");
        let lk_guard = state.lk.lock().map_err(|e| format!("lock: {e}"))?;
        if let Some(conn) = lk_guard.as_ref() {
            conn.set_mic_enabled(enabled)
                .map_err(|e| format!("failed to set mic enabled: {e}"))?;
        }
        Ok::<(), String>(())
    }));

    match result {
        Ok(inner) => inner?,
        Err(payload) => {
            let panic_msg = if let Some(msg) = payload.downcast_ref::<&str>() {
                (*msg).to_string()
            } else if let Some(msg) = payload.downcast_ref::<String>() {
                msg.clone()
            } else {
                "unknown panic payload".to_string()
            };
            return Err(format!("media_set_mic_enabled panicked: {panic_msg}"));
        }
    }
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
        Err(screen_capture::CaptureError::NoBackendAvailable(msg)) => return Err(msg),
        Err(screen_capture::CaptureError::CaptureStartFailed(msg)) => return Err(msg),
        Err(e) => return Err(format!("{e}")),
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
    state: State<'_, MediaState>,
    app: AppHandle,
) -> Result<bool, String> {
    use screen_capture::frame_processor::{cap_resolution, FrameThrottler};
    use screen_capture::win_capture::{WinCapture, WinCaptureConfig};
    use screen_capture::ScreenCapture;

    log::info!(
        "{LOG} [diag] screen_share_start_source ENTERED, source_id={source_id}, share_session_id={}",
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

    // NOTE (cleanup 2026-03): the `has_native_lk` branch that published a video
    // track via the Rust LiveKit SDK and fed frames with `conn.feed_video_frame()`
    // was removed here. On Windows, LiveKit is always managed by the JS SDK inside
    // WebView2; `state.lk` is always `None`, making that branch unreachable.
    // The native-LK frame-feeding path lives in the Linux `screen_share_start()`
    // function (no `_source` suffix) which is #[cfg(target_os = "linux")].

    // Create and start the Windows capture.
    let capture = WinCapture::start(WinCaptureConfig {
        source_id: source_id.clone(),
        app_handle: app.clone(),
    })
    .map_err(|e| {
        log::error!("{LOG} [diag] WinCapture::start() FAILED: {e}");
        format!("{e}")
    })?;

    log::info!("{LOG} [diag] WinCapture::start() returned OK");

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
                started_at_ms: unix_now_ms(),
                first_rust_frame_at_ms: None,
                frames_buffered: 0,
            });
        if let Some(session) = leak_guard.as_ref() {
            log::info!(
                "{LOG} native capture: session={} stage=native_capture_start source_id={}",
                session.share_session_id,
                session.source_id
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
        let throttler = Arc::new(FrameThrottler::new(config.max_fps()));
        let frame_count = Arc::new(std::sync::atomic::AtomicU64::new(0));
        let latest_frame = Arc::clone(&state.latest_frame);

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

        capture.on_frame(Box::new(move |frame| {
            use base64::Engine;
            use image::codecs::jpeg::JpegEncoder;
            use std::io::Cursor;

            let max_w = config.max_width();
            let max_h = config.max_height();
            throttler.set_fps(config.max_fps());

            // Throttle BEFORE downscaling — no point processing a frame we'll drop.
            if !throttler.should_emit() {
                return;
            }

            let capped = cap_resolution(frame, max_w, max_h);

            let n = frame_count.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
            if let Ok(mut leak_guard) = native_share_leak_session.lock() {
                if let Some(session) = leak_guard.as_mut() {
                    session.frames_buffered = n + 1;
                    if session.first_rust_frame_at_ms.is_none() {
                        let first_frame_at_ms = unix_now_ms();
                        session.first_rust_frame_at_ms = Some(first_frame_at_ms);
                        log::info!(
                            "{LOG} native capture: session={} stage=first_rust_frame source_id={} elapsed_ms={}",
                            session.share_session_id,
                            session.source_id,
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

            // Strip alpha channel into reusable RGB buffer (JPEG only supports RGB).
            let pixel_count = (capped.width * capped.height) as usize;
            let rgb_len = pixel_count * 3;
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

            // Encode RGB → JPEG into reusable buffer.
            let jpeg_quality = config.jpeg_quality();
            let mut jpeg_guard = match jpeg_buf.lock() {
                Ok(g) => g,
                Err(_) => return,
            };
            jpeg_guard.clear();
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

            if n == 0 {
                log::info!("{LOG} first JPEG: {} bytes", jpeg_guard.len());
            }

            let b64 = base64::engine::general_purpose::STANDARD.encode(&*jpeg_guard);

            // Drop buffers before taking the latest_frame lock to minimize hold time.
            drop(rgb_guard);
            drop(jpeg_guard);

            // Write to shared buffer — JS polls this via screen_share_poll_frame.
            // No PostMessage, no HWND, no window handle dependency.
            if let Ok(mut lf) = latest_frame.lock() {
                *lf = Some(LatestFrame {
                    frame: b64,
                    width: capped.width,
                    height: capped.height,
                    seq: n + 1,
                });
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
    *sc_guard = Some(Box::new(capture));

    log::info!("{LOG} [diag] capture stored in MediaState, function complete");

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
        if let Ok(mut leak_guard) = state.native_share_leak_session.lock() {
            if let Some(session) = leak_guard.take() {
                let ended_at_ms = unix_now_ms();
                let first_rust_frame_latency_ms = session
                    .first_rust_frame_at_ms
                    .map(|ts| ts.saturating_sub(session.started_at_ms));
                log::info!(
                    "{LOG} native capture: session={} stage=session_closed source_id={} duration_ms={} frames_buffered={} first_rust_frame_latency_ms={}",
                    session.share_session_id,
                    session.source_id,
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
pub fn screen_share_poll_frame(
    state: State<'_, MediaState>,
) -> Result<Option<LatestFrame>, String> {
    let lf = state
        .latest_frame
        .lock()
        .map_err(|e| format!("latest_frame lock: {e}"))?;
    Ok(lf.clone())
}

/// Non-Windows stub for `screen_share_poll_frame`.
#[tauri::command]
#[cfg(not(target_os = "windows"))]
pub fn screen_share_poll_frame() -> Result<Option<()>, String> {
    Ok(None)
}

/// Apply a screen share quality preset to the native capture pipeline.
///
/// Accepts `"low"`, `"high"`, or `"max"`. Takes effect immediately on the
/// next captured frame — no need to restart the capture pipeline.
///
/// Preset values:
/// - `low`:  1920×1080 @ 30fps, JPEG 85
/// - `high`: 2560×1440 @ 30fps, JPEG 92
/// - `max`:  2560×1440 @ 60fps, JPEG 95
#[tauri::command]
#[cfg(any(target_os = "linux", target_os = "windows"))]
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

/// Non-Linux/non-Windows stub — screen sharing is handled by LiveKit JS SDK on other platforms.
#[tauri::command]
#[cfg(not(any(target_os = "linux", target_os = "windows")))]
pub fn screen_share_stop() -> Result<(), String> {
    Err("screen sharing via native capture is only available on Linux and Windows".to_string())
}
