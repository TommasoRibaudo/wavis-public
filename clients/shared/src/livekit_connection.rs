//! Room lifecycle orchestration for the LiveKit SFU backend.
//!
//! Owns [`RealLiveKitConnection`], the production implementation of
//! [`crate::room_session::LiveKitConnection`]. Responsibilities: connect/disconnect
//! lifecycle, spawning background tasks, and aborting them on shutdown.
//!
//! Audio mixing: [`super::livekit_audio_mixing`]
//! Network monitoring: [`super::livekit_network_monitor`]
//! Video track management: [`super::livekit_video`]

#![allow(clippy::duplicated_attributes)]
#![cfg(feature = "livekit")]

use super::livekit_video::{detect_preferred_video_codec, rgba_to_i420};
use crate::audio::AudioTrack;
#[cfg(feature = "real-backends")]
use crate::audio_network_monitor::NetworkMonitorHandle;
#[cfg(feature = "real-backends")]
use crate::cpal_audio::AudioBuffer;
#[cfg(feature = "real-backends")]
use crate::cpal_audio::PeerVolumes;
#[cfg(feature = "real-backends")]
use crate::denoise_filter::DenoiseFilter;
use crate::room_session::{LiveKitConnection, RoomError};
use livekit::track::{LocalAudioTrack, LocalTrack, RemoteTrack, TrackSource};
use livekit::Room as LkRoom;
use log::warn;
#[cfg(feature = "real-backends")]
use std::collections::{HashMap, HashSet, VecDeque};
use std::sync::atomic::AtomicBool;
#[cfg(feature = "real-backends")]
use std::sync::atomic::AtomicU64;
use std::sync::{Arc, Mutex};
use tokio::runtime::Handle;
use tokio::task::JoinHandle;

// ---------------------------------------------------------------------------
// RealLiveKitConnection
// ---------------------------------------------------------------------------

/// Production implementation of `LiveKitConnection` using the LiveKit Rust SDK.
///
/// Bridges the synchronous `LiveKitConnection` trait with the async LiveKit SDK
/// via `tokio::task::block_in_place` + `Handle::block_on`.
///
/// Must be constructed from within a multi-threaded Tokio runtime context.
pub struct RealLiveKitConnection {
    /// Tokio runtime handle for bridging sync trait → async SDK.
    rt_handle: Handle,
    /// SDK-independent connection state flag. Used for fast precondition checks
    /// in trait methods without locking the room mutex.
    /// Also serves as a test seam: tests can call `store(true, SeqCst)` to
    /// simulate a connected state without a real LiveKit Room handle.
    connected: Arc<AtomicBool>,
    /// Connected LiveKit Room (None when disconnected).
    room: Arc<Mutex<Option<LkRoom>>>,
    /// Published local audio track handle (for cleanup on disconnect).
    published_track: Arc<Mutex<Option<LocalAudioTrack>>>,
    /// Desired local mic state (true = enabled/unmuted).
    mic_enabled: Arc<AtomicBool>,
    /// Stored audio frame callback (may be registered before connect).
    /// The callback MUST NOT block — it is invoked on a background task.
    #[allow(clippy::type_complexity)]
    audio_cb: Arc<Mutex<Option<Box<dyn Fn(&str, &[f32]) + Send + 'static>>>>,
    /// Background task handle for the room event listener loop.
    event_task: Arc<Mutex<Option<JoinHandle<()>>>>,
    /// Background task that pushes mic PCM into the NativeAudioSource.
    capture_task: Arc<Mutex<Option<JoinHandle<()>>>>,
    /// Flag to signal the event loop to stop (avoids races during disconnect).
    closing: Arc<AtomicBool>,
    /// CPAL capture buffer — set via `set_capture_buffer` before `publish_audio`.
    /// Provides mono 48kHz f32 samples from the mic.
    #[cfg(feature = "real-backends")]
    capture_buffer: Arc<Mutex<Option<AudioBuffer>>>,
    /// CPAL playback buffer — set via `set_playback_buffer` before `connect`.
    /// Receives mono 48kHz f32 samples from remote participants.
    #[cfg(feature = "real-backends")]
    playback_buffer: Arc<Mutex<Option<AudioBuffer>>>,
    /// Network monitor handle for feeding RTT/loss/jitter from LiveKit stats.
    /// Set via `set_network_monitor` before `connect`.
    #[cfg(feature = "real-backends")]
    net_monitor_handle: Arc<Mutex<Option<NetworkMonitorHandle>>>,
    /// Background task that polls LiveKit transport stats at 1/s.
    stats_task: Arc<Mutex<Option<JoinHandle<()>>>>,
    /// Background task that logs unified `Pipeline:` telemetry at 1/s.
    #[cfg(feature = "real-backends")]
    telemetry_task: Arc<Mutex<Option<JoinHandle<()>>>>,
    /// Shared sender counter: frames sent this interval (reset each second by telemetry loop).
    #[cfg(feature = "real-backends")]
    sender_frames_sent: Arc<AtomicU64>,
    /// Shared sender counter: frames dropped this interval.
    #[cfg(feature = "real-backends")]
    sender_frames_dropped: Arc<AtomicU64>,
    /// Per-peer volume map for scaling individual participant audio.
    #[cfg(feature = "real-backends")]
    peer_volumes: Arc<Mutex<Option<PeerVolumes>>>,
    /// Participants whose remote screen-share audio is currently allowed to
    /// enter the playback mix. This is driven by the viewer open/close state
    /// on Linux/WebKit where remote media is handled entirely on the Rust side.
    #[cfg(feature = "real-backends")]
    screen_share_audio_enabled: Arc<Mutex<HashSet<String>>>,
    /// Per-subscribed-audio-track decoded queues consumed by the mix loop.
    /// The key includes participant identity plus track identity/source so a
    /// participant's mic and screen-share audio do not collapse into one queue.
    #[cfg(feature = "real-backends")]
    remote_audio_queues: Arc<Mutex<HashMap<String, VecDeque<f32>>>>,
    /// Background task that mixes remote participants into a single playout stream.
    #[cfg(feature = "real-backends")]
    mix_task: Arc<Mutex<Option<JoinHandle<()>>>>,
    /// Grouped video fields (published track, source, callbacks).
    video: super::livekit_video::VideoState,
    /// Published screen share audio track handle (separate from mic track).
    published_screen_audio_track: Arc<Mutex<Option<LocalAudioTrack>>>,
    /// NativeAudioSource for feeding captured system audio into the screen share audio track.
    screen_audio_source:
        Arc<Mutex<Option<livekit::webrtc::audio_source::native::NativeAudioSource>>>,
    /// Shared DenoiseFilter for noise suppression in the capture task.
    /// Set via `set_denoise_filter()` before `publish_audio()`.
    /// The `Option` covers the construction-time window before the filter is wired;
    /// the `AtomicBool` inside `DenoiseFilter` handles enable/disable at runtime.
    #[cfg(feature = "real-backends")]
    denoise: Arc<Mutex<Option<Arc<DenoiseFilter>>>>,
    /// Callback for emitting transport stats (RTT, packet loss, jitter) to the
    /// frontend. Called at 1/s from the stats polling task. Receives
    /// `(rtt_ms, packet_loss_percent, jitter_ms)`.
    #[allow(clippy::type_complexity)]
    stats_cb: Arc<Mutex<Option<Box<dyn Fn(f64, f64, f64) + Send + 'static>>>>,
}

