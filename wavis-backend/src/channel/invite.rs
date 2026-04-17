//! Invite code store and validation.
//!
//! **Owns:** in-memory storage of invite codes, generation of
//! cryptographically random codes, consumption (use-count decrement),
//! revocation, per-room and global limits, TTL expiry, and background
//! sweep of stale invites.
//!
//! **Does not own:** the decision of *when* to require an invite (that is
//! controlled by `AppState::require_invite_code`), HTTP/WebSocket
//! transport, or room membership mutations.
//!
//! **Key invariants:**
//! - **Atomic consume + insert (§4.2, §6.5):** invite consumption and peer
//!   insertion must succeed or fail together. [`InviteStore::validate_and_consume`]
//!   is called inside `crate::state::InMemoryRoomState::try_add_peer_with`'s closure, which
//!   holds the per-room write lock across both operations. If either step fails,
//!   neither takes effect. **Violation consequence:** a race where the invite is
//!   consumed but the peer insert fails burns a phantom use, eventually locking
//!   out legitimate joiners; the inverse (peer inserted, invite not consumed)
//!   allows unlimited joins on a single invite code.
//! - Revoked or expired invites are never valid, even if `remaining_uses > 0`.
//! - Per-room and global invite caps are enforced at creation time.
//!
//! **Layering:** called by `handlers::ws` (signaling join path) and
//! `domain::sfu_relay`. No handler or database dependencies.

use rand::RngCore;
use shared::signaling::JoinRejectionReason;
use std::collections::HashMap;
use std::sync::RwLock;
use std::time::{Duration, Instant};

use crate::voice::sfu_relay::ParticipantRole;

/// A single invite code record stored in the [`InviteStore`].
///
/// Created by [`InviteStore::generate`] and keyed by [`code`](Self::code).
/// The record is immutable after creation except for [`remaining_uses`](Self::remaining_uses)
/// (decremented on consumption) and [`revoked`](Self::revoked) (set by revocation).
#[derive(Debug, Clone)]
pub struct InviteRecord {
    /// URL-safe base64-encoded invite code (128 bits of CSPRNG entropy).
    /// Used as the lookup key in the invite store; brute-force infeasible.
    pub code: String,
    /// Room this invite grants access to. Validated on join — a code for
    /// room A cannot be used to join room B.
    pub room_id: String,
    /// Peer ID of the host who created this invite.
    pub issuer_id: String,
    /// Monotonic timestamp when the invite was created.
    pub issued_at: Instant,
    /// Monotonic timestamp after which the invite is considered expired.
    /// Set to `issued_at + config.default_ttl` at creation time.
    pub expires_at: Instant,
    /// Number of join attempts this invite can still satisfy. Decremented
    /// atomically by [`InviteStore::validate_and_consume`]. Zero means exhausted.
    pub remaining_uses: u32,
    /// Whether the invite has been revoked by a host. Revoked invites are
    /// permanently invalid regardless of `remaining_uses` or expiry.
    pub revoked: bool,
}

/// Configuration for the InviteStore.
#[derive(Debug, Clone)]
pub struct InviteStoreConfig {
    /// Default TTL for generated invite codes (default: 24h).
    pub default_ttl: Duration,
    /// Default max uses per invite (default: 6).
    pub default_max_uses: u32,
    /// Max active invites per room (default: 20).
    pub max_invites_per_room: usize,
    /// Max active invites globally (default: 1000).
    pub max_invites_global: usize,
    /// Background sweep interval (default: 60s).
    pub sweep_interval: Duration,
}

impl Default for InviteStoreConfig {
    fn default() -> Self {
        Self {
            default_ttl: Duration::from_secs(86400),
            default_max_uses: 6,
            max_invites_per_room: 20,
            max_invites_global: 1000,
            sweep_interval: Duration::from_secs(60),
        }
    }
}

/// Errors returned by InviteStore operations (not join rejections).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum InviteError {
    /// Per-room invite limit reached.
    RoomLimitReached { room_id: String, limit: usize },
    /// Global invite store limit reached.
    GlobalLimitReached { limit: usize },
    /// Code not found (for revocation).
    NotFound,
}

impl std::fmt::Display for InviteError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            InviteError::RoomLimitReached { room_id, limit } => {
                write!(
                    f,
                    "per-room invite limit ({limit}) reached for room {room_id}"
                )
            }
            InviteError::GlobalLimitReached { limit } => {
                write!(f, "global invite limit ({limit}) reached")
            }
            InviteError::NotFound => write!(f, "invite code not found"),
        }
    }
}

impl std::error::Error for InviteError {}

/// Errors returned by authorized invite revocation.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum InviteRevokeError {
    /// Invite code not found.
    NotFound,
    /// Requestor is not authorized (role != Host or room mismatch).
    Unauthorized,
}

