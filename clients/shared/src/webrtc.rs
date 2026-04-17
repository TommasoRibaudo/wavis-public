//! WebRTC call orchestration abstractions shared across client backends.
//!
//! This module owns the peer-connection lifecycle seam, the `CallManager`
//! state machine, and transport-independent test doubles for call setup. It
//! does not own signaling transport, SFU room management, or platform-specific
//! media backends.
//!
//! Key invariants:
//! - `CallManager` owns a single active call lifecycle at a time.
//! - `PeerConnectionBackend` is the only WebRTC seam that higher layers depend on.
//! - Connection-state callbacks are responsible for driving call-state updates.

use crate::audio::{AudioBackend, AudioError, AudioTrack};
use crate::ice_config::IceConfig;
use shared::signaling::IceCandidate;
use std::sync::{Arc, Mutex};
use thiserror::Error;

/// Shared callback type used for event handlers throughout the WebRTC module.
type EventCb<T> = Arc<Mutex<Option<Box<dyn Fn(T) + Send + 'static>>>>;

// ---------------------------------------------------------------------------
// Error types
// ---------------------------------------------------------------------------

/// Errors surfaced by call negotiation, signaling, and peer-connection setup.
#[derive(Debug, Error)]
pub enum CallError {
    /// Microphone capture failed because access was denied by the platform.
    #[error("microphone access denied")]
    MicrophoneDenied,
    /// SDP negotiation or peer-connection setup failed with the given detail.
    #[error("negotiation failed: {0}")]
    NegotiationFailed(String),
    /// ICE connectivity failed and the call can no longer progress.
    #[error("ICE connection failed")]
    IceFailed,
    /// Signaling transport or message handling failed outside the peer connection itself.
    #[error("signaling error: {0}")]
    SignalingError(String),
    /// A new call was requested while another call lifecycle is still active.
    #[error("already in a call")]
    AlreadyInCall,
    /// An operation requiring an active call was attempted while idle or already closed.
    #[error("no active call")]
    NoActiveCall,
    /// Audio backend setup or teardown failed and was mapped into the call domain.
    #[error("audio error: {0}")]
    Audio(String),
}

impl From<AudioError> for CallError {
    fn from(e: AudioError) -> Self {
        match e {
            AudioError::MicrophoneDenied => CallError::MicrophoneDenied,
            other => CallError::Audio(other.to_string()),
        }
    }
}

// ---------------------------------------------------------------------------
// Call state
// ---------------------------------------------------------------------------

/// High-level lifecycle state for a single call managed by `CallManager`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CallState {
    /// No call is active and a new call may be started or accepted.
    Idle,
    /// SDP offer or answer exchange is in progress.
    Negotiating,
    /// ICE connectivity checks are underway but media is not yet established.
    Connecting,
    /// The peer connection is established and the call is considered live.
    Connected,
    /// The call failed irrecoverably during negotiation or ICE establishment.
    Failed,
    /// Local teardown completed and resources have been released.
    Closed,
}

// ---------------------------------------------------------------------------
// Connection state (ICE-level)
// ---------------------------------------------------------------------------

/// ICE-level connection state reported by the peer-connection backend.
///
/// These states mirror the WebRTC ICE lifecycle so higher layers can react
/// without depending on a concrete WebRTC library.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ConnectionState {
    /// The connection has been created but no ICE checks have started yet.
    New,
    /// ICE candidates are being checked to establish connectivity.
    Checking,
    /// A working ICE connection has been found.
    Connected,
    /// ICE gathering and checking are complete and the selected path is stable.
    Completed,
    /// ICE failed to establish or maintain connectivity.
    Failed,
    /// Connectivity was temporarily lost after previously being established.
    Disconnected,
    /// The peer connection has been closed and will not recover.
    Closed,
}

// ---------------------------------------------------------------------------
// Remote track types (SFU subscribe tracks)
// ---------------------------------------------------------------------------

/// Trait for receiving decoded audio data from a remote track.
/// Implementations provide the actual audio data channel; the trait
/// enables testing with mock audio sources.
pub trait RemoteAudioSource: Send + 'static {
    /// Receive the next Opus packet from this track.
    /// Returns None when the track has ended.
    fn recv(&mut self) -> Option<Vec<u8>>;
}