// ---------------------------------------------------------------------------
// Error mapping helpers
// ---------------------------------------------------------------------------

/// Maps any LiveKit SDK error (or other Display-able error) to a
/// `RoomError::SfuConnectionFailed`, preserving the original message.
fn map_sdk_error(e: impl std::fmt::Display, context: &str) -> RoomError {
    RoomError::SfuConnectionFailed(format!("{context}: {e}"))
}

/// Maps a publish-related LiveKit SDK error to `RoomError::PublishFailed`,
/// preserving the original message.
fn map_publish_error(e: impl std::fmt::Display) -> RoomError {
    RoomError::PublishFailed(format!("{e}"))
}

impl RealLiveKitConnection {
    /// Create a new instance. Must be called from within a Tokio runtime context.
    pub fn new() -> Self {
        Self {
            rt_handle: Handle::current(),
            connected: Arc::new(AtomicBool::new(false)),
            room: Arc::new(Mutex::new(None)),
            published_track: Arc::new(Mutex::new(None)),
            mic_enabled: Arc::new(AtomicBool::new(true)),
            audio_cb: Arc::new(Mutex::new(None)),
            event_task: Arc::new(Mutex::new(None)),
            capture_task: Arc::new(Mutex::new(None)),
            closing: Arc::new(AtomicBool::new(false)),
            #[cfg(feature = "real-backends")]
            capture_buffer: Arc::new(Mutex::new(None)),
            #[cfg(feature = "real-backends")]
            playback_buffer: Arc::new(Mutex::new(None)),
            #[cfg(feature = "real-backends")]
            net_monitor_handle: Arc::new(Mutex::new(None)),
            stats_task: Arc::new(Mutex::new(None)),
            #[cfg(feature = "real-backends")]
            telemetry_task: Arc::new(Mutex::new(None)),
            #[cfg(feature = "real-backends")]
            sender_frames_sent: Arc::new(AtomicU64::new(0)),
            #[cfg(feature = "real-backends")]
            sender_frames_dropped: Arc::new(AtomicU64::new(0)),
            #[cfg(feature = "real-backends")]
            peer_volumes: Arc::new(Mutex::new(None)),
            #[cfg(feature = "real-backends")]
            screen_share_audio_enabled: Arc::new(Mutex::new(HashSet::new())),
            #[cfg(feature = "real-backends")]
            remote_audio_queues: Arc::new(Mutex::new(HashMap::new())),
            #[cfg(feature = "real-backends")]
            mix_task: Arc::new(Mutex::new(None)),
            video: super::livekit_video::VideoState::new(),
            published_screen_audio_track: Arc::new(Mutex::new(None)),
            screen_audio_source: Arc::new(Mutex::new(None)),
            #[cfg(feature = "real-backends")]
            denoise: Arc::new(Mutex::new(None)),
            stats_cb: Arc::new(Mutex::new(None)),
        }
    }

    /// Set the CPAL capture buffer so `publish_audio` can read real mic samples.
    /// Must be called before `publish_audio` for audio to flow.
    #[cfg(feature = "real-backends")]
    pub fn set_capture_buffer(&self, buffer: AudioBuffer) {
        log::debug!("buffer_type=capture status=wired");
        *self.capture_buffer.lock().unwrap() = Some(buffer);
    }

    /// Set the CPAL playback buffer so received remote audio frames are written
    /// directly into it. Must be called before `connect` to avoid dropping early frames.
    #[cfg(feature = "real-backends")]
    pub fn set_playback_buffer(&self, buffer: AudioBuffer) {
        log::debug!("buffer_type=playback status=wired");
        *self.playback_buffer.lock().unwrap() = Some(buffer);
    }

    /// Wire a shared per-peer volume map. Must be called before `connect()`
    /// so per-participant audio streams can look up their gain.
    #[cfg(feature = "real-backends")]
    pub fn set_peer_volumes(&self, volumes: PeerVolumes) {
        *self.peer_volumes.lock().unwrap() = Some(volumes);
    }

    /// Allow or block a participant's remote ScreenShareAudio track from being
    /// mixed into playback.
    #[cfg(feature = "real-backends")]
    pub fn set_screen_share_audio_enabled(&self, participant_id: &str, enabled: bool) {
        let mut allowed = self.screen_share_audio_enabled.lock().unwrap();
        if enabled {
            allowed.insert(participant_id.to_string());
        } else {
            allowed.remove(participant_id);
            let prefix = format!("{participant_id}::ScreenshareAudio::");
            self.remote_audio_queues
                .lock()
                .unwrap()
                .retain(|key, _| !key.starts_with(&prefix));
        }
    }

