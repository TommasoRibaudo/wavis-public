use crate::voice::relay::{PeerId, RoomId, RoomState};
use crate::voice::sfu_bridge::SfuRoomHandle;
use shared::signaling::{JoinRejectionReason, ParticipantInfo};
use std::collections::{HashMap, HashSet};
use std::sync::{Arc, RwLock};
use std::time::{Duration, Instant};

/// Discriminates between Phase 2 P2P rooms and Phase 3 SFU rooms.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RoomType {
    /// Phase 2: max 2 participants, relay_signaling() path.
    P2P,
    /// Phase 3: 3–6 participants, SfuBridge forwarding path.
    Sfu,
}

/// Screen share permission policy for a room.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub enum SharePermission {
    /// Any participant may start a screen share (MVP default).
    #[default]
    AllParticipants,
    /// Only the host may start a screen share.
    HostOnly,
}

impl From<SharePermission> for shared::signaling::WireSharePermission {
    fn from(p: SharePermission) -> Self {
        match p {
            SharePermission::AllParticipants => Self::Anyone,
            SharePermission::HostOnly => Self::HostOnly,
        }
    }
}

impl From<shared::signaling::WireSharePermission> for SharePermission {
    fn from(w: shared::signaling::WireSharePermission) -> Self {
        match w {
            shared::signaling::WireSharePermission::Anyone => Self::AllParticipants,
            shared::signaling::WireSharePermission::HostOnly => Self::HostOnly,
        }
    }
}

/// Source of a participant's sub-room assignment.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SubRoomMembershipSource {
    /// Participant explicitly joined a sub-room via the synchronized sub-room UI.
    Explicit,
    /// Participant is on an older client that does not support sub-rooms and is treated as ROOM 1.
    LegacyRoomOneFallback,
}

/// Metadata for a synchronized sub-room layered on top of the active channel voice session.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SubRoomInfo {
    /// Stable sub-room identifier within the channel voice session.
    pub sub_room_id: String,
    /// Display number shown to users as ROOM N.
    pub room_number: u32,
    /// Whether this is the non-deletable default ROOM 1.
    pub is_default: bool,
    /// Participant identifiers assigned to this sub-room.
    pub participant_ids: Vec<String>,
    /// When this empty room should auto-delete. `None` for ROOM 1 and non-empty rooms.
    pub delete_at: Option<Instant>,
}

impl SubRoomInfo {
    pub fn room_one(sub_room_id: impl Into<String>) -> Self {
        Self {
            sub_room_id: sub_room_id.into(),
            room_number: 1,
            is_default: true,
            participant_ids: vec![],
            delete_at: None,
        }
    }
}

/// Synchronized sub-room state attached to a channel voice session.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SubRoomState {
    /// Ordered list of synchronized sub-rooms. Ordering drives ROOM N rendering.
    pub rooms: Vec<SubRoomInfo>,
    /// Reverse index of participant_id -> sub_room_id.
    pub participant_assignments: HashMap<String, String>,
    /// How each participant's current assignment was chosen.
    pub membership_sources: HashMap<String, SubRoomMembershipSource>,
    /// Optional active passthrough pair for this voice session.
    pub active_passthrough: Option<PassthroughPair>,
}

impl SubRoomState {
    pub fn new(room_one_id: impl Into<String>) -> Self {
        Self {
            rooms: vec![SubRoomInfo::room_one(room_one_id)],
            participant_assignments: HashMap::new(),
            membership_sources: HashMap::new(),
            active_passthrough: None,
        }
    }
}

/// Normalized authoritative passthrough pair between two synchronized sub-rooms.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PassthroughPair {
    pub source_sub_room_id: String,
    pub target_sub_room_id: String,
}

/// Room metadata stored alongside the peer list.
#[derive(Debug, Clone)]
pub struct RoomInfo {
    pub room_type: RoomType,
    pub sfu_handle: Option<SfuRoomHandle>,
    pub max_participants: u8,
    pub participants: Vec<ParticipantInfo>,
    pub created_at: Instant,
    /// When each participant's MediaToken was last issued (for preemptive refresh).
    pub token_issued_at: HashMap<String, Instant>,
    /// Participants that have completed SFU media connection.
    pub media_connected: HashSet<String>,
    /// Participants removed from this room (kick/leave) — blocks MediaToken re-issuance.
    /// Maps participant_id → time of revocation. Cleaned up lazily on token issuance.
    pub revoked_participants: HashMap<String, Instant>,
    /// Participant IDs currently sharing their screen. SFU rooms only.
    pub active_shares: HashSet<String>,
    /// Screen share permission policy (default: AllParticipants).
    pub share_permission: SharePermission,
    /// Optional synchronized sub-room state for channel-based voice sessions.
    /// Legacy direct rooms keep this as `None`.
    pub sub_room_state: Option<SubRoomState>,
}

