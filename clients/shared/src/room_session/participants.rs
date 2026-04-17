//! Owns participant tracking for `RoomSession`.
//!
//! This module manages remote participant subscription state and participant
//! callbacks. It does not own room lifecycle or screen-share coordination.

use super::{EventCb, LiveKitConnection, RoomSession};
use crate::audio::AudioBackend;
use crate::audio_pipeline::{AdaptiveBitrateController, AdaptiveJitterBuffer, BitrateConfig};
use crate::signaling::WebSocketConnection;
use crate::webrtc::PeerConnectionBackend;
use shared::signaling::{
    ParticipantInfo, ParticipantJoinedPayload, ParticipantLeftPayload, RoomStatePayload,
};
use std::collections::HashMap;
use std::sync::{Arc, Mutex};

#[allow(dead_code)] // fields used when SFU audio subscription tracking is wired up
pub(crate) struct SubscribeTrackState {
    pub participant_id: String,
    pub jitter_buffer: AdaptiveJitterBuffer,
    pub bitrate_controller: AdaptiveBitrateController,
}

impl SubscribeTrackState {
    fn new(participant_id: String) -> Self {
        Self {
            participant_id,
            jitter_buffer: AdaptiveJitterBuffer::new(),
            bitrate_controller: AdaptiveBitrateController::new(BitrateConfig::default()),
        }
    }
}

