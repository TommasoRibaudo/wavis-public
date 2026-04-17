use crate::results::InvariantViolation;
use std::collections::HashMap;

/// Typed representation of a room snapshot from the test metrics endpoint.
#[derive(serde::Deserialize)]
pub struct RoomSnapshotData {
    pub peer_ids: Vec<String>,
    pub participant_count: usize,
    pub room_type: String,
    pub active_shares: Vec<String>,
}

/// Typed representation of the full test metrics response.
#[derive(serde::Deserialize)]
pub struct TestMetricsSnapshot {
    pub rooms: HashMap<String, RoomSnapshotData>,
    pub abuse_metrics: serde_json::Value, // keep as Value for flexibility
    pub total_rooms: usize,
    pub total_participants: usize,
}

/// Fetch the current metrics snapshot from the test metrics endpoint.
/// Returns Err if the endpoint is unreachable or returns non-200.
/// Retries once with 1s delay on failure (per design.md error handling).
pub async fn fetch_metrics(
    http_client: &reqwest::Client,
    metrics_url: &str,
    metrics_token: &str,
) -> Result<serde_json::Value, String> {
    let attempt = do_fetch(http_client, metrics_url, metrics_token).await;
    match attempt {
        Ok(v) => Ok(v),
        Err(e) => {
            // Retry once after 1s delay.
            tokio::time::sleep(std::time::Duration::from_secs(1)).await;
            do_fetch(http_client, metrics_url, metrics_token)
                .await
                .map_err(|e2| format!("metrics fetch failed after retry: first={e}, second={e2}"))
        }
    }
}

async fn do_fetch(
    http_client: &reqwest::Client,
    metrics_url: &str,
    metrics_token: &str,
) -> Result<serde_json::Value, String> {
    let resp = http_client
        .get(metrics_url)
        .header("Authorization", format!("Bearer {metrics_token}"))
        .send()
        .await
        .map_err(|e| format!("request error: {e}"))?;

    if !resp.status().is_success() {
        return Err(format!("non-200 status: {}", resp.status()));
    }

    resp.json::<serde_json::Value>()
        .await
        .map_err(|e| format!("JSON decode error: {e}"))
}

/// Assert that no room exceeds the capacity invariant (max 6 participants).
/// Returns a list of violations (empty = pass).
pub fn assert_room_capacity(metrics: &serde_json::Value) -> Vec<InvariantViolation> {
    const MAX_CAPACITY: usize = 6;
    let mut violations = Vec::new();

    let rooms = match metrics.get("rooms").and_then(|r| r.as_object()) {
        Some(r) => r,
        None => return violations,
    };

    for (room_id, room) in rooms {
        let count = room
            .get("participant_count")
            .and_then(|v| v.as_u64())
            .unwrap_or(0) as usize;

        if count > MAX_CAPACITY {
            violations.push(InvariantViolation {
                invariant: format!("room_capacity[{room_id}]"),
                expected: format!("<= {MAX_CAPACITY}"),
                actual: count.to_string(),
            });
        }
    }

    violations
}

/// Assert that there are no ghost peers in any room.
/// A ghost peer is a peer listed in Room_State that has no active WebSocket connection.
/// In the metrics snapshot, we can only check that participant_count == peer_ids.len().
pub fn assert_no_ghost_peers(metrics: &serde_json::Value) -> Vec<InvariantViolation> {
    let mut violations = Vec::new();

    let rooms = match metrics.get("rooms").and_then(|r| r.as_object()) {
        Some(r) => r,
        None => return violations,
    };

    for (room_id, room) in rooms {
        let participant_count = room
            .get("participant_count")
            .and_then(|v| v.as_u64())
            .unwrap_or(0) as usize;

        let peer_ids_len = room
            .get("peer_ids")
            .and_then(|v| v.as_array())
            .map(|a| a.len())
            .unwrap_or(0);

        if participant_count != peer_ids_len {
            violations.push(InvariantViolation {
                invariant: format!(
                    "no_ghost_peers[{room_id}]: participant_count == peer_ids.len()"
                ),
                expected: peer_ids_len.to_string(),
                actual: participant_count.to_string(),
            });
        }
    }

    violations
}

/// Assert that a specific abuse metrics counter has increased by at least `min_delta`
/// compared to a baseline snapshot.
pub fn assert_counter_delta(
    baseline: &serde_json::Value,
    current: &serde_json::Value,
    counter_name: &str,
    min_delta: u64,
) -> Option<InvariantViolation> {
    let baseline_val = baseline
        .get("abuse_metrics")
        .and_then(|m| m.get(counter_name))
        .and_then(|v| v.as_u64())
        .unwrap_or(0);

    let current_val = current
        .get("abuse_metrics")
        .and_then(|m| m.get(counter_name))
        .and_then(|v| v.as_u64())
        .unwrap_or(0);

    let delta = current_val.saturating_sub(baseline_val);

    if delta < min_delta {
        Some(InvariantViolation {
            invariant: format!("counter_delta[{counter_name}]"),
            expected: format!(">= {min_delta}"),
            actual: delta.to_string(),
        })
    } else {
        None
    }
}

/// Assert that a specific room has exactly `expected_count` participants.
pub fn assert_room_participant_count(
    metrics: &serde_json::Value,
    room_id: &str,
    expected_count: usize,
) -> Option<InvariantViolation> {
    let actual = metrics
        .get("rooms")
        .and_then(|r| r.get(room_id))
        .and_then(|r| r.get("participant_count"))
        .and_then(|v| v.as_u64())
        .map(|v| v as usize);

    match actual {
        Some(count) if count == expected_count => None,
        Some(count) => Some(InvariantViolation {
            invariant: format!("room_participant_count[{room_id}]"),
            expected: expected_count.to_string(),
            actual: count.to_string(),
        }),
        None => Some(InvariantViolation {
            invariant: format!("room_participant_count[{room_id}]"),
            expected: expected_count.to_string(),
            actual: "room not found".to_string(),
        }),
    }
}

#[cfg(test)]
mod tests {
    use super::assert_room_capacity;

    /// Property 1: Capacity invariant under concurrent joins
    ///
    /// For any room snapshot where participant_count > 6, assert_room_capacity
    /// returns a violation. For any room snapshot where participant_count <= 6,
    /// assert_room_capacity returns no violations.
    ///
    /// **Validates: Requirements 2.2, 2.3**
    use proptest::prelude::*;

    proptest! {
        #[test]
        fn prop_capacity_invariant(count in 0usize..=20usize) {
            let metrics = serde_json::json!({
                "rooms": {
                    "test-room": {
                        "participant_count": count,
                        "peer_ids": [],
                        "room_type": "p2p",
                        "active_shares": []
                    }
                }
            });
            let violations = assert_room_capacity(&metrics);
            if count > 6 {
                prop_assert!(!violations.is_empty(), "count={count} should produce a violation");
            } else {
                prop_assert!(violations.is_empty(), "count={count} should not produce a violation");
            }
        }
    }
}