impl RoomInfo {
    pub fn new_p2p() -> Self {
        Self {
            room_type: RoomType::P2P,
            sfu_handle: None,
            max_participants: 2,
            participants: vec![],
            created_at: Instant::now(),
            token_issued_at: HashMap::new(),
            media_connected: HashSet::new(),
            revoked_participants: HashMap::new(),
            active_shares: HashSet::new(),
            share_permission: SharePermission::default(),
            sub_room_state: None,
        }
    }

    pub fn new_sfu(max_participants: u8, sfu_handle: SfuRoomHandle) -> Self {
        Self {
            room_type: RoomType::Sfu,
            sfu_handle: Some(sfu_handle),
            max_participants: max_participants.clamp(3, 6),
            participants: vec![],
            created_at: Instant::now(),
            token_issued_at: HashMap::new(),
            media_connected: HashSet::new(),
            revoked_participants: HashMap::new(),
            active_shares: HashSet::new(),
            share_permission: SharePermission::default(),
            sub_room_state: None,
        }
    }

    /// Add a participant to the revoked set (Req 4.3).
    /// Called on kick or leave to block MediaToken re-issuance within the TTL window.
    pub fn add_revoked_participant(&mut self, participant_id: &str, now: Instant) {
        self.revoked_participants
            .insert(participant_id.to_string(), now);
    }

    /// Check if a participant is in the revoked set (Req 4.2, 4.3).
    /// Also lazily cleans up entries older than `ttl_window` to bound memory usage.
    /// Returns true if the participant was revoked within the TTL window.
    pub fn is_participant_revoked(&mut self, participant_id: &str, ttl_window: Duration) -> bool {
        let now = Instant::now();
        // Lazy cleanup: remove entries older than ttl_window
        self.revoked_participants
            .retain(|_, revoked_at| now.duration_since(*revoked_at) < ttl_window);
        self.revoked_participants.contains_key(participant_id)
    }

    /// Record that a token was just issued for a participant.
    pub fn record_token_issued(&mut self, peer_id: &str) {
        self.token_issued_at
            .insert(peer_id.to_string(), Instant::now());
    }

    /// Mark a participant as having completed SFU media connection.
    pub fn mark_media_connected(&mut self, peer_id: &str) {
        self.media_connected.insert(peer_id.to_string());
    }

    /// Mark a participant as having lost SFU media connection.
    pub fn mark_media_disconnected(&mut self, peer_id: &str) {
        self.media_connected.remove(peer_id);
    }

    /// Returns peer IDs that need a preemptive token refresh:
    /// token was issued more than `refresh_after_secs` ago and media is not yet connected.
    pub fn peers_needing_token_refresh(&self, refresh_after_secs: u64) -> Vec<String> {
        self.token_issued_at
            .iter()
            .filter(|(peer_id, issued_at)| {
                !self.media_connected.contains(*peer_id)
                    && issued_at.elapsed().as_secs() >= refresh_after_secs
            })
            .map(|(peer_id, _)| peer_id.clone())
            .collect()
    }
}

/// Per-room state protected by its own RwLock.
/// Combines the peer list and room metadata into a single lockable unit.
pub struct RoomMembers {
    pub peers: Vec<PeerId>,
    pub info: RoomInfo,
}

/// Error returned when an operation targets a room that does not exist.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RoomNotFound;

impl std::fmt::Display for RoomNotFound {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "room not found")
    }
}

impl std::error::Error for RoomNotFound {}

/// In-memory room state with per-room lock granularity.
///
/// Lock ordering (always acquire in this order to prevent deadlocks):
///   1. `rooms` global map — read lock (to find per-room Arc)
///   2. per-room `RwLock<RoomMembers>` — write lock (capacity check + insert)
///   3. `peer_to_room` — write lock (update reverse index)
///
/// Rate limiter locks are NEVER held while acquiring room or peer_to_room locks.
pub struct InMemoryRoomState {
    /// Global lock protects the map itself (add/remove rooms).
    /// Per-room RwLock protects membership and metadata within a room.
    rooms: RwLock<HashMap<RoomId, Arc<RwLock<RoomMembers>>>>,
    /// Reverse index: peer → room. Protected by its own lock.
    peer_to_room: RwLock<HashMap<PeerId, RoomId>>,
}

impl Default for InMemoryRoomState {
    fn default() -> Self {
        Self {
            rooms: RwLock::new(HashMap::new()),
            peer_to_room: RwLock::new(HashMap::new()),
        }
    }
}

impl InMemoryRoomState {
    pub fn new() -> Self {
        Self::default()
    }

    /// Create room metadata. Returns false if the room already exists.
    pub fn create_room(&self, room_id: RoomId, info: RoomInfo) -> bool {
        let mut rooms = self.rooms.write().unwrap();
        if rooms.contains_key(&room_id) {
            return false;
        }
        let members = RoomMembers {
            peers: vec![],
            info,
        };
        rooms.insert(room_id, Arc::new(RwLock::new(members)));
        true
    }