/// A remote audio track received from the SFU.
pub struct RemoteTrack {
    /// Track identifier reported by the remote media backend.
    pub track_id: String,
    /// Participant identifier that owns the remote track.
    pub participant_id: String,
    /// The audio data source for this track. In production, backed by
    /// the webrtc-rs track receiver. In tests, backed by a channel or
    /// pre-recorded buffer.
    pub audio_source: Box<dyn RemoteAudioSource>,
}

/// Mock remote audio source that wraps a `Vec<Vec<u8>>` of pre-recorded
/// Opus packets and returns them sequentially.
pub struct MockRemoteAudioSource {
    packets: Vec<Vec<u8>>,
    index: usize,
}

impl MockRemoteAudioSource {
    /// Create a mock source that yields the provided packets in order.
    pub fn new(packets: Vec<Vec<u8>>) -> Self {
        Self { packets, index: 0 }
    }
}

impl RemoteAudioSource for MockRemoteAudioSource {
    fn recv(&mut self) -> Option<Vec<u8>> {
        if self.index < self.packets.len() {
            let packet = self.packets[self.index].clone();
            self.index += 1;
            Some(packet)
        } else {
            None
        }
    }
}

// ---------------------------------------------------------------------------
// PeerConnection abstraction — allows mocking in tests
// ---------------------------------------------------------------------------

/// Trait abstracting a WebRTC peer connection so `CallManager` is testable
/// without real WebRTC infrastructure.
pub trait PeerConnectionBackend: Send + Sync {
    /// Create a new peer connection with the given ICE configuration.
    fn create_peer_connection(&self, ice_config: &IceConfig) -> Result<(), CallError>;

    /// Add a local audio track to the peer connection.
    fn add_audio_track(&self, track: &AudioTrack) -> Result<(), CallError>;

    /// Create an SDP offer and set it as local description.
    /// Returns the SDP string.
    fn create_offer(&self) -> Result<String, CallError>;

    /// Set a remote SDP offer and create an SDP answer, setting it as local description.
    /// Returns the answer SDP string.
    fn create_answer(&self, offer_sdp: &str) -> Result<String, CallError>;

    /// Set the remote SDP answer on the peer connection.
    fn set_remote_answer(&self, answer_sdp: &str) -> Result<(), CallError>;

    /// Add a remote ICE candidate to the peer connection.
    fn add_ice_candidate(&self, candidate: &IceCandidate) -> Result<(), CallError>;