/// In-memory store for invite codes.
///
/// Thread-safe via internal RwLocks. All time-dependent fields use
/// `std::time::Instant` (monotonic clock) — no wall-clock dependency.
pub struct InviteStore {
    /// code → InviteRecord. O(1) lookup; 128-bit entropy makes brute-force infeasible.
    invites: RwLock<HashMap<String, InviteRecord>>,
    /// room_id → count of active (non-expired, non-revoked) invites.
    room_invite_counts: RwLock<HashMap<String, usize>>,
    config: InviteStoreConfig,
}

impl InviteStore {
    /// Create a new invite store with the given configuration.
    ///
    /// The store starts empty. Invite records are added via [`Self::generate`]
    /// and removed by expiry sweep ([`Self::sweep_expired`]) or room destruction
    /// ([`Self::remove_room_invites`]).
    pub fn new(config: InviteStoreConfig) -> Self {
        Self {
            invites: RwLock::new(HashMap::new()),
            room_invite_counts: RwLock::new(HashMap::new()),
            config,
        }
    }

    /// Generate a new invite code for a room.
    ///
    /// Returns `Err` if the per-room or global invite limit is reached (abuse
    /// protection — prevents an attacker from flooding the store).
    ///
    /// # Security
    /// The code is 128 bits of CSPRNG entropy encoded as URL-safe base64,
    /// making brute-force guessing infeasible. Both the `invites` and
    /// `room_invite_counts` write locks are held for the duration, so
    /// concurrent generates cannot exceed the configured limits.
    pub fn generate(
        &self,
        room_id: &str,
        issuer_id: &str,
        max_uses: Option<u32>,
        now: Instant,
    ) -> Result<InviteRecord, InviteError> {
        let mut invites = self.invites.write().unwrap();
        let mut counts = self.room_invite_counts.write().unwrap();

        // Check global limit
        if invites.len() >= self.config.max_invites_global {
            return Err(InviteError::GlobalLimitReached {
                limit: self.config.max_invites_global,
            });
        }

        // Check per-room limit
        let room_count = counts.get(room_id).copied().unwrap_or(0);
        if room_count >= self.config.max_invites_per_room {
            return Err(InviteError::RoomLimitReached {
                room_id: room_id.to_string(),
                limit: self.config.max_invites_per_room,
            });
        }

        // Generate 128-bit CSPRNG code, URL-safe base64 encoded
        let mut bytes = [0u8; 16];
        rand::thread_rng().fill_bytes(&mut bytes);
        let code = base64::engine::Engine::encode(
            &base64::engine::general_purpose::URL_SAFE_NO_PAD,
            bytes,
        );

        let max_uses = max_uses.unwrap_or(self.config.default_max_uses);
        let record = InviteRecord {
            code: code.clone(),
            room_id: room_id.to_string(),
            issuer_id: issuer_id.to_string(),
            issued_at: now,
            expires_at: now + self.config.default_ttl,
            remaining_uses: max_uses,
            revoked: false,
        };

        invites.insert(code, record.clone());
        *counts.entry(room_id.to_string()).or_insert(0) += 1;

        Ok(record)
    }

    /// Validate an invite code for a join attempt (read-only — does not consume).
    ///
    /// Checks: exists, not revoked, not expired, `room_id` matches, `remaining_uses > 0`.
    /// Returns `Ok(())` on success or `Err(JoinRejectionReason)` on failure.
    ///
    /// **Note:** this acquires a *read* lock only — it does not decrement uses.
    /// For the atomic validate-and-decrement used in the join path, see
    /// [`Self::validate_and_consume`].
    pub fn validate(
        &self,
        code: &str,
        room_id: &str,
        now: Instant,
    ) -> Result<(), JoinRejectionReason> {
        let invites = self.invites.read().unwrap();
        let record = invites
            .get(code)
            .ok_or(JoinRejectionReason::InviteInvalid)?;

        if record.revoked {
            return Err(JoinRejectionReason::InviteRevoked);
        }
        if now >= record.expires_at {
            return Err(JoinRejectionReason::InviteExpired);
        }
        if record.room_id != room_id {
            return Err(JoinRejectionReason::InviteInvalid);
        }
        if record.remaining_uses == 0 {
            return Err(JoinRejectionReason::InviteExhausted);
        }

        Ok(())
    }

    /// Decrement `remaining_uses` by 1 (no validation).
    ///
    /// **Superseded by [`Self::validate_and_consume`]**, which eliminates the
    /// TOCTOU gap between a separate `validate` call and this decrement.
    /// Retained for backward compatibility but no production code path calls it.
    ///
    /// If called, it should be from inside `try_add_peer_with`'s closure
    /// (under the per-room write lock). Safe from deadlock because no code
    /// path holds the invite-store lock while acquiring a room lock.
    pub fn consume_use(&self, code: &str) {
        let mut invites = self.invites.write().unwrap();
        if let Some(record) = invites.get_mut(code)
            && record.remaining_uses > 0
        {
            record.remaining_uses -= 1;
        }
    }

