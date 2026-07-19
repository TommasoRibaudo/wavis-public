//! Short-lived, one-use WebSocket pre-auth tickets.
//!
//! A ticket lets an already Bearer-authenticated client prove its identity
//! to the `/ws` upgrade handler without ever sending a long-lived access
//! token over a connection that hasn't upgraded yet. Minted by
//! `POST /auth/ws-ticket`, consumed exactly once by the `/ws` pre-upgrade
//! check.
//!
//! Stored in-memory (not Postgres): a 60-second TTL has no value once the
//! process restarts, and every other pre-upgrade abuse control (temp ban,
//! IP cap, global ceiling) is already an in-memory `RwLock<HashMap<..>>` —
//! this follows the same pattern rather than introducing a DB round-trip
//! into the hot `/ws` upgrade path.

use hmac::{Hmac, Mac};
use rand::RngCore;
use sha2::Sha256;
use std::collections::HashMap;
use std::sync::RwLock;
use std::time::{Duration, Instant};
use uuid::Uuid;

type HmacSha256 = Hmac<Sha256>;

/// Ticket time-to-live (Req: short-lived).
pub const TICKET_TTL_SECS: u64 = 60;

#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum WsTicketError {
    #[error("ticket not found")]
    NotFound,
    #[error("ticket expired")]
    Expired,
    #[error("ticket already consumed")]
    AlreadyConsumed,
}

struct TicketRecord {
    user_id: Uuid,
    device_id: Uuid,
    expires_at: Instant,
    consumed: bool,
}

/// Compute HMAC-SHA256 of a raw ticket using the server-side pepper.
/// Only this hash is ever stored — the raw ticket exists solely in the
/// REST response and the client's `?ticket=` query parameter.
fn hash_ticket(raw_ticket: &str, pepper: &[u8]) -> Vec<u8> {
    let mut mac = HmacSha256::new_from_slice(pepper).expect("HMAC-SHA256 accepts any key length");
    mac.update(raw_ticket.as_bytes());
    mac.finalize().into_bytes().to_vec()
}

/// Generate a cryptographically random ticket (256-bit, base64url-encoded, no padding).
fn generate_ticket() -> String {
    use base64::{Engine as _, engine::general_purpose::URL_SAFE_NO_PAD};
    let mut bytes = [0u8; 32];
    rand::thread_rng().fill_bytes(&mut bytes);
    URL_SAFE_NO_PAD.encode(bytes)
}

pub struct WsTicketStore {
    tickets: RwLock<HashMap<Vec<u8>, TicketRecord>>,
    pepper: Vec<u8>,
    ttl: Duration,
    now: fn() -> Instant,
}

impl WsTicketStore {
    pub fn new(pepper: Vec<u8>) -> Self {
        Self::with_clock(pepper, Instant::now)
    }

    pub fn with_clock(pepper: Vec<u8>, now: fn() -> Instant) -> Self {
        Self {
            tickets: RwLock::new(HashMap::new()),
            pepper,
            ttl: Duration::from_secs(TICKET_TTL_SECS),
            now,
        }
    }

    /// Mint a new ticket for an authenticated user/device. Returns the raw
    /// (plaintext) ticket — only its hash is retained.
    pub fn issue(&self, user_id: Uuid, device_id: Uuid) -> String {
        let raw = generate_ticket();
        let hash = hash_ticket(&raw, &self.pepper);
        let expires_at = (self.now)() + self.ttl;

        self.tickets
            .write()
            .expect("ws tickets write lock poisoned")
            .insert(
                hash,
                TicketRecord {
                    user_id,
                    device_id,
                    expires_at,
                    consumed: false,
                },
            );

        raw
    }

    /// Validate and atomically consume a raw ticket. One-use: the check and
    /// the `consumed` mutation happen under a single write-lock acquisition,
    /// so two concurrent consumers of the same ticket cannot both succeed.
    pub fn validate_and_consume(&self, raw_ticket: &str) -> Result<(Uuid, Uuid), WsTicketError> {
        let hash = hash_ticket(raw_ticket, &self.pepper);
        let now = (self.now)();
        let mut tickets = self
            .tickets
            .write()
            .expect("ws tickets write lock poisoned");

        match tickets.get_mut(&hash) {
            None => Err(WsTicketError::NotFound),
            Some(record) if record.consumed => Err(WsTicketError::AlreadyConsumed),
            Some(record) if record.expires_at <= now => Err(WsTicketError::Expired),
            Some(record) => {
                record.consumed = true;
                Ok((record.user_id, record.device_id))
            }
        }
    }