    /// Register a callback for locally gathered ICE candidates.
    fn on_ice_candidate(&self, cb: Box<dyn Fn(IceCandidate) + Send + 'static>);

    /// Register a callback for ICE connection state changes.
    fn on_connection_state_change(&self, cb: Box<dyn Fn(ConnectionState) + Send + 'static>);

    /// Close the peer connection and release resources.
    fn close(&self) -> Result<(), CallError>;

    /// Whether a peer connection is currently active.
    fn is_active(&self) -> bool;

    /// Register a callback for incoming remote tracks (SFU subscribe tracks).
    /// Default no-op for backward compatibility with Phase 2 implementations.
    fn on_track(&self, _cb: Box<dyn Fn(RemoteTrack) + Send + 'static>) {}
}

// ---------------------------------------------------------------------------
// CallManager
// ---------------------------------------------------------------------------

/// Manages the lifecycle of a single 1:1 voice call.
///
/// Generic over `A: AudioBackend` for mic/speaker abstraction and
/// `P: PeerConnectionBackend` for WebRTC abstraction.
pub struct CallManager<A: AudioBackend, P: PeerConnectionBackend> {
    /// Audio backend used for microphone capture and remote playback.
    pub audio: Arc<A>,
    /// Peer-connection backend used for SDP, ICE, and connection lifecycle work.
    pub pc_backend: Arc<P>,
    /// Wrapped in Arc<Mutex<>> so credentials can be zeroed on hangup/disconnect.
    /// Requirements: 7.5
    pub(crate) ice_config: Arc<Mutex<IceConfig>>,
    state: Arc<Mutex<CallState>>,
    ice_candidate_cb: EventCb<IceCandidate>,
    connection_state_cb: EventCb<ConnectionState>,
}

impl<A: AudioBackend + 'static, P: PeerConnectionBackend + 'static> CallManager<A, P> {
    /// Create a new call manager with the given audio backend, peer backend, and ICE config.
    pub fn new(audio: A, pc_backend: P, ice_config: IceConfig) -> Self {
        Self {
            audio: Arc::new(audio),
            pc_backend: Arc::new(pc_backend),
            ice_config: Arc::new(Mutex::new(ice_config)),
            state: Arc::new(Mutex::new(CallState::Idle)),
            ice_candidate_cb: Arc::new(Mutex::new(None)),
            connection_state_cb: Arc::new(Mutex::new(None)),
        }
    }

    /// Current call state.
    pub fn state(&self) -> CallState {
        *self.state.lock().unwrap()
    }

    /// Create a PeerConnection, capture mic, add audio track, create SDP offer.
    /// Returns the offer SDP string.
    pub fn start_call(&self) -> Result<String, CallError> {
        {
            let st = self.state.lock().unwrap();
            if *st != CallState::Idle && *st != CallState::Closed {
                return Err(CallError::AlreadyInCall);
            }
        }

        // Create peer connection
        {
            let ice_cfg = self.ice_config.lock().unwrap().clone();
            self.pc_backend.create_peer_connection(&ice_cfg)?;
        }

        // Install ICE candidate forwarding
        self.install_ice_candidate_handler();
        // Install connection state handler
        self.install_connection_state_handler();

        // Capture mic
        let track = self.audio.capture_mic()?;

        // Add audio track
        self.pc_backend.add_audio_track(&track)?;

        // Create offer
        let offer_sdp = self.pc_backend.create_offer()?;

        *self.state.lock().unwrap() = CallState::Negotiating;
        Ok(offer_sdp)
    }

    /// Receive an SDP offer, create PeerConnection, capture mic, create answer.
    /// Returns the answer SDP string.
    pub fn accept_call(&self, offer_sdp: &str) -> Result<String, CallError> {
        {
            let st = self.state.lock().unwrap();
            if *st != CallState::Idle && *st != CallState::Closed {
                return Err(CallError::AlreadyInCall);
            }
        }

        // Create peer connection
        {
            let ice_cfg = self.ice_config.lock().unwrap().clone();
            self.pc_backend.create_peer_connection(&ice_cfg)?;
        }

        // Install ICE candidate forwarding
        self.install_ice_candidate_handler();
        // Install connection state handler
        self.install_connection_state_handler();

        // Capture mic
        let track = self.audio.capture_mic()?;

        // Add audio track
        self.pc_backend.add_audio_track(&track)?;

        // Create answer (sets remote offer + creates answer + sets local answer)
        let answer_sdp = self.pc_backend.create_answer(offer_sdp)?;

        *self.state.lock().unwrap() = CallState::Negotiating;
        Ok(answer_sdp)
    }

    /// Set the remote SDP answer on the PeerConnection (initiator side).
    pub fn set_answer(&self, answer_sdp: &str) -> Result<(), CallError> {
        let st = *self.state.lock().unwrap();
        if st != CallState::Negotiating && st != CallState::Connecting {
            return Err(CallError::NoActiveCall);
        }
        self.pc_backend.set_remote_answer(answer_sdp)?;
        *self.state.lock().unwrap() = CallState::Connecting;
        Ok(())
    }

    /// Add a remote ICE candidate to the PeerConnection.
    pub fn add_ice_candidate(&self, candidate: &IceCandidate) -> Result<(), CallError> {
        let st = *self.state.lock().unwrap();
        match st {
            CallState::Negotiating | CallState::Connecting | CallState::Connected => {
                self.pc_backend.add_ice_candidate(candidate)
            }
            _ => Err(CallError::NoActiveCall),
        }
    }

    /// Register a callback for locally gathered ICE candidates.
    pub fn on_ice_candidate(&self, cb: impl Fn(IceCandidate) + Send + 'static) {
        *self.ice_candidate_cb.lock().unwrap() = Some(Box::new(cb));
    }

    /// Register a callback for ICE connection state changes.
    pub fn on_connection_state(&self, cb: impl Fn(ConnectionState) + Send + 'static) {
        *self.connection_state_cb.lock().unwrap() = Some(Box::new(cb));
    }

    /// Close PeerConnection, stop audio, release all resources.
    /// Zero out stored TURN credentials from memory.
    /// Call on hangup/disconnect to satisfy Requirements: 7.5
    pub fn clear_credentials(&self) {
        let mut cfg = self.ice_config.lock().unwrap();
        cfg.turn_username.clear();
        cfg.turn_credential.clear();
    }

    /// Tear down the active call if one exists and move the manager to `Closed`.
    ///
    /// This is idempotent for idle or already-closed managers. Peer-connection
    /// close and audio stop failures are intentionally swallowed so teardown
    /// still finishes and the state transitions to `Closed`.
    pub fn hangup(&self) -> Result<(), CallError> {
        let st = *self.state.lock().unwrap();
        if st == CallState::Idle || st == CallState::Closed {
            return Ok(()); // nothing to clean up
        }

        // Close peer connection
        let _ = self.pc_backend.close();

        // Stop audio
        let _ = self.audio.stop();

        *self.state.lock().unwrap() = CallState::Closed;
        Ok(())
    }

    // -----------------------------------------------------------------------
    // Internal helpers
    // -----------------------------------------------------------------------

    fn install_ice_candidate_handler(&self) {
        let cb_ref = Arc::clone(&self.ice_candidate_cb);
        self.pc_backend.on_ice_candidate(Box::new(move |candidate| {
            let cb = cb_ref.lock().unwrap();
            if let Some(ref f) = *cb {
                f(candidate);
            }
        }));
    }

    fn install_connection_state_handler(&self) {
        let state = Arc::clone(&self.state);
        let audio = Arc::clone(&self.audio);
        let pc = Arc::clone(&self.pc_backend);
        let user_cb = Arc::clone(&self.connection_state_cb);

        self.pc_backend
            .on_connection_state_change(Box::new(move |conn_state| {
                match conn_state {
                    ConnectionState::Connected | ConnectionState::Completed => {
                        *state.lock().unwrap() = CallState::Connected;
                        // Start playing remote audio
                        let _ = audio.play_remote(AudioTrack {
                            id: "remote-track".to_string(),
                        });
                    }
                    ConnectionState::Failed => {
                        *state.lock().unwrap() = CallState::Failed;
                        let _ = pc.close();
                        let _ = audio.stop();
                    }
                    ConnectionState::Checking => {
                        *state.lock().unwrap() = CallState::Connecting;
                    }
                    ConnectionState::Closed => {
                        let mut st = state.lock().unwrap();
                        if *st != CallState::Closed {
                            *st = CallState::Closed;
                        }
                    }
                    _ => {}
                }

                // Forward to user callback
                let cb = user_cb.lock().unwrap();
                if let Some(ref f) = *cb {
                    f(conn_state);
                }
            }));
    }
}

// ---------------------------------------------------------------------------
// Mock PeerConnection backend for testing
// ---------------------------------------------------------------------------

/// Recorded interactions with the mock peer-connection backend.
#[derive(Debug, Clone)]
pub enum MockPcCall {
    /// `create_peer_connection()` was invoked.
    CreatePeerConnection,
    /// `add_audio_track()` was invoked with the given local track.
    AddAudioTrack(AudioTrack),
    /// `create_offer()` was invoked.
    CreateOffer,
    /// `create_answer()` was invoked with the given remote offer SDP.
    CreateAnswer(String),
    /// `set_remote_answer()` was invoked with the given answer SDP.
    SetRemoteAnswer(String),
    /// `add_ice_candidate()` was invoked with the given remote candidate.
    AddIceCandidate(IceCandidate),
    /// `close()` was invoked.
    Close,
}

/// Mock peer connection backend that records calls and allows injecting
/// ICE candidates and connection state changes for testing.
pub struct MockPeerConnectionBackend {
    calls: Arc<Mutex<Vec<MockPcCall>>>,
    active: Arc<Mutex<bool>>,
    offer_sdp: Arc<Mutex<String>>,
    answer_sdp: Arc<Mutex<String>>,
    ice_cb: EventCb<IceCandidate>,
    conn_state_cb: EventCb<ConnectionState>,
    on_track_cb: EventCb<RemoteTrack>,
}

impl MockPeerConnectionBackend {
    /// Create a mock peer backend with empty call history and default SDP values.
    pub fn new() -> Self {
        Self {
            calls: Arc::new(Mutex::new(Vec::new())),
            active: Arc::new(Mutex::new(false)),
            offer_sdp: Arc::new(Mutex::new("mock-offer-sdp".to_string())),
            answer_sdp: Arc::new(Mutex::new("mock-answer-sdp".to_string())),
            ice_cb: Arc::new(Mutex::new(None)),
            conn_state_cb: Arc::new(Mutex::new(None)),
            on_track_cb: Arc::new(Mutex::new(None)),
        }
    }

    /// Return a snapshot of all recorded backend interactions in call order.
    pub fn calls(&self) -> Vec<MockPcCall> {
        self.calls.lock().unwrap().clone()
    }

    /// Override the SDP returned by future `create_offer()` calls.
    pub fn set_offer_sdp(&self, sdp: &str) {
        *self.offer_sdp.lock().unwrap() = sdp.to_string();
    }

    /// Override the SDP returned by future `create_answer()` calls.
    pub fn set_answer_sdp(&self, sdp: &str) {
        *self.answer_sdp.lock().unwrap() = sdp.to_string();
    }

    /// Simulate an ICE candidate being gathered by the peer connection.
    pub fn simulate_ice_candidate(&self, candidate: IceCandidate) {
        let cb = self.ice_cb.lock().unwrap();
        if let Some(ref f) = *cb {
            f(candidate);
        }
    }

    /// Simulate a connection state change.
    pub fn simulate_connection_state(&self, state: ConnectionState) {
        let cb = self.conn_state_cb.lock().unwrap();
        if let Some(ref f) = *cb {
            f(state);
        }
    }

    /// Simulate a remote track arriving (e.g. from SFU subscribe).
    /// Invokes the registered `on_track` callback with the given track.
    pub fn simulate_remote_track(&self, track: RemoteTrack) {
        let cb = self.on_track_cb.lock().unwrap();
        if let Some(ref f) = *cb {
            f(track);
        }
    }
}

impl Default for MockPeerConnectionBackend {
    fn default() -> Self {
        Self::new()
    }
}

impl PeerConnectionBackend for MockPeerConnectionBackend {
    fn create_peer_connection(&self, _ice_config: &IceConfig) -> Result<(), CallError> {
        self.calls
            .lock()
            .unwrap()
            .push(MockPcCall::CreatePeerConnection);
        *self.active.lock().unwrap() = true;
        Ok(())
    }

    fn add_audio_track(&self, track: &AudioTrack) -> Result<(), CallError> {
        self.calls
            .lock()
            .unwrap()
            .push(MockPcCall::AddAudioTrack(track.clone()));
        Ok(())
    }

    fn create_offer(&self) -> Result<String, CallError> {
        self.calls.lock().unwrap().push(MockPcCall::CreateOffer);
        Ok(self.offer_sdp.lock().unwrap().clone())
    }

    fn create_answer(&self, offer_sdp: &str) -> Result<String, CallError> {
        self.calls
            .lock()
            .unwrap()
            .push(MockPcCall::CreateAnswer(offer_sdp.to_string()));
        Ok(self.answer_sdp.lock().unwrap().clone())
    }

    fn set_remote_answer(&self, answer_sdp: &str) -> Result<(), CallError> {
        self.calls
            .lock()
            .unwrap()
            .push(MockPcCall::SetRemoteAnswer(answer_sdp.to_string()));
        Ok(())
    }

    fn add_ice_candidate(&self, candidate: &IceCandidate) -> Result<(), CallError> {
        self.calls
            .lock()
            .unwrap()
            .push(MockPcCall::AddIceCandidate(candidate.clone()));
        Ok(())
    }

    fn on_ice_candidate(&self, cb: Box<dyn Fn(IceCandidate) + Send + 'static>) {
        *self.ice_cb.lock().unwrap() = Some(cb);
    }

    fn on_connection_state_change(&self, cb: Box<dyn Fn(ConnectionState) + Send + 'static>) {
        *self.conn_state_cb.lock().unwrap() = Some(cb);
    }

    fn close(&self) -> Result<(), CallError> {
        self.calls.lock().unwrap().push(MockPcCall::Close);
        *self.active.lock().unwrap() = false;
        Ok(())
    }

    fn is_active(&self) -> bool {
        *self.active.lock().unwrap()
    }

    fn on_track(&self, cb: Box<dyn Fn(RemoteTrack) + Send + 'static>) {
        *self.on_track_cb.lock().unwrap() = Some(cb);
    }
}

#[cfg(test)]
#[path = "webrtc_tests.rs"]
mod webrtc_tests;
