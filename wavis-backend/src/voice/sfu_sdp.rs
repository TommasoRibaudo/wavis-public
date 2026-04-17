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
