//! Owns `RoomSession` lifecycle orchestration, shared room-session types, and
//! the single signaling dispatcher for room state updates.
//!
//! This module does not own participant-specific tracking or screen-share
//! workflows long term; those concerns are split into child modules so state
//! mutation paths remain explicit.

mod participants;
mod screen_share;

use self::participants::SubscribeTrackState;
use crate::audio::{AudioBackend, AudioError, AudioTrack};
use crate::ice_config::IceConfig;
use crate::sdp_ice_guards::{check_ice_candidate_size, check_sdp_size};
use crate::signaling::{SignalingClient, WebSocketConnection};
use crate::webrtc::PeerConnectionBackend;
use shared::signaling::{JoinPayload, ParticipantInfo, SignalingMessage};
use std::collections::{HashMap, HashSet};
use std::sync::{Arc, Mutex};
use thiserror::Error;

/// Shared callback type for event handlers.
type EventCb<T> = Arc<Mutex<Option<Box<dyn Fn(T) + Send + 'static>>>>;

/// Shared callback type for screen-share event handlers (Arc-wrapped, `Sync`-bound).
type ShareCb<T> = Arc<Mutex<Option<Arc<dyn Fn(T) + Send + Sync>>>>;

// ---------------------------------------------------------------------------
// Error types
// ---------------------------------------------------------------------------

#[derive(Debug, Error, PartialEq)]
pub enum RoomError {
    #[error("not in a room")]
    NotInRoom,
    #[error("already in a room")]
    AlreadyInRoom,
    #[error("SFU connection failed: {0}")]
    SfuConnectionFailed(String),
    #[error("publish failed: {0}")]
    PublishFailed(String),
    #[error("audio error: {0}")]
    Audio(String),
    #[error("signaling error: {0}")]
    Signaling(String),
}

impl From<AudioError> for RoomError {
    fn from(e: AudioError) -> Self {
        RoomError::Audio(e.to_string())
    }
}

// ---------------------------------------------------------------------------
// LiveKitConnection trait + implementations
// ---------------------------------------------------------------------------