    /// Set the network monitor handle so LiveKit transport stats (RTT, loss, jitter)
    /// are fed into the same `NetworkMonitorInput` used by the webrtc-rs path.
    /// Must be called before `connect` for stats to flow.
    #[cfg(feature = "real-backends")]
    pub fn set_network_monitor(&self, handle: NetworkMonitorHandle) {
        log::debug!("livekit: network_monitor wired");
        *self.net_monitor_handle.lock().unwrap() = Some(handle);
    }

    /// Wire a shared `DenoiseFilter` for noise suppression in the capture task.
    /// Must be called before `publish_audio()` for denoise to be applied.
    #[cfg(feature = "real-backends")]
    pub fn set_denoise_filter(&self, filter: Arc<DenoiseFilter>) {
        log::debug!("livekit: denoise_filter wired");
        *self.denoise.lock().unwrap() = Some(filter);
    }

    /// Register a callback for transport stats (RTT, packet loss, jitter).
    /// Called at 1/s from the stats polling task. Must be called before
    /// `connect()` for stats to flow to the frontend.
    pub fn on_stats(&self, cb: Box<dyn Fn(f64, f64, f64) + Send + 'static>) {
        *self.stats_cb.lock().unwrap() = Some(cb);
    }
}

impl Default for RealLiveKitConnection {
    fn default() -> Self {
        Self::new()
    }
}

impl LiveKitConnection for RealLiveKitConnection {
    fn is_available(&self) -> bool {
        true
    }

    // Task 4.2
    fn connect(&self, url: &str, token: &str) -> Result<(), RoomError> {
        use std::sync::atomic::Ordering::SeqCst;

        // Fast path: already connected — no mutex needed.
        if self.connected.load(SeqCst) {
            return Err(RoomError::AlreadyInRoom);
        }

        // Bridge sync → async: block_in_place moves the current worker thread out of
        // the Tokio thread pool so block_on cannot deadlock.
        let (room, mut events) = tokio::task::block_in_place(|| {
            self.rt_handle.block_on(async {
                LkRoom::connect(url, token, livekit::RoomOptions::default())
                    .await
                    .map_err(|e| map_sdk_error(e, "connect"))
            })
        })?;

        // Store room handle and flip state flags.
        *self.room.lock().unwrap() = Some(room);
        self.closing.store(false, SeqCst);
        self.connected.store(true, SeqCst);

        // Clone Arcs for the background event task.
        let audio_cb = Arc::clone(&self.audio_cb);
        let video_frame_cb = Arc::clone(&self.video.video_frame_cb);
        let video_track_ended_cb = Arc::clone(&self.video.video_track_ended_cb);
        let connected = Arc::clone(&self.connected);
        let closing = Arc::clone(&self.closing);
        #[cfg(feature = "real-backends")]
        let peer_volumes_outer = Arc::clone(&self.peer_volumes);
        #[cfg(feature = "real-backends")]
        let screen_share_audio_enabled_outer = Arc::clone(&self.screen_share_audio_enabled);
        #[cfg(feature = "real-backends")]
        let remote_queues_outer = Arc::clone(&self.remote_audio_queues);

        // Start remote audio mixer loop once per connection.
        // It consumes per-participant decoded PCM queues and writes one
        // time-aligned 20ms mixed frame to playback per tick.
        #[cfg(feature = "real-backends")]
        {
            let mix_handle = tokio::spawn(super::livekit_audio_mixing::run_mix_task(
                Arc::clone(&self.closing),
                Arc::clone(&self.playback_buffer),
                Arc::clone(&self.remote_audio_queues),
            ));
            *self.mix_task.lock().unwrap() = Some(mix_handle);
        }

        // Spawn background event listener.
        let handle = tokio::spawn(async move {
            use livekit::webrtc::audio_stream::native::NativeAudioStream;

            while let Some(event) = events.recv().await {
                if closing.load(std::sync::atomic::Ordering::SeqCst) {
                    break;
                }

                match event {
                    livekit::RoomEvent::TrackSubscribed {
                        track: RemoteTrack::Audio(audio_track),
                        publication,
                        participant,
                        ..
                    } => {
                        let rtc_track = audio_track.rtc_track();
                        // 48kHz mono — LiveKit typically delivers this format.
                        let stream = NativeAudioStream::new(rtc_track, 48_000, 1);
                        let participant_id = participant.identity().to_string();
                        let source = publication.source();
                        let queue_key =
                            format!("{participant_id}::{source:?}::{}", publication.sid());
                        let volume_key = if source == TrackSource::ScreenshareAudio {
                            format!("{participant_id}:screen-share")
                        } else {
                            participant_id.clone()
                        };
                        log::info!(
                            "livekit_audio: subscribed to audio track from participant={} source={:?} sid={} name={}",
                            participant_id,
                            source,
                            publication.sid(),
                            publication.name(),
                        );
                        let audio_cb = Arc::clone(&audio_cb);
                        let closing2 = Arc::clone(&closing);
                        #[cfg(feature = "real-backends")]
                        let peer_vols = Arc::clone(&peer_volumes_outer);
                        #[cfg(feature = "real-backends")]
                        let screen_share_audio_enabled =
                            Arc::clone(&screen_share_audio_enabled_outer);
                        #[cfg(feature = "real-backends")]
                        let remote_queues = Arc::clone(&remote_queues_outer);

                        #[cfg(feature = "real-backends")]
                        tokio::spawn(super::livekit_audio_mixing::run_participant_audio_decoder(
                            stream,
                            participant_id,
                            super::livekit_audio_mixing::ParticipantAudioDecoderContext {
                                volume_key,
                                source,
                                queue_key,
                                audio_cb,
                                closing: closing2,
                                peer_volumes: peer_vols,
                                screen_share_audio_enabled,
                                remote_queues,
                            },
                        ));
                        #[cfg(not(feature = "real-backends"))]
                        tokio::spawn(super::livekit_audio_mixing::run_participant_audio_decoder(
                            stream,
                            participant_id,
                            queue_key,
                            audio_cb,
                            closing2,
                        ));
                    }
                    livekit::RoomEvent::TrackSubscribed {
                        track: RemoteTrack::Video(video_track),
                        participant,
                        ..
                    } => {
                        use livekit::webrtc::video_stream::native::NativeVideoStream;

                        let rtc_track = video_track.rtc_track();
                        let stream = NativeVideoStream::new(rtc_track);
                        let participant_id = participant.identity().to_string();
                        log::info!(
                            "livekit_video: subscribed to video track from {participant_id}"
                        );

                        tokio::spawn(super::livekit_video::run_video_receiver_task(
                            stream,
                            participant_id,
                            Arc::clone(&video_frame_cb),
                            Arc::clone(&video_track_ended_cb),
                            Arc::clone(&closing),
                        ));
                    }
                    livekit::RoomEvent::TrackUnsubscribed {
                        track: RemoteTrack::Video(_),
                        participant,
                        ..
                    } => {
                        let participant_id = participant.identity().to_string();
                        log::info!("livekit_video: video track unsubscribed from {participant_id}");
                        if let Some(cb) = video_track_ended_cb.lock().unwrap().as_ref() {
                            cb(&participant_id);
                        }
                    }
                    livekit::RoomEvent::ParticipantDisconnected(participant) => {
                        let participant_id = participant.identity().to_string();
                        log::info!("livekit_video: participant disconnected: {participant_id}");
                        if let Some(cb) = video_track_ended_cb.lock().unwrap().as_ref() {
                            cb(&participant_id);
                        }
                    }
                    livekit::RoomEvent::Disconnected { .. } => {
                        // Server-initiated disconnect: clean up connected flag.
                        connected.store(false, std::sync::atomic::Ordering::SeqCst);
                        break;
                    }
                    _ => {}
                }
            }
        });

        *self.event_task.lock().unwrap() = Some(handle);

        // Spawn background stats polling task (1/s) to feed LiveKit transport
        // stats into the NetworkMonitorInput and emit stats to the frontend
        // via the stats callback.
        #[cfg(feature = "real-backends")]
        {
            let stats_handle = tokio::spawn(super::livekit_network_monitor::run_stats_task(
                Arc::clone(&self.net_monitor_handle),
                Arc::clone(&self.room),
                Arc::clone(&self.closing),
                Arc::clone(&self.stats_cb),
            ));
            *self.stats_task.lock().unwrap() = Some(stats_handle);
        }

        // Spawn unified pipeline telemetry loop (1/s) — mirrors the control
        // loop in webrtc_backend.rs so LiveKit mode emits the same `Pipeline:`
        // log line that §35 of TESTING.md documents.
        #[cfg(feature = "real-backends")]
        {
            let cap_buf = self.capture_buffer.lock().unwrap().clone();
            let play_buf = self.playback_buffer.lock().unwrap().clone();
            let net_handle_telem = self.net_monitor_handle.lock().unwrap().clone();
            let telem_handle = tokio::spawn(super::livekit_network_monitor::run_telemetry_task(
                Arc::clone(&self.closing),
                cap_buf,
                play_buf,
                net_handle_telem,
                Arc::clone(&self.sender_frames_sent),
                Arc::clone(&self.sender_frames_dropped),
            ));
            *self.telemetry_task.lock().unwrap() = Some(telem_handle);
        }

        Ok(())
    }