    /// Remove a room only if it has zero peers. Used for rollback when a newly
    /// created room needs to be cleaned up before any peer was added.
    /// Returns true if the room was removed, false if it didn't exist or had peers.
    pub fn remove_empty_room(&self, room_id: &str) -> bool {
        let rooms_read = self.rooms.read().unwrap();
        let arc = match rooms_read.get(room_id) {
            Some(a) => a.clone(),
            None => return false,
        };
        let members = arc.read().unwrap();
        if !members.peers.is_empty() {
            return false;
        }
        drop(members);
        drop(rooms_read);
        self.rooms.write().unwrap().remove(room_id).is_some()
    }

    /// Get a snapshot of room metadata.
    pub fn get_room_info(&self, room_id: &str) -> Option<RoomInfo> {
        let rooms = self.rooms.read().unwrap();
        let arc = rooms.get(room_id)?;
        Some(arc.read().unwrap().info.clone())
    }

    /// Apply a mutation to room info atomically (under per-room write lock).
    pub fn update_room_info(&self, room_id: &str, f: impl FnOnce(&mut RoomInfo)) {
        let rooms = self.rooms.read().unwrap();
        if let Some(arc) = rooms.get(room_id) {
            let mut members = arc.write().unwrap();
            f(&mut members.info);
        }
    }

    /// Add a participant to the room's revoked set (Req 4.3).
    /// No-op if the room no longer exists (already destroyed).
    pub fn add_revoked_participant(&self, room_id: &str, participant_id: &str) {
        self.update_room_info(room_id, |info| {
            info.add_revoked_participant(participant_id, Instant::now());
        });
    }

    /// Check if a participant is revoked in a room (Req 4.2).
    /// Returns false if the room doesn't exist (no revocation possible).
    pub fn is_participant_revoked(
        &self,
        room_id: &str,
        participant_id: &str,
        ttl_window: Duration,
    ) -> bool {
        self.with_room_write(room_id, |members| {
            members
                .info
                .is_participant_revoked(participant_id, ttl_window)
        })
        .unwrap_or(false)
    }

    /// Current participant count for a room.
    pub fn peer_count(&self, room_id: &str) -> usize {
        let rooms = self.rooms.read().unwrap();
        rooms
            .get(room_id)
            .map(|arc| arc.read().unwrap().peers.len())
            .unwrap_or(0)
    }

    /// Execute a closure while holding the per-room write lock.
    /// Returns Err(RoomNotFound) if the room does not exist.
    pub fn with_room_write<F, R>(&self, room_id: &str, f: F) -> Result<R, RoomNotFound>
    where
        F: FnOnce(&mut RoomMembers) -> R,
    {
        let arc = {
            let rooms = self.rooms.read().unwrap();
            rooms.get(room_id).cloned().ok_or(RoomNotFound)?
        };
        let mut members = arc.write().unwrap();
        Ok(f(&mut members))
    }

    /// Atomic capacity-checked join.
    ///
    /// Lock ordering:
    ///   1. Acquire global `rooms` read lock → get Arc<RwLock<RoomMembers>>
    ///   2. Release global lock
    ///   3. Acquire per-room write lock
    ///   4. Check capacity → return RoomFull if at limit
    ///   5. Insert peer
    ///   6. Call `f()` (e.g. invite_store.validate_and_consume) — still under per-room lock
    ///   7. Release per-room lock
    ///   8. Acquire peer_to_room write lock → update reverse index
    ///
    /// Returns Ok(new_peer_count) or Err(JoinRejectionReason::RoomFull).
    pub fn try_add_peer_with<F>(
        &self,
        peer_id: PeerId,
        room_id: &RoomId,
        f: F,
    ) -> Result<usize, JoinRejectionReason>
    where
        F: FnOnce() -> Result<(), JoinRejectionReason>,
    {
        // Step 1+2: get Arc, release global lock
        let arc = {
            let rooms = self.rooms.read().unwrap();
            rooms
                .get(room_id)
                .cloned()
                .ok_or(JoinRejectionReason::RoomFull)?
        };

        // Step 3–6: per-room write lock
        let new_count = {
            let mut members = arc.write().unwrap();
            let max = members.info.max_participants as usize;
            // Already in room counts as success (idempotent re-join)
            if !members.peers.contains(&peer_id) {
                if members.peers.len() >= max {
                    return Err(JoinRejectionReason::RoomFull);
                }
                // Run the closure (e.g. validate_and_consume) BEFORE inserting
                // the peer so that invite exhaustion is checked atomically
                // under the per-room write lock.
                f()?;
                members.peers.push(peer_id.clone());
            }
            members.peers.len()
        };

        // Step 7+8: update reverse index
        {
            let mut ptr = self.peer_to_room.write().unwrap();
            ptr.insert(peer_id, room_id.clone());
        }

        Ok(new_count)
    }

