use std::time::Instant;

use crate::channel::invite::InviteStore;
use crate::state::{InMemoryRoomState, RoomInfo};
use crate::voice::sfu_relay::OutboundSignal;
use shared::signaling::{ErrorPayload, JoinRejectionReason, JoinedPayload, SignalingMessage};

/// Type alias for peer identifiers.
pub type PeerId = String;

/// Type alias for room identifiers.
pub type RoomId = String;

/// Trait for querying room membership state.
/// Phase 1 provides the real implementation. This trait allows the relay
/// logic to be tested independently with mock state.
pub trait RoomState {
    /// Get the room ID for a given peer, if the peer is in a room.
    fn get_room_for_peer(&self, peer_id: &str) -> Option<RoomId>;

    /// Get all peer IDs in a given room.
    fn get_peers_in_room(&self, room_id: &RoomId) -> Vec<PeerId>;
}

/// Result of attempting to relay a signaling message.
#[derive(Debug, PartialEq)]
pub enum RelayResult {
    /// Message successfully relayed to the target peer.
    Relayed {
        target_peer_id: PeerId,
        message: SignalingMessage,
    },
    /// Error: sender not in a room or alone in the room.
    NoPeer { error: SignalingMessage },
}

/// Relay a signaling message from sender to the other peer in the room.
///
/// This function:
/// 1. Looks up which room the sender is in
/// 2. Finds the other peer in that room
/// 3. Returns the message to forward to the target peer
///
/// If the sender is not in a room or is alone in the room, returns an error.
///
/// # Requirements
/// - 1.1: Relay offer messages
/// - 1.2: Relay answer messages
/// - 1.3: Relay ice_candidate messages
/// - 1.4: Reject messages when no peer is available
pub fn relay_signaling(
    state: &dyn RoomState,
    sender_peer_id: &str,
    message: SignalingMessage,
) -> RelayResult {
    // 1. Find which room the sender is in
    let room_id = match state.get_room_for_peer(sender_peer_id) {
        Some(id) => id,
        None => {
            return RelayResult::NoPeer {
                error: SignalingMessage::Error(ErrorPayload {
                    message: "not in a room".to_string(),
                }),
            };
        }
    };

    // 2. Find all peers in that room
    let peers = state.get_peers_in_room(&room_id);

    // 3. Find the other peer (not the sender)
    let target_peer_id = peers.into_iter().find(|peer_id| peer_id != sender_peer_id);

    match target_peer_id {
        Some(target) => RelayResult::Relayed {
            target_peer_id: target,
            message,
        },
        None => RelayResult::NoPeer {
            error: SignalingMessage::Error(ErrorPayload {
                message: "no peer available".to_string(),
            }),
        },
    }
}

/// Result of a P2P join attempt.
#[derive(Debug)]
pub enum P2PJoinResult {
    /// Join succeeded — outbound signals to dispatch.
    Joined(Vec<OutboundSignal>),
    /// Room is at capacity.
    RoomFull,
    /// Invite validation failed (exhausted, expired, revoked, invalid).
    InviteRejected(JoinRejectionReason),
}

/// Handle a peer joining a P2P room.
///
/// Performs an atomic capacity-checked join via `try_add_peer_with`, which
/// holds the per-room write lock across the capacity check, peer insertion,
/// and invite use decrement. The handler is responsible for validating the
/// invite BEFORE calling this function; this function only does the atomic
/// join + consume.
///
/// # Requirements
/// - 2.1: Enforce max 2 peers in P2P rooms
/// - 2.2: Respond with Joined on success
/// - 5.2: Decrement invite remaining_uses atomically with peer insertion
/// - 8.1, 8.2: Atomic capacity enforcement
pub fn handle_p2p_join(
    state: &InMemoryRoomState,
    room_id: &str,
    peer_id: &str,
    invite_store: &InviteStore,
    invite_code: Option<&str>,
) -> P2PJoinResult {
    // Snapshot existing peers before the join (for notifications).
    // This is a best-effort snapshot; the authoritative join is atomic below.
    let peers_before = state.get_peers_in_room(&room_id.to_string());

    // Ensure RoomInfo exists for this P2P room so that subsequent messages
    // (Offer/Answer/ICE/Leave/disconnect) can determine the room type via
    // get_room_info(). create_room is a no-op if the room already exists.
    state.create_room(room_id.to_string(), RoomInfo::new_p2p());

    // Atomic join: capacity check + invite validate + consume under one lock.
    let peer_count =
        match state.try_add_peer_with(peer_id.to_string(), &room_id.to_string(), || {
            if let Some(code) = invite_code {
                invite_store.validate_and_consume(code, room_id, Instant::now())
            } else {
                Ok(())
            }
        }) {
            Ok(count) => count as u32,
            Err(JoinRejectionReason::RoomFull) => return P2PJoinResult::RoomFull,
            Err(reason) => return P2PJoinResult::InviteRejected(reason),
        };

    let mut signals = vec![OutboundSignal::to_peer(
        peer_id,
        SignalingMessage::Joined(JoinedPayload {
            room_id: room_id.to_string(),
            peer_id: peer_id.to_string(),
            peer_count,
            participants: vec![],
            ice_config: None,
            share_permission: None, // P2P rooms have no share permission concept
        }),
    )];

    // Notify existing peers that a new peer joined.
    for existing in &peers_before {
        if existing != peer_id {
            signals.push(OutboundSignal::to_peer(
                existing,
                SignalingMessage::Joined(JoinedPayload {
                    room_id: room_id.to_string(),
                    peer_id: existing.to_string(),
                    peer_count,
                    participants: vec![],
                    ice_config: None,
                    share_permission: None, // P2P rooms have no share permission concept
                }),
            ));
        }
    }

    P2PJoinResult::Joined(signals)
}