    /// Prune expired/consumed entries so the map stays bounded. Returns the
    /// count removed. Wired into the background auth-token sweep.
    pub fn prune_expired(&self) -> usize {
        let now = (self.now)();
        let mut tickets = self
            .tickets
            .write()
            .expect("ws tickets write lock poisoned");
        let before = tickets.len();
        tickets.retain(|_, r| !r.consumed && r.expires_at > now);
        before - tickets.len()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use proptest::prelude::*;
    use std::sync::atomic::{AtomicU64, Ordering};

    fn test_store() -> WsTicketStore {
        WsTicketStore::new(b"test-ws-ticket-pepper-32-bytes!!".to_vec())
    }

    #[test]
    fn issue_then_consume_succeeds_once() {
        let store = test_store();
        let user_id = Uuid::new_v4();
        let device_id = Uuid::new_v4();

        let ticket = store.issue(user_id, device_id);
        let result = store.validate_and_consume(&ticket);
        assert_eq!(result, Ok((user_id, device_id)));
    }

    #[test]
    fn replayed_ticket_fails() {
        let store = test_store();
        let ticket = store.issue(Uuid::new_v4(), Uuid::new_v4());

        assert!(store.validate_and_consume(&ticket).is_ok());
        assert_eq!(
            store.validate_and_consume(&ticket),
            Err(WsTicketError::AlreadyConsumed)
        );
    }

    #[test]
    fn unknown_ticket_fails() {
        let store = test_store();
        assert_eq!(
            store.validate_and_consume("not-a-real-ticket"),
            Err(WsTicketError::NotFound)
        );
    }

    #[test]
    fn expired_ticket_fails_via_clock_injection() {
        static TICK: AtomicU64 = AtomicU64::new(0);
        TICK.store(0, Ordering::SeqCst);
        fn fake_clock() -> Instant {
            Instant::now() + Duration::from_secs(TICK.load(Ordering::SeqCst))
        }

        let store =
            WsTicketStore::with_clock(b"test-ws-ticket-pepper-32-bytes!!".to_vec(), fake_clock);
        let ticket = store.issue(Uuid::new_v4(), Uuid::new_v4());

        TICK.store(TICKET_TTL_SECS + 1, Ordering::SeqCst);
        assert_eq!(
            store.validate_and_consume(&ticket),
            Err(WsTicketError::Expired)
        );
    }

    #[test]
    fn prune_expired_removes_stale_and_consumed_entries() {
        static TICK: AtomicU64 = AtomicU64::new(0);
        TICK.store(0, Ordering::SeqCst);
        fn fake_clock() -> Instant {
            Instant::now() + Duration::from_secs(TICK.load(Ordering::SeqCst))
        }

        let store =
            WsTicketStore::with_clock(b"test-ws-ticket-pepper-32-bytes!!".to_vec(), fake_clock);
        let consumed_ticket = store.issue(Uuid::new_v4(), Uuid::new_v4());
        let _unconsumed_ticket = store.issue(Uuid::new_v4(), Uuid::new_v4());
        store.validate_and_consume(&consumed_ticket).unwrap();

        TICK.store(TICKET_TTL_SECS + 1, Ordering::SeqCst);
        let pruned = store.prune_expired();
        assert_eq!(
            pruned, 2,
            "both consumed and expired-unconsumed tickets are pruned"
        );
    }

    proptest! {
        #![proptest_config(ProptestConfig::with_cases(100))]

        #[test]
        fn prop_hash_ticket_deterministic(
            ticket in "[A-Za-z0-9_-]{8,64}",
            pepper in prop::collection::vec(any::<u8>(), 16..64),
        ) {
            let hash1 = hash_ticket(&ticket, &pepper);
            let hash2 = hash_ticket(&ticket, &pepper);
            prop_assert_eq!(hash1, hash2);
        }

        #[test]
        fn prop_different_tickets_different_hashes(
            ticket_a in "[A-Za-z0-9_-]{8,64}",
            ticket_b in "[A-Za-z0-9_-]{8,64}",
            pepper in prop::collection::vec(any::<u8>(), 16..64),
        ) {
            prop_assume!(ticket_a != ticket_b);
            let hash_a = hash_ticket(&ticket_a, &pepper);
            let hash_b = hash_ticket(&ticket_b, &pepper);
            prop_assert_ne!(hash_a, hash_b);
        }
    }
}