    /// Add a peer to a room. Creates the room entry if it doesn't exist yet.
    /// Delegates to try_add_peer_with with a no-op closure when the room exists,
    /// or falls back to direct insertion for rooms not yet in the map (P2P path
    /// where create_room is called separately).
    pub fn add_peer(&self, peer_id: PeerId, room_id: RoomId) {
        // Ensure the room entry exists (P2P rooms may not have been created yet)
        {
            let mut rooms = self.rooms.write().unwrap();
            rooms.entry(room_id.clone()).or_insert_with(|| {
                Arc::new(RwLock::new(RoomMembers {
                    peers: vec![],
                    info: RoomInfo::new_p2p(),
                }))
            });
        }

        // Now do the actual insert under per-room lock
        let arc = {
            let rooms = self.rooms.read().unwrap();
            rooms.get(&room_id).cloned().unwrap()
        };

        {
            let mut members = arc.write().unwrap();
            // Handle peer moving between rooms: remove from old room
            let old_room = self.peer_to_room.read().unwrap().get(&peer_id).cloned();
            if let Some(ref old_id) = old_room
                && old_id != &room_id
            {
                // Need to clean up old room — drop per-room lock first to avoid
                // potential ordering issues, then re-acquire
                drop(members);
                self.remove_peer_from_room_only(&peer_id, old_id);
                let rooms = self.rooms.read().unwrap();
                let arc2 = rooms.get(&room_id).cloned().unwrap();
                drop(rooms);
                let mut m2 = arc2.write().unwrap();
                if !m2.peers.contains(&peer_id) {
                    m2.peers.push(peer_id.clone());
                }
                drop(m2);
                self.peer_to_room.write().unwrap().insert(peer_id, room_id);
                return;
            }
            if !members.peers.contains(&peer_id) {
                members.peers.push(peer_id.clone());
            }
        }

        self.peer_to_room.write().unwrap().insert(peer_id, room_id);
    }

    /// Internal helper: remove a peer from a room's peer list only (no peer_to_room update).
    fn remove_peer_from_room_only(&self, peer_id: &str, room_id: &str) {
        let arc = {
            let rooms = self.rooms.read().unwrap();
            rooms.get(room_id).cloned()
        };
        if let Some(arc) = arc {
            let mut members = arc.write().unwrap();
            members.peers.retain(|p| p != peer_id);
            let is_empty = members.peers.is_empty();
            drop(members);
            if is_empty {
                self.rooms.write().unwrap().remove(room_id);
            }
        }
    }

    fn remove_peer_internal(&self, peer_id: &str, remove_empty_room: bool) {
        // Step 1: remove from peer_to_room to get the room_id
        let room_id = self.peer_to_room.write().unwrap().remove(peer_id);

        if let Some(room_id) = room_id {
            // Step 2: remove from per-room peers list
            let arc = {
                let rooms = self.rooms.read().unwrap();
                rooms.get(&room_id).cloned()
            };
            if let Some(arc) = arc {
                let mut members = arc.write().unwrap();
                members.peers.retain(|p| p != peer_id);
                let is_empty = members.peers.is_empty();
                drop(members);
                // Step 3: if room is now empty, remove it from the global map
                if remove_empty_room && is_empty {
                    self.rooms.write().unwrap().remove(&room_id);
                }
            }
        }
    }

    pub fn remove_peer(&self, peer_id: &str) {
        self.remove_peer_internal(peer_id, true);
    }

    /// Remove a peer from the reverse index and room membership without
    /// destroying the room if it becomes empty. Used by temporary teardown
    /// paths such as stale-session eviction during rejoin.
    pub fn remove_peer_preserve_room(&self, peer_id: &str) {
        self.remove_peer_internal(peer_id, false);
    }

    /// Clone current room snapshot for debug/API responses.
    pub fn snapshot_rooms(&self) -> HashMap<RoomId, Vec<PeerId>> {
        let rooms = self.rooms.read().unwrap();
        rooms
            .iter()
            .map(|(room_id, arc)| {
                let members = arc.read().unwrap();
                (room_id.clone(), members.peers.clone())
            })
            .collect()
    }

    /// Number of rooms that currently have at least one participant.
    pub fn active_room_count(&self) -> usize {
        self.rooms.read().unwrap().len()
    }

    /// Total number of participants across all active rooms.
    pub fn total_participant_count(&self) -> usize {
        let rooms = self.rooms.read().unwrap();
        rooms
            .values()
            .map(|arc| arc.read().unwrap().peers.len())
            .sum()
    }

    /// Returns all current room IDs (snapshot).
    pub fn snapshot_room_ids(&self) -> Vec<RoomId> {
        self.rooms.read().unwrap().keys().cloned().collect()
    }

    /// For each SFU room, returns (room_id, sfu_handle, peers_needing_refresh).
    pub fn rooms_needing_token_refresh(
        &self,
        refresh_after_secs: u64,
    ) -> Vec<(RoomId, SfuRoomHandle, Vec<String>)> {
        let rooms = self.rooms.read().unwrap();
        rooms
            .iter()
            .filter_map(|(room_id, arc)| {
                let members = arc.read().unwrap();
                if members.info.room_type != RoomType::Sfu {
                    return None;
                }
                let handle = members.info.sfu_handle.clone()?;
                let peers = members.info.peers_needing_token_refresh(refresh_after_secs);
                if peers.is_empty() {
                    None
                } else {
                    Some((room_id.clone(), handle, peers))
                }
            })
            .collect()
    }
}

