/// Fail-closed configuration validation for security-critical values.
///
/// All fields are read from environment variables at startup and validated
/// before `AppState` is constructed. Any zero-value field causes a fatal
/// startup error — the backend refuses to run with unsafe defaults.
///
/// Security-critical configuration values validated at startup.
#[derive(Debug, Clone)]
pub struct SecurityConfig {
    pub global_ws_per_sec: u32,
    pub global_joins_per_sec: u32,
    pub invite_ttl_secs: u64,
    pub token_ttl_secs: u64,
    pub ban_duration_secs: u64,
    pub rate_limit_window_secs: u64,
    pub bug_report_rate_limit_max: u32,
    pub bug_report_rate_limit_window_secs: u64,
    pub github_bug_report_token_set: bool,
    pub github_bug_report_repo_set: bool,
}

/// Validates that no security-critical config value is zero.
///
/// Returns `Err(String)` with a descriptive message for the first zero-value
/// field found. Called from `main()` before constructing `AppState`.
pub fn validate_security_config(config: &SecurityConfig) -> Result<(), String> {
    if config.global_ws_per_sec == 0 {
        return Err("GLOBAL_WS_UPGRADES_PER_SEC must be > 0".into());
    }
    if config.global_joins_per_sec == 0 {
        return Err("GLOBAL_JOINS_PER_SEC must be > 0".into());
    }
    if config.invite_ttl_secs == 0 {
        return Err("invite TTL must be > 0".into());
    }
    if config.token_ttl_secs == 0 {
        return Err("token TTL must be > 0".into());
    }
    if config.ban_duration_secs == 0 {
        return Err("ban duration must be > 0".into());
    }
    if config.rate_limit_window_secs == 0 {
        return Err("rate limit window must be > 0".into());
    }
    if config.bug_report_rate_limit_max == 0 {
        return Err("BUG_REPORT_RATE_LIMIT_MAX must be > 0".into());
    }
    if config.bug_report_rate_limit_window_secs == 0 {
        return Err("BUG_REPORT_RATE_LIMIT_WINDOW_SECS must be > 0".into());
    }
    if !config.github_bug_report_token_set {
        return Err("GITHUB_BUG_REPORT_TOKEN must be set and non-empty".into());
    }
    if !config.github_bug_report_repo_set {
        return Err("GITHUB_BUG_REPORT_REPO must be set and non-empty".into());
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Helper: returns a valid config where all fields are non-zero.
    fn valid_config() -> SecurityConfig {
        SecurityConfig {
            global_ws_per_sec: 100,
            global_joins_per_sec: 50,
            invite_ttl_secs: 86400,
            token_ttl_secs: 600,
            ban_duration_secs: 600,
            rate_limit_window_secs: 10,
            bug_report_rate_limit_max: 5,
            bug_report_rate_limit_window_secs: 3600,
            github_bug_report_token_set: true,
            github_bug_report_repo_set: true,
        }
    }

    #[test]
    fn valid_config_passes() {
        assert!(validate_security_config(&valid_config()).is_ok());
    }

    #[test]
    fn zero_global_ws_per_sec_rejected() {
        let mut c = valid_config();
        c.global_ws_per_sec = 0;
        let err = validate_security_config(&c).unwrap_err();
        assert!(err.contains("GLOBAL_WS_UPGRADES_PER_SEC"));
    }

    #[test]
    fn zero_global_joins_per_sec_rejected() {
        let mut c = valid_config();
        c.global_joins_per_sec = 0;
        let err = validate_security_config(&c).unwrap_err();
        assert!(err.contains("GLOBAL_JOINS_PER_SEC"));
    }

    #[test]
    fn zero_invite_ttl_rejected() {
        let mut c = valid_config();
        c.invite_ttl_secs = 0;
        let err = validate_security_config(&c).unwrap_err();
        assert!(err.contains("invite TTL"));
    }

    #[test]
    fn zero_token_ttl_rejected() {
        let mut c = valid_config();
        c.token_ttl_secs = 0;
        let err = validate_security_config(&c).unwrap_err();
        assert!(err.contains("token TTL"));
    }

    #[test]
    fn zero_ban_duration_rejected() {
        let mut c = valid_config();
        c.ban_duration_secs = 0;
        let err = validate_security_config(&c).unwrap_err();
        assert!(err.contains("ban duration"));
    }

    #[test]
    fn zero_rate_limit_window_rejected() {
        let mut c = valid_config();
        c.rate_limit_window_secs = 0;
        let err = validate_security_config(&c).unwrap_err();
        assert!(err.contains("rate limit window"));
    }

    #[test]
    fn zero_bug_report_rate_limit_max_rejected() {
        let mut c = valid_config();
        c.bug_report_rate_limit_max = 0;
        let err = validate_security_config(&c).unwrap_err();
        assert!(err.contains("BUG_REPORT_RATE_LIMIT_MAX"));
    }

    #[test]
    fn zero_bug_report_rate_limit_window_rejected() {
        let mut c = valid_config();
        c.bug_report_rate_limit_window_secs = 0;
        let err = validate_security_config(&c).unwrap_err();
        assert!(err.contains("BUG_REPORT_RATE_LIMIT_WINDOW_SECS"));
    }

    #[test]
    fn empty_github_bug_report_token_rejected() {
        let mut c = valid_config();
        c.github_bug_report_token_set = false;
        let err = validate_security_config(&c).unwrap_err();
        assert!(err.contains("GITHUB_BUG_REPORT_TOKEN"));
    }

    #[test]
    fn empty_github_bug_report_repo_rejected() {
        let mut c = valid_config();
        c.github_bug_report_repo_set = false;
        let err = validate_security_config(&c).unwrap_err();
        assert!(err.contains("GITHUB_BUG_REPORT_REPO"));
    }

    // -----------------------------------------------------------------------
    // Property-based tests
    // -----------------------------------------------------------------------
    use proptest::prelude::*;

    /// Strategy: generate a SecurityConfig with all positive (non-zero) values,
    /// then randomly zero out at least one field via a bitmask.
    fn config_with_at_least_one_zero() -> impl Strategy<Value = SecurityConfig> {
        // 10 fields → bitmask 1..=0b1111111111 guarantees at least one bit set
        (
            1u16..=0b1111111111u16,
            1u32..=10_000u32,
            1u32..=10_000u32,
            1u64..=1_000_000u64,
            1u64..=1_000_000u64,
            1u64..=1_000_000u64,
            1u64..=1_000_000u64,
            1u32..=10_000u32,
            1u64..=1_000_000u64,
        )
            .prop_map(
                |(mask, ws, joins, invite_ttl, token_ttl, ban, window, br_max, br_window)| {
                    SecurityConfig {
                        global_ws_per_sec: if mask & 0b0000000001 != 0 { 0 } else { ws },
                        global_joins_per_sec: if mask & 0b0000000010 != 0 { 0 } else { joins },
                        invite_ttl_secs: if mask & 0b0000000100 != 0 {
                            0
                        } else {
                            invite_ttl
                        },
                        token_ttl_secs: if mask & 0b0000001000 != 0 {
                            0
                        } else {
                            token_ttl
                        },
                        ban_duration_secs: if mask & 0b0000010000 != 0 { 0 } else { ban },
                        rate_limit_window_secs: if mask & 0b0000100000 != 0 { 0 } else { window },
                        bug_report_rate_limit_max: if mask & 0b0001000000 != 0 {
                            0
                        } else {
                            br_max
                        },
                        bug_report_rate_limit_window_secs: if mask & 0b0010000000 != 0 {
                            0
                        } else {
                            br_window
                        },
                        github_bug_report_token_set: mask & 0b0100000000 == 0,
                        github_bug_report_repo_set: mask & 0b1000000000 == 0,
                    }
                },
            )
    }

    fn all_positive_config() -> impl Strategy<Value = SecurityConfig> {
        (
            1u32..=10_000u32,
            1u32..=10_000u32,
            1u64..=1_000_000u64,
            1u64..=1_000_000u64,
            1u64..=1_000_000u64,
            1u64..=1_000_000u64,
            1u32..=10_000u32,
            1u64..=1_000_000u64,
        )
            .prop_map(
                |(ws, joins, invite_ttl, token_ttl, ban, window, br_max, br_window)| {
                    SecurityConfig {
                        global_ws_per_sec: ws,
                        global_joins_per_sec: joins,
                        invite_ttl_secs: invite_ttl,
                        token_ttl_secs: token_ttl,
                        ban_duration_secs: ban,
                        rate_limit_window_secs: window,
                        bug_report_rate_limit_max: br_max,
                        bug_report_rate_limit_window_secs: br_window,
                        github_bug_report_token_set: true,
                        github_bug_report_repo_set: true,
                    }
                },
            )
    }

    // Feature: security-hardening, Property 10: Zero-value security config is rejected
    // Validates: Requirements 6.1, 6.2, 6.3
    proptest! {
        #![proptest_config(ProptestConfig::with_cases(100))]

        #[test]
        fn prop_zero_value_config_rejected(config in config_with_at_least_one_zero()) {
            prop_assert!(
                validate_security_config(&config).is_err(),
                "Expected Err for config with at least one zero field: {:?}",
                config
            );
        }

        #[test]
        fn prop_all_positive_config_accepted(config in all_positive_config()) {
            prop_assert!(
                validate_security_config(&config).is_ok(),
                "Expected Ok for config with all positive fields: {:?}",
                config
            );
        }
    }
}
