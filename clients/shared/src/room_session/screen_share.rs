//! Owns screen-share coordination for `RoomSession`.
//!
//! This module handles local share signaling commands and the explicit mutation
//! path for `active_shares`. `mod.rs` remains the single signaling dispatcher,
//! but delegates share-related state updates here so mutation ownership stays
//! obvious. Share callbacks use a lock-then-clone-then-call pattern to avoid
//! running user code while a mutex is held.

use super::{LiveKitConnection, RoomError, RoomSession, ShareCb};
use crate::audio::AudioBackend;
use crate::signaling::WebSocketConnection;
use crate::webrtc::PeerConnectionBackend;
use shared::signaling::{
    ShareStartedPayload, ShareStatePayload, ShareStoppedPayload, SignalingMessage, StopSharePayload,
};
use std::collections::HashSet;
use std::sync::{Arc, Mutex};

impl<A, P, W, L> RoomSession<A, P, W, L>
where
    A: AudioBackend + 'static,
    P: PeerConnectionBackend + 'static,
    W: WebSocketConnection + 'static,
    L: LiveKitConnection + 'static,
{
    /// Register a callback invoked when a participant starts sharing.
    pub fn on_share_started(&self, cb: impl Fn(String) + Send + Sync + 'static) {
        *self.on_share_started.lock().unwrap() = Some(Arc::new(cb));
    }

    /// Register a callback invoked when a participant stops sharing.
    pub fn on_share_stopped(&self, cb: impl Fn(String) + Send + Sync + 'static) {
        *self.on_share_stopped.lock().unwrap() = Some(Arc::new(cb));
    }

    /// Register a callback invoked when a full share state snapshot arrives.
    pub fn on_share_state(&self, cb: impl Fn(Vec<String>) + Send + Sync + 'static) {
        *self.on_share_state.lock().unwrap() = Some(Arc::new(cb));
    }

    /// Returns a clone of the current set of active sharer participant IDs.
    pub fn active_shares(&self) -> HashSet<String> {
        self.active_shares.lock().unwrap().clone()
    }

    /// Send a start_share signaling message.
    pub fn start_share(&self) -> Result<(), RoomError> {
        if !*self.in_room.lock().unwrap() {
            return Err(RoomError::NotInRoom);
        }
        self.signaling
            .send(&SignalingMessage::StartShare)
            .map_err(|e| RoomError::Signaling(e.to_string()))
    }

    /// Send a stop_share signaling message, optionally targeting another participant.
    pub fn stop_share(&self, target: Option<&str>) -> Result<(), RoomError> {
        if !*self.in_room.lock().unwrap() {
            return Err(RoomError::NotInRoom);
        }
        self.signaling
            .send(&SignalingMessage::StopShare(StopSharePayload {
                target_participant_id: target.map(|participant_id| participant_id.to_string()),
            }))
            .map_err(|e| RoomError::Signaling(e.to_string()))
    }

    /// Send a stop_all_shares signaling message.
    pub fn stop_all_shares(&self) -> Result<(), RoomError> {
        if !*self.in_room.lock().unwrap() {
            return Err(RoomError::NotInRoom);
        }
        self.signaling
            .send(&SignalingMessage::StopAllShares)
            .map_err(|e| RoomError::Signaling(e.to_string()))
    }

    pub(super) fn handle_share_state_message(
        active_shares: &Arc<Mutex<HashSet<String>>>,
        on_share_state: &ShareCb<Vec<String>>,
        payload: ShareStatePayload,
    ) {
        let snapshot: Vec<String> = {
            let mut shares = active_shares.lock().unwrap();
            *shares = payload.participant_ids.into_iter().collect();
            shares.iter().cloned().collect()
        };
        let cb = on_share_state.lock().unwrap().clone();
        if let Some(ref callback) = cb {
            callback(snapshot);
        }
    }

    pub(super) fn handle_share_started_message(
        active_shares: &Arc<Mutex<HashSet<String>>>,
        on_share_started: &ShareCb<String>,
        payload: ShareStartedPayload,
    ) {
        let participant_id = payload.participant_id;
        {
            active_shares.lock().unwrap().insert(participant_id.clone());
        }
        let cb = on_share_started.lock().unwrap().clone();
        if let Some(ref callback) = cb {
            callback(participant_id);
        }
    }

    pub(super) fn handle_share_stopped_message(
        active_shares: &Arc<Mutex<HashSet<String>>>,
        on_share_stopped: &ShareCb<String>,
        payload: ShareStoppedPayload,
    ) {
        let participant_id = payload.participant_id;
        {
            active_shares.lock().unwrap().remove(&participant_id);
        }
        let cb = on_share_stopped.lock().unwrap().clone();
        if let Some(ref callback) = cb {
            callback(participant_id);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::super::{make_session, LiveKitConnection, MockLiveKitCall, MockLiveKitConnection};
    use crate::audio::{AudioBackend, MockAudioBackend};
    use proptest::prelude::*;
    use std::collections::HashSet;

    #[derive(Debug, Clone)]
    enum ShareEvent {
        ShareState(Vec<String>),
        ShareStarted(String),
        ShareStopped(String),
        ParticipantLeft(String),
    }

    fn arb_share_events() -> impl Strategy<Value = Vec<ShareEvent>> {
        let peer_pool = proptest::collection::vec("[a-z]{3,6}", 1..=6);
        peer_pool.prop_flat_map(|peers| {
            let peers_clone = peers.clone();
            proptest::collection::vec((0usize..4, 0usize..100), 1..=30).prop_map(move |choices| {
                choices
                    .into_iter()
                    .map(|(kind, idx)| {
                        let peer = peers_clone[idx % peers_clone.len()].clone();
                        match kind {
                            0 => {
                                let subset: Vec<String> = peers_clone
                                    .iter()
                                    .enumerate()
                                    .filter(|(i, _)| (idx >> (i % 8)) & 1 == 1)
                                    .map(|(_, participant_id)| participant_id.clone())
                                    .collect();
                                ShareEvent::ShareState(subset)
                            }
                            1 => ShareEvent::ShareStarted(peer),
                            2 => ShareEvent::ShareStopped(peer),
                            _ => ShareEvent::ParticipantLeft(peer),
                        }
                    })
                    .collect()
            })
        })
    }

    fn share_event_to_json(event: &ShareEvent) -> String {
        match event {
            ShareEvent::ShareState(ids) => {
                let ids_json: Vec<String> = ids.iter().map(|id| format!("\"{}\"", id)).collect();
                format!(
                    r#"{{"type":"share_state","participantIds":[{}]}}"#,
                    ids_json.join(",")
                )
            }
            ShareEvent::ShareStarted(id) => {
                format!(
                    r#"{{"type":"share_started","participantId":"{}","displayName":"Test User"}}"#,
                    id
                )
            }
            ShareEvent::ShareStopped(id) => {
                format!(
                    r#"{{"type":"share_stopped","participantId":"{}","displayName":"Test User"}}"#,
                    id
                )
            }
            ShareEvent::ParticipantLeft(id) => {
                format!(r#"{{"type":"participant_left","participantId":"{}"}}"#, id)
            }
        }
    }

    proptest! {
        #![proptest_config(ProptestConfig::with_cases(100))]

        #[test]
        fn prop_client_share_state_tracking(events in arb_share_events()) {
            let session = make_session();
            session.join_room("testroom", None).unwrap();

            let mut expected: HashSet<String> = HashSet::new();

            for event in &events {
                match event {
                    ShareEvent::ShareState(ids) => {
                        expected = ids.iter().cloned().collect();
                    }
                    ShareEvent::ShareStarted(id) => {
                        expected.insert(id.clone());
                    }
                    ShareEvent::ShareStopped(id) => {
                        expected.remove(id);
                    }
                    ShareEvent::ParticipantLeft(id) => {
                        expected.remove(id);
                    }
                }

                let json = share_event_to_json(event);
                session.handle_incoming(&json).unwrap();
            }

            let actual = session.active_shares();
            prop_assert_eq!(actual, expected);
        }
    }

    #[test]
    fn track_source_classification_mic_vs_screen_audio() {
        let mock_lk = MockLiveKitConnection::new();

        let audio = MockAudioBackend::new();
        let track = audio.capture_mic().unwrap();
        mock_lk.publish_audio(&track).unwrap();
        mock_lk.publish_screen_audio().unwrap();

        let calls = mock_lk.get_calls();
        assert!(calls.contains(&MockLiveKitCall::PublishAudio));
        assert!(calls.contains(&MockLiveKitCall::PublishScreenAudio));
        assert_ne!(
            MockLiveKitCall::PublishAudio,
            MockLiveKitCall::PublishScreenAudio
        );
    }

    #[test]
    fn unpublish_screen_audio_does_not_affect_mic() {
        let mock_lk = MockLiveKitConnection::new();

        let audio = MockAudioBackend::new();
        let track = audio.capture_mic().unwrap();
        mock_lk.publish_audio(&track).unwrap();
        mock_lk.publish_screen_audio().unwrap();

        let calls_before = mock_lk.get_calls();
        assert_eq!(calls_before.len(), 2);

        mock_lk.unpublish_screen_audio().unwrap();

        let calls = mock_lk.get_calls();
        assert!(calls.contains(&MockLiveKitCall::UnpublishScreenAudio));
        assert!(!calls.contains(&MockLiveKitCall::Disconnect));
        assert_eq!(calls.len(), 3);
    }

    #[test]
    fn screen_audio_lifecycle_independent_of_mic() {
        let mock_lk = MockLiveKitConnection::new();

        let audio = MockAudioBackend::new();
        let track = audio.capture_mic().unwrap();
        mock_lk.publish_audio(&track).unwrap();

        mock_lk.publish_screen_audio().unwrap();
        mock_lk.feed_screen_audio(&[0i16; 960]).unwrap();
        mock_lk.unpublish_screen_audio().unwrap();

        mock_lk.publish_screen_audio().unwrap();
        mock_lk.unpublish_screen_audio().unwrap();

        let calls = mock_lk.get_calls();
        let publish_audio_count = calls
            .iter()
            .filter(|call| matches!(call, MockLiveKitCall::PublishAudio))
            .count();
        let publish_screen_count = calls
            .iter()
            .filter(|call| matches!(call, MockLiveKitCall::PublishScreenAudio))
            .count();
        let feed_screen_count = calls
            .iter()
            .filter(|call| matches!(call, MockLiveKitCall::FeedScreenAudio { .. }))
            .count();
        let unpublish_screen_count = calls
            .iter()
            .filter(|call| matches!(call, MockLiveKitCall::UnpublishScreenAudio))
            .count();
        let disconnect_count = calls
            .iter()
            .filter(|call| matches!(call, MockLiveKitCall::Disconnect))
            .count();

        assert_eq!(publish_audio_count, 1);
        assert_eq!(publish_screen_count, 2);
        assert_eq!(feed_screen_count, 1);
        assert_eq!(unpublish_screen_count, 2);
        assert_eq!(disconnect_count, 0);
    }
}
