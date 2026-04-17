//! Audio capture and playback abstractions shared across client backends.
//!
//! This module owns the transport-independent audio seam that higher-level call
//! orchestration uses for microphone capture and remote playback. It does not
//! own platform-specific CPAL integration or native device management.
//!
//! The main invariant is that `AudioBackend` is the canonical boundary for
//! production backends and tests alike, so call orchestration can depend on one
//! consistent contract.

use std::fmt;
use std::sync::{Arc, Mutex};
use thiserror::Error;

/// Opaque handle representing an audio track.
/// Real implementations will wrap platform-specific audio stream handles.
#[derive(Debug, Clone, PartialEq)]
pub struct AudioTrack {
    /// Backend-defined identifier for the captured or remote audio track.
    pub id: String,
}

/// Errors returned by audio capture and playback backends.
#[derive(Debug, Error)]
pub enum AudioError {
    /// The backend could not start microphone capture because access was denied.
    #[error("microphone access denied")]
    MicrophoneDenied,
    /// The backend could not find or initialize an output device for playback.
    #[error("no audio output available")]
    OutputUnavailable,
    /// Any other backend-specific audio failure that does not fit a dedicated variant.
    #[error("audio backend error: {0}")]
    Other(String),
}

/// Trait abstracting audio capture and playback.
/// Real implementations use platform APIs (CPAL for desktop, platform-specific for mobile).
/// Tests use `MockAudioBackend`.
pub trait AudioBackend: Send + Sync {
    /// Start microphone capture and return a handle for the new local audio track.
    ///
    /// Returns an `AudioTrack` that can be attached to the active call pipeline.
    /// Fails when microphone access is denied or the backend cannot initialize
    /// capture successfully.
    fn capture_mic(&self) -> Result<AudioTrack, AudioError>;

    /// Begin playing audio from a remote track.
    ///
    /// The provided `track` is a backend-agnostic handle describing the remote
    /// source to play. Fails when no output path is available or playback setup
    /// cannot be completed.
    fn play_remote(&self, track: AudioTrack) -> Result<(), AudioError>;

    /// Stop any active capture or playback owned by this backend instance.
    ///
    /// Implementations should make this safe to call during normal teardown and
    /// return an error only when cleanup itself fails.
    fn stop(&self) -> Result<(), AudioError>;
}

/// Records all calls for test assertions.
#[derive(Debug, Clone)]
pub enum MockCall {
    /// `capture_mic()` was invoked.
    CaptureMic,
    /// `play_remote()` was invoked with the given track.
    PlayRemote(AudioTrack),
    /// `stop()` was invoked.
    Stop,
}

/// Mock audio backend that records calls and returns configurable results.
pub struct MockAudioBackend {
    calls: Arc<Mutex<Vec<MockCall>>>,
    capture_result: Arc<Mutex<Option<Result<AudioTrack, String>>>>,
    play_result: Arc<Mutex<Option<Result<(), String>>>>,
    stop_result: Arc<Mutex<Option<Result<(), String>>>>,
}

impl MockAudioBackend {
    /// Create a mock backend with empty call history and success defaults.
    pub fn new() -> Self {
        Self {
            calls: Arc::new(Mutex::new(Vec::new())),
            capture_result: Arc::new(Mutex::new(None)),
            play_result: Arc::new(Mutex::new(None)),
            stop_result: Arc::new(Mutex::new(None)),
        }
    }

    /// Set what `capture_mic()` should return. `None` means success with a default track.
    pub fn set_capture_result(&self, result: Result<AudioTrack, String>) {
        *self.capture_result.lock().unwrap() = Some(result);
    }

    /// Set what `play_remote()` should return. `None` means success.
    pub fn set_play_result(&self, result: Result<(), String>) {
        *self.play_result.lock().unwrap() = Some(result);
    }

    /// Set what `stop()` should return. `None` means success.
    pub fn set_stop_result(&self, result: Result<(), String>) {
        *self.stop_result.lock().unwrap() = Some(result);
    }

    /// Return a snapshot of all recorded backend interactions in call order.
    pub fn calls(&self) -> Vec<MockCall> {
        self.calls.lock().unwrap().clone()
    }
}

impl Default for MockAudioBackend {
    fn default() -> Self {
        Self::new()
    }
}

impl fmt::Debug for MockAudioBackend {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("MockAudioBackend")
            .field("calls", &self.calls())
            .finish()
    }
}

impl AudioBackend for MockAudioBackend {
    fn capture_mic(&self) -> Result<AudioTrack, AudioError> {
        self.calls.lock().unwrap().push(MockCall::CaptureMic);
        match self.capture_result.lock().unwrap().as_ref() {
            Some(Ok(track)) => Ok(track.clone()),
            Some(Err(_)) => Err(AudioError::MicrophoneDenied),
            None => Ok(AudioTrack {
                id: "mock-mic-track".to_string(),
            }),
        }
    }

    fn play_remote(&self, track: AudioTrack) -> Result<(), AudioError> {
        self.calls.lock().unwrap().push(MockCall::PlayRemote(track));
        match self.play_result.lock().unwrap().as_ref() {
            Some(Ok(())) => Ok(()),
            Some(Err(_)) => Err(AudioError::OutputUnavailable),
            None => Ok(()),
        }
    }

    fn stop(&self) -> Result<(), AudioError> {
        self.calls.lock().unwrap().push(MockCall::Stop);
        match self.stop_result.lock().unwrap().as_ref() {
            Some(Ok(())) => Ok(()),
            Some(Err(msg)) => Err(AudioError::Other(msg.clone())),
            None => Ok(()),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn mock_records_capture_mic_call() {
        let mock = MockAudioBackend::new();
        let result = mock.capture_mic();
        assert!(result.is_ok());
        assert_eq!(mock.calls().len(), 1);
        assert!(matches!(mock.calls()[0], MockCall::CaptureMic));
    }

    #[test]
    fn mock_records_play_remote_call() {
        let mock = MockAudioBackend::new();
        let track = AudioTrack {
            id: "test".to_string(),
        };
        let result = mock.play_remote(track.clone());
        assert!(result.is_ok());
        assert_eq!(mock.calls().len(), 1);
        assert!(matches!(&mock.calls()[0], MockCall::PlayRemote(t) if t == &track));
    }

    #[test]
    fn mock_records_stop_call() {
        let mock = MockAudioBackend::new();
        let result = mock.stop();
        assert!(result.is_ok());
        assert_eq!(mock.calls().len(), 1);
        assert!(matches!(mock.calls()[0], MockCall::Stop));
    }

    #[test]
    fn mock_capture_returns_configured_error() {
        let mock = MockAudioBackend::new();
        mock.set_capture_result(Err("denied".to_string()));
        let result = mock.capture_mic();
        assert!(result.is_err());
        assert!(matches!(result.unwrap_err(), AudioError::MicrophoneDenied));
    }

    #[test]
    fn mock_records_full_call_sequence() {
        let mock = MockAudioBackend::new();
        let _ = mock.capture_mic();
        let _ = mock.play_remote(AudioTrack {
            id: "remote".to_string(),
        });
        let _ = mock.stop();
        let calls = mock.calls();
        assert_eq!(calls.len(), 3);
        assert!(matches!(calls[0], MockCall::CaptureMic));
        assert!(matches!(&calls[1], MockCall::PlayRemote(_)));
        assert!(matches!(calls[2], MockCall::Stop));
    }
}