impl<A, P, W, L> RoomSession<A, P, W, L>
where
    A: AudioBackend + 'static,
    P: PeerConnectionBackend + 'static,
    W: WebSocketConnection + 'static,
    L: LiveKitConnection + 'static,
{
    /// Register a callback invoked when a remote participant joins.
    pub fn on_participant_joined(&self, cb: impl Fn(ParticipantInfo) + Send + 'static) {
        *self.on_participant_joined.lock().unwrap() = Some(Box::new(cb));
    }

    /// Register a callback invoked when a remote participant leaves.
    pub fn on_participant_left(&self, cb: impl Fn(String) + Send + 'static) {
        *self.on_participant_left.lock().unwrap() = Some(Box::new(cb));
    }

    /// Register a callback invoked when a full room state snapshot arrives.
    pub fn on_room_state(&self, cb: impl Fn(Vec<ParticipantInfo>) + Send + 'static) {
        *self.on_room_state.lock().unwrap() = Some(Box::new(cb));
    }

    /// Returns the number of subscribe tracks currently tracked (for testing).
    pub fn subscribe_track_count(&self) -> usize {
        self.subscribe_tracks.lock().unwrap().len()
    }

    pub(super) fn handle_room_state_message(
        on_room_state: &EventCb<Vec<ParticipantInfo>>,
        payload: RoomStatePayload,
    ) {
        let cb = on_room_state.lock().unwrap();
        if let Some(ref f) = *cb {
            f(payload.participants);
        }
    }

    pub(super) fn handle_participant_joined_message(
        subscribe_tracks: &Arc<Mutex<HashMap<String, SubscribeTrackState>>>,
        on_participant_joined: &EventCb<ParticipantInfo>,
        payload: ParticipantJoinedPayload,
    ) {
        let state = SubscribeTrackState::new(payload.participant_id.clone());
        subscribe_tracks
            .lock()
            .unwrap()
            .insert(payload.participant_id.clone(), state);

        let cb = on_participant_joined.lock().unwrap();
        if let Some(ref f) = *cb {
            f(ParticipantInfo {
                participant_id: payload.participant_id,
                display_name: payload.display_name,
                user_id: None,
                profile_color: payload.profile_color,
            });
        }
    }

    pub(super) fn handle_participant_left_message(
        subscribe_tracks: &Arc<Mutex<HashMap<String, SubscribeTrackState>>>,
        active_shares: &Arc<Mutex<std::collections::HashSet<String>>>,
        on_participant_left: &EventCb<String>,
        payload: ParticipantLeftPayload,
    ) {
        subscribe_tracks
            .lock()
            .unwrap()
            .remove(&payload.participant_id);

        active_shares
            .lock()
            .unwrap()
            .remove(&payload.participant_id);

        let cb = on_participant_left.lock().unwrap();
        if let Some(ref f) = *cb {
            f(payload.participant_id);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::super::{make_session, participant_joined_json, participant_left_json};
    use super::*;
    use crate::audio_pipeline::JitterBuffering;
    use proptest::prelude::*;
    use std::collections::HashSet;
    use std::sync::atomic::{AtomicUsize, Ordering};

    /// An event in the join/leave sequence.
    #[derive(Debug, Clone)]
    enum ParticipantEvent {
        Join(String),
        Leave(String),
    }

    fn arb_participant_events() -> impl Strategy<Value = Vec<ParticipantEvent>> {
        proptest::collection::vec("[a-z]{4,8}", 1..=6).prop_flat_map(|ids| {
            let ids_clone = ids.clone();
            proptest::collection::vec(0usize..ids.len() * 2, 1..=20).prop_map(move |indices| {
                let mut events = Vec::new();
                let mut joined: HashSet<String> = HashSet::new();

                for idx in &indices {
                    let id = ids_clone[idx % ids_clone.len()].clone();
                    if joined.contains(&id) {
                        events.push(ParticipantEvent::Leave(id.clone()));
                        joined.remove(&id);
                    } else {
                        events.push(ParticipantEvent::Join(id.clone()));
                        joined.insert(id);
                    }
                }

                events
            })
        })
    }

    proptest! {
        #![proptest_config(ProptestConfig { cases: 100, .. ProptestConfig::default() })]

        /// Feature: sfu-multi-party-voice, Property 8: Subscribe track map reflects current remote participants
        #[test]
        fn subscribe_track_map_reflects_current_participants(
            events in arb_participant_events(),
        ) {
            let session = make_session();
            session.join_room("testroom", None).unwrap();

            let joined_count = Arc::new(AtomicUsize::new(0));
            let left_count = Arc::new(AtomicUsize::new(0));
            let joined_count_clone = Arc::clone(&joined_count);
            let left_count_clone = Arc::clone(&left_count);

            session.on_participant_joined(move |_| {
                joined_count_clone.fetch_add(1, Ordering::SeqCst);
            });
            session.on_participant_left(move |_| {
                left_count_clone.fetch_add(1, Ordering::SeqCst);
            });

            let mut expected: HashSet<String> = HashSet::new();

            for event in &events {
                match event {
                    ParticipantEvent::Join(id) => {
                        let json = participant_joined_json(id, "Test User");
                        session.handle_incoming(&json).unwrap();
                        expected.insert(id.clone());
                    }
                    ParticipantEvent::Leave(id) => {
                        let json = participant_left_json(id);
                        session.handle_incoming(&json).unwrap();
                        expected.remove(id);
                    }
                }
            }

            let actual_count = session.subscribe_track_count();
            prop_assert_eq!(
                actual_count,
                expected.len(),
                "Track map size {} != expected {}",
                actual_count,
                expected.len()
            );
        }
    }

    proptest! {
        #![proptest_config(ProptestConfig { cases: 50, .. ProptestConfig::default() })]

        /// Feature: sfu-multi-party-voice, Property 9: Per-track audio pipeline independence
        #[test]
        fn per_track_audio_pipeline_independence(
            packet_count in 1usize..=20,
            avg_jitter in 0.0f64..100.0,
            jitter_stddev in 0.0f64..50.0,
        ) {
            let session = make_session();
            session.join_room("testroom", None).unwrap();

            session.handle_incoming(&participant_joined_json("peer-a", "Alice")).unwrap();
            session.handle_incoming(&participant_joined_json("peer-b", "Bob")).unwrap();

            prop_assert_eq!(session.subscribe_track_count(), 2);

            {
                let mut tracks = session.subscribe_tracks.lock().unwrap();
                let track_a = tracks.get_mut("peer-a").unwrap();
                for i in 0..packet_count {
                    track_a.jitter_buffer.push(i as u16, vec![0xAA; 10]);
                }
                track_a.jitter_buffer.update_stats(avg_jitter, jitter_stddev);
            }

            {
                let tracks = session.subscribe_tracks.lock().unwrap();
                let track_b = tracks.get("peer-b").unwrap();
                let expected_default = crate::audio_pipeline::MIN_JITTER_DELAY_MS;

                prop_assert_eq!(
                    track_b.jitter_buffer.len(),
                    0,
                    "peer-b jitter buffer should be empty"
                );
                prop_assert!(
                    (track_b.jitter_buffer.target_delay_ms() - expected_default).abs() < 1e-9,
                    "peer-b target delay should be default {}, got {}",
                    expected_default,
                    track_b.jitter_buffer.target_delay_ms()
                );
            }
        }
    }

    #[test]
    fn participant_joined_invokes_callback() {
        let session = make_session();
        session.join_room("room1", None).unwrap();

        let received = Arc::new(Mutex::new(Vec::new()));
        let received_clone = Arc::clone(&received);
        session.on_participant_joined(move |info| {
            received_clone
                .lock()
                .unwrap()
                .push(info.participant_id.clone());
        });

        session
            .handle_incoming(&participant_joined_json("peer-x", "Xavier"))
            .unwrap();

        let ids = received.lock().unwrap();
        assert_eq!(ids.len(), 1);
        assert_eq!(ids[0], "peer-x");
    }

    #[test]
    fn participant_left_invokes_callback_and_removes_track() {
        let session = make_session();
        session.join_room("room1", None).unwrap();

        let left_ids = Arc::new(Mutex::new(Vec::new()));
        let left_ids_clone = Arc::clone(&left_ids);
        session.on_participant_left(move |id| {
            left_ids_clone.lock().unwrap().push(id);
        });

        session
            .handle_incoming(&participant_joined_json("peer-y", "Yara"))
            .unwrap();
        assert_eq!(session.subscribe_track_count(), 1);

        session
            .handle_incoming(&participant_left_json("peer-y"))
            .unwrap();
        assert_eq!(session.subscribe_track_count(), 0);

        let ids = left_ids.lock().unwrap();
        assert_eq!(ids.len(), 1);
        assert_eq!(ids[0], "peer-y");
    }

    #[test]
    fn room_state_invokes_callback() {
        let session = make_session();
        session.join_room("room1", None).unwrap();

        let received = Arc::new(Mutex::new(Vec::new()));
        let received_clone = Arc::clone(&received);
        session.on_room_state(move |participants| {
            received_clone.lock().unwrap().extend(participants);
        });

        let room_state_json = r#"{"type":"room_state","participants":[{"participantId":"p1","displayName":"Alice"},{"participantId":"p2","displayName":"Bob"}]}"#;
        session.handle_incoming(room_state_json).unwrap();

        let participants = received.lock().unwrap();
        assert_eq!(participants.len(), 2);
    }
}