/// Handle a peer disconnect and notify the remaining peer if applicable.
///
/// This function:
/// 1. Looks up which room the disconnected peer was in
/// 2. Finds the remaining peer in that room
/// 3. Returns the target peer ID and a `peer_left` message
///
/// Returns `None` if the peer was not in a room or was alone in the room.
///
/// # Requirements
/// - 1.5: Notify remaining peer when a peer disconnects
/// - 6.3: Handle WebSocket disconnect as implicit leave
pub fn handle_disconnect(
    state: &dyn RoomState,
    peer_id: &str,
) -> Option<(PeerId, SignalingMessage)> {
    // 1. Find which room the disconnected peer was in
    let room_id = state.get_room_for_peer(peer_id)?;

    // 2. Find all peers in that room
    let peers = state.get_peers_in_room(&room_id);

    // 3. Find the remaining peer (not the disconnected one)
    let remaining_peer_id = peers.into_iter().find(|p| p != peer_id)?;

    // 4. Return the target peer and peer_left message
    Some((remaining_peer_id, SignalingMessage::PeerLeft))
}

#[cfg(test)]
mod tests {
    use super::*;
    use proptest::prelude::*;
    use std::collections::HashMap;

    // Import Arbitrary implementations from shared crate
    #[allow(unused_imports)]
    use shared::signaling::proptest_support;

    // Feature: p2p-voice, Property 1: Signaling relay delivers messages unchanged
    // Validates: Requirements 1.1, 1.2, 1.3

    // Feature: p2p-voice, Property 2: Signaling relay rejects messages when no peer is available
    // Validates: Requirements 1.4

    // --- Mock RoomState implementation for testing ---

    /// Mock implementation of RoomState for property testing.
    /// Allows us to construct arbitrary room configurations.
    #[derive(Debug, Clone)]
    struct MockRoomState {
        /// Maps peer_id -> room_id
        peer_to_room: HashMap<String, String>,
        /// Maps room_id -> Vec<peer_id>
        room_to_peers: HashMap<String, Vec<String>>,
    }

    impl MockRoomState {
        fn new() -> Self {
            Self {
                peer_to_room: HashMap::new(),
                room_to_peers: HashMap::new(),
            }
        }

        fn add_peer_to_room(&mut self, peer_id: String, room_id: String) {
            self.peer_to_room.insert(peer_id.clone(), room_id.clone());
            self.room_to_peers.entry(room_id).or_default().push(peer_id);
        }
    }

    impl RoomState for MockRoomState {
        fn get_room_for_peer(&self, peer_id: &str) -> Option<RoomId> {
            self.peer_to_room.get(peer_id).cloned()
        }

        fn get_peers_in_room(&self, room_id: &RoomId) -> Vec<PeerId> {
            self.room_to_peers.get(room_id).cloned().unwrap_or_default()
        }
    }

    // --- Property test strategies ---

    /// Strategy for generating a two-peer room state.
    /// Returns (state, peer_a_id, peer_b_id, room_id)
    fn two_peer_room_strategy() -> impl Strategy<Value = (MockRoomState, String, String, String)> {
        (any::<String>(), any::<String>(), any::<String>())
            .prop_filter("peers must be distinct", |(peer_a, peer_b, _)| {
                peer_a != peer_b
            })
            .prop_map(|(peer_a, peer_b, room_id)| {
                let mut state = MockRoomState::new();
                state.add_peer_to_room(peer_a.clone(), room_id.clone());
                state.add_peer_to_room(peer_b.clone(), room_id.clone());
                (state, peer_a, peer_b, room_id)
            })
    }