    // Task 4.5
    fn disconnect(&self) -> Result<(), RoomError> {
        use std::sync::atomic::Ordering::SeqCst;

        // Idempotent: already disconnected.
        if !self.connected.load(SeqCst) {
            return Ok(());
        }

        // Signal event loop to stop.
        self.closing.store(true, SeqCst);

        // Abort and await the event task.
        if let Some(handle) = self.event_task.lock().unwrap().take() {
            handle.abort();
            tokio::task::block_in_place(|| {
                self.rt_handle.block_on(async {
                    let _ = handle.await;
                })
            });
        }

        // Abort the capture task (mic → NativeAudioSource pump).
        if let Some(handle) = self.capture_task.lock().unwrap().take() {
            handle.abort();
            tokio::task::block_in_place(|| {
                self.rt_handle.block_on(async {
                    let _ = handle.await;
                })
            });
        }

        // Abort the stats polling task.
        if let Some(handle) = self.stats_task.lock().unwrap().take() {
            handle.abort();
            tokio::task::block_in_place(|| {
                self.rt_handle.block_on(async {
                    let _ = handle.await;
                })
            });
        }

        // Abort the telemetry task.
        #[cfg(feature = "real-backends")]
        if let Some(handle) = self.telemetry_task.lock().unwrap().take() {
            handle.abort();
            tokio::task::block_in_place(|| {
                self.rt_handle.block_on(async {
                    let _ = handle.await;
                })
            });
        }

        // Abort the remote mix task.
        #[cfg(feature = "real-backends")]
        if let Some(handle) = self.mix_task.lock().unwrap().take() {
            handle.abort();
            tokio::task::block_in_place(|| {
                self.rt_handle.block_on(async {
                    let _ = handle.await;
                })
            });
        }

        // Disconnect the room.
        let room_opt = self.room.lock().unwrap().take();
        if let Some(room) = room_opt {
            tokio::task::block_in_place(|| {
                self.rt_handle.block_on(async {
                    let _ = room.close().await;
                })
            });
        }

        // Clear remaining state.
        *self.published_track.lock().unwrap() = None;
        *self.video.published_video_track.lock().unwrap() = None;
        *self.video.video_source.lock().unwrap() = None;
        *self.published_screen_audio_track.lock().unwrap() = None;
        *self.screen_audio_source.lock().unwrap() = None;
        #[cfg(feature = "real-backends")]
        self.screen_share_audio_enabled.lock().unwrap().clear();
        #[cfg(feature = "real-backends")]
        self.remote_audio_queues.lock().unwrap().clear();
        self.connected.store(false, SeqCst);
        Ok(())
    }

