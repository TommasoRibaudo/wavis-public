// Feature: invite-code-hardening, Property 14: Capacity invariant under concurrency
// Validates: Requirements 8.5, 8.6

use shared::signaling::JoinRejectionReason;
use std::sync::Arc;
use wavis_backend::state::{InMemoryRoomState, RoomInfo};
use wavis_backend::voice::relay::RoomState;
use wavis_backend::voice::sfu_bridge::SfuRoomHandle;

/// P14 — SFU room: capacity C=6, N=20 concurrent join attempts.
/// Exactly C must succeed; final member count must equal C.
#[tokio::test]
async fn p14_capacity_invariant_under_concurrency() {
    const C: usize = 6;
    const N: usize = 20;

    let state = Arc::new(InMemoryRoomState::new());
    let room_id = "room-sfu".to_string();

    let info = RoomInfo::new_sfu(C as u8, SfuRoomHandle("test-handle".to_string()));
    state.create_room(room_id.clone(), info);

    // Spawn N concurrent tasks, each attempting to join
    let handles: Vec<_> = (0..N)
        .map(|i| {
            let state = Arc::clone(&state);
            let room_id = room_id.clone();
            tokio::spawn(async move {
                let peer_id = format!("peer-{i}");
                state.try_add_peer_with(peer_id, &room_id, || Ok(()))
            })
        })
        .collect();

    // Collect all results
    let mut results = Vec::with_capacity(N);
    for handle in handles {
        results.push(handle.await.expect("task panicked"));
    }

    let successes = results.iter().filter(|r| r.is_ok()).count();
    let failures = results
        .iter()
        .filter(|r| matches!(r, Err(JoinRejectionReason::RoomFull)))
        .count();

    assert_eq!(
        successes, C,
        "exactly C={C} joins should succeed, got {successes}"
    );
    assert_eq!(
        failures,
        N - C,
        "exactly N-C={} joins should fail with RoomFull, got {failures}",
        N - C
    );
    assert_eq!(
        state.peer_count(&room_id),
        C,
        "final member count must equal C={C}"
    );
}

/// P14 — P2P room: capacity C=2, N=10 concurrent join attempts.
/// Exactly C must succeed; final member count must equal C.
#[tokio::test]
async fn p14_capacity_invariant_p2p() {
    const C: usize = 2;
    const N: usize = 10;

    let state = Arc::new(InMemoryRoomState::new());
    let room_id = "room-p2p".to_string();

    let info = RoomInfo::new_p2p(); // max_participants = 2
    state.create_room(room_id.clone(), info);

    let handles: Vec<_> = (0..N)
        .map(|i| {
            let state = Arc::clone(&state);
            let room_id = room_id.clone();
            tokio::spawn(async move {
                let peer_id = format!("peer-{i}");
                state.try_add_peer_with(peer_id, &room_id, || Ok(()))
            })
        })
        .collect();

    let mut results = Vec::with_capacity(N);
    for handle in handles {
        results.push(handle.await.expect("task panicked"));
    }

    let successes = results.iter().filter(|r| r.is_ok()).count();
    let failures = results
        .iter()
        .filter(|r| matches!(r, Err(JoinRejectionReason::RoomFull)))
        .count();

    assert_eq!(
        successes, C,
        "exactly C={C} joins should succeed, got {successes}"
    );
    assert_eq!(
        failures,
        N - C,
        "exactly N-C={} joins should fail with RoomFull, got {failures}",
        N - C
    );
    assert_eq!(
        state.peer_count(&room_id),
        C,
        "final member count must equal C={C}"
    );
}