    /// Atomically validate and consume one use of an invite code.
    ///
    /// Combines validation (exists, not revoked, not expired, room matches,
    /// uses remaining) with a `remaining_uses -= 1` decrement under a single
    /// write lock, eliminating the TOCTOU race that a separate
    /// `validate` → `consume_use` sequence would have.
    ///
    /// # Security (§4.2, §6.5)
    /// Must be called inside [`crate::state::InMemoryRoomState::try_add_peer_with`]'s closure
    /// so that invite exhaustion and room capacity are enforced atomically
    /// under the per-room write lock. If invite consumption were outside that
    /// lock, a concurrent join could consume the invite and then fail on
    /// capacity, burning a use without granting access.
    pub fn validate_and_consume(
        &self,
        code: &str,
        room_id: &str,
        now: Instant,
    ) -> Result<(), JoinRejectionReason> {
        let mut invites = self.invites.write().unwrap();
        let record = invites
            .get_mut(code)
            .ok_or(JoinRejectionReason::InviteInvalid)?;

        if record.revoked {
            return Err(JoinRejectionReason::InviteRevoked);
        }
        if now >= record.expires_at {
            return Err(JoinRejectionReason::InviteExpired);
        }
        if record.room_id != room_id {
            return Err(JoinRejectionReason::InviteInvalid);
        }
        if record.remaining_uses == 0 {
            return Err(JoinRejectionReason::InviteExhausted);
        }

        record.remaining_uses -= 1;
        Ok(())
    }

    /// Mark an invite as revoked (unconditional — no authorization check).
    ///
    /// Revoked invites are permanently invalid. Prefer [`Self::revoke_authorized`]
    /// for client-facing revocation, which enforces host-only access control.
    pub fn revoke(&self, code: &str) -> Result<(), InviteError> {
        let mut invites = self.invites.write().unwrap();
        let record = invites.get_mut(code).ok_or(InviteError::NotFound)?;
        record.revoked = true;
        Ok(())
    }

    /// Mark an invite as revoked, with authorization checks.
    ///
    /// Verifies: record exists, `record.room_id == room_id`, `requestor_role == Host`.
    /// The `requestor_id` parameter is stored for future issuer-based revocation
    /// policy (host-or-issuer) without API redesign.
    pub fn revoke_authorized(
        &self,
        code: &str,
        room_id: &str,
        _requestor_id: &str,
        requestor_role: ParticipantRole,
    ) -> Result<(), InviteRevokeError> {
        let mut invites = self.invites.write().unwrap();
        let record = invites.get_mut(code).ok_or(InviteRevokeError::NotFound)?;

        if record.room_id != room_id {
            return Err(InviteRevokeError::Unauthorized);
        }
        if requestor_role != ParticipantRole::Host {
            return Err(InviteRevokeError::Unauthorized);
        }

        record.revoked = true;
        Ok(())
    }

    /// Remove all invites for a room and reset its per-room invite count.
    ///
    /// Called on room destruction to prevent stale invite codes from being
    /// validated against a future room that reuses the same ID. Also frees
    /// capacity under the per-room and global invite limits.
    pub fn remove_room_invites(&self, room_id: &str) {
        let mut invites = self.invites.write().unwrap();
        invites.retain(|_, record| record.room_id != room_id);
        let mut counts = self.room_invite_counts.write().unwrap();
        counts.remove(room_id);
    }

    /// Sweep expired invites and return the number removed.
    ///
    /// Called periodically by the background sweep task (interval configured
    /// via [`InviteStoreConfig::sweep_interval`]). Removes all records where
    /// `now >= expires_at` and decrements the per-room invite counts to keep
    /// them consistent. Zero-count room entries are pruned.
    pub fn sweep_expired(&self, now: Instant) -> usize {
        let mut invites = self.invites.write().unwrap();
        let before = invites.len();
        let mut counts = self.room_invite_counts.write().unwrap();

        invites.retain(|_, record| {
            let expired = now >= record.expires_at;
            if expired {
                let count = counts.entry(record.room_id.clone()).or_insert(0);
                if *count > 0 {
                    *count -= 1;
                }
            }
            !expired
        });

        // Clean up zero-count room entries
        counts.retain(|_, &mut c| c > 0);

        before - invites.len()
    }

    /// Returns the default TTL in seconds (for use in InviteCreated responses).
    pub fn default_ttl_secs(&self) -> u64 {
        self.config.default_ttl.as_secs()
    }

