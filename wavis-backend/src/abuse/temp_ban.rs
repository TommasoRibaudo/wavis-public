use std::collections::{HashMap, VecDeque};
use std::net::IpAddr;
use std::sync::RwLock;
use std::time::{Duration, Instant};

pub struct TempBanConfig {
    pub threshold: u32,
    pub window: Duration,
    pub ban_duration: Duration,
    pub max_entries: usize,
}

impl TempBanConfig {
    pub fn from_env() -> Self {
        let threshold = std::env::var("TEMP_BAN_THRESHOLD")
            .ok()
            .and_then(|v| v.parse().ok())
            .unwrap_or(3);
        let window_secs = std::env::var("TEMP_BAN_WINDOW_SECS")
            .ok()
            .and_then(|v| v.parse::<u64>().ok())
            .unwrap_or(300);
        let ban_duration_secs = std::env::var("TEMP_BAN_DURATION_SECS")
            .ok()
            .and_then(|v| v.parse::<u64>().ok())
            .unwrap_or(600);
        let max_entries = std::env::var("TEMP_BAN_MAX_ENTRIES")
            .ok()
            .and_then(|v| v.parse().ok())
            .unwrap_or(1000);

        Self {
            threshold,
            window: Duration::from_secs(window_secs),
            ban_duration: Duration::from_secs(ban_duration_secs),
            max_entries,
        }
    }
}

struct BanEntry {
    #[allow(dead_code)]
    banned_at: Instant,
    expires_at: Instant,
}

struct ViolationWindow {
    timestamps: VecDeque<Instant>,
}

pub struct TempBanList {
    bans: RwLock<HashMap<IpAddr, BanEntry>>,
    violations: RwLock<HashMap<IpAddr, ViolationWindow>>,
    config: TempBanConfig,
    now: fn() -> Instant,
}

impl TempBanList {
    pub fn new(config: TempBanConfig) -> Self {
        Self::with_clock(config, Instant::now)
    }

    pub fn with_clock(config: TempBanConfig, now: fn() -> Instant) -> Self {
        Self {
            bans: RwLock::new(HashMap::new()),
            violations: RwLock::new(HashMap::new()),
            config,
            now,
        }
    }

    /// Check if an IP is currently banned (lazy expiry).
    pub fn is_banned(&self, ip: IpAddr) -> bool {
        let bans = self.bans.read().expect("bans read lock poisoned");
        match bans.get(&ip) {
            Some(entry) => entry.expires_at > (self.now)(),
            None => false,
        }
    }

    /// Record a rate-limit violation for an IP.
    /// If violations exceed threshold within window, the IP is banned.
    pub fn record_violation(&self, ip: IpAddr) {
        let now = (self.now)();

        // Step 1: update violation window
        let should_ban = {
            let mut violations = self
                .violations
                .write()
                .expect("violations write lock poisoned");
            let window = violations.entry(ip).or_insert_with(|| ViolationWindow {
                timestamps: VecDeque::new(),
            });

            // Add current timestamp
            window.timestamps.push_back(now);

            // Prune timestamps older than config.window
            let cutoff = now - self.config.window;
            while let Some(&front) = window.timestamps.front() {
                if front <= cutoff {
                    window.timestamps.pop_front();
                } else {
                    break;
                }
            }

            window.timestamps.len() >= self.config.threshold as usize
        };

        if !should_ban {
            return;
        }

        // Step 2: attempt to ban
        let mut bans = self.bans.write().expect("bans write lock poisoned");

        // If already banned, skip
        if let Some(entry) = bans.get(&ip)
            && entry.expires_at > now
        {
            return;
        }

        // If at capacity, prune expired entries first
        if bans.len() >= self.config.max_entries {
            bans.retain(|_, entry| entry.expires_at > now);
        }

        // If still full after pruning, fail-open (don't insert)
        if bans.len() >= self.config.max_entries {
            return;
        }

        bans.insert(
            ip,
            BanEntry {
                banned_at: now,
                expires_at: now + self.config.ban_duration,
            },
        );
    }