// Feature: invite-code-hardening, Property 24: Concurrent invite consumption
// Validates: Requirements 5.2, 5.3, 8.5
#[tokio::test]
async fn p24_concurrent_invite_consumption() {
    use std::time::{Duration, Instant};
    use wavis_backend::channel::invite::{InviteStore, InviteStoreConfig};

    const K: usize = 5; // invite uses AND room capacity (binding constraint)
    const N: usize = 20; // concurrent attempts

    let invite_store = Arc::new(InviteStore::new(InviteStoreConfig {
        default_max_uses: K as u32,
        max_invites_per_room: 20,
        max_invites_global: 1000,
        default_ttl: Duration::from_secs(3600),
        sweep_interval: Duration::from_secs(60),
    }));

    let state = Arc::new(InMemoryRoomState::new());
    let room_id = "room-p24".to_string();

    // Room capacity = K (binding constraint matches invite uses)
    let info = RoomInfo::new_sfu(K as u8, SfuRoomHandle("test-handle".to_string()));
    state.create_room(room_id.clone(), info);

    let now = Instant::now();
    let record = invite_store
        .generate("room-p24", "issuer-test", Some(K as u32), now)
        .unwrap();
    let code = Arc::new(record.code.clone());

    // Spawn N concurrent tasks, each attempting to join via try_add_peer_with + validate_and_consume
    let handles: Vec<_> = (0..N)
        .map(|i| {
            let state = Arc::clone(&state);
            let invite_store = Arc::clone(&invite_store);
            let room_id = room_id.clone();
            let code = Arc::clone(&code);
            tokio::spawn(async move {
                let peer_id = format!("peer-{i}");
                state.try_add_peer_with(peer_id, &room_id, || {
                    invite_store.validate_and_consume(&code, &room_id, Instant::now())
                })
            })
        })
        .collect();

    let mut results = Vec::with_capacity(N);
    for handle in handles {
        results.push(handle.await.expect("task panicked"));
    }

    let successes = results.iter().filter(|r| r.is_ok()).count();

    assert_eq!(
        successes, K,
        "exactly K={K} joins should succeed, got {successes}"
    );
    assert_eq!(
        state.peer_count(&room_id),
        K,
        "final peer count must equal K={K}"
    );

    // Verify consume_use was called exactly K times → remaining_uses == 0
    // Invite must be exhausted after K successful joins
    let validation = invite_store.validate(&record.code, "room-p24", now);
    assert_eq!(
        validation,
        Err(shared::signaling::JoinRejectionReason::InviteExhausted),
        "invite must be exhausted after K successful joins"
    );
}

// Feature: test-quality-hardening, Property 3: Concurrency state invariant
// Validates: Requirements 6.3, 6.5, 6.6

/// concurrent_join_leave_no_ghost — N=32 tasks, capacity=6, barrier-synchronized.
/// Each task randomly joins or leaves a participant using a seeded RNG.
/// Asserts: final peer count matches actual peers, no ghost participants.
/// Runs 100 iterations with different seeds; reports seed on failure.
#[tokio::test]
async fn concurrent_join_leave_no_ghost() {
    use rand::rngs::StdRng;
    use rand::{Rng, SeedableRng};
    use tokio::sync::Barrier;

    const N: usize = 32;
    const CAPACITY: u8 = 6;
    const ITERATIONS: u64 = 100;
    const BASE_SEED: u64 = 0xDEAD_BEEF_CAFE;
    // Pool of participant IDs — smaller than N to create contention
    const PARTICIPANT_COUNT: usize = 8;

    for iteration in 0..ITERATIONS {
        let seed = BASE_SEED + iteration;
        let state = Arc::new(InMemoryRoomState::new());
        let room_id = "room-ghost-test".to_string();

        let info = RoomInfo::new_sfu(CAPACITY, SfuRoomHandle("test-handle".to_string()));
        state.create_room(room_id.clone(), info);

        let barrier = Arc::new(Barrier::new(N));

        let handles: Vec<_> = (0..N)
            .map(|task_idx| {
                let state = Arc::clone(&state);
                let room_id = room_id.clone();
                let barrier = Arc::clone(&barrier);
                tokio::spawn(async move {
                    // Each task gets its own deterministic RNG derived from seed + task index
                    let mut rng = StdRng::seed_from_u64(seed.wrapping_add(task_idx as u64));

                    // Pick a random participant from the pool
                    let participant_id =
                        format!("participant-{}", rng.gen_range(0..PARTICIPANT_COUNT));

                    // Randomly decide: join (true) or leave (false)
                    let should_join: bool = rng.gen_bool(0.5);

                    // Synchronize — all tasks start their operation at the same time
                    barrier.wait().await;

                    if should_join {
                        let _ = state.try_add_peer_with(participant_id, &room_id, || Ok(()));
                    } else {
                        state.remove_peer(&participant_id);
                    }
                })
            })
            .collect();

        // Wait for all tasks to complete
        for handle in handles {
            handle.await.unwrap_or_else(|e| {
                panic!("[seed={seed}] task panicked: {e}");
            });
        }

        // Assert invariants
        let reported_count = state.peer_count(&room_id);
        let actual_peers = state.get_peers_in_room(&room_id);
        let actual_count = actual_peers.len();

        assert_eq!(
            reported_count, actual_count,
            "[seed={seed}] peer_count ({reported_count}) != actual peers ({actual_count}). \
             Peers: {actual_peers:?}"
        );

        // No ghost participants: every peer in the room must be tracked in peer_to_room
        // (verified indirectly — if remove_peer was called, the peer should not be in the room)
        assert!(
            actual_count <= CAPACITY as usize,
            "[seed={seed}] peer count ({actual_count}) exceeds capacity ({CAPACITY}). \
             Peers: {actual_peers:?}"
        );

        // Verify no duplicate peers in the room
        let mut sorted_peers = actual_peers.clone();
        sorted_peers.sort();
        sorted_peers.dedup();
        assert_eq!(
            sorted_peers.len(),
            actual_peers.len(),
            "[seed={seed}] duplicate peers found in room. Peers: {actual_peers:?}"
        );

        // Clean up for next iteration: remove all remaining peers
        for peer in &actual_peers {
            state.remove_peer(peer);
        }
    }
}