    // Task 4.4
    fn on_audio_frame(&self, cb: Box<dyn Fn(&str, &[f32]) + Send + 'static>) {
        *self.audio_cb.lock().unwrap() = Some(cb);
    }

    fn on_video_frame(&self, cb: Box<dyn Fn(&str, &[u8], u32, u32) + Send + 'static>) {
        *self.video.video_frame_cb.lock().unwrap() = Some(cb);
    }

    fn on_video_track_ended(&self, cb: Box<dyn Fn(&str) + Send + 'static>) {
        *self.video.video_track_ended_cb.lock().unwrap() = Some(cb);
    }

    // Task 4.3
    fn publish_audio(&self, _track: &AudioTrack) -> Result<(), RoomError> {
        use livekit::options::TrackPublishOptions;
        use livekit::webrtc::audio_source::native::NativeAudioSource;
        use livekit::webrtc::audio_source::AudioSourceOptions;
        use std::sync::atomic::Ordering::SeqCst;

        // Fast path: not connected.
        if !self.connected.load(SeqCst) {
            return Err(RoomError::NotInRoom);
        }

        let room_guard = self.room.lock().unwrap();
        let room = room_guard.as_ref().ok_or(RoomError::NotInRoom)?;

        // Create a NativeAudioSource (48kHz mono, 100ms queue).
        let source = NativeAudioSource::new(
            AudioSourceOptions::default(),
            48_000,
            1,
            20, // keep screen-share audio latency tight
        );
        let rtc_source = livekit::webrtc::audio_source::RtcAudioSource::Native(source.clone());
        let lk_track = LocalAudioTrack::create_audio_track("mic", rtc_source);

        let local_track = LocalTrack::Audio(lk_track.clone());
        let publish_opts = TrackPublishOptions {
            source: TrackSource::Microphone,
            ..Default::default()
        };

        tokio::task::block_in_place(|| {
            self.rt_handle.block_on(async {
                room.local_participant()
                    .publish_track(local_track, publish_opts)
                    .await
                    .map_err(map_publish_error)
            })
        })?;

        *self.published_track.lock().unwrap() = Some(lk_track);
        // Apply the current desired mic state (may have been toggled before
        // publish_audio finished during reconnect races).
        if let Ok(guard) = self.published_track.lock() {
            if let Some(track) = guard.as_ref() {
                let enabled = self.mic_enabled.load(SeqCst);
                let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                    if enabled {
                        track.enable();
                        track.unmute();
                    } else {
                        track.disable();
                        track.mute();
                    }
                }));
                if result.is_err() {
                    return Err(RoomError::PublishFailed(
                        "failed to apply initial mic enabled state".to_string(),
                    ));
                }
            }
        }

        // Spawn background task to push mic PCM into the NativeAudioSource.
        // Reads 960 mono f32 samples (20ms @ 48kHz) from the capture buffer,
        // applies denoise (if wired), converts to i16, and calls capture_frame
        // every 20ms using absolute deadline pacing to eliminate cumulative drift.
        #[cfg(feature = "real-backends")]
        {
            let capture_buf = self.capture_buffer.lock().unwrap().clone();
            let closing = Arc::clone(&self.closing);
            let shared_frames_sent = Arc::clone(&self.sender_frames_sent);
            let denoise = self.denoise.lock().unwrap().clone();
            let rt_handle = self.rt_handle.clone();

            if let Some(buf) = capture_buf {
                let handle = rt_handle.spawn(async move {
                    use livekit::webrtc::audio_frame::AudioFrame;
                    use std::borrow::Cow;
                    use std::collections::VecDeque;
                    use std::time::{Duration, Instant};
                    use tokio::time::{interval, MissedTickBehavior};

                    const FRAME_SAMPLES: usize = 960; // 20ms @ 48kHz mono
                    const READ_CHUNK_SAMPLES: usize = FRAME_SAMPLES * 2;
                    const MAX_PENDING_SAMPLES: usize = FRAME_SAMPLES * 6; // 120ms
                    const TARGET_PENDING_SAMPLES: usize = FRAME_SAMPLES * 3; // 60ms
                    const PREFILL_SAMPLES: usize = FRAME_SAMPLES * 3; // 60ms startup cushion

                    let mut read_buf = vec![0.0f32; READ_CHUNK_SAMPLES];
                    let mut pending = VecDeque::<f32>::with_capacity(MAX_PENDING_SAMPLES);
                    let mut frame_f32 = vec![0.0f32; FRAME_SAMPLES];
                    let mut frame_i16 = vec![0i16; FRAME_SAMPLES];
                    let mut last_sample_f32: f32 = 0.0;
                    let mut primed = false;

                    // Absolute-deadline pacing state.
                    let start = Instant::now();
                    let cadence = Duration::from_millis(20);
                    let mut frame_idx: u64 = 0;
                    let mut ticker = interval(cadence);
                    ticker.set_missed_tick_behavior(MissedTickBehavior::Skip);

                    // Local diagnostic counters.
                    let mut frames_sent: u64 = 0;
                    let mut underrun_count: u64 = 0;
                    let mut total_samples_read: u64 = 0;
                    let mut pending_drops: u64 = 0;

                    log::info!("capture_loop=start pacing=absolute-deadline frame_size=960 sample_rate=48000");

                    loop {
                        // Wait for the next absolute deadline tick.
                        ticker.tick().await;

                        if closing.load(std::sync::atomic::Ordering::SeqCst) {
                            break;
                        }

                        // Compute expected deadline for this frame and measure lateness.
                        let now = Instant::now();
                        let expected = start + cadence * frame_idx as u32;
                        let lateness = now.saturating_duration_since(expected);

                        // If we're more than one full cadence late, realign frame_idx
                        // to the current wall-clock position (skip missed deadlines).
                        if lateness > cadence {
                            let skipped = lateness.as_millis() / cadence.as_millis();
                            log::warn!(
                                "capture_loop=skip skipped_frames={skipped} drift_ms={}",
                                lateness.as_millis()
                            );
                            frame_idx = ((now - start).as_millis() / cadence.as_millis()) as u64;
                        }

                        // Pull available capture samples into a small queue first.
                        // This decouples CPAL callback chunking from 20ms frame boundaries.
                        let available = buf.available();
                        let to_read = available.min(READ_CHUNK_SAMPLES);
                        if to_read > 0 {
                            let read = buf.read(&mut read_buf[..to_read]);
                            total_samples_read += read as u64;
                            pending.extend(read_buf[..read].iter().copied());
                        }

                        // Bound queue growth to avoid latency creep if sender is late.
                        if pending.len() > MAX_PENDING_SAMPLES {
                            let drop_n = pending.len() - TARGET_PENDING_SAMPLES;
                            pending.drain(..drop_n);
                            pending_drops += drop_n as u64;
                        }

                        // Wait for a small startup cushion before publishing so
                        // minor CPAL callback cadence mismatch does not cause
                        // immediate underrun-driven robotic artifacts.
                        if !primed {
                            if pending.len() < PREFILL_SAMPLES {
                                continue;
                            }
                            primed = true;
                        }

                        // Fill one 20ms f32 frame from queued samples.
                        let mut filled = 0usize;
                        while filled < FRAME_SAMPLES {
                            if let Some(sample) = pending.pop_front() {
                                let s = sample.clamp(-1.0, 1.0);
                                frame_f32[filled] = s;
                                last_sample_f32 = s;
                                filled += 1;
                            } else {
                                break;
                            }
                        }

                        // If we still don't have a full frame, hold the last sample
                        // instead of injecting hard zeros (sounds less robotic).
                        if filled < FRAME_SAMPLES {
                            underrun_count += 1;
                            frame_f32[filled..].fill(last_sample_f32);
                        }

                        // Apply denoise on the full f32 frame before i16 conversion.
                        // No APM NS coordination needed — the LiveKit path has no APM.
                        if let Some(ref dn) = denoise {
                            dn.process(&mut frame_f32[..FRAME_SAMPLES]);
                        }

                        // Convert f32 → i16 for the LiveKit NativeAudioSource.
                        for (i, &s) in frame_f32.iter().enumerate() {
                            frame_i16[i] = (s * 32767.0) as i16;
                        }

                        let frame = AudioFrame {
                            data: Cow::Borrowed(&frame_i16),
                            sample_rate: 48_000,
                            num_channels: 1,
                            samples_per_channel: FRAME_SAMPLES as u32,
                        };

                        if let Err(e) = source.capture_frame(&frame).await {
                            warn!("LiveKit capture_frame error: {e}");
                        }

                        frames_sent += 1;
                        frame_idx += 1;
                        shared_frames_sent.fetch_add(1, std::sync::atomic::Ordering::Relaxed);

                        // Periodic stats every 250 ticks (~5 seconds).
                        if frames_sent.is_multiple_of(250) {
                            let avg_samples_read =
                                total_samples_read.checked_div(frames_sent).unwrap_or(0);
                            log::info!(
                                "capture_loop=stats frames_sent={frames_sent} underruns={underrun_count} pending_samples={} avg_samples_read={avg_samples_read} pending_drops={pending_drops}",
                                pending.len()
                            );
                        }
                    }

                    let duration_secs = start.elapsed().as_secs();
                    log::info!(
                        "capture_loop=stop total_frames={frames_sent} total_underruns={underrun_count} duration_secs={duration_secs} pending_drops={pending_drops}"
                    );
                });

                *self.capture_task.lock().unwrap() = Some(handle);
            } else {
                warn!("No capture buffer set — LiveKit track published but mic audio will not flow. Call set_capture_buffer() before publish_audio().");
            }
        }

        #[cfg(not(feature = "real-backends"))]
        {
            warn!("real-backends feature not enabled — LiveKit track published without mic audio.");
        }

        Ok(())
    }

    fn set_mic_enabled(&self, enabled: bool) -> Result<(), RoomError> {
        use std::sync::atomic::Ordering::SeqCst;

        self.mic_enabled.store(enabled, SeqCst);
        if let Ok(guard) = self.published_track.lock() {
            if let Some(track) = guard.as_ref() {
                let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                    if enabled {
                        track.enable();
                        track.unmute();
                    } else {
                        track.disable();
                        track.mute();
                    }
                }));
                if result.is_err() {
                    return Err(RoomError::PublishFailed(
                        "failed to toggle mic state".to_string(),
                    ));
                }
            }
        }
        Ok(())
    }

    fn publish_video(&self, width: u32, height: u32) -> Result<(), RoomError> {
        use livekit::options::TrackPublishOptions;
        use livekit::webrtc::video_source::native::NativeVideoSource;
        use livekit::webrtc::video_source::RtcVideoSource;
        use livekit::webrtc::video_source::VideoResolution;
        use std::sync::atomic::Ordering::SeqCst;

        if !self.connected.load(SeqCst) {
            return Err(RoomError::NotInRoom);
        }

        // Already publishing video — idempotent.
        if self.video.published_video_track.lock().unwrap().is_some() {
            log::debug!("publish_video: already publishing, returning Ok");
            return Ok(());
        }

        let room_guard = self.room.lock().unwrap();
        let room = room_guard.as_ref().ok_or(RoomError::NotInRoom)?;

        let resolution = VideoResolution { width, height };

        // Create a NativeVideoSource optimised for screen content.
        let source = NativeVideoSource::new(resolution, true);
        let rtc_source = RtcVideoSource::Native(source.clone());
        let lk_track = livekit::track::LocalVideoTrack::create_video_track("screen", rtc_source);

        // Detect codec preference: H.264 if VA-API hardware encoder available,
        // VP8 otherwise. Detection is Linux-only; other platforms use default.
        let video_codec = detect_preferred_video_codec();

        let publish_opts = TrackPublishOptions {
            source: TrackSource::Screenshare,
            video_codec,
            video_encoding: Some(livekit::options::VideoEncoding {
                max_bitrate: 3_000_000, // 3 Mbps for crisp screen content
                max_framerate: 30.0,
            }),
            simulcast: false, // no need for simulcast on screen share
            ..Default::default()
        };

        log::info!(
            "publish_video: publishing {}x{} screen share track, codec={:?}",
            width,
            height,
            video_codec
        );
        self.video
            .next_timestamp_us
            .store(1, std::sync::atomic::Ordering::Relaxed);

        tokio::task::block_in_place(|| {
            self.rt_handle.block_on(async {
                room.local_participant()
                    .publish_track(
                        livekit::track::LocalTrack::Video(lk_track.clone()),
                        publish_opts,
                    )
                    .await
                    .map_err(map_publish_error)
            })
        })?;

        *self.video.published_video_track.lock().unwrap() = Some(lk_track);
        *self.video.video_source.lock().unwrap() = Some(source);

        log::info!("publish_video: screen share track published successfully");
        Ok(())
    }

    fn feed_video_frame(&self, data: &[u8], width: u32, height: u32) -> Result<(), RoomError> {
        use livekit::webrtc::video_frame::{VideoFrame, VideoRotation};
        use std::sync::atomic::Ordering::Relaxed;

        let source_guard = self.video.video_source.lock().unwrap();
        let source = source_guard.as_ref().ok_or_else(|| {
            RoomError::PublishFailed("no video source — call publish_video first".to_string())
        })?;

        let expected_len = (width as usize) * (height as usize) * 4;
        if data.len() != expected_len {
            return Err(RoomError::PublishFailed(format!(
                "RGBA data length mismatch: expected {} bytes ({}x{}x4), got {}",
                expected_len,
                width,
                height,
                data.len()
            )));
        }

        // Convert RGBA → I420 (YUV 4:2:0) for the LiveKit SDK.
        let i420 = rgba_to_i420(data, width, height);
        let timestamp_us = self.video.next_timestamp_us.fetch_add(33_333, Relaxed);

        let frame = VideoFrame {
            rotation: VideoRotation::VideoRotation0,
            buffer: i420,
            timestamp_us,
        };

        // capture_frame is synchronous in the LiveKit Rust SDK.
        source.capture_frame(&frame);
        Ok(())
    }

    fn unpublish_video(&self) -> Result<(), RoomError> {
        use std::sync::atomic::Ordering::SeqCst;

        // Clear video source first to stop accepting new frames.
        *self.video.video_source.lock().unwrap() = None;

        let video_track = self.video.published_video_track.lock().unwrap().take();
        if video_track.is_none() {
            return Ok(()); // Nothing to unpublish.
        }

        if !self.connected.load(SeqCst) {
            return Ok(()); // Room already disconnected, track is gone.
        }

        // Unpublish the video track from the room.
        let room_guard = self.room.lock().unwrap();
        if let Some(room) = room_guard.as_ref() {
            let track_sid = video_track.as_ref().unwrap().sid();

            tokio::task::block_in_place(|| {
                self.rt_handle.block_on(async {
                    if let Err(e) = room.local_participant().unpublish_track(&track_sid).await {
                        warn!("unpublish_video: failed to unpublish track: {e}");
                    }
                })
            });
        }

        log::info!("unpublish_video: screen share track unpublished");
        Ok(())
    }

    fn publish_screen_audio(&self) -> Result<(), RoomError> {
        use livekit::options::TrackPublishOptions;
        use livekit::webrtc::audio_source::native::NativeAudioSource;
        use livekit::webrtc::audio_source::AudioSourceOptions;
        use std::sync::atomic::Ordering::SeqCst;

        if !self.connected.load(SeqCst) {
            return Err(RoomError::NotInRoom);
        }

        // Guard: don't publish a second screen audio track.
        if self.published_screen_audio_track.lock().unwrap().is_some() {
            return Err(RoomError::PublishFailed(
                "screen share audio track already published".to_string(),
            ));
        }

        let room_guard = self.room.lock().unwrap();
        let room = room_guard.as_ref().ok_or(RoomError::NotInRoom)?;

        // Create a NativeAudioSource (48kHz mono, 100ms queue) — same params as mic.
        let source = NativeAudioSource::new(
            AudioSourceOptions::default(),
            48_000,
            1,
            100, // 100ms queue
        );
        let rtc_source = livekit::webrtc::audio_source::RtcAudioSource::Native(source.clone());
        let lk_track = LocalAudioTrack::create_audio_track("screen_audio", rtc_source);

        let local_track = LocalTrack::Audio(lk_track.clone());
        let publish_opts = TrackPublishOptions {
            source: TrackSource::ScreenshareAudio,
            ..Default::default()
        };

        tokio::task::block_in_place(|| {
            self.rt_handle.block_on(async {
                room.local_participant()
                    .publish_track(local_track, publish_opts)
                    .await
                    .map_err(map_publish_error)
            })
        })?;

        *self.published_screen_audio_track.lock().unwrap() = Some(lk_track);
        *self.screen_audio_source.lock().unwrap() = Some(source);

        log::info!("publish_screen_audio: screen share audio track published");
        Ok(())
    }

    fn feed_screen_audio(&self, samples: &[i16]) -> Result<(), RoomError> {
        use livekit::webrtc::audio_frame::AudioFrame;
        use std::borrow::Cow;

        let source_guard = self.screen_audio_source.lock().unwrap();
        let source = source_guard.as_ref().ok_or_else(|| {
            RoomError::PublishFailed("screen share audio not published".to_string())
        })?;

        let frame = AudioFrame {
            data: Cow::Borrowed(samples),
            sample_rate: 48_000,
            num_channels: 1,
            samples_per_channel: samples.len() as u32,
        };

        let source = source.clone();
        tokio::task::block_in_place(|| {
            self.rt_handle.block_on(async {
                source
                    .capture_frame(&frame)
                    .await
                    .map_err(|e| RoomError::PublishFailed(format!("feed_screen_audio: {e}")))
            })
        })
    }

    fn unpublish_screen_audio(&self) -> Result<(), RoomError> {
        use std::sync::atomic::Ordering::SeqCst;

        // Clear source first to stop accepting new frames.
        *self.screen_audio_source.lock().unwrap() = None;

        let screen_audio_track = self.published_screen_audio_track.lock().unwrap().take();
        if screen_audio_track.is_none() {
            return Ok(()); // Nothing to unpublish.
        }

        if !self.connected.load(SeqCst) {
            return Ok(()); // Room already disconnected, track is gone.
        }

        let room_guard = self.room.lock().unwrap();
        if let Some(room) = room_guard.as_ref() {
            let track_sid = screen_audio_track.as_ref().unwrap().sid();

            tokio::task::block_in_place(|| {
                self.rt_handle.block_on(async {
                    if let Err(e) = room.local_participant().unpublish_track(&track_sid).await {
                        warn!("unpublish_screen_audio: failed to unpublish track: {e}");
                    }
                })
            });
        }

        log::info!("unpublish_screen_audio: screen share audio track unpublished");
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use proptest::prelude::*;

    /// Compile-time assertion that RealLiveKitConnection is Send + Sync (Req 1.6).
    #[allow(dead_code)]
    fn _assert_send_sync<T: Send + Sync>() {}
    #[allow(dead_code)]
    fn _check() {
        _assert_send_sync::<RealLiveKitConnection>();
    }

    // -----------------------------------------------------------------------
    // Feature: livekit-client-connection, Property 5: Error mapping preserves original message
    // **Validates: Requirements 5.1, 5.3**
    //
    // For any error message string, mapping it through the LiveKit-to-RoomError
    // conversion SHALL produce a RoomError variant whose Display output contains
    // the original error message string.
    // -----------------------------------------------------------------------

    proptest! {
        /// map_sdk_error wraps the message in SfuConnectionFailed; Display contains original msg.
        #[test]
        fn prop_map_sdk_error_preserves_message(msg in ".*", ctx in ".*") {
            let err = map_sdk_error(&msg, &ctx);
            let displayed = format!("{err}");
            prop_assert!(
                displayed.contains(msg.as_str()),
                "Display output {:?} does not contain original message {:?}",
                displayed,
                msg
            );
        }

        /// map_publish_error wraps the message in PublishFailed; Display contains original msg.
        #[test]
        fn prop_map_publish_error_preserves_message(msg in ".*") {
            let err = map_publish_error(&msg);
            let displayed = format!("{err}");
            prop_assert!(
                displayed.contains(msg.as_str()),
                "Display output {:?} does not contain original message {:?}",
                displayed,
                msg
            );
        }
    }

    // -----------------------------------------------------------------------
    // Feature: livekit-client-connection, Property 4: Disconnect is idempotent
    // **Validates: Requirements 4.2**
    //
    // For any RealLiveKitConnection (connected or not), calling disconnect() any
    // number of times SHALL always return Ok(()).
    // Tests on a never-connected instance — no SDK room handle needed.
    // -----------------------------------------------------------------------

    proptest! {
        #[test]
        fn prop_disconnect_idempotent(times in 1usize..=10usize) {
            // Build a minimal runtime so Handle::current() works inside new().
            let rt = tokio::runtime::Builder::new_multi_thread()
                .worker_threads(1)
                .build()
                .unwrap();
            let conn = rt.block_on(async { RealLiveKitConnection::new() });
            for _ in 0..times {
                prop_assert_eq!(conn.disconnect(), Ok(()));
            }
        }
    }

    // -----------------------------------------------------------------------
    // Feature: livekit-client-connection, Property 2: Publish before connect returns NotInRoom
    // **Validates: Requirements 2.2**
    //
    // For any AudioTrack, calling publish_audio(track) on a RealLiveKitConnection
    // that has never connected SHALL return RoomError::NotInRoom.
    // -----------------------------------------------------------------------

    proptest! {
        #[test]
        fn prop_publish_before_connect_returns_not_in_room(_dummy in 0u8..=255u8) {
            let rt = tokio::runtime::Builder::new_multi_thread()
                .worker_threads(1)
                .build()
                .unwrap();
            let conn = rt.block_on(async { RealLiveKitConnection::new() });
            let track = crate::audio::AudioTrack { id: "test-track".to_string() };
            prop_assert_eq!(conn.publish_audio(&track), Err(RoomError::NotInRoom));
        }
    }

    // -----------------------------------------------------------------------
    // Feature: livekit-client-connection, Property 1: Double connect returns AlreadyInRoom
    // **Validates: Requirements 1.4**
    //
    // For any RealLiveKitConnection that has successfully connected, calling
    // connect() a second time SHALL return RoomError::AlreadyInRoom.
    // Uses the `connected` AtomicBool test seam to simulate connected state.
    // -----------------------------------------------------------------------

    proptest! {
        #[test]
        fn prop_double_connect_returns_already_in_room(
            url in ".*",
            token in ".*",
        ) {
            let rt = tokio::runtime::Builder::new_multi_thread()
                .worker_threads(1)
                .build()
                .unwrap();
            let conn = rt.block_on(async { RealLiveKitConnection::new() });
            // Simulate already-connected state via the test seam.
            conn.connected.store(true, std::sync::atomic::Ordering::SeqCst);
            prop_assert_eq!(
                conn.connect(&url, &token),
                Err(RoomError::AlreadyInRoom)
            );
        }
    }
}
