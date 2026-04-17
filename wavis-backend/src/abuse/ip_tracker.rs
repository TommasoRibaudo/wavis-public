use std::collections::HashMap;
use std::net::IpAddr;
use std::sync::RwLock;
use std::sync::atomic::{AtomicU32, Ordering};

/// Tracks concurrent WebSocket connections per IP address.
/// Checked at upgrade time, before the WebSocket handshake.
pub struct IpConnectionTracker {
    connections: RwLock<HashMap<IpAddr, u32>>,
    max_per_ip: AtomicU32,
}

impl IpConnectionTracker {
    pub fn new(max_per_ip: u32) -> Self {
        Self {
            connections: RwLock::new(HashMap::new()),
            max_per_ip: AtomicU32::new(max_per_ip),
        }
    }

    /// Read `MAX_CONNECTIONS_PER_IP` env var (default: 10) and construct.
    pub fn from_env() -> Self {
        let max_per_ip = std::env::var("MAX_CONNECTIONS_PER_IP")
            .ok()
            .and_then(|v| v.parse::<u32>().ok())
            .unwrap_or(10);
        Self::new(max_per_ip)
    }

    /// Returns the configured per-IP connection cap.
    pub fn max_per_ip(&self) -> u32 {
        self.max_per_ip.load(Ordering::Relaxed)
    }

    /// Update the per-IP connection cap at runtime.
    /// Used by the stress harness to temporarily lower/raise the cap per-scenario.
    pub fn set_max_per_ip(&self, max: u32) {
        self.max_per_ip.store(max, Ordering::Relaxed);
    }

    /// Try to register a new connection for this IP.
    /// Returns true if under the limit, false if at/over.
    pub fn try_add(&self, ip: IpAddr) -> bool {
        let max = self.max_per_ip.load(Ordering::Relaxed);
        let mut map = self.connections.write().unwrap();
        let count = map.entry(ip).or_insert(0);
        if *count < max {
            *count += 1;
            true
        } else {
            false
        }
    }

    /// Decrement the connection count for this IP.
    /// Removes the entry if count reaches 0.
    pub fn remove(&self, ip: IpAddr) {
        let mut map = self.connections.write().unwrap();
        if let Some(count) = map.get_mut(&ip) {
            if *count <= 1 {
                map.remove(&ip);
            } else {
                *count -= 1;
            }
        }
    }

    /// Current connection count for an IP (0 if not present).
    pub fn count(&self, ip: IpAddr) -> u32 {
        let map = self.connections.read().unwrap();
        map.get(&ip).copied().unwrap_or(0)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use proptest::prelude::*;
    use std::net::{IpAddr, Ipv4Addr};

    fn ip(a: u8) -> IpAddr {
        IpAddr::V4(Ipv4Addr::new(127, 0, 0, a))
    }

    #[test]
    fn try_add_allows_up_to_max() {
        let tracker = IpConnectionTracker::new(3);
        let addr = ip(1);
        assert!(tracker.try_add(addr));
        assert!(tracker.try_add(addr));
        assert!(tracker.try_add(addr));
        assert!(!tracker.try_add(addr)); // 4th should fail
    }

    #[test]
    fn count_reflects_adds() {
        let tracker = IpConnectionTracker::new(5);
        let addr = ip(2);
        assert_eq!(tracker.count(addr), 0);
        tracker.try_add(addr);
        tracker.try_add(addr);
        assert_eq!(tracker.count(addr), 2);
    }

    #[test]
    fn remove_decrements_and_cleans_up() {
        let tracker = IpConnectionTracker::new(5);
        let addr = ip(3);
        tracker.try_add(addr);
        tracker.try_add(addr);
        tracker.remove(addr);
        assert_eq!(tracker.count(addr), 1);
        tracker.remove(addr);
        assert_eq!(tracker.count(addr), 0);
    }

    #[test]
    fn remove_after_zero_is_noop() {
        let tracker = IpConnectionTracker::new(5);
        let addr = ip(4);
        // Should not panic
        tracker.remove(addr);
        assert_eq!(tracker.count(addr), 0);
    }

    #[test]
    fn remove_allows_new_add() {
        let tracker = IpConnectionTracker::new(1);
        let addr = ip(5);
        assert!(tracker.try_add(addr));
        assert!(!tracker.try_add(addr));
        tracker.remove(addr);
        assert!(tracker.try_add(addr));
    }

    #[test]
    fn different_ips_are_independent() {
        let tracker = IpConnectionTracker::new(1);
        let a = ip(10);
        let b = ip(11);
        assert!(tracker.try_add(a));
        assert!(tracker.try_add(b));
        assert!(!tracker.try_add(a));
        assert!(!tracker.try_add(b));
    }

    // Feature: signaling-auth-and-abuse-controls, Property 4: IP connection tracking round-trip
    // Validates: Requirements 2.1, 2.3
    proptest! {
        #![proptest_config(ProptestConfig::with_cases(100))]

        #[test]
        fn prop_ip_connection_tracking_round_trip(
            max_per_ip in 1u32..=20u32,
        ) {
            let test_ip = IpAddr::V4(Ipv4Addr::new(10, 0, 0, 1));
            let tracker = IpConnectionTracker::new(max_per_ip);

            // All max_per_ip adds should succeed
            for i in 0..max_per_ip {
                prop_assert!(
                    tracker.try_add(test_ip),
                    "try_add #{} should succeed (max_per_ip={})",
                    i + 1,
                    max_per_ip
                );
            }

            // The (max_per_ip + 1)th add should fail
            prop_assert!(
                !tracker.try_add(test_ip),
                "try_add #{} should fail (max_per_ip={})",
                max_per_ip + 1,
                max_per_ip
            );

            // Remove one slot, then one more add should succeed
            tracker.remove(test_ip);
            prop_assert!(
                tracker.try_add(test_ip),
                "try_add after remove should succeed (max_per_ip={})",
                max_per_ip
            );
        }

        #[test]
        fn prop_ip_count_invariant(
            max_per_ip in 1u32..=20u32,
            n in 1u32..=20u32,
            m in 0u32..=20u32,
        ) {
            // Clamp n to max_per_ip and m to n
            let n = n.min(max_per_ip);
            let m = m.min(n);

            let test_ip = IpAddr::V4(Ipv4Addr::new(10, 0, 0, 1));
            let tracker = IpConnectionTracker::new(max_per_ip);

            // N adds
            for _ in 0..n {
                tracker.try_add(test_ip);
            }

            // M removes
            for _ in 0..m {
                tracker.remove(test_ip);
            }

            // Count should equal N - M
            prop_assert_eq!(
                tracker.count(test_ip),
                n - m,
                "count after {} adds and {} removes should be {} (max_per_ip={})",
                n,
                m,
                n - m,
                max_per_ip
            );
        }
    }
}