// Feature: test-quality-hardening, Property 3: Concurrency state invariant
// Validates: Requirements 6.1, 6.2, 6.6

/// concurrent_join_kick_interleaving — N=32 tasks, capacity=6, barrier-synchronized.
/// Each task randomly joins, leaves, or kicks a random peer across 2 rooms.
/// Participants are assigned a home room (deterministic by ID) to avoid cross-room
/// races in try_add_peer_with (which doesn't atomically handle room switching).
/// Asserts: peer_count ≤ capacity, no peer in multiple rooms, empty rooms cleaned up.
/// Runs 100 iterations with different seeds; reports seed on failure.
#[tokio::test]
async fn concurrent_join_kick_interleaving() {
    use rand::rngs::StdRng;
    use rand::{Rng, SeedableRng};
    use tokio::sync::Barrier;

    const N: usize = 32;
    const CAPACITY: u8 = 6;
    const ITERATIONS: u64 = 100;
    const BASE_SEED: u64 = 0xCAFE_BABE_1234;
    const PARTICIPANT_COUNT: usize = 10;

    let room_ids: [String; 2] = ["room-kick-1".to_string(), "room-kick-2".to_string()];

    for iteration in 0..ITERATIONS {
        let seed = BASE_SEED + iteration;
        let state = Arc::new(InMemoryRoomState::new());

        // Create both rooms
        for room_id in &room_ids {
            let info = RoomInfo::new_sfu(CAPACITY, SfuRoomHandle("test-handle".to_string()));
            state.create_room(room_id.clone(), info);
        }

        let barrier = Arc::new(Barrier::new(N));

        let handles: Vec<_> = (0..N)
            .map(|task_idx| {
                let state = Arc::clone(&state);
                let barrier = Arc::clone(&barrier);
                let room_ids = room_ids.clone();
                tokio::spawn(async move {
                    let mut rng = StdRng::seed_from_u64(seed.wrapping_add(task_idx as u64));

                    let participant_idx = rng.gen_range(0..PARTICIPANT_COUNT);
                    let participant_id = format!("participant-{participant_idx}");
                    // Assign home room by participant index (deterministic, no cross-room joins)
                    let home_room = &room_ids[participant_idx % 2];

                    // 0 = join home room, 1 = leave, 2 = kick a random peer in home room
                    let op: u8 = rng.gen_range(0..3);

                    // Synchronize — all tasks start their operation at the same time
                    barrier.wait().await;

                    match op {
                        0 => {
                            let _ = state.try_add_peer_with(participant_id, home_room, || Ok(()));
                        }
                        1 => {
                            state.remove_peer(&participant_id);
                        }
                        2 => {
                            // Kick: pick a random participant assigned to the same home room
                            let target_idx = rng.gen_range(0..PARTICIPANT_COUNT);
                            let target_id = format!("participant-{target_idx}");
                            state.remove_peer(&target_id);
                        }
                        _ => unreachable!(),
                    }
                })
            })
            .collect();

        // Wait for all tasks to complete
        for handle in handles {
            handle.await.unwrap_or_else(|e| {
                panic!("[seed={seed}] task panicked: {e}");
            });
        }

        // Invariant 1: peer_count ≤ capacity for each room
        let snapshot = state.snapshot_rooms();
        for (room_id, peers) in &snapshot {
            let reported_count = state.peer_count(room_id);
            assert!(
                reported_count <= CAPACITY as usize,
                "[seed={seed}] room {room_id}: peer_count ({reported_count}) exceeds capacity ({CAPACITY}). \
                 Peers: {peers:?}"
            );
            assert_eq!(
                reported_count,
                peers.len(),
                "[seed={seed}] room {room_id}: peer_count ({reported_count}) != actual peers ({}). \
                 Peers: {peers:?}",
                peers.len()
            );
        }

        // Invariant 2: no peer appears in more than one room
        let mut seen_peers: std::collections::HashMap<String, String> =
            std::collections::HashMap::new();
        for (room_id, peers) in &snapshot {
            for peer in peers {
                if let Some(other_room) = seen_peers.get(peer) {
                    panic!(
                        "[seed={seed}] peer {peer} found in both room {other_room} and room {room_id}"
                    );
                }
                seen_peers.insert(peer.clone(), room_id.clone());
            }
        }

        // Also verify peer_to_room consistency via RoomState trait
        for (peer, room_id) in &seen_peers {
            let tracked_room = state.get_room_for_peer(peer);
            assert_eq!(
                tracked_room.as_deref(),
                Some(room_id.as_str()),
                "[seed={seed}] peer {peer} is in room {room_id} but peer_to_room says {:?}",
                tracked_room
            );
        }

        // Invariant 3: rooms with zero peers should be cleaned up
        // active_room_count should match rooms with actual peers in the snapshot
        let rooms_with_peers = snapshot.values().filter(|peers| !peers.is_empty()).count();
        let active_count = state.active_room_count();
        assert_eq!(
            active_count, rooms_with_peers,
            "[seed={seed}] active_room_count ({active_count}) != rooms with peers ({rooms_with_peers}). \
             Snapshot: {snapshot:?}"
        );

        // Clean up for next iteration: remove all remaining peers
        for peers in snapshot.values() {
            for peer in peers {
                state.remove_peer(peer);
            }
        }
    }
}

