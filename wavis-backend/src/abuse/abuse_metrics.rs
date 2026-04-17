use serde::{Deserialize, Serialize};
use std::collections::{HashMap, VecDeque};
use std::net::IpAddr;
use std::sync::RwLock;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, Instant};

pub struct AbuseMetrics {
    pub ws_rate_limit_rejections: AtomicU64,
    pub ws_burst_rejections: AtomicU64,
    pub action_rate_limit_rejections: AtomicU64,
    pub join_rate_limit_rejections: AtomicU64,
    pub join_invite_rejections: AtomicU64,
    pub connections_closed_rate_limit: AtomicU64,
    pub payload_size_violations: AtomicU64,
    pub connections_rejected_ip_cap: AtomicU64,
    pub connections_rejected_temp_ban: AtomicU64,
    // Phase 3 security hardening counters
    pub global_ws_ceiling_rejections: AtomicU64,
    pub global_join_ceiling_rejections: AtomicU64,
    pub schema_validation_rejections: AtomicU64,
    pub state_machine_rejections: AtomicU64,
    pub screen_share_rejections: AtomicU64,
    pub revoke_authorization_rejections: AtomicU64,
    pub tls_proto_rejections: AtomicU64,
    pub invite_usage_anomalies: AtomicU64,
}

impl AbuseMetrics {
    pub fn new() -> Self {
        Self {
            ws_rate_limit_rejections: AtomicU64::new(0),
            ws_burst_rejections: AtomicU64::new(0),
            action_rate_limit_rejections: AtomicU64::new(0),
            join_rate_limit_rejections: AtomicU64::new(0),
            join_invite_rejections: AtomicU64::new(0),
            connections_closed_rate_limit: AtomicU64::new(0),
            payload_size_violations: AtomicU64::new(0),
            connections_rejected_ip_cap: AtomicU64::new(0),
            connections_rejected_temp_ban: AtomicU64::new(0),
            global_ws_ceiling_rejections: AtomicU64::new(0),
            global_join_ceiling_rejections: AtomicU64::new(0),
            schema_validation_rejections: AtomicU64::new(0),
            state_machine_rejections: AtomicU64::new(0),
            screen_share_rejections: AtomicU64::new(0),
            revoke_authorization_rejections: AtomicU64::new(0),
            tls_proto_rejections: AtomicU64::new(0),
            invite_usage_anomalies: AtomicU64::new(0),
        }
    }

    /// Increment a specific counter.
    pub fn increment(&self, counter: &AtomicU64) {
        counter.fetch_add(1, Ordering::Relaxed);
    }

    /// Snapshot all counters for reporting.
    pub fn snapshot(&self) -> AbuseMetricsSnapshot {
        AbuseMetricsSnapshot {
            ws_rate_limit_rejections: self.ws_rate_limit_rejections.load(Ordering::Relaxed),
            ws_burst_rejections: self.ws_burst_rejections.load(Ordering::Relaxed),
            action_rate_limit_rejections: self.action_rate_limit_rejections.load(Ordering::Relaxed),
            join_rate_limit_rejections: self.join_rate_limit_rejections.load(Ordering::Relaxed),
            join_invite_rejections: self.join_invite_rejections.load(Ordering::Relaxed),
            connections_closed_rate_limit: self
                .connections_closed_rate_limit
                .load(Ordering::Relaxed),
            payload_size_violations: self.payload_size_violations.load(Ordering::Relaxed),
            connections_rejected_ip_cap: self.connections_rejected_ip_cap.load(Ordering::Relaxed),
            connections_rejected_temp_ban: self
                .connections_rejected_temp_ban
                .load(Ordering::Relaxed),
            global_ws_ceiling_rejections: self.global_ws_ceiling_rejections.load(Ordering::Relaxed),
            global_join_ceiling_rejections: self
                .global_join_ceiling_rejections
                .load(Ordering::Relaxed),
            schema_validation_rejections: self.schema_validation_rejections.load(Ordering::Relaxed),
            state_machine_rejections: self.state_machine_rejections.load(Ordering::Relaxed),
            screen_share_rejections: self.screen_share_rejections.load(Ordering::Relaxed),
            revoke_authorization_rejections: self
                .revoke_authorization_rejections
                .load(Ordering::Relaxed),
            tls_proto_rejections: self.tls_proto_rejections.load(Ordering::Relaxed),
            invite_usage_anomalies: self.invite_usage_anomalies.load(Ordering::Relaxed),
        }
    }
}