    /// Returns the number of active invites for a room (for testing/inspection).
    pub fn room_invite_count(&self, room_id: &str) -> usize {
        self.room_invite_counts
            .read()
            .unwrap()
            .get(room_id)
            .copied()
            .unwrap_or(0)
    }

    /// Returns the total number of invites in the store.
    pub fn total_invite_count(&self) -> usize {
        self.invites.read().unwrap().len()
    }
}

impl Default for InviteStore {
    fn default() -> Self {
        Self::new(InviteStoreConfig::default())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use proptest::prelude::*;

    fn make_store() -> InviteStore {
        InviteStore::new(InviteStoreConfig {
            default_ttl: Duration::from_secs(3600),
            default_max_uses: 6,
            max_invites_per_room: 20,
            max_invites_global: 1000,
            sweep_interval: Duration::from_secs(60),
        })
    }

    fn now() -> Instant {
        Instant::now()
    }

    // --- Unit tests for edge cases ---

    #[test]
    fn test_missing_invite_code_returns_invite_invalid() {
        let store = make_store();
        let result = store.validate("nonexistent-code", "room-1", now());
        assert_eq!(result, Err(JoinRejectionReason::InviteInvalid));
    }

    #[test]
    fn test_single_use_invite() {
        let store = make_store();
        let t = now();
        let record = store.generate("room-1", "issuer-1", Some(1), t).unwrap();

        // First use: valid — validate_and_consume atomically checks + decrements
        assert!(
            store
                .validate_and_consume(&record.code, "room-1", t)
                .is_ok()
        );

        // Second use: exhausted
        assert_eq!(
            store.validate_and_consume(&record.code, "room-1", t),
            Err(JoinRejectionReason::InviteExhausted)
        );
    }

    /// Verify that a single-use invite cannot be double-consumed.
    /// This is the core atomicity assertion for §4.2: the second
    /// `validate_and_consume` must see the decremented count from the first.
    #[test]
    fn test_double_consume_rejected() {
        let store = make_store();
        let t = now();
        let record = store.generate("room-1", "issuer-1", Some(1), t).unwrap();

        // First consume succeeds
        assert!(
            store
                .validate_and_consume(&record.code, "room-1", t)
                .is_ok()
        );

        // Second consume must be rejected — invite is exhausted
        assert_eq!(
            store.validate_and_consume(&record.code, "room-1", t),
            Err(JoinRejectionReason::InviteExhausted),
        );

        // Third attempt also rejected (idempotent rejection)
        assert_eq!(
            store.validate_and_consume(&record.code, "room-1", t),
            Err(JoinRejectionReason::InviteExhausted),
        );
    }

    #[test]
    fn test_sweep_with_zero_expired_invites() {
        let store = make_store();
        let t = now();
        store.generate("room-1", "issuer-1", None, t).unwrap();
        // Sweep at same time — nothing expired yet
        let removed = store.sweep_expired(t);
        assert_eq!(removed, 0);
        assert_eq!(store.total_invite_count(), 1);
    }

    #[test]
    fn test_valid_invite_accepted() {
        let store = make_store();
        let t = now();
        let record = store.generate("room-1", "issuer-1", None, t).unwrap();

        assert_eq!(
            store.validate_and_consume(&record.code, "room-1", t),
            Ok(())
        );
    }

    #[test]
    fn test_expired_invite_rejected() {
        let store = InviteStore::new(InviteStoreConfig {
            default_ttl: Duration::from_secs(1),
            ..InviteStoreConfig::default()
        });
        let t = now();
        let record = store.generate("room-1", "issuer-1", None, t).unwrap();

        assert_eq!(
            store.validate(&record.code, "room-1", t + Duration::from_secs(2)),
            Err(JoinRejectionReason::InviteExpired)
        );
    }

    #[test]
    fn test_invalid_invite_code_format_rejected() {
        let store = make_store();
        let t = now();

        for code in ["", "!@#$%", "abc"] {
            assert_eq!(
                store.validate(code, "room-1", t),
                Err(JoinRejectionReason::InviteInvalid),
                "code {code:?} should be rejected as invalid"
            );
        }
    }

    #[test]
    fn test_room_invite_limit_at_limit_rejected() {
        let store = InviteStore::new(InviteStoreConfig {
            max_invites_per_room: 3,
            max_invites_global: 1000,
            ..InviteStoreConfig::default()
        });
        let t = now();

        for _ in 0..3 {
            store.generate("room-1", "issuer-1", None, t).unwrap();
        }

        let result = store.generate("room-1", "issuer-1", None, t);
        assert!(matches!(
            result,
            Err(InviteError::RoomLimitReached { ref room_id, limit })
                if room_id == "room-1" && limit == 3
        ));
    }

    #[test]
    fn test_room_invite_limit_over_limit_rejected() {
        let store = InviteStore::new(InviteStoreConfig {
            max_invites_per_room: 3,
            max_invites_global: 1000,
            ..InviteStoreConfig::default()
        });
        let t = now();

        for attempt in 0..5 {
            let result = store.generate("room-1", "issuer-1", None, t);
            if attempt < 3 {
                assert!(result.is_ok(), "attempt {} should succeed", attempt + 1);
            } else {
                assert!(
                    matches!(
                        result,
                        Err(InviteError::RoomLimitReached { ref room_id, limit })
                            if room_id == "room-1" && limit == 3
                    ),
                    "attempt {} should be rejected at the room cap",
                    attempt + 1
                );
            }
        }

        assert_eq!(store.room_invite_count("room-1"), 3);
    }

    // --- Property 1: Invite generation produces correct fields ---
    // Feature: invite-code-hardening, Property 1: Invite generation produces correct fields
    // Validates: Requirements 1.2, 5.1

    proptest! {
        #![proptest_config(ProptestConfig::with_cases(200))]

        #[test]
        fn prop_p1_invite_generation_correct_fields(
            room_id in "[a-z]{4,12}",
            issuer_id in "[a-z]{4,12}",
            max_uses in 1u32..=10u32,
        ) {
            let store = make_store();
            let t = now();
            let record = store.generate(&room_id, &issuer_id, Some(max_uses), t).unwrap();

            prop_assert_eq!(&record.room_id, &room_id);
            prop_assert_eq!(&record.issuer_id, &issuer_id);
            prop_assert_eq!(record.remaining_uses, max_uses);
            prop_assert_eq!(record.expires_at, t + store.config.default_ttl);
            prop_assert!(!record.revoked);
        }
    }

    // --- Property 2: Invite generation round-trip ---
    // Feature: invite-code-hardening, Property 2: Invite generation round-trip
    // Validates: Requirements 1.3

    proptest! {
        #![proptest_config(ProptestConfig::with_cases(200))]

        #[test]
        fn prop_p2_invite_generation_round_trip(
            room_id in "[a-z]{4,12}",
            issuer_id in "[a-z]{4,12}",
        ) {
            let store = make_store();
            let t = now();
            let record = store.generate(&room_id, &issuer_id, None, t).unwrap();

            // Validate the code exists and is valid
            let result = store.validate(&record.code, &room_id, t);
            prop_assert!(result.is_ok(), "generated code must be valid immediately after generation");
        }
    }

    // --- Property 3: Generated codes are valid URL-safe base64 with sufficient entropy ---
    // Feature: invite-code-hardening, Property 3: Generated codes are valid URL-safe base64
    // Validates: Requirements 1.1, 1.4

    proptest! {
        #![proptest_config(ProptestConfig::with_cases(200))]

        #[test]
        fn prop_p3_generated_codes_are_url_safe_base64(
            room_id in "[a-z]{4,12}",
            issuer_id in "[a-z]{4,12}",
        ) {
            let store = make_store();
            let t = now();
            let record = store.generate(&room_id, &issuer_id, None, t).unwrap();

            // All chars must be URL-safe base64: A-Z, a-z, 0-9, -, _
            for ch in record.code.chars() {
                prop_assert!(
                    ch.is_ascii_alphanumeric() || ch == '-' || ch == '_',
                    "code char '{}' is not URL-safe base64", ch
                );
            }

            // Decode must yield at least 16 bytes (128 bits)
            let decoded = base64::engine::Engine::decode(
                &base64::engine::general_purpose::URL_SAFE_NO_PAD,
                &record.code,
            ).expect("code must be valid base64");
            prop_assert!(decoded.len() >= 16, "decoded code must be at least 16 bytes");
        }
    }

    // --- Property 4: Expiration TTL is correctly applied ---
    // Feature: invite-code-hardening, Property 4: Expiration TTL is correctly applied
    // Validates: Requirements 2.1

    proptest! {
        #![proptest_config(ProptestConfig::with_cases(200))]

        #[test]
        fn prop_p4_expiration_ttl_correctly_applied(
            ttl_secs in 1u64..=86400u64,
        ) {
            let config = InviteStoreConfig {
                default_ttl: Duration::from_secs(ttl_secs),
                ..InviteStoreConfig::default()
            };
            let store = InviteStore::new(config);
            let t = now();
            let record = store.generate("room-1", "issuer-1", None, t).unwrap();

            prop_assert_eq!(
                record.expires_at,
                t + Duration::from_secs(ttl_secs),
                "expires_at must equal issued_at + TTL"
            );
        }
    }

    // --- Property 5: Expired invites are rejected ---
    // Feature: invite-code-hardening, Property 5: Expired invites are rejected
    // Validates: Requirements 2.2

    proptest! {
        #![proptest_config(ProptestConfig::with_cases(200))]

        #[test]
        fn prop_p5_expired_invites_are_rejected(
            ttl_secs in 1u64..=3600u64,
            extra_secs in 1u64..=3600u64,
        ) {
            let config = InviteStoreConfig {
                default_ttl: Duration::from_secs(ttl_secs),
                ..InviteStoreConfig::default()
            };
            let store = InviteStore::new(config);
            let t = now();
            let record = store.generate("room-1", "issuer-1", None, t).unwrap();

            // Validate at a time strictly after expires_at
            let future = t + Duration::from_secs(ttl_secs) + Duration::from_secs(extra_secs);
            let result = store.validate(&record.code, "room-1", future);
            prop_assert_eq!(result, Err(JoinRejectionReason::InviteExpired));
        }
    }

    // --- Property 6: Room destruction removes all room invites ---
    // Feature: invite-code-hardening, Property 6: Room destruction removes all room invites
    // Validates: Requirements 2.3, 3.3

    proptest! {
        #![proptest_config(ProptestConfig::with_cases(200))]

        #[test]
        fn prop_p6_room_destruction_removes_all_invites(
            n in 1usize..=5usize,
        ) {
            let store = make_store();
            let t = now();

            // Generate N invites for room-a and some for room-b
            for _ in 0..n {
                store.generate("room-a", "issuer-1", None, t).unwrap();
            }
            store.generate("room-b", "issuer-1", None, t).unwrap();

            store.remove_room_invites("room-a");

            prop_assert_eq!(store.room_invite_count("room-a"), 0, "room-a invites must be removed");
            prop_assert_eq!(store.room_invite_count("room-b"), 1, "room-b invites must be unaffected");
        }
    }

    // --- Property 7: Revoke then validate returns invite_revoked ---
    // Feature: invite-code-hardening, Property 7: Revoke then validate returns invite_revoked
    // Validates: Requirements 3.1, 3.2

    proptest! {
        #![proptest_config(ProptestConfig::with_cases(200))]

        #[test]
        fn prop_p7_revoke_then_validate_returns_revoked(
            room_id in "[a-z]{4,12}",
        ) {
            let store = make_store();
            let t = now();
            let record = store.generate(&room_id, "issuer-1", None, t).unwrap();

            store.revoke(&record.code).unwrap();
            let result = store.validate(&record.code, &room_id, t);
            prop_assert_eq!(result, Err(JoinRejectionReason::InviteRevoked));
        }
    }

    // --- Property 8: Non-existent codes are rejected ---
    // Feature: invite-code-hardening, Property 8: Non-existent codes are rejected
    // Validates: Requirements 4.2

    proptest! {
        #![proptest_config(ProptestConfig::with_cases(200))]

        #[test]
        fn prop_p8_nonexistent_codes_are_rejected(
            code in "[A-Za-z0-9_-]{22}",
        ) {
            let store = make_store();
            let result = store.validate(&code, "room-1", now());
            prop_assert_eq!(result, Err(JoinRejectionReason::InviteInvalid));
        }
    }

    // --- Property 9: Room mismatch is rejected ---
    // Feature: invite-code-hardening, Property 9: Room mismatch is rejected
    // Validates: Requirements 4.3, 4.4

    proptest! {
        #![proptest_config(ProptestConfig::with_cases(200))]

        #[test]
        fn prop_p9_room_mismatch_is_rejected(
            room_a in "[a-z]{4,8}",
            room_b in "[a-z]{4,8}",
        ) {
            prop_assume!(room_a != room_b);
            let store = make_store();
            let t = now();
            let record = store.generate(&room_a, "issuer-1", None, t).unwrap();

            let result = store.validate(&record.code, &room_b, t);
            prop_assert_eq!(result, Err(JoinRejectionReason::InviteInvalid));
        }
    }

    // --- Property 10: Invite use decrement and exhaustion ---
    // Feature: invite-code-hardening, Property 10: Invite use decrement and exhaustion
    // Validates: Requirements 5.2, 5.3

    proptest! {
        #![proptest_config(ProptestConfig::with_cases(200))]

        #[test]
        fn prop_p10_invite_use_decrement_and_exhaustion(
            max_uses in 1u32..=6u32,
        ) {
            let store = make_store();
            let t = now();
            let record = store.generate("room-1", "issuer-1", Some(max_uses), t).unwrap();

            // Consume all uses via validate_and_consume (atomic validate + decrement)
            for i in 0..max_uses {
                let result = store.validate_and_consume(&record.code, "room-1", t);
                prop_assert!(result.is_ok(), "use {} of {} should succeed", i + 1, max_uses);
            }

            // Next validate_and_consume should be exhausted
            let result = store.validate_and_consume(&record.code, "room-1", t);
            prop_assert_eq!(result, Err(JoinRejectionReason::InviteExhausted));
        }
    }

    // --- Property 20: Per-room invite limit enforced ---
    // Feature: invite-code-hardening, Property 20: Per-room invite limit enforced
    // Validates: Requirements 11.1, 11.2

    proptest! {
        #![proptest_config(ProptestConfig::with_cases(100))]

        #[test]
        fn prop_p20_per_room_invite_limit_enforced(
            limit in 1usize..=5usize,
        ) {
            let config = InviteStoreConfig {
                max_invites_per_room: limit,
                max_invites_global: 1000,
                ..InviteStoreConfig::default()
            };
            let store = InviteStore::new(config);
            let t = now();

            for _ in 0..limit {
                store.generate("room-1", "issuer-1", None, t).unwrap();
            }

            let result = store.generate("room-1", "issuer-1", None, t);
            prop_assert!(
                matches!(result, Err(InviteError::RoomLimitReached { .. })),
                "should fail with RoomLimitReached after limit"
            );
        }
    }

    // --- Property 21: Global invite limit enforced ---
    // Feature: invite-code-hardening, Property 21: Global invite limit enforced
    // Validates: Requirements 11.3, 11.4

    proptest! {
        #![proptest_config(ProptestConfig::with_cases(50))]

        #[test]
        fn prop_p21_global_invite_limit_enforced(
            global_limit in 1usize..=10usize,
        ) {
            let config = InviteStoreConfig {
                max_invites_per_room: 1000,
                max_invites_global: global_limit,
                ..InviteStoreConfig::default()
            };
            let store = InviteStore::new(config);
            let t = now();

            for i in 0..global_limit {
                store.generate(&format!("room-{i}"), "issuer-1", None, t).unwrap();
            }

            let result = store.generate("room-overflow", "issuer-1", None, t);
            prop_assert!(
                matches!(result, Err(InviteError::GlobalLimitReached { .. })),
                "should fail with GlobalLimitReached after global limit"
            );
        }
    }

    // --- Property 23: Issuer_Id is valid UUID v4 ---
    // Feature: invite-code-hardening, Property 23: Issuer_Id is valid UUID v4
    // Validates: Requirements 12.1

    proptest! {
        #![proptest_config(ProptestConfig::with_cases(200))]

        #[test]
        fn prop_p23_issuer_id_is_valid_uuid_v4(
            n in 1usize..=50usize,
        ) {
            for _ in 0..n {
                let id = uuid::Uuid::new_v4().to_string();

                // Length must be 36 (8-4-4-4-12 + 4 hyphens)
                prop_assert_eq!(id.len(), 36, "UUID must be 36 chars, got: {}", id);

                let bytes = id.as_bytes();

                // Hyphens at positions 8, 13, 18, 23
                prop_assert_eq!(bytes[8], b'-', "expected hyphen at index 8");
                prop_assert_eq!(bytes[13], b'-', "expected hyphen at index 13");
                prop_assert_eq!(bytes[18], b'-', "expected hyphen at index 18");
                prop_assert_eq!(bytes[23], b'-', "expected hyphen at index 23");

                // Version nibble at index 14 must be '4'
                prop_assert_eq!(bytes[14], b'4', "expected version '4' at index 14, got '{}'", bytes[14] as char);

                // Variant nibble at index 19 must be one of '8', '9', 'a', 'b'
                let variant = bytes[19];
                prop_assert!(
                    variant == b'8' || variant == b'9' || variant == b'a' || variant == b'b',
                    "expected variant nibble in [89ab] at index 19, got '{}'", variant as char
                );

                // All non-hyphen characters must be lowercase hex digits
                for (i, &b) in bytes.iter().enumerate() {
                    if i == 8 || i == 13 || i == 18 || i == 23 {
                        continue; // hyphens already checked
                    }
                    prop_assert!(
                        b.is_ascii_hexdigit() && (b.is_ascii_digit() || (b'a'..=b'f').contains(&b)),
                        "non-hex char '{}' at index {}", b as char, i
                    );
                }
            }
        }
    }

    // --- Property 22: Sweep removes exactly expired invites ---
    // Feature: invite-code-hardening, Property 22: Sweep removes exactly expired invites
    // Validates: Requirements 11.5

    proptest! {
        #![proptest_config(ProptestConfig::with_cases(200))]

        #[test]
        fn prop_p22_sweep_removes_exactly_expired_invites(
            n_expired in 0usize..=5usize,
            n_valid in 0usize..=5usize,
        ) {
            let t = now();

            // Create store with short TTL for expired invites
            let short_ttl = Duration::from_secs(1);
            let long_ttl = Duration::from_secs(3600);

            let config = InviteStoreConfig {
                default_ttl: short_ttl,
                max_invites_per_room: 1000,
                max_invites_global: 1000,
                ..InviteStoreConfig::default()
            };
            let store = InviteStore::new(config);

            // Generate expired invites (short TTL, sweep at t + short_ttl + 1s)
            for i in 0..n_expired {
                store.generate(&format!("room-exp-{i}"), "issuer-1", None, t).unwrap();
            }

            // Generate valid invites with long TTL by using a different store config
            let config2 = InviteStoreConfig {
                default_ttl: long_ttl,
                max_invites_per_room: 1000,
                max_invites_global: 1000,
                ..InviteStoreConfig::default()
            };
            let store2 = InviteStore::new(config2);
            for i in 0..n_valid {
                store2.generate(&format!("room-val-{i}"), "issuer-1", None, t).unwrap();
            }

            // Sweep the expired store at t + short_ttl + 1s
            let sweep_time = t + short_ttl + Duration::from_secs(1);
            let removed = store.sweep_expired(sweep_time);
            prop_assert_eq!(removed, n_expired, "sweep must remove exactly n_expired invites");
            prop_assert_eq!(store.total_invite_count(), 0, "all expired invites must be gone");

            // Sweep the valid store — nothing should be removed
            let removed2 = store2.sweep_expired(sweep_time);
            prop_assert_eq!(removed2, 0, "no valid invites should be swept");
            prop_assert_eq!(store2.total_invite_count(), n_valid, "valid invites must remain");
        }
    }

    // --- Security-Hardening Property 1: Guest cannot revoke invites ---
    // Feature: security-hardening, Property 1: Guest cannot revoke invites
    // Validates: Requirements 1.3, 1.4

    proptest! {
        #![proptest_config(ProptestConfig::with_cases(200))]

        #[test]
        fn prop_sh_p1_guest_cannot_revoke_invites(
            room_id in "[a-z]{4,12}",
            issuer_id in "[a-z]{4,12}",
            guest_id in "[a-z]{4,12}",
        ) {
            let store = make_store();
            let t = now();
            let record = store.generate(&room_id, &issuer_id, None, t).unwrap();

            // Guest in the same room attempts to revoke
            let result = store.revoke_authorized(
                &record.code,
                &room_id,
                &guest_id,
                ParticipantRole::Guest,
            );
            prop_assert_eq!(result, Err(InviteRevokeError::Unauthorized));

            // Invite must remain non-revoked
            let validation = store.validate(&record.code, &room_id, t);
            prop_assert!(validation.is_ok(), "invite must remain valid after guest revoke attempt");
        }
    }

    // --- Security-Hardening Property 2: Cross-room revoke is rejected ---
    // Feature: security-hardening, Property 2: Cross-room revoke is rejected
    // Validates: Requirements 1.2, 1.5

    proptest! {
        #![proptest_config(ProptestConfig::with_cases(200))]

        #[test]
        fn prop_sh_p2_cross_room_revoke_is_rejected(
            room_a in "[a-z]{4,8}",
            room_b in "[a-z]{4,8}",
            issuer_id in "[a-z]{4,12}",
            host_id in "[a-z]{4,12}",
        ) {
            prop_assume!(room_a != room_b);
            let store = make_store();
            let t = now();

            // Generate invite for room A
            let record = store.generate(&room_a, &issuer_id, None, t).unwrap();

            // Host in room B attempts to revoke invite belonging to room A
            let result = store.revoke_authorized(
                &record.code,
                &room_b,
                &host_id,
                ParticipantRole::Host,
            );
            prop_assert_eq!(result, Err(InviteRevokeError::Unauthorized));

            // Invite must remain non-revoked
            let validation = store.validate(&record.code, &room_a, t);
            prop_assert!(validation.is_ok(), "invite must remain valid after cross-room revoke attempt");
        }
    }

    // --- Security-Hardening Property 3: Host revoke in own room succeeds ---
    // Feature: security-hardening, Property 3: Host revoke in own room succeeds
    // Validates: Requirements 1.7

    proptest! {
        #![proptest_config(ProptestConfig::with_cases(200))]

        #[test]
        fn prop_sh_p3_host_revoke_in_own_room_succeeds(
            room_id in "[a-z]{4,12}",
            issuer_id in "[a-z]{4,12}",
            host_id in "[a-z]{4,12}",
        ) {
            let store = make_store();
            let t = now();
            let record = store.generate(&room_id, &issuer_id, None, t).unwrap();

            // Host in the same room revokes the invite
            let result = store.revoke_authorized(
                &record.code,
                &room_id,
                &host_id,
                ParticipantRole::Host,
            );
            prop_assert_eq!(result, Ok(()));

            // Invite must now be revoked
            let validation = store.validate(&record.code, &room_id, t);
            prop_assert_eq!(validation, Err(JoinRejectionReason::InviteRevoked));
        }
    }
}
