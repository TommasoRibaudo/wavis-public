use super::sfu_bridge::{SfuError, SfuHealth, SfuRoomHandle, SfuRoomManager, SfuSignalingProxy};
use async_trait::async_trait;
use shared::signaling::IceCandidate;
use std::sync::{Arc, Mutex};

/// Records all calls made to the mock for test assertions.
#[derive(Debug, Clone, PartialEq)]
pub enum MockSfuCall {
    CreateRoom(String),
    DestroyRoom(String),
    AddParticipant {
        room_handle: String,
        participant_id: String,
    },
    RemoveParticipant {
        room_handle: String,
        participant_id: String,
    },
    HealthCheck,
    ForwardOffer {
        room_handle: String,
        participant_id: String,
        sdp: String,
    },
    ForwardIceCandidate {
        room_handle: String,
        participant_id: String,
    },
    PollSfuIceCandidates {
        room_handle: String,
        participant_id: String,
    },
}

/// Configurable responses for the mock.
#[derive(Debug, Clone)]
pub struct MockSfuConfig {
    pub health_result: Result<SfuHealth, String>,
    pub create_room_result: Result<(), String>,
    pub add_participant_result: Result<(), String>,
    pub remove_participant_result: Result<(), String>,
    pub destroy_room_result: Result<(), String>,
    pub forward_offer_answer: String,
    pub poll_ice_candidates: Vec<IceCandidate>,
}

impl Default for MockSfuConfig {
    fn default() -> Self {
        Self {
            health_result: Ok(SfuHealth::Available),
            create_room_result: Ok(()),
            add_participant_result: Ok(()),
            remove_participant_result: Ok(()),
            destroy_room_result: Ok(()),
            forward_offer_answer: "mock-answer-sdp".to_string(),
            poll_ice_candidates: vec![],
        }
    }
}

/// Mock SFU bridge for testing backend logic without a running SFU process.
/// Records all calls in `Arc<Mutex<Vec<MockSfuCall>>>` and returns configurable results.
pub struct MockSfuBridge {
    pub calls: Arc<Mutex<Vec<MockSfuCall>>>,
    pub config: Arc<Mutex<MockSfuConfig>>,
}

#[allow(dead_code)]
impl MockSfuBridge {
    pub fn new() -> Self {
        Self {
            calls: Arc::new(Mutex::new(Vec::new())),
            config: Arc::new(Mutex::new(MockSfuConfig::default())),
        }
    }

    pub fn with_config(config: MockSfuConfig) -> Self {
        Self {
            calls: Arc::new(Mutex::new(Vec::new())),
            config: Arc::new(Mutex::new(config)),
        }
    }

    pub fn get_calls(&self) -> Vec<MockSfuCall> {
        self.calls.lock().unwrap().clone()
    }

    pub fn set_health(&self, result: Result<SfuHealth, String>) {
        self.config.lock().unwrap().health_result = result;
    }

    pub fn set_poll_ice_candidates(&self, candidates: Vec<IceCandidate>) {
        self.config.lock().unwrap().poll_ice_candidates = candidates;
    }
}

impl Default for MockSfuBridge {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl SfuRoomManager for MockSfuBridge {
    async fn create_room(&self, room_id: &str) -> Result<SfuRoomHandle, SfuError> {
        self.calls
            .lock()
            .unwrap()
            .push(MockSfuCall::CreateRoom(room_id.to_string()));
        let config = self.config.lock().unwrap();
        match &config.create_room_result {
            Ok(()) => Ok(SfuRoomHandle(room_id.to_string())),
            Err(e) => Err(SfuError::Unavailable(e.clone())),
        }
    }

    async fn destroy_room(&self, handle: &SfuRoomHandle) -> Result<(), SfuError> {
        self.calls
            .lock()
            .unwrap()
            .push(MockSfuCall::DestroyRoom(handle.0.clone()));
        let config = self.config.lock().unwrap();
        match &config.destroy_room_result {
            Ok(()) => Ok(()),
            Err(e) => Err(SfuError::Unavailable(e.clone())),
        }
    }

    async fn add_participant(
        &self,
        handle: &SfuRoomHandle,
        participant_id: &str,
    ) -> Result<(), SfuError> {
        self.calls
            .lock()
            .unwrap()
            .push(MockSfuCall::AddParticipant {
                room_handle: handle.0.clone(),
                participant_id: participant_id.to_string(),
            });
        let config = self.config.lock().unwrap();
        match &config.add_participant_result {
            Ok(()) => Ok(()),
            Err(e) => Err(SfuError::ParticipantError(e.clone())),
        }
    }

    async fn remove_participant(
        &self,
        handle: &SfuRoomHandle,
        participant_id: &str,
    ) -> Result<(), SfuError> {
        self.calls
            .lock()
            .unwrap()
            .push(MockSfuCall::RemoveParticipant {
                room_handle: handle.0.clone(),
                participant_id: participant_id.to_string(),
            });
        let config = self.config.lock().unwrap();
        match &config.remove_participant_result {
            Ok(()) => Ok(()),
            Err(e) => Err(SfuError::ParticipantError(e.clone())),
        }
    }

    async fn health_check(&self) -> Result<SfuHealth, SfuError> {
        self.calls.lock().unwrap().push(MockSfuCall::HealthCheck);
        let config = self.config.lock().unwrap();
        match &config.health_result {
            Ok(health) => Ok(health.clone()),
            Err(e) => Err(SfuError::Unavailable(e.clone())),
        }
    }
}

#[async_trait]
impl SfuSignalingProxy for MockSfuBridge {
    async fn forward_offer(
        &self,
        handle: &SfuRoomHandle,
        participant_id: &str,
        sdp: &str,
    ) -> Result<String, SfuError> {
        self.calls.lock().unwrap().push(MockSfuCall::ForwardOffer {
            room_handle: handle.0.clone(),
            participant_id: participant_id.to_string(),
            sdp: sdp.to_string(),
        });
        let config = self.config.lock().unwrap();
        Ok(config.forward_offer_answer.clone())
    }

    async fn forward_ice_candidate(
        &self,
        handle: &SfuRoomHandle,
        participant_id: &str,
        _candidate: &IceCandidate,
    ) -> Result<(), SfuError> {
        self.calls
            .lock()
            .unwrap()
            .push(MockSfuCall::ForwardIceCandidate {
                room_handle: handle.0.clone(),
                participant_id: participant_id.to_string(),
            });
        Ok(())
    }

    async fn poll_sfu_ice_candidates(
        &self,
        handle: &SfuRoomHandle,
        participant_id: &str,
    ) -> Result<Vec<IceCandidate>, SfuError> {
        self.calls
            .lock()
            .unwrap()
            .push(MockSfuCall::PollSfuIceCandidates {
                room_handle: handle.0.clone(),
                participant_id: participant_id.to_string(),
            });
        let config = self.config.lock().unwrap();
        Ok(config.poll_ice_candidates.clone())
    }
}