    /// Prune expired bans and stale violation windows.
    /// Returns the count of pruned ban entries.
    pub fn prune_expired(&self) -> usize {
        let now = (self.now)();

        // Step 1: prune expired bans
        let mut bans = self.bans.write().expect("bans write lock poisoned");
        let before = bans.len();
        bans.retain(|_, entry| entry.expires_at > now);
        let pruned = before - bans.len();

        // Step 2: prune empty violation windows
        let mut violations = self
            .violations
            .write()
            .expect("violations write lock poisoned");
        violations.retain(|_, window| !window.timestamps.is_empty());

        pruned
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use proptest::prelude::*;
    use std::net::Ipv4Addr;
    use std::sync::atomic::{AtomicU64, Ordering};

    fn test_ip(n: u8) -> IpAddr {
        IpAddr::V4(Ipv4Addr::new(10, 0, 0, n))
    }

    fn default_config() -> TempBanConfig {
        TempBanConfig {
            threshold: 3,
            window: Duration::from_secs(300),
            ban_duration: Duration::from_secs(600),
            max_entries: 10,
        }
    }

    #[test]
    fn not_banned_initially() {
        let list = TempBanList::new(default_config());
        assert!(!list.is_banned(test_ip(1)));
    }

    #[test]
    fn ban_triggers_after_threshold() {
        let list = TempBanList::new(default_config());
        let ip = test_ip(1);
        for _ in 0..3 {
            list.record_violation(ip);
        }
        assert!(list.is_banned(ip));
    }

    #[test]
    fn no_ban_below_threshold() {
        let list = TempBanList::new(default_config());
        let ip = test_ip(1);
        for _ in 0..2 {
            list.record_violation(ip);
        }
        assert!(!list.is_banned(ip));
    }

    #[test]
    fn ban_expires_with_clock_injection() {
        static TICK: AtomicU64 = AtomicU64::new(0);
        TICK.store(0, Ordering::SeqCst);

        fn fake_clock() -> Instant {
            let ticks = TICK.load(Ordering::SeqCst);
            Instant::now() + Duration::from_secs(ticks)
        }

        let config = TempBanConfig {
            threshold: 1,
            window: Duration::from_secs(300),
            ban_duration: Duration::from_secs(60),
            max_entries: 10,
        };
        let list = TempBanList::with_clock(config, fake_clock);
        let ip = test_ip(2);

        list.record_violation(ip);
        assert!(list.is_banned(ip));

        // Advance clock past ban_duration
        TICK.store(61, Ordering::SeqCst);
        assert!(!list.is_banned(ip));
    }

    #[test]
    fn fail_open_when_full() {
        let config = TempBanConfig {
            threshold: 1,
            window: Duration::from_secs(300),
            ban_duration: Duration::from_secs(600),
            max_entries: 2,
        };
        let list = TempBanList::new(config);

        // Fill up the ban list with 2 IPs
        for i in 1..=2u8 {
            list.record_violation(test_ip(i));
        }

        // Third IP should fail-open (not banned, list stays at 2)
        let overflow_ip = test_ip(3);
        list.record_violation(overflow_ip);
        assert!(!list.is_banned(overflow_ip));

        let bans = list.bans.read().unwrap();
        assert_eq!(bans.len(), 2);
    }

    #[test]
    fn prune_expired_returns_count() {
        static TICK: AtomicU64 = AtomicU64::new(0);
        TICK.store(0, Ordering::SeqCst);

        fn fake_clock() -> Instant {
            Instant::now() + Duration::from_secs(TICK.load(Ordering::SeqCst))
        }

        let config = TempBanConfig {
            threshold: 1,
            window: Duration::from_secs(300),
            ban_duration: Duration::from_secs(60),
            max_entries: 10,
        };
        let list = TempBanList::with_clock(config, fake_clock);

        list.record_violation(test_ip(1));
        list.record_violation(test_ip(2));

        // Advance past ban duration
        TICK.store(61, Ordering::SeqCst);

        let pruned = list.prune_expired();
        assert_eq!(pruned, 2);
    }

    // -------------------------------------------------------------------------
    // Property-based tests
    // -------------------------------------------------------------------------

    // Module-level static clock tick used by property tests.
    // Each proptest iteration resets this before use.
    static PROP_TICK: AtomicU64 = AtomicU64::new(0);

    fn prop_clock() -> Instant {
        Instant::now() + Duration::from_secs(PROP_TICK.load(Ordering::SeqCst))
    }

    // Feature: signaling-auth-and-abuse-controls, Property 9: Temp ban triggers after threshold violations
    // Validates: Requirements 6.1
    proptest! {
        #![proptest_config(ProptestConfig::with_cases(100))]

        #[test]
        fn prop_ban_triggers_after_threshold(threshold in 1u32..=10u32) {
            let ip = IpAddr::V4(Ipv4Addr::new(10, 1, 0, 1));

            // --- exactly threshold violations → banned ---
            let config_exact = TempBanConfig {
                threshold,
                window: Duration::from_secs(300),
                ban_duration: Duration::from_secs(600),
                max_entries: 100,
            };
            let list_exact = TempBanList::new(config_exact);
            for _ in 0..threshold {
                list_exact.record_violation(ip);
            }
            prop_assert!(
                list_exact.is_banned(ip),
                "expected banned after exactly {} violations (threshold={})",
                threshold,
                threshold
            );

            // --- threshold - 1 violations → not banned ---
            if threshold > 1 {
                let config_below = TempBanConfig {
                    threshold,
                    window: Duration::from_secs(300),
                    ban_duration: Duration::from_secs(600),
                    max_entries: 100,
                };
                let list_below = TempBanList::new(config_below);
                for _ in 0..(threshold - 1) {
                    list_below.record_violation(ip);
                }
                prop_assert!(
                    !list_below.is_banned(ip),
                    "expected NOT banned after {} violations (threshold={})",
                    threshold - 1,
                    threshold
                );
            }
        }
    }

    // Feature: signaling-auth-and-abuse-controls, Property 10: Temp ban expires after configured duration
    // Validates: Requirements 6.3
    proptest! {
        #![proptest_config(ProptestConfig::with_cases(100))]

        #[test]
        fn prop_ban_expires_after_duration(ban_duration_secs in 1u64..=100u64) {
            // Reset the shared clock to 0 at the start of each iteration.
            PROP_TICK.store(0, Ordering::SeqCst);

            let ip = IpAddr::V4(Ipv4Addr::new(10, 2, 0, 1));
            let config = TempBanConfig {
                threshold: 1,
                window: Duration::from_secs(300),
                ban_duration: Duration::from_secs(ban_duration_secs),
                max_entries: 100,
            };
            let list = TempBanList::with_clock(config, prop_clock);

            // Record one violation → triggers ban (threshold=1)
            list.record_violation(ip);

            // Immediately after ban: should be banned
            prop_assert!(
                list.is_banned(ip),
                "expected banned immediately after violation (ban_duration={}s)",
                ban_duration_secs
            );

            // Advance clock past ban_duration
            PROP_TICK.store(ban_duration_secs + 1, Ordering::SeqCst);

            // After expiry: should no longer be banned
            prop_assert!(
                !list.is_banned(ip),
                "expected NOT banned after {}s elapsed (ban_duration={}s)",
                ban_duration_secs + 1,
                ban_duration_secs
            );
        }
    }

    // Feature: signaling-auth-and-abuse-controls, Property 11: Temp ban list stays bounded
    // Validates: Requirements 6.5, 6.6
    proptest! {
        #![proptest_config(ProptestConfig::with_cases(100))]

        #[test]
        fn prop_ban_list_stays_bounded(max_entries in 1usize..=10usize) {
            let config = TempBanConfig {
                threshold: 1,
                window: Duration::from_secs(300),
                ban_duration: Duration::from_secs(600),
                max_entries,
            };
            let list = TempBanList::new(config);

            // Fill the list with max_entries distinct IPs (each gets 1 violation = threshold)
            for i in 0..max_entries {
                let ip = IpAddr::V4(Ipv4Addr::new(10, 3, 0, i as u8 + 1));
                list.record_violation(ip);
            }

            // Verify the list is exactly at capacity
            {
                let bans = list.bans.read().unwrap();
                prop_assert_eq!(
                    bans.len(),
                    max_entries,
                    "expected {} bans after filling (max_entries={})",
                    max_entries,
                    max_entries
                );
            }

            // Try to add one more IP — should fail-open (list must not grow)
            let overflow_ip = IpAddr::V4(Ipv4Addr::new(10, 3, 1, 1));
            list.record_violation(overflow_ip);

            let bans_len = list.bans.read().unwrap().len();
            prop_assert!(
                bans_len <= max_entries,
                "ban list grew beyond max_entries={}: len={}",
                max_entries,
                bans_len
            );
        }
    }
}
