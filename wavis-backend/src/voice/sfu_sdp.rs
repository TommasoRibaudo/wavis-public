//! SDP/ICE negotiation primitives for SFU media signaling.
//!
//! **Owns:** SDP offer/answer relay and ICE candidate forwarding between
//! clients and the SFU. Provides the low-level media negotiation operations
//! that the WebSocket handler calls when a connected client sends an Offer
//! or IceCandidate message.
//!
//! **Does not own:** room lifecycle (join, leave, kick, mute) — that is
//! `domain::sfu_relay`. Does not own SFU server communication — the actual
//! transport is behind `domain::sfu_bridge` traits. Does not own WebSocket
//! framing or message dispatch (that is `handlers::ws`).
//!
//! **Key invariants:**
//! - Offer/answer relay requires a valid `SfuRoomHandle` and an active
//!   `SfuSignalingProxy` — in LiveKit mode (no proxy), the handler skips
//!   these functions entirely.
//! - Errors are wrapped into `SignalingMessage::Error` for client delivery.
//!
//! **Layering:** domain utility. Called by `handlers::ws_dispatch`.
//! Depends on `domain::sfu_bridge` traits.

use crate::voice::sfu_bridge::{SfuRoomHandle, SfuSignalingProxy};
use shared::signaling::{IceCandidate, SignalingMessage};

use super::sfu_relay::PeerId;

/// Result of handling an SFU-specific signaling message (offer/ICE forwarding).
#[derive(Debug)]
pub enum SfuRelayResult {
    /// SDP answer from SFU to send back to the client.
    SdpAnswer { peer_id: PeerId, answer_sdp: String },
    /// ICE candidate forwarded successfully.
    IceForwarded,
    /// Error to send back to the client.
    Error {
        peer_id: PeerId,
        error: SignalingMessage,
    },
}

/// Handle an Offer from a client in an SFU room — forward to SFU, return answer.
pub async fn handle_sfu_offer(
    bridge: &dyn SfuSignalingProxy,
    handle: &SfuRoomHandle,
    sender_peer_id: &str,
    offer_sdp: &str,
) -> SfuRelayResult {
    match bridge
        .forward_offer(handle, sender_peer_id, offer_sdp)
        .await
    {
        Ok(answer_sdp) => SfuRelayResult::SdpAnswer {
            peer_id: sender_peer_id.to_string(),
            answer_sdp,
        },
        Err(e) => SfuRelayResult::Error {
            peer_id: sender_peer_id.to_string(),
            error: SignalingMessage::Error(shared::signaling::ErrorPayload {
                message: format!("SFU offer failed: {e}"),
            }),
        },
    }
}

/// Handle an ICE candidate from a client in an SFU room — forward to SFU.
pub async fn handle_sfu_ice(
    bridge: &dyn SfuSignalingProxy,
    handle: &SfuRoomHandle,
    sender_peer_id: &str,
    candidate: &IceCandidate,
) -> SfuRelayResult {
    match bridge
        .forward_ice_candidate(handle, sender_peer_id, candidate)
        .await
    {
        Ok(()) => SfuRelayResult::IceForwarded,
        Err(e) => SfuRelayResult::Error {
            peer_id: sender_peer_id.to_string(),
            error: SignalingMessage::Error(shared::signaling::ErrorPayload {
                message: format!("SFU ICE forward failed: {e}"),
            }),
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::voice::mock_sfu_bridge::MockSfuBridge;
    use crate::voice::sfu_bridge::SfuError;
    use async_trait::async_trait;

    /// Signaling proxy that always fails, to exercise the error-wrapping path.
    /// `MockSfuBridge` always succeeds on forward_offer/forward_ice_candidate,
    /// so it can't express this case.
    struct FailingSfuProxy;

    #[async_trait]
    impl SfuSignalingProxy for FailingSfuProxy {
        async fn forward_offer(
            &self,
            _handle: &SfuRoomHandle,
            _participant_id: &str,
            _sdp: &str,
        ) -> Result<String, SfuError> {
            Err(SfuError::Unavailable("sfu offline".to_string()))
        }

        async fn forward_ice_candidate(
            &self,
            _handle: &SfuRoomHandle,
            _participant_id: &str,
            _candidate: &IceCandidate,
        ) -> Result<(), SfuError> {
            Err(SfuError::Unavailable("sfu offline".to_string()))
        }

        async fn poll_sfu_ice_candidates(
            &self,
            _handle: &SfuRoomHandle,
            _participant_id: &str,
        ) -> Result<Vec<IceCandidate>, SfuError> {
            Ok(vec![])
        }
    }

    fn test_ice_candidate() -> IceCandidate {
        IceCandidate {
            candidate: "candidate:1 1 UDP 1 127.0.0.1 1 typ host".to_string(),
            sdp_mid: "0".to_string(),
            sdp_mline_index: 0,
        }
    }

    #[tokio::test]
    async fn offer_success_returns_sdp_answer() {
        let bridge = MockSfuBridge::new();
        let handle = SfuRoomHandle("room-1".to_string());

        let result = handle_sfu_offer(&bridge, &handle, "peer1", "offer-sdp").await;

        match result {
            SfuRelayResult::SdpAnswer {
                peer_id,
                answer_sdp,
            } => {
                assert_eq!(peer_id, "peer1");
                assert_eq!(answer_sdp, "mock-answer-sdp");
            }
            other => panic!("expected SdpAnswer, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn offer_error_wraps_into_error_signal() {
        let bridge = FailingSfuProxy;
        let handle = SfuRoomHandle("room-1".to_string());

        let result = handle_sfu_offer(&bridge, &handle, "peer1", "offer-sdp").await;

        match result {
            SfuRelayResult::Error { peer_id, error } => {
                assert_eq!(peer_id, "peer1");
                match error {
                    SignalingMessage::Error(payload) => {
                        assert!(payload.message.contains("SFU offer failed"));
                    }
                    other => panic!("expected SignalingMessage::Error, got {other:?}"),
                }
            }
            other => panic!("expected SfuRelayResult::Error, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn ice_success_returns_forwarded() {
        let bridge = MockSfuBridge::new();
        let handle = SfuRoomHandle("room-1".to_string());

        let result = handle_sfu_ice(&bridge, &handle, "peer1", &test_ice_candidate()).await;

        assert!(matches!(result, SfuRelayResult::IceForwarded));
    }

    #[tokio::test]
    async fn ice_error_wraps_into_error_signal() {
        let bridge = FailingSfuProxy;
        let handle = SfuRoomHandle("room-1".to_string());

        let result = handle_sfu_ice(&bridge, &handle, "peer1", &test_ice_candidate()).await;

        match result {
            SfuRelayResult::Error { peer_id, error } => {
                assert_eq!(peer_id, "peer1");
                match error {
                    SignalingMessage::Error(payload) => {
                        assert!(payload.message.contains("SFU ICE forward failed"));
                    }
                    other => panic!("expected SignalingMessage::Error, got {other:?}"),
                }
            }
            other => panic!("expected SfuRelayResult::Error, got {other:?}"),
        }
    }
}
