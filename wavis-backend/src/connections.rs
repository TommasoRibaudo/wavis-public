//! Connection routing abstraction for signaling message delivery.
//!
//! **Owns:** the `ConnectionManager` trait and its production implementation
//! `LiveConnections`. Manages a `peer_id → mpsc::UnboundedSender` map so
//! that domain code can send signaling messages to any connected peer
//! without depending on WebSocket implementation details.
//!
//! **Does not own:** WebSocket upgrade, message parsing, session lifecycle,
//! or any business logic. This module is a pure delivery mechanism.
//!
//! **Key invariants:**
//! - `register` replaces any existing sender for a peer_id — reconnecting
//!   peers overwrite the stale channel.
//! - `send_to` silently drops messages for unknown or disconnected peers
//!   (the channel may have been closed). Callers must not rely on delivery
//!   guarantees.
//! - The internal `RwLock` is `std::sync::RwLock` (not tokio), so it must
//!   not be held across `.await` points.
//!
//! **Layering:** infrastructure layer. Used by `handlers::ws` for
//! registration/teardown and by domain code (via the `ConnectionManager`
//! trait) for message dispatch.

use shared::signaling::{self, SignalingMessage};
use std::collections::HashMap;
use std::sync::RwLock;
use tokio::sync::mpsc;

// Abstraction used by handler/domain code to send a signaling message to a peer.
// This keeps relay logic independent from websocket implementation details.
pub trait ConnectionManager: Send + Sync {
    fn send_to(&self, peer_id: &str, message: &SignalingMessage);
}

pub struct LiveConnections {
    // peer_id -> channel sender for that peer's websocket task.
    senders: RwLock<HashMap<String, mpsc::UnboundedSender<String>>>,
}

impl Default for LiveConnections {
    fn default() -> Self {
        Self::new()
    }
}

impl LiveConnections {
    pub fn new() -> Self {
        Self {
            senders: RwLock::new(HashMap::new()),
        }
    }

    // Register/replace an active websocket sender for this peer.
    pub fn register(&self, peer_id: String, sender: mpsc::UnboundedSender<String>) {
        self.senders.write().unwrap().insert(peer_id, sender);
    }

    // Remove sender when peer disconnects.
    pub fn unregister(&self, peer_id: &str) {
        self.senders.write().unwrap().remove(peer_id);
    }
}
impl ConnectionManager for LiveConnections {
    fn send_to(&self, peer_id: &str, message: &SignalingMessage) {
        // Serialize signaling enum to JSON wire format and forward through channel.
        if let Ok(payload) = signaling::to_json(message)
            && let Some(sender) = self.senders.read().unwrap().get(peer_id)
        {
            let _ = sender.send(payload);
        }
    }
}