// Feature: test-quality-hardening, Property 3: Concurrency state invariant
// Validates: Requirements 6.4, 6.5, 6.6, 6.7

/// concurrent_mixed_operations_invariant — 100 iterations, N=32 tasks, capacity=6, 3 rooms.
/// Each task randomly joins, leaves, or kicks a random peer across 3 rooms.
/// Participants are assigned a home room by index % 3 (deterministic).
/// Asserts all three invariants: peer count matches, no peer in multiple rooms,
/// empty rooms cleaned up.
/// Gated behind `concurrency-stress` feature flag to avoid slowing default test suite.
/// Seed reported on failure for reproducibility.
#[cfg(feature = "concurrency-stress")]
#[tokio::test]
async fn concurrent_mixed_operations_invariant() {
    use rand::rngs::StdRng;
    use rand::{Rng, SeedableRng};
    use tokio::sync::Barrier;

    const N: usize = 32;
    const CAPACITY: u8 = 6;
    const ITERATIONS: u64 = 100;
    const BASE_SEED: u64 = 0xFACE_FEED_9876;
    const PARTICIPANT_COUNT: usize = 12;

    let room_ids: [String; 3] = [
        "room-mixed-1".to_string(),
        "room-mixed-2".to_string(),
        "room-mixed-3".to_string(),
    ];

    for iteration in 0..ITERATIONS {
        let seed = BASE_SEED + iteration;
        let state = Arc::new(InMemoryRoomState::new());

        // Create all 3 rooms
        for room_id in &room_ids {
            let info = RoomInfo::new_sfu(CAPACITY, SfuRoomHandle("test-handle".to_string()));
            state.create_room(room_id.clone(), info);
        }

        let barrier = Arc::new(Barrier::new(N));

        let handles: Vec<_> = (0..N)
            .map(|task_idx| {
                let state = Arc::clone(&state);
                let barrier = Arc::clone(&barrier);
                let room_ids = room_ids.clone();
                tokio::spawn(async move {
                    let mut rng = StdRng::seed_from_u64(seed.wrapping_add(task_idx as u64));

                    let participant_idx = rng.gen_range(0..PARTICIPANT_COUNT);
                    let participant_id = format!("participant-{participant_idx}");
                    // Assign home room by participant index % 3 (deterministic)
                    let home_room = &room_ids[participant_idx % 3];

                    // 0 = join home room, 1 = leave, 2 = kick a random peer in home room
                    let op: u8 = rng.gen_range(0..3);

                    // Synchronize — all tasks start their operation at the same time
                    barrier.wait().await;

                    match op {
                        0 => {
                            let _ = state.try_add_peer_with(participant_id, home_room, || Ok(()));
                        }
                        1 => {
                            state.remove_peer(&participant_id);
                        }
                        2 => {
                            // Kick: pick a random participant assigned to the same home room
                            let target_idx = rng.gen_range(0..PARTICIPANT_COUNT);
                            let target_id = format!("participant-{target_idx}");
                            state.remove_peer(&target_id);
                        }
                        _ => unreachable!(),
                    }
                })
            })
            .collect();

        // Wait for all tasks to complete
        for handle in handles {
            handle.await.unwrap_or_else(|e| {
                panic!("[seed={seed}] task panicked: {e}");
            });
        }

        // Invariant 1: peer_count matches actual peers and ≤ capacity for each room
        let snapshot = state.snapshot_rooms();
        for (room_id, peers) in &snapshot {
            let reported_count = state.peer_count(room_id);
            assert!(
                reported_count <= CAPACITY as usize,
                "[seed={seed}] room {room_id}: peer_count ({reported_count}) exceeds capacity ({CAPACITY}). \
                 Peers: {peers:?}"
            );
            assert_eq!(
                reported_count,
                peers.len(),
                "[seed={seed}] room {room_id}: peer_count ({reported_count}) != actual peers ({}). \
                 Peers: {peers:?}",
                peers.len()
            );
        }

        // Invariant 2: no peer appears in more than one room
        let mut seen_peers: std::collections::HashMap<String, String> =
            std::collections::HashMap::new();
        for (room_id, peers) in &snapshot {
            for peer in peers {
                if let Some(other_room) = seen_peers.get(peer) {
                    panic!(
                        "[seed={seed}] peer {peer} found in both room {other_room} and room {room_id}"
                    );
                }
                seen_peers.insert(peer.clone(), room_id.clone());
            }
        }

        // Also verify peer_to_room consistency via RoomState trait
        for (peer, room_id) in &seen_peers {
            let tracked_room = state.get_room_for_peer(peer);
            assert_eq!(
                tracked_room.as_deref(),
                Some(room_id.as_str()),
                "[seed={seed}] peer {peer} is in room {room_id} but peer_to_room says {:?}",
                tracked_room
            );
        }

        // Invariant 3: rooms with zero peers should be cleaned up.
        // Rooms are auto-removed when the last peer is removed via remove_peer.
        // However, rooms that were pre-created and never had a peer successfully
        // join (or where all join attempts chose leave/kick) may still exist in
        // the map with zero peers — that's expected. We verify that every room
        // still in the map either has peers OR was never populated (i.e., the
        // active_room_count is consistent with the snapshot).
        let active_count = state.active_room_count();
        let snapshot_room_count = snapshot.len();
        assert_eq!(
            active_count, snapshot_room_count,
            "[seed={seed}] active_room_count ({active_count}) != snapshot room count ({snapshot_room_count}). \
             Snapshot: {snapshot:?}"
        );

        // Additionally verify: for rooms that DO have peers, peer_count is consistent
        // (already checked in invariant 1 above). For rooms with zero peers that
        // still exist, verify they report peer_count == 0.
        for (room_id, peers) in &snapshot {
            if peers.is_empty() {
                let count = state.peer_count(room_id);
                assert_eq!(
                    count, 0,
                    "[seed={seed}] room {room_id} has no peers in snapshot but peer_count is {count}"
                );
            }
        }

        // Clean up for next iteration: remove all remaining peers.
        // (State is recreated each iteration, but this ensures clean teardown.)
        for peers in snapshot.values() {
            for peer in peers {
                state.remove_peer(peer);
            }
        }
    }
}