impl RoomState for InMemoryRoomState {
    fn get_room_for_peer(&self, peer_id: &str) -> Option<RoomId> {
        self.peer_to_room.read().unwrap().get(peer_id).cloned()
    }

    fn get_peers_in_room(&self, room_id: &RoomId) -> Vec<PeerId> {
        let rooms = self.rooms.read().unwrap();
        rooms
            .get(room_id)
            .map(|arc| arc.read().unwrap().peers.clone())
            .unwrap_or_default()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use proptest::prelude::*;

    #[test]
    fn add_and_remove_peer_updates_both_indexes() {
        let state = InMemoryRoomState::new();
        state.add_peer("peer-a".to_string(), "room-1".to_string());

        assert_eq!(
            state.get_room_for_peer("peer-a"),
            Some("room-1".to_string())
        );
        assert_eq!(
            state.get_peers_in_room(&"room-1".to_string()),
            vec!["peer-a".to_string()]
        );

        state.remove_peer("peer-a");
        assert_eq!(state.get_room_for_peer("peer-a"), None);
        assert!(state.get_peers_in_room(&"room-1".to_string()).is_empty());
    }

    #[test]
    fn moving_peer_between_rooms_cleans_previous_room_membership() {
        let state = InMemoryRoomState::new();
        state.add_peer("peer-a".to_string(), "room-1".to_string());
        state.add_peer("peer-a".to_string(), "room-2".to_string());

        assert_eq!(
            state.get_room_for_peer("peer-a"),
            Some("room-2".to_string())
        );
        assert!(state.get_peers_in_room(&"room-1".to_string()).is_empty());
        assert_eq!(
            state.get_peers_in_room(&"room-2".to_string()),
            vec!["peer-a".to_string()]
        );
    }

    #[test]
    fn empty_room_is_removed_from_snapshot() {
        let state = InMemoryRoomState::new();
        state.add_peer("peer-a".to_string(), "room-1".to_string());
        state.remove_peer("peer-a");

        let rooms = state.snapshot_rooms();
        assert!(!rooms.contains_key("room-1"));
    }

    #[test]
    fn create_room_returns_false_if_already_exists() {
        let state = InMemoryRoomState::new();
        let info = RoomInfo::new_sfu(4, SfuRoomHandle("handle-1".to_string()));
        assert!(state.create_room("room-1".to_string(), info.clone()));
        let info2 = RoomInfo::new_sfu(4, SfuRoomHandle("handle-2".to_string()));
        assert!(!state.create_room("room-1".to_string(), info2));
    }

    #[test]
    fn peer_count_reflects_current_members() {
        let state = InMemoryRoomState::new();
        assert_eq!(state.peer_count("room-1"), 0);
        state.add_peer("peer-a".to_string(), "room-1".to_string());
        assert_eq!(state.peer_count("room-1"), 1);
        state.add_peer("peer-b".to_string(), "room-1".to_string());
        assert_eq!(state.peer_count("room-1"), 2);
        state.remove_peer("peer-a");
        assert_eq!(state.peer_count("room-1"), 1);
    }

    #[test]
    fn active_room_count_and_total_participant_count() {
        let state = InMemoryRoomState::new();
        assert_eq!(state.active_room_count(), 0);
        assert_eq!(state.total_participant_count(), 0);

        state.add_peer("peer-a".to_string(), "room-1".to_string());
        assert_eq!(state.active_room_count(), 1);
        assert_eq!(state.total_participant_count(), 1);

        state.add_peer("peer-b".to_string(), "room-1".to_string());
        state.add_peer("peer-c".to_string(), "room-2".to_string());
        assert_eq!(state.active_room_count(), 2);
        assert_eq!(state.total_participant_count(), 3);

        state.remove_peer("peer-a");
        state.remove_peer("peer-b");
        // room-1 is now empty and removed
        assert_eq!(state.active_room_count(), 1);
        assert_eq!(state.total_participant_count(), 1);
    }

    #[test]
    fn room_info_cleaned_up_when_last_peer_leaves() {
        let state = InMemoryRoomState::new();
        let info = RoomInfo::new_sfu(4, SfuRoomHandle("h".to_string()));
        state.create_room("room-1".to_string(), info);
        state.add_peer("peer-a".to_string(), "room-1".to_string());
        state.remove_peer("peer-a");
        assert!(state.get_room_info("room-1").is_none());
    }

    #[test]
    fn try_add_peer_with_respects_capacity() {
        let state = InMemoryRoomState::new();
        let info = RoomInfo::new_p2p(); // max 2
        state.create_room("room-1".to_string(), info);

        let r1 = state.try_add_peer_with("peer-a".to_string(), &"room-1".to_string(), || Ok(()));
        assert_eq!(r1, Ok(1));
        let r2 = state.try_add_peer_with("peer-b".to_string(), &"room-1".to_string(), || Ok(()));
        assert_eq!(r2, Ok(2));
        // 3rd peer should be rejected
        let r3 = state.try_add_peer_with("peer-c".to_string(), &"room-1".to_string(), || Ok(()));
        assert_eq!(r3, Err(JoinRejectionReason::RoomFull));
        assert_eq!(state.peer_count("room-1"), 2);
    }

    #[test]
    fn try_add_peer_with_calls_closure_on_success() {
        let state = InMemoryRoomState::new();
        state.create_room("room-1".to_string(), RoomInfo::new_p2p());

        let mut called = false;
        let result = state.try_add_peer_with("peer-a".to_string(), &"room-1".to_string(), || {
            called = true;
            Ok(())
        });
        assert_eq!(result, Ok(1));
        assert!(called, "closure must be called on successful join");
    }

    #[test]
    fn try_add_peer_with_does_not_call_closure_when_full() {
        let state = InMemoryRoomState::new();
        state.create_room("room-1".to_string(), RoomInfo::new_p2p());
        state
            .try_add_peer_with("peer-a".to_string(), &"room-1".to_string(), || Ok(()))
            .unwrap();
        state
            .try_add_peer_with("peer-b".to_string(), &"room-1".to_string(), || Ok(()))
            .unwrap();

        let mut called = false;
        let result = state.try_add_peer_with("peer-c".to_string(), &"room-1".to_string(), || {
            called = true;
            Ok(())
        });
        assert_eq!(result, Err(JoinRejectionReason::RoomFull));
        assert!(!called, "closure must NOT be called when room is full");
    }

    #[test]
    fn try_add_peer_with_nonexistent_room_returns_room_full() {
        let state = InMemoryRoomState::new();
        let result =
            state.try_add_peer_with("peer-a".to_string(), &"no-such-room".to_string(), || Ok(()));
        assert_eq!(result, Err(JoinRejectionReason::RoomFull));
    }

    #[test]
    fn with_room_write_mutates_info() {
        let state = InMemoryRoomState::new();
        state.create_room("room-1".to_string(), RoomInfo::new_p2p());

        let result = state.with_room_write("room-1", |members| {
            members.info.max_participants = 6;
            members.info.max_participants
        });
        assert_eq!(result, Ok(6));
        assert_eq!(state.get_room_info("room-1").unwrap().max_participants, 6);
    }

    #[test]
    fn with_room_write_returns_not_found_for_missing_room() {
        let state = InMemoryRoomState::new();
        let result = state.with_room_write("ghost", |_| ());
        assert_eq!(result, Err(RoomNotFound));
    }

    #[test]
    fn remove_peer_is_idempotent() {
        let state = InMemoryRoomState::new();
        state.add_peer("peer-a".to_string(), "room-1".to_string());
        state.remove_peer("peer-a");
        // Second removal should be a no-op
        state.remove_peer("peer-a");
        assert_eq!(state.peer_count("room-1"), 0);
        assert_eq!(state.get_room_for_peer("peer-a"), None);
    }

    #[test]
    fn remove_nonmember_is_noop() {
        let state = InMemoryRoomState::new();
        state.add_peer("peer-a".to_string(), "room-1".to_string());
        state.remove_peer("ghost-peer");
        assert_eq!(state.peer_count("room-1"), 1);
    }

    #[test]
    fn sub_room_state_starts_with_room_one() {
        let sub_rooms = SubRoomState::new("room-1");
        assert_eq!(sub_rooms.rooms.len(), 1);
        assert_eq!(sub_rooms.rooms[0].sub_room_id, "room-1");
        assert_eq!(sub_rooms.rooms[0].room_number, 1);
        assert!(sub_rooms.rooms[0].is_default);
        assert!(sub_rooms.rooms[0].participant_ids.is_empty());
        assert_eq!(sub_rooms.rooms[0].delete_at, None);
        assert!(sub_rooms.participant_assignments.is_empty());
        assert!(sub_rooms.membership_sources.is_empty());
    }

    #[test]
    fn room_info_defaults_without_sub_rooms() {
        let p2p = RoomInfo::new_p2p();
        let sfu = RoomInfo::new_sfu(6, SfuRoomHandle("test-handle".to_string()));
        assert_eq!(p2p.sub_room_state, None);
        assert_eq!(sfu.sub_room_state, None);
    }

    // --- Property 11: InMemoryRoomState supports 1–6 peers ---
    // Feature: sfu-multi-party-voice, Property 11: State supports 1–6 peers
    // Validates: Requirements 5.5, 10.6

    proptest! {
        #![proptest_config(ProptestConfig::with_cases(100))]

        #[test]
        fn prop_state_supports_1_to_6_peers(
            peer_count in 1usize..=6usize,
        ) {
            let state = InMemoryRoomState::new();
            let room_id = "test-room".to_string();

            let peer_ids: Vec<String> = (0..peer_count)
                .map(|i| format!("peer-{i}"))
                .collect();

            for peer_id in &peer_ids {
                state.add_peer(peer_id.clone(), room_id.clone());
            }

            let peers_in_room = state.get_peers_in_room(&room_id);
            prop_assert_eq!(peers_in_room.len(), peer_count, "all peers should be in room");

            for peer_id in &peer_ids {
                prop_assert!(
                    peers_in_room.contains(peer_id),
                    "peer {peer_id} should be in room"
                );
            }

            prop_assert_eq!(state.peer_count(&room_id), peer_count);

            if peer_count > 0 {
                let removed = &peer_ids[0];
                state.remove_peer(removed);
                prop_assert_eq!(
                    state.peer_count(&room_id),
                    peer_count - 1,
                    "count should decrease by 1 after removal"
                );
                prop_assert!(
                    !state.get_peers_in_room(&room_id).contains(removed),
                    "removed peer should not be in room"
                );
            }
        }

        #[test]
        fn prop_p2p_rooms_behave_identically_to_phase2(
            peer_a in "[a-z]{4,8}",
            peer_b in "[a-z]{4,8}",
            room_id in "[a-z]{4,8}",
        ) {
            prop_assume!(peer_a != peer_b);

            let state = InMemoryRoomState::new();
            state.add_peer(peer_a.clone(), room_id.clone());
            state.add_peer(peer_b.clone(), room_id.clone());

            let peers = state.get_peers_in_room(&room_id);
            prop_assert_eq!(peers.len(), 2);
            prop_assert!(peers.contains(&peer_a));
            prop_assert!(peers.contains(&peer_b));

            prop_assert_eq!(state.get_room_for_peer(&peer_a), Some(room_id.clone()));
            prop_assert_eq!(state.get_room_for_peer(&peer_b), Some(room_id.clone()));

            state.remove_peer(&peer_a);
            prop_assert_eq!(state.peer_count(&room_id), 1);
            state.remove_peer(&peer_b);
            prop_assert_eq!(state.peer_count(&room_id), 0);
        }
    }

    // --- Property 13: Atomic join capacity enforcement ---
    // Feature: invite-code-hardening, Property 13: Atomic join capacity enforcement
    // Validates: Requirements 8.1, 8.2, 8.4

    proptest! {
        #![proptest_config(ProptestConfig::with_cases(200))]

        #[test]
        fn prop_p13_atomic_join_capacity_enforcement(
            // capacity C in 2..=6; current member count M in 0..=C
            capacity in 2u8..=6u8,
            pre_fill in 0usize..=6usize,
        ) {
            // Clamp pre_fill to [0, capacity] so M is always valid
            let capacity_usize = capacity as usize;
            let m = pre_fill.min(capacity_usize);

            let state = InMemoryRoomState::new();
            let room_id = "room-cap".to_string();

            // Use new_sfu for capacity > 2, new_p2p for capacity == 2
            let info = if capacity == 2 {
                RoomInfo::new_p2p()
            } else {
                RoomInfo::new_sfu(capacity, SfuRoomHandle("test-handle".to_string()))
            };
            state.create_room(room_id.clone(), info);

            // Pre-fill M peers
            for i in 0..m {
                let peer_id = format!("pre-peer-{i}");
                state.try_add_peer_with(peer_id, &room_id, || Ok(())).unwrap();
            }

            prop_assert_eq!(state.peer_count(&room_id), m);

            if m < capacity_usize {
                // Should succeed and return M+1
                let result = state.try_add_peer_with(
                    format!("new-peer-{m}"),
                    &room_id,
                    || Ok(()),
                );
                prop_assert_eq!(result, Ok(m + 1), "join should succeed when M < C");
                prop_assert_eq!(state.peer_count(&room_id), m + 1);
            } else {
                // M == C: should fail with RoomFull, state unchanged
                let result = state.try_add_peer_with(
                    "overflow-peer".to_string(),
                    &room_id,
                    || Ok(()),
                );
                prop_assert_eq!(result, Err(JoinRejectionReason::RoomFull), "join should fail when M == C");
                prop_assert_eq!(state.peer_count(&room_id), capacity_usize, "state must be unchanged on RoomFull");
            }
        }
    }

    // --- Property 15: Peer removal decrements count atomically ---
    // Feature: invite-code-hardening, Property 15: Peer removal decrements count atomically
    // Validates: Requirements 9.1, 9.3

    proptest! {
        #![proptest_config(ProptestConfig::with_cases(200))]

        #[test]
        fn prop_p15_peer_removal_decrements_count_atomically(
            n in 1usize..=6usize,
            remove_idx in 0usize..6usize,
        ) {
            // remove_idx is clamped to [0, n-1]
            let remove_idx = remove_idx % n;

            let state = InMemoryRoomState::new();
            let room_id = "room-remove".to_string();
            state.create_room(room_id.clone(), RoomInfo::new_sfu(6, SfuRoomHandle("test-handle".to_string())));

            let peer_ids: Vec<String> = (0..n).map(|i| format!("peer-{i}")).collect();
            for peer_id in &peer_ids {
                state.add_peer(peer_id.clone(), room_id.clone());
            }

            prop_assert_eq!(state.peer_count(&room_id), n);

            let to_remove = &peer_ids[remove_idx];
            state.remove_peer(to_remove);

            // Count decrements by exactly 1
            prop_assert_eq!(state.peer_count(&room_id), n - 1, "count must be N-1 after removal");

            // Removed peer no longer in member list
            let members = state.get_peers_in_room(&room_id);
            prop_assert!(
                !members.contains(to_remove),
                "removed peer must not appear in member list"
            );
        }
    }

    // --- Property 16: Idempotent peer removal ---
    // Feature: invite-code-hardening, Property 16: Idempotent peer removal
    // Validates: Requirements 9.2

    proptest! {
        #![proptest_config(ProptestConfig::with_cases(200))]

        #[test]
        fn prop_p16_idempotent_peer_removal(
            n in 1usize..=6usize,
            remove_idx in 0usize..6usize,
        ) {
            let remove_idx = remove_idx % n;

            let state = InMemoryRoomState::new();
            let room_id = "room-idem".to_string();
            state.create_room(room_id.clone(), RoomInfo::new_sfu(6, SfuRoomHandle("test-handle".to_string())));

            let peer_ids: Vec<String> = (0..n).map(|i| format!("peer-{i}")).collect();
            for peer_id in &peer_ids {
                state.add_peer(peer_id.clone(), room_id.clone());
            }

            let to_remove = &peer_ids[remove_idx];

            // First removal
            state.remove_peer(to_remove);
            let count_after_first = state.peer_count(&room_id);

            // Second removal — must be a no-op
            state.remove_peer(to_remove);
            let count_after_second = state.peer_count(&room_id);

            prop_assert_eq!(
                count_after_first, count_after_second,
                "second remove_peer must be a no-op"
            );
            prop_assert_eq!(
                count_after_first, n - 1,
                "member count must decrease by exactly 1 total"
            );
        }
    }

    // --- Property 17: Leave for non-member is no-op ---
    // Feature: invite-code-hardening, Property 17: Leave for non-member is no-op
    // Validates: Requirements 9.4

    proptest! {
        #![proptest_config(ProptestConfig::with_cases(200))]

        #[test]
        fn prop_p17_leave_for_nonmember_is_noop(
            room_sizes in prop::collection::vec(1usize..=6usize, 1..=3),
        ) {
            let state = InMemoryRoomState::new();

            // Set up rooms with known members
            for (room_idx, &size) in room_sizes.iter().enumerate() {
                let room_id = format!("room-{room_idx}");
                state.create_room(
                    room_id.clone(),
                    RoomInfo::new_sfu(6, SfuRoomHandle("test-handle".to_string())),
                );
                for peer_idx in 0..size {
                    state.add_peer(format!("r{room_idx}-p{peer_idx}"), room_id.clone());
                }
            }

            // Snapshot counts before
            let counts_before: Vec<usize> = (0..room_sizes.len())
                .map(|i| state.peer_count(&format!("room-{i}")))
                .collect();

            // Remove a peer that is not in any room
            state.remove_peer("ghost-peer-xyz");

            // All room counts must be unchanged
            for (room_idx, &before) in counts_before.iter().enumerate() {
                let after = state.peer_count(&format!("room-{room_idx}"));
                prop_assert_eq!(
                    after, before,
                    "room count must not change when non-member leaves (room index {})", room_idx
                );
            }
        }
    }

    // --- Property 13: Metrics track active rooms and participants ---
    // Feature: sfu-multi-party-voice, Property 13: Metrics tracking
    // Validates: Requirements 7.4

    proptest! {
        #![proptest_config(ProptestConfig::with_cases(200))]

        #[test]
        fn prop_metrics_track_active_rooms_and_participants(
            room_sizes in prop::collection::vec(1usize..=6usize, 1..=4),
            remove_counts in prop::collection::vec(0usize..=6usize, 1..=4),
        ) {
            let state = InMemoryRoomState::new();

            for (room_idx, &size) in room_sizes.iter().enumerate() {
                let room_id = format!("room-{room_idx}");
                for peer_idx in 0..size {
                    state.add_peer(format!("r{room_idx}-p{peer_idx}"), room_id.clone());
                }
            }

            let snapshot = state.snapshot_rooms();
            let expected_rooms = snapshot.len();
            let expected_total: usize = snapshot.values().map(|p| p.len()).sum();
            prop_assert_eq!(state.active_room_count(), expected_rooms);
            prop_assert_eq!(state.total_participant_count(), expected_total);

            for (room_idx, &remove_count) in remove_counts.iter().enumerate() {
                if room_idx >= room_sizes.len() { break; }
                let room_size = room_sizes[room_idx];
                let to_remove = remove_count.min(room_size);

                for peer_idx in 0..to_remove {
                    state.remove_peer(&format!("r{room_idx}-p{peer_idx}"));
                }

                let snap = state.snapshot_rooms();
                let non_empty = snap.values().filter(|p| !p.is_empty()).count();
                prop_assert_eq!(
                    state.active_room_count(), non_empty,
                    "active_room_count must equal non-empty rooms"
                );

                let snap_total: usize = snap.values().map(|p| p.len()).sum();
                prop_assert_eq!(
                    state.total_participant_count(), snap_total,
                    "total_participant_count must equal sum across snapshot"
                );
            }
        }
    }
}