/// Abstraction over the LiveKit client SDK for testability.
/// In LiveKit mode, `RoomSession` calls `connect` + `publish_audio` instead of
/// the `PeerConnectionBackend` SDP/ICE flow.
#[allow(clippy::type_complexity)]
pub trait LiveKitConnection: Send + Sync {
    /// Connect to a LiveKit room with the given URL and token.
    fn connect(&self, url: &str, token: &str) -> Result<(), RoomError>;
    /// Disconnect from the LiveKit room.
    fn disconnect(&self) -> Result<(), RoomError>;
    /// Register callback for receiving decoded PCM audio from a remote participant.
    fn on_audio_frame(&self, cb: Box<dyn Fn(&str, &[f32]) + Send + 'static>);
    /// Publish local audio track to the LiveKit room.
    fn publish_audio(&self, track: &AudioTrack) -> Result<(), RoomError>;
    /// Enable/disable local microphone publishing state.
    /// Implementations should map this to track mute/unmute when available.
    fn set_mic_enabled(&self, _enabled: bool) -> Result<(), RoomError> {
        Ok(())
    }
    /// Returns true if this is a real LiveKit implementation that will handle
    /// media transport directly (bypassing the WebRTC PeerConnection path).
    fn is_available(&self) -> bool {
        false
    }

    /// Publish a video track for screen sharing. Creates a `LocalVideoTrack`
    /// from a video source and publishes it to the room with screen share
    /// source type and detected codec preference (H.264 if VA-API available,
    /// VP8 otherwise).
    /// Default: returns error (not supported).
    fn publish_video(&self, _width: u32, _height: u32) -> Result<(), RoomError> {
        Err(RoomError::PublishFailed(
            "video publishing not supported".to_string(),
        ))
    }

    /// Feed a captured RGBA frame into the published video track.
    /// The `data` must be `width * height * 4` bytes of RGBA pixel data.
    /// Default: returns error (not supported).
    fn feed_video_frame(&self, _data: &[u8], _width: u32, _height: u32) -> Result<(), RoomError> {
        Err(RoomError::PublishFailed(
            "video publishing not supported".to_string(),
        ))
    }

    /// Unpublish the video track and clean up resources.
    /// Default: no-op.
    fn unpublish_video(&self) -> Result<(), RoomError> {
        Ok(())
    }

    /// Register callback for receiving decoded video frames from a remote
    /// participant's screen share. The callback receives
    /// `(identity, rgba_data, width, height)`.
    /// Default: no-op.
    fn on_video_frame(&self, _cb: Box<dyn Fn(&str, &[u8], u32, u32) + Send + 'static>) {}

    /// Register callback for when a remote screen share video track ends
    /// (unsubscribed or participant disconnected). Receives `(identity)`.
    /// Default: no-op.
    fn on_video_track_ended(&self, _cb: Box<dyn Fn(&str) + Send + 'static>) {}

    /// Publish a second audio track for system audio capture (screen share audio).
    /// Creates a `NativeAudioSource` and `LocalAudioTrack` published with
    /// `TrackSource::ScreenShareAudio` so receivers can distinguish it from the mic.
    /// Default: returns error (not supported).
    fn publish_screen_audio(&self) -> Result<(), RoomError> {
        Err(RoomError::PublishFailed(
            "screen share audio not supported".to_string(),
        ))
    }

    /// Feed captured system audio samples into the screen share audio source.
    /// `samples` must be mono 48kHz i16 PCM. Typically called from the audio
    /// capture engine with 960-sample (20ms) frames.
    /// Default: no-op.
    fn feed_screen_audio(&self, _samples: &[i16]) -> Result<(), RoomError> {
        Ok(())
    }

    /// Unpublish the screen share audio track and clean up the source.
    /// Does not affect the mic track.
    /// Default: no-op.
    fn unpublish_screen_audio(&self) -> Result<(), RoomError> {
        Ok(())
    }
}

/// No-op implementation used when the `livekit` feature is disabled or
/// when `RoomSession` is in proxy mode. All methods return errors.
pub struct NoLiveKit;

impl LiveKitConnection for NoLiveKit {
    fn connect(&self, _url: &str, _token: &str) -> Result<(), RoomError> {
        Err(RoomError::SfuConnectionFailed(
            "LiveKit not configured".to_string(),
        ))
    }
    fn disconnect(&self) -> Result<(), RoomError> {
        Ok(())
    }
    fn on_audio_frame(&self, _cb: Box<dyn Fn(&str, &[f32]) + Send + 'static>) {}
    fn publish_audio(&self, _track: &AudioTrack) -> Result<(), RoomError> {
        Err(RoomError::PublishFailed(
            "LiveKit not configured".to_string(),
        ))
    }
}

/// Records all calls made to the mock for test assertions.
#[cfg(test)]
#[derive(Debug, Clone, PartialEq)]
pub(super) enum MockLiveKitCall {
    Connect { url: String, token: String },
    Disconnect,
    PublishAudio,
    PublishScreenAudio,
    FeedScreenAudio { num_samples: usize },
    UnpublishScreenAudio,
}

/// Mock LiveKit connection for testing. Records calls and returns configurable results.
#[cfg(test)]
pub(super) struct MockLiveKitConnection {
    pub(super) calls: Arc<Mutex<Vec<MockLiveKitCall>>>,
    pub(super) connect_result: Arc<Mutex<Result<(), String>>>,
    pub(super) publish_result: Arc<Mutex<Result<(), String>>>,
    pub(super) available: Arc<Mutex<bool>>,
}

#[cfg(test)]
impl MockLiveKitConnection {
    pub(super) fn new() -> Self {
        Self {
            calls: Arc::new(Mutex::new(Vec::new())),
            connect_result: Arc::new(Mutex::new(Ok(()))),
            publish_result: Arc::new(Mutex::new(Ok(()))),
            available: Arc::new(Mutex::new(false)),
        }
    }

    pub(super) fn get_calls(&self) -> Vec<MockLiveKitCall> {
        self.calls.lock().unwrap().clone()
    }
}

#[cfg(test)]
impl Default for MockLiveKitConnection {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
impl LiveKitConnection for MockLiveKitConnection {
    fn is_available(&self) -> bool {
        *self.available.lock().unwrap()
    }

    fn connect(&self, url: &str, token: &str) -> Result<(), RoomError> {
        self.calls.lock().unwrap().push(MockLiveKitCall::Connect {
            url: url.to_string(),
            token: token.to_string(),
        });
        self.connect_result
            .lock()
            .unwrap()
            .clone()
            .map_err(RoomError::SfuConnectionFailed)
    }

    fn disconnect(&self) -> Result<(), RoomError> {
        self.calls.lock().unwrap().push(MockLiveKitCall::Disconnect);
        Ok(())
    }

    fn on_audio_frame(&self, _cb: Box<dyn Fn(&str, &[f32]) + Send + 'static>) {
        // No-op in mock — audio frames are injected directly in tests if needed
    }

    fn publish_audio(&self, _track: &AudioTrack) -> Result<(), RoomError> {
        self.calls
            .lock()
            .unwrap()
            .push(MockLiveKitCall::PublishAudio);
        self.publish_result
            .lock()
            .unwrap()
            .clone()
            .map_err(RoomError::PublishFailed)
    }

    fn publish_screen_audio(&self) -> Result<(), RoomError> {
        self.calls
            .lock()
            .unwrap()
            .push(MockLiveKitCall::PublishScreenAudio);
        self.publish_result
            .lock()
            .unwrap()
            .clone()
            .map_err(RoomError::PublishFailed)
    }

    fn feed_screen_audio(&self, samples: &[i16]) -> Result<(), RoomError> {
        self.calls
            .lock()
            .unwrap()
            .push(MockLiveKitCall::FeedScreenAudio {
                num_samples: samples.len(),
            });
        Ok(())
    }

    fn unpublish_screen_audio(&self) -> Result<(), RoomError> {
        self.calls
            .lock()
            .unwrap()
            .push(MockLiveKitCall::UnpublishScreenAudio);
        Ok(())
    }
}

// ---------------------------------------------------------------------------
// SFU connection mode
// ---------------------------------------------------------------------------

/// Which path the client uses to connect media to the SFU.
#[derive(Debug, Clone, PartialEq)]
pub enum SfuConnectionMode {
    /// Proxy mode: SDP/ICE through backend WS (existing behavior).
    Proxy,
    /// LiveKit mode: connect directly to LiveKit using the SDK.
    LiveKit { livekit_url: String, token: String },
}

// ---------------------------------------------------------------------------
// RoomSession
// ---------------------------------------------------------------------------

pub struct RoomSession<
    A: AudioBackend,
    P: PeerConnectionBackend,
    W: WebSocketConnection,
    L: LiveKitConnection = NoLiveKit,
> {
    audio: Arc<A>,
    pc_backend: Arc<P>,
    signaling: Arc<SignalingClient<W>>,
    /// Wrapped in Arc<Mutex<>> so credentials can be zeroed on leave/disconnect.
    /// Requirements: 7.5
    ice_config: Arc<Mutex<IceConfig>>,
    livekit: Arc<L>,
    sfu_mode: Arc<Mutex<SfuConnectionMode>>,
    subscribe_tracks: Arc<Mutex<HashMap<String, SubscribeTrackState>>>,
    on_participant_joined: EventCb<ParticipantInfo>,
    on_participant_left: EventCb<String>,
    on_room_state: EventCb<Vec<ParticipantInfo>>,
    on_share_started: ShareCb<String>,
    on_share_stopped: ShareCb<String>,
    on_share_state: ShareCb<Vec<String>>,
    active_shares: Arc<Mutex<HashSet<String>>>,
    in_room: Arc<Mutex<bool>>,
    stored_track: Arc<Mutex<Option<AudioTrack>>>,
}

impl<A, P, W> RoomSession<A, P, W, NoLiveKit>
where
    A: AudioBackend + 'static,
    P: PeerConnectionBackend + 'static,
    W: WebSocketConnection + 'static,
{
    /// Create a new `RoomSession` in proxy mode (no LiveKit connection).
    pub fn new(audio: A, pc_backend: P, ice_config: IceConfig, ws: W) -> Self {
        RoomSession::with_livekit(audio, pc_backend, ice_config, ws, NoLiveKit)
    }
}

impl<A, P, W, L> RoomSession<A, P, W, L>
where
    A: AudioBackend + 'static,
    P: PeerConnectionBackend + 'static,
    W: WebSocketConnection + 'static,
    L: LiveKitConnection + 'static,
{
    /// Create a new `RoomSession` with an explicit `LiveKitConnection`.
    /// When a `MediaToken` arrives and `livekit` is not `NoLiveKit`, the session
    /// switches to LiveKit mode and calls `connect` + `publish_audio`.
    pub fn with_livekit(audio: A, pc_backend: P, ice_config: IceConfig, ws: W, livekit: L) -> Self {
        let signaling = Arc::new(SignalingClient::new(ws));
        let session = Self {
            audio: Arc::new(audio),
            pc_backend: Arc::new(pc_backend),
            signaling,
            ice_config: Arc::new(Mutex::new(ice_config)),
            livekit: Arc::new(livekit),
            sfu_mode: Arc::new(Mutex::new(SfuConnectionMode::Proxy)),
            subscribe_tracks: Arc::new(Mutex::new(HashMap::new())),
            on_participant_joined: Arc::new(Mutex::new(None)),
            on_participant_left: Arc::new(Mutex::new(None)),
            on_room_state: Arc::new(Mutex::new(None)),
            on_share_started: Arc::new(Mutex::new(None)),
            on_share_stopped: Arc::new(Mutex::new(None)),
            on_share_state: Arc::new(Mutex::new(None)),
            active_shares: Arc::new(Mutex::new(HashSet::new())),
            in_room: Arc::new(Mutex::new(false)),
            stored_track: Arc::new(Mutex::new(None)),
        };
        session.wire_signaling();
        session
    }

    /// Join a room: send Join message, capture mic, set up media transport.
    pub fn join_room(&self, room_id: &str, invite_code: Option<&str>) -> Result<(), RoomError> {
        self.join_room_with_name(room_id, invite_code, None)
    }

    /// Join a room with an optional display name.
    pub fn join_room_with_name(
        &self,
        room_id: &str,
        invite_code: Option<&str>,
        display_name: Option<&str>,
    ) -> Result<(), RoomError> {
        {
            let in_room = self.in_room.lock().unwrap();
            if *in_room {
                return Err(RoomError::AlreadyInRoom);
            }
        }

        // Send Join signaling message
        let join_msg = SignalingMessage::Join(JoinPayload {
            room_id: room_id.to_string(),
            room_type: Some("sfu".to_string()),
            invite_code: invite_code.map(|s| s.to_string()),
            display_name: display_name.map(|s| s.to_string()),
            profile_color: None,
        });
        self.signaling
            .send(&join_msg)
            .map_err(|e| RoomError::Signaling(e.to_string()))?;

        self.start_media()
    }

    /// Capture mic and set up media transport (WebRTC or LiveKit) without
    /// sending a Join signaling message. Used by room creators who join
    /// implicitly via CreateRoom.
    pub fn start_media(&self) -> Result<(), RoomError> {
        {
            let in_room = self.in_room.lock().unwrap();
            if *in_room {
                return Err(RoomError::AlreadyInRoom);
            }
        }

        // Capture mic early so the track is ready for either path.
        let track = self.audio.capture_mic()?;
        *self.stored_track.lock().unwrap() = Some(track.clone());

        if self.livekit.is_available() {
            // LiveKit mode: skip PeerConnection setup entirely.
            // The capture buffer stays uncontested — LiveKit's capture_loop
            // will be the sole consumer once publish_audio() is called after
            // receiving the MediaToken.
        } else {
            // Proxy mode: create PeerConnection and wire the audio send loop.
            let ice_cfg = self.ice_config.lock().unwrap().clone();
            self.pc_backend
                .create_peer_connection(&ice_cfg)
                .map_err(|e| RoomError::PublishFailed(e.to_string()))?;
            self.pc_backend
                .add_audio_track(&track)
                .map_err(|e| RoomError::PublishFailed(e.to_string()))?;
        }

        *self.in_room.lock().unwrap() = true;
        Ok(())
    }

    /// Leave the room: close peer connection, stop audio, clear tracks, send Leave.
    pub fn leave_room(&self) -> Result<(), RoomError> {
        {
            let in_room = self.in_room.lock().unwrap();
            if !*in_room {
                return Err(RoomError::NotInRoom);
            }
        }

        let mode = self.sfu_mode.lock().unwrap().clone();
        if matches!(mode, SfuConnectionMode::LiveKit { .. }) {
            let _ = self.livekit.disconnect();
        }

        let _ = self.pc_backend.close();
        let _ = self.audio.stop();

        self.subscribe_tracks.lock().unwrap().clear();
        self.active_shares.lock().unwrap().clear();
        *self.stored_track.lock().unwrap() = None;

        // Clear TURN credentials from memory on leave (Requirements: 7.5)
        {
            let mut cfg = self.ice_config.lock().unwrap();
            cfg.turn_username.clear();
            cfg.turn_credential.clear();
        }

        let leave_msg = SignalingMessage::Leave;
        self.signaling
            .send(&leave_msg)
            .map_err(|e| RoomError::Signaling(e.to_string()))?;

        *self.in_room.lock().unwrap() = false;
        Ok(())
    }

    /// Returns the current SFU connection mode (for testing).
    pub fn sfu_mode(&self) -> SfuConnectionMode {
        self.sfu_mode.lock().unwrap().clone()
    }

    /// Feed a raw incoming text frame into the signaling client for dispatch.
    pub fn handle_incoming(&self, text: &str) -> Result<(), RoomError> {
        self.signaling
            .handle_incoming(text)
            .map_err(|e| RoomError::Signaling(e.to_string()))
    }

    /// Wire the signaling message dispatcher. Called once in `with_livekit()`.
    fn wire_signaling(&self) {
        let subscribe_tracks = Arc::clone(&self.subscribe_tracks);
        let on_participant_joined = Arc::clone(&self.on_participant_joined);
        let on_participant_left = Arc::clone(&self.on_participant_left);
        let on_room_state = Arc::clone(&self.on_room_state);
        let on_share_started = Arc::clone(&self.on_share_started);
        let on_share_stopped = Arc::clone(&self.on_share_stopped);
        let on_share_state = Arc::clone(&self.on_share_state);
        let active_shares = Arc::clone(&self.active_shares);
        let pc_backend = Arc::clone(&self.pc_backend);
        let livekit = Arc::clone(&self.livekit);
        let sfu_mode = Arc::clone(&self.sfu_mode);
        let stored_track = Arc::clone(&self.stored_track);

        self.signaling.on_message(move |msg| {
            match msg {
                SignalingMessage::Joined(_) => {
                    // Join confirmed — already handled by join_room
                }
                SignalingMessage::MediaToken(payload) => {
                    // Try LiveKit mode first: attempt connect + publish_audio.
                    // If connect succeeds, switch to LiveKit mode and suppress SDP/ICE.
                    // If connect fails (e.g. NoLiveKit), stay in Proxy mode.
                    match livekit.connect(&payload.sfu_url, &payload.token) {
                        Ok(()) => {
                            // Switch to LiveKit mode
                            *sfu_mode.lock().unwrap() = SfuConnectionMode::LiveKit {
                                livekit_url: payload.sfu_url.clone(),
                                token: payload.token.clone(),
                            };
                            // Publish local audio via LiveKit SDK (reuse track from join_room)
                            if let Some(track) = stored_track.lock().unwrap().as_ref() {
                                let _ = livekit.publish_audio(track);
                            }
                        }
                        Err(_) => {
                            // NoLiveKit or connection failed — stay in Proxy mode
                        }
                    }
                }
                SignalingMessage::RoomState(payload) => {
                    Self::handle_room_state_message(&on_room_state, payload);
                }
                SignalingMessage::ParticipantJoined(payload) => {
                    Self::handle_participant_joined_message(
                        &subscribe_tracks,
                        &on_participant_joined,
                        payload,
                    );
                }
                SignalingMessage::ParticipantLeft(payload) => {
                    Self::handle_participant_left_message(
                        &subscribe_tracks,
                        &active_shares,
                        &on_participant_left,
                        payload,
                    );
                }
                SignalingMessage::ShareState(payload) => {
                    Self::handle_share_state_message(&active_shares, &on_share_state, payload);
                }
                SignalingMessage::ShareStarted(payload) => {
                    Self::handle_share_started_message(&active_shares, &on_share_started, payload);
                }
                SignalingMessage::ShareStopped(payload) => {
                    Self::handle_share_stopped_message(&active_shares, &on_share_stopped, payload);
                }
                SignalingMessage::Answer(payload) => {
                    // Only process SDP in proxy mode — suppress in LiveKit mode
                    let mode = sfu_mode.lock().unwrap().clone();
                    if matches!(mode, SfuConnectionMode::Proxy) {
                        if !check_sdp_size(&payload.session_description.sdp) {
                            return;
                        }
                        let _ = pc_backend.set_remote_answer(&payload.session_description.sdp);
                    }
                }
                SignalingMessage::IceCandidate(payload) => {
                    // Only process ICE in proxy mode — suppress in LiveKit mode
                    let mode = sfu_mode.lock().unwrap().clone();
                    if matches!(mode, SfuConnectionMode::Proxy) {
                        if !check_ice_candidate_size(&payload.candidate) {
                            return;
                        }
                        let _ = pc_backend.add_ice_candidate(&payload.candidate);
                    }
                }
                SignalingMessage::Error(_) => {
                    // No-op
                }
                _ => {
                    // All other variants — no-op
                }
            }
        });
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests;

#[cfg(test)]
#[derive(Clone)]
pub(super) struct MockWs {
    pub(super) sent: Arc<Mutex<Vec<String>>>,
}

#[cfg(test)]
impl MockWs {
    pub(super) fn new() -> Self {
        Self {
            sent: Arc::new(Mutex::new(Vec::new())),
        }
    }
}

#[cfg(test)]
impl WebSocketConnection for MockWs {
    fn send_text(&self, text: &str) -> Result<(), String> {
        self.sent.lock().unwrap().push(text.to_string());
        Ok(())
    }
}

#[cfg(test)]
pub(super) fn make_session() -> RoomSession<
    crate::audio::MockAudioBackend,
    crate::webrtc::MockPeerConnectionBackend,
    MockWs,
    NoLiveKit,
> {
    let audio = crate::audio::MockAudioBackend::new();
    let pc = crate::webrtc::MockPeerConnectionBackend::new();
    let ice = IceConfig {
        stun_urls: vec!["stun:stun.example.com:19302".to_string()],
        turn_urls: vec!["turn:turn.example.com:3478".to_string()],
        turn_username: "user".to_string(),
        turn_credential: "pass".to_string(),
    };
    let ws = MockWs::new();
    RoomSession::new(audio, pc, ice, ws)
}

#[cfg(test)]
pub(super) fn participant_joined_json(id: &str, name: &str) -> String {
    format!(
        r#"{{"type":"participant_joined","participantId":"{}","displayName":"{}"}}"#,
        id, name
    )
}

#[cfg(test)]
pub(super) fn participant_left_json(id: &str) -> String {
    format!(r#"{{"type":"participant_left","participantId":"{}"}}"#, id)
}