    /// Strategy for generating an empty or solo room state.
    /// Returns (state, peer_id) where peer is either not in a room or alone.
    fn no_peer_available_strategy() -> impl Strategy<Value = (MockRoomState, String)> {
        prop_oneof![
            // Strategy 1: Peer not in any room
            any::<String>().prop_map(|peer_id| {
                let state = MockRoomState::new();
                (state, peer_id)
            }),
            // Strategy 2: Peer alone in a room
            (any::<String>(), any::<String>()).prop_map(|(peer_id, room_id)| {
                let mut state = MockRoomState::new();
                state.add_peer_to_room(peer_id.clone(), room_id);
                (state, peer_id)
            }),
        ]
    }

    // --- Property tests ---

    proptest! {
        #![proptest_config(ProptestConfig::with_cases(100))]

        /// Property 1: Signaling relay delivers messages unchanged
        /// For any signaling message sent by a peer in a two-peer room,
        /// the relay shall deliver that exact message to the other peer.
        #[test]
        fn prop_relay_delivers_messages_unchanged(
            (state, peer_a, peer_b, _room_id) in two_peer_room_strategy(),
            message in any::<SignalingMessage>(),
        ) {
            // Relay message from peer_a
            let result = relay_signaling(&state, &peer_a, message.clone());

            // Assert the message was relayed to peer_b unchanged
            match result {
                RelayResult::Relayed { target_peer_id, message: relayed_message } => {
                    prop_assert_eq!(&target_peer_id, &peer_b, "Message should be relayed to peer_b");
                    prop_assert_eq!(&relayed_message, &message, "Relayed message should equal sent message");
                }
                RelayResult::NoPeer { .. } => {
                    return Err(proptest::test_runner::TestCaseError::fail(
                        "Expected Relayed result, got NoPeer"
                    ));
                }
            }

            // Also test relay from peer_b to peer_a
            let result_reverse = relay_signaling(&state, &peer_b, message.clone());

            match result_reverse {
                RelayResult::Relayed { target_peer_id, message: relayed_message } => {
                    prop_assert_eq!(&target_peer_id, &peer_a, "Message should be relayed to peer_a");
                    prop_assert_eq!(&relayed_message, &message, "Relayed message should equal sent message");
                }
                RelayResult::NoPeer { .. } => {
                    return Err(proptest::test_runner::TestCaseError::fail(
                        "Expected Relayed result, got NoPeer"
                    ));
                }
            }
        }

        /// Property 2: Signaling relay rejects messages when no peer is available
        /// For any signaling message sent by a peer who is not in a room or is alone,
        /// the relay shall return an error.
        #[test]
        fn prop_relay_rejects_when_no_peer_available(
            (state, peer_id) in no_peer_available_strategy(),
            message in any::<SignalingMessage>(),
        ) {
            // Relay message from peer
            let result = relay_signaling(&state, &peer_id, message);

            // Assert the result is NoPeer with an error message
            match result {
                RelayResult::NoPeer { error } => {
                    // Verify it's an error message
                    match error {
                        SignalingMessage::Error(payload) => {
                            prop_assert!(
                                payload.message == "not in a room" || payload.message == "no peer available",
                                "Error message should indicate no peer available, got: {}",
                                payload.message
                            );
                        }
                        _ => {
                            return Err(proptest::test_runner::TestCaseError::fail(
                                "Expected Error message variant"
                            ));
                        }
                    }
                }
                RelayResult::Relayed { .. } => {
                    return Err(proptest::test_runner::TestCaseError::fail(
                        "Expected NoPeer result, got Relayed"
                    ));
                }
            }
        }

        /// Property 3: Disconnect triggers peer_left notification
        /// For any room with two peers, when one peer disconnects,
        /// the remaining peer shall receive a PeerLeft message.
        #[test]
        fn prop_disconnect_triggers_peer_left(
            (state, peer_a, peer_b, _room_id) in two_peer_room_strategy(),
        ) {
            // Disconnect peer_a
            let result = handle_disconnect(&state, &peer_a);

            // Assert peer_b receives PeerLeft message
            match result {
                Some((target_peer_id, message)) => {
                    prop_assert_eq!(&target_peer_id, &peer_b, "PeerLeft should be sent to peer_b");
                    prop_assert_eq!(message, SignalingMessage::PeerLeft, "Message should be PeerLeft");
                }
                None => {
                    return Err(proptest::test_runner::TestCaseError::fail(
                        "Expected Some((peer_id, PeerLeft)), got None"
                    ));
                }
            }

            // Also test disconnect peer_b
            let result_reverse = handle_disconnect(&state, &peer_b);

            match result_reverse {
                Some((target_peer_id, message)) => {
                    prop_assert_eq!(&target_peer_id, &peer_a, "PeerLeft should be sent to peer_a");
                    prop_assert_eq!(message, SignalingMessage::PeerLeft, "Message should be PeerLeft");
                }
                None => {
                    return Err(proptest::test_runner::TestCaseError::fail(
                        "Expected Some((peer_id, PeerLeft)), got None"
                    ));
                }
            }
        }
    }
}