impl Default for AbuseMetrics {
    fn default() -> Self {
        Self::new()
    }
}

#[derive(Serialize, Deserialize)]
pub struct AbuseMetricsSnapshot {
    pub ws_rate_limit_rejections: u64,
    pub ws_burst_rejections: u64,
    pub action_rate_limit_rejections: u64,
    pub join_rate_limit_rejections: u64,
    pub join_invite_rejections: u64,
    pub connections_closed_rate_limit: u64,
    pub payload_size_violations: u64,
    pub connections_rejected_ip_cap: u64,
    pub connections_rejected_temp_ban: u64,
    // Phase 3 security hardening counters
    pub global_ws_ceiling_rejections: u64,
    pub global_join_ceiling_rejections: u64,
    pub schema_validation_rejections: u64,
    pub state_machine_rejections: u64,
    pub screen_share_rejections: u64,
    pub revoke_authorization_rejections: u64,
    pub tls_proto_rejections: u64,
    pub invite_usage_anomalies: u64,
}

/// Tracks per-IP failed join attempts within a sliding window.
/// When the failure count exceeds the threshold, `record_failure` returns `true`
/// to signal that the handler should emit a structured warn log.
pub struct IpFailedJoinTracker {
    entries: RwLock<HashMap<IpAddr, VecDeque<Instant>>>,
    threshold: u32,
    window: Duration,
}

impl IpFailedJoinTracker {
    pub fn new(threshold: u32, window: Duration) -> Self {
        Self {
            entries: RwLock::new(HashMap::new()),
            threshold,
            window,
        }
    }

    /// Record a failed join from `ip` at time `now`.
    /// Returns `None` if below threshold, `Some(count)` if the failure count exceeds the threshold.
    pub fn record_failure(&self, ip: IpAddr, now: Instant) -> Option<u32> {
        let mut entries = self.entries.write().unwrap();
        let deque = entries.entry(ip).or_default();

        // Prune entries outside the window
        let cutoff = now.checked_sub(self.window).unwrap_or(now);
        while let Some(&front) = deque.front() {
            if front < cutoff {
                deque.pop_front();
            } else {
                break;
            }
        }

        // Add the new failure
        deque.push_back(now);

        let count = deque.len() as u32;
        if count > self.threshold {
            Some(count)
        } else {
            None
        }
    }

    /// Returns the configured threshold.
    pub fn window_secs(&self) -> u64 {
        self.window.as_secs()
    }
}

impl Default for IpFailedJoinTracker {
    fn default() -> Self {
        Self::new(10, Duration::from_secs(60))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use proptest::prelude::*;

    // Feature: signaling-auth-and-abuse-controls, Property 8: Abuse metrics increment monotonically
    // Validates: Requirements 5.1
    proptest! {
        #![proptest_config(proptest::test_runner::Config::with_cases(100))]

        #[test]
        fn prop_ws_rate_limit_rejections_increment_monotonically(n in 0u32..=1000u32) {
            let metrics = AbuseMetrics::new();
            for _ in 0..n {
                metrics.ws_rate_limit_rejections.fetch_add(1, Ordering::Relaxed);
            }
            let snapshot = metrics.snapshot();
            prop_assert_eq!(snapshot.ws_rate_limit_rejections, n as u64);
            prop_assert_eq!(snapshot.ws_burst_rejections, 0);
            prop_assert_eq!(snapshot.action_rate_limit_rejections, 0);
            prop_assert_eq!(snapshot.join_rate_limit_rejections, 0);
            prop_assert_eq!(snapshot.join_invite_rejections, 0);
            prop_assert_eq!(snapshot.connections_closed_rate_limit, 0);
            prop_assert_eq!(snapshot.payload_size_violations, 0);
            prop_assert_eq!(snapshot.connections_rejected_ip_cap, 0);
            prop_assert_eq!(snapshot.connections_rejected_temp_ban, 0);
        }

        #[test]
        fn prop_ws_burst_rejections_increment_monotonically(n in 0u32..=1000u32) {
            let metrics = AbuseMetrics::new();
            for _ in 0..n {
                metrics.ws_burst_rejections.fetch_add(1, Ordering::Relaxed);
            }
            let snapshot = metrics.snapshot();
            prop_assert_eq!(snapshot.ws_burst_rejections, n as u64);
            prop_assert_eq!(snapshot.ws_rate_limit_rejections, 0);
            prop_assert_eq!(snapshot.action_rate_limit_rejections, 0);
            prop_assert_eq!(snapshot.join_rate_limit_rejections, 0);
            prop_assert_eq!(snapshot.join_invite_rejections, 0);
            prop_assert_eq!(snapshot.connections_closed_rate_limit, 0);
            prop_assert_eq!(snapshot.payload_size_violations, 0);
            prop_assert_eq!(snapshot.connections_rejected_ip_cap, 0);
            prop_assert_eq!(snapshot.connections_rejected_temp_ban, 0);
        }

        #[test]
        fn prop_action_rate_limit_rejections_increment_monotonically(n in 0u32..=1000u32) {
            let metrics = AbuseMetrics::new();
            for _ in 0..n {
                metrics.action_rate_limit_rejections.fetch_add(1, Ordering::Relaxed);
            }
            let snapshot = metrics.snapshot();
            prop_assert_eq!(snapshot.action_rate_limit_rejections, n as u64);
            prop_assert_eq!(snapshot.ws_rate_limit_rejections, 0);
            prop_assert_eq!(snapshot.ws_burst_rejections, 0);
            prop_assert_eq!(snapshot.join_rate_limit_rejections, 0);
            prop_assert_eq!(snapshot.join_invite_rejections, 0);
            prop_assert_eq!(snapshot.connections_closed_rate_limit, 0);
            prop_assert_eq!(snapshot.payload_size_violations, 0);
            prop_assert_eq!(snapshot.connections_rejected_ip_cap, 0);
            prop_assert_eq!(snapshot.connections_rejected_temp_ban, 0);
        }

        #[test]
        fn prop_join_rate_limit_rejections_increment_monotonically(n in 0u32..=1000u32) {
            let metrics = AbuseMetrics::new();
            for _ in 0..n {
                metrics.join_rate_limit_rejections.fetch_add(1, Ordering::Relaxed);
            }
            let snapshot = metrics.snapshot();
            prop_assert_eq!(snapshot.join_rate_limit_rejections, n as u64);
            prop_assert_eq!(snapshot.ws_rate_limit_rejections, 0);
            prop_assert_eq!(snapshot.ws_burst_rejections, 0);
            prop_assert_eq!(snapshot.action_rate_limit_rejections, 0);
            prop_assert_eq!(snapshot.join_invite_rejections, 0);
            prop_assert_eq!(snapshot.connections_closed_rate_limit, 0);
            prop_assert_eq!(snapshot.payload_size_violations, 0);
            prop_assert_eq!(snapshot.connections_rejected_ip_cap, 0);
            prop_assert_eq!(snapshot.connections_rejected_temp_ban, 0);
        }

        #[test]
        fn prop_join_invite_rejections_increment_monotonically(n in 0u32..=1000u32) {
            let metrics = AbuseMetrics::new();
            for _ in 0..n {
                metrics.join_invite_rejections.fetch_add(1, Ordering::Relaxed);
            }
            let snapshot = metrics.snapshot();
            prop_assert_eq!(snapshot.join_invite_rejections, n as u64);
            prop_assert_eq!(snapshot.ws_rate_limit_rejections, 0);
            prop_assert_eq!(snapshot.ws_burst_rejections, 0);
            prop_assert_eq!(snapshot.action_rate_limit_rejections, 0);
            prop_assert_eq!(snapshot.join_rate_limit_rejections, 0);
            prop_assert_eq!(snapshot.connections_closed_rate_limit, 0);
            prop_assert_eq!(snapshot.payload_size_violations, 0);
            prop_assert_eq!(snapshot.connections_rejected_ip_cap, 0);
            prop_assert_eq!(snapshot.connections_rejected_temp_ban, 0);
        }

        #[test]
        fn prop_connections_closed_rate_limit_increment_monotonically(n in 0u32..=1000u32) {
            let metrics = AbuseMetrics::new();
            for _ in 0..n {
                metrics.connections_closed_rate_limit.fetch_add(1, Ordering::Relaxed);
            }
            let snapshot = metrics.snapshot();
            prop_assert_eq!(snapshot.connections_closed_rate_limit, n as u64);
            prop_assert_eq!(snapshot.ws_rate_limit_rejections, 0);
            prop_assert_eq!(snapshot.ws_burst_rejections, 0);
            prop_assert_eq!(snapshot.action_rate_limit_rejections, 0);
            prop_assert_eq!(snapshot.join_rate_limit_rejections, 0);
            prop_assert_eq!(snapshot.join_invite_rejections, 0);
            prop_assert_eq!(snapshot.payload_size_violations, 0);
            prop_assert_eq!(snapshot.connections_rejected_ip_cap, 0);
            prop_assert_eq!(snapshot.connections_rejected_temp_ban, 0);
        }

        #[test]
        fn prop_payload_size_violations_increment_monotonically(n in 0u32..=1000u32) {
            let metrics = AbuseMetrics::new();
            for _ in 0..n {
                metrics.payload_size_violations.fetch_add(1, Ordering::Relaxed);
            }
            let snapshot = metrics.snapshot();
            prop_assert_eq!(snapshot.payload_size_violations, n as u64);
            prop_assert_eq!(snapshot.ws_rate_limit_rejections, 0);
            prop_assert_eq!(snapshot.ws_burst_rejections, 0);
            prop_assert_eq!(snapshot.action_rate_limit_rejections, 0);
            prop_assert_eq!(snapshot.join_rate_limit_rejections, 0);
            prop_assert_eq!(snapshot.join_invite_rejections, 0);
            prop_assert_eq!(snapshot.connections_closed_rate_limit, 0);
            prop_assert_eq!(snapshot.connections_rejected_ip_cap, 0);
            prop_assert_eq!(snapshot.connections_rejected_temp_ban, 0);
        }

        #[test]
        fn prop_connections_rejected_ip_cap_increment_monotonically(n in 0u32..=1000u32) {
            let metrics = AbuseMetrics::new();
            for _ in 0..n {
                metrics.connections_rejected_ip_cap.fetch_add(1, Ordering::Relaxed);
            }
            let snapshot = metrics.snapshot();
            prop_assert_eq!(snapshot.connections_rejected_ip_cap, n as u64);
            prop_assert_eq!(snapshot.ws_rate_limit_rejections, 0);
            prop_assert_eq!(snapshot.ws_burst_rejections, 0);
            prop_assert_eq!(snapshot.action_rate_limit_rejections, 0);
            prop_assert_eq!(snapshot.join_rate_limit_rejections, 0);
            prop_assert_eq!(snapshot.join_invite_rejections, 0);
            prop_assert_eq!(snapshot.connections_closed_rate_limit, 0);
            prop_assert_eq!(snapshot.payload_size_violations, 0);
            prop_assert_eq!(snapshot.connections_rejected_temp_ban, 0);
        }

        #[test]
        fn prop_connections_rejected_temp_ban_increment_monotonically(n in 0u32..=1000u32) {
            let metrics = AbuseMetrics::new();
            for _ in 0..n {
                metrics.connections_rejected_temp_ban.fetch_add(1, Ordering::Relaxed);
            }
            let snapshot = metrics.snapshot();
            prop_assert_eq!(snapshot.connections_rejected_temp_ban, n as u64);
            prop_assert_eq!(snapshot.ws_rate_limit_rejections, 0);
            prop_assert_eq!(snapshot.ws_burst_rejections, 0);
            prop_assert_eq!(snapshot.action_rate_limit_rejections, 0);
            prop_assert_eq!(snapshot.join_rate_limit_rejections, 0);
            prop_assert_eq!(snapshot.join_invite_rejections, 0);
            prop_assert_eq!(snapshot.connections_closed_rate_limit, 0);
            prop_assert_eq!(snapshot.payload_size_violations, 0);
            prop_assert_eq!(snapshot.connections_rejected_ip_cap, 0);
        }
    }

    // --- IpFailedJoinTracker unit tests ---

    #[test]
    fn tracker_below_threshold_returns_none() {
        let tracker = IpFailedJoinTracker::new(3, Duration::from_secs(60));
        let ip: IpAddr = "10.0.0.1".parse().unwrap();
        let now = Instant::now();

        assert!(tracker.record_failure(ip, now).is_none());
        assert!(tracker.record_failure(ip, now).is_none());
        assert!(tracker.record_failure(ip, now).is_none()); // 3 == threshold, not exceeded
    }

    #[test]
    fn tracker_exceeds_threshold_returns_count() {
        let tracker = IpFailedJoinTracker::new(3, Duration::from_secs(60));
        let ip: IpAddr = "10.0.0.1".parse().unwrap();
        let now = Instant::now();

        for _ in 0..3 {
            tracker.record_failure(ip, now);
        }
        // 4th failure exceeds threshold of 3
        assert_eq!(tracker.record_failure(ip, now), Some(4));
    }

    #[test]
    fn tracker_prunes_old_entries() {
        let tracker = IpFailedJoinTracker::new(3, Duration::from_secs(10));
        let ip: IpAddr = "10.0.0.1".parse().unwrap();
        let start = Instant::now();

        // Record 3 failures at start
        for _ in 0..3 {
            tracker.record_failure(ip, start);
        }

        // After the window expires, old entries are pruned
        let later = start + Duration::from_secs(11);
        // This is the 1st failure in the new window
        assert!(tracker.record_failure(ip, later).is_none());
    }

    #[test]
    fn tracker_isolates_different_ips() {
        let tracker = IpFailedJoinTracker::new(2, Duration::from_secs(60));
        let ip_a: IpAddr = "10.0.0.1".parse().unwrap();
        let ip_b: IpAddr = "10.0.0.2".parse().unwrap();
        let now = Instant::now();

        // 2 failures for ip_a (at threshold, not exceeded)
        tracker.record_failure(ip_a, now);
        tracker.record_failure(ip_a, now);

        // ip_b is independent — 1 failure, well below threshold
        assert!(tracker.record_failure(ip_b, now).is_none());

        // ip_a exceeds on 3rd
        assert_eq!(tracker.record_failure(ip_a, now), Some(3));
    }

    #[test]
    fn tracker_default_has_expected_config() {
        let tracker = IpFailedJoinTracker::default();
        assert_eq!(tracker.window_secs(), 60);
    }

    // --- Security-Hardening Property 11: Per-IP failed join threshold detection ---
    // Feature: security-hardening, Property 11: Per-IP failed join threshold detection
    // Validates: Requirements 8.1, 8.6
    proptest! {
        #![proptest_config(ProptestConfig::with_cases(100))]

        #[test]
        fn prop_sh_p11_per_ip_failed_join_threshold_detection(
            threshold in 2u32..20u32,
            extra in 1u32..10u32,
        ) {
            let tracker = IpFailedJoinTracker::new(threshold, Duration::from_secs(60));
            let ip: IpAddr = "10.0.0.1".parse().unwrap();
            let now = Instant::now();

            // Record exactly `threshold` failures — all should return None (not exceeded)
            for i in 0..threshold {
                let result = tracker.record_failure(ip, now);
                prop_assert!(
                    result.is_none(),
                    "failure {} of {} (at threshold) should return None, got {:?}",
                    i + 1, threshold, result
                );
            }

            // Record `extra` more failures beyond threshold — all should return Some(count)
            for i in 0..extra {
                let result = tracker.record_failure(ip, now);
                let expected_count = threshold + i + 1;
                prop_assert_eq!(
                    result,
                    Some(expected_count),
                    "failure {} beyond threshold should return Some({})",
                    i + 1, expected_count
                );
            }
        }
    }

    // --- Security-Hardening Property 12: Invite usage anomaly detection ---
    // Feature: security-hardening, Property 12: Invite usage anomaly detection
    // Validates: Requirements 8.2
    //
    // Note: The invite_usage_anomalies counter increment is wired in the handler
    // layer (ws.rs), not in the domain. This property test verifies the domain
    // behavior: after M successful consumptions of an invite with max_uses=M,
    // the next validate_and_consume call returns InviteExhausted.
    proptest! {
        #![proptest_config(ProptestConfig::with_cases(100))]

        #[test]
        fn prop_sh_p12_invite_usage_anomaly_detection(
            max_uses in 1u32..10u32,
        ) {
            use crate::channel::invite::{InviteStore, InviteStoreConfig};
            use shared::signaling::JoinRejectionReason;

            let config = InviteStoreConfig {
                default_ttl: Duration::from_secs(3600),
                default_max_uses: max_uses,
                max_invites_per_room: 20,
                max_invites_global: 1000,
                sweep_interval: Duration::from_secs(60),
            };
            let store = InviteStore::new(config);
            let t = Instant::now();

            let record = store.generate("room-1", "issuer-1", Some(max_uses), t).unwrap();

            // Consume all M uses via validate_and_consume (atomic validate + decrement)
            for i in 0..max_uses {
                let result = store.validate_and_consume(&record.code, "room-1", t);
                prop_assert!(
                    result.is_ok(),
                    "consumption {} of {} should succeed, got {:?}",
                    i + 1, max_uses, result
                );
            }

            // The (M+1)th attempt must return InviteExhausted
            let result = store.validate_and_consume(&record.code, "room-1", t);
            prop_assert_eq!(
                result,
                Err(JoinRejectionReason::InviteExhausted),
                "attempt after {} consumptions must return InviteExhausted",
                max_uses
            );
        }
    }
}
