//! Size guards for SDP and ICE candidate payloads.
//!
//! These guards reject oversized payloads before they reach the WebRTC backend,
//! protecting against resource exhaustion from a compromised or misbehaving server.

use log::warn;
use shared::signaling::IceCandidate;

/// Maximum allowed SDP body size in bytes (64 KB).
pub const MAX_SDP_SIZE: usize = 64 * 1024;

/// Maximum allowed ICE candidate string size in bytes (2 KB).
pub const MAX_ICE_CANDIDATE_SIZE: usize = 2 * 1024;

/// Returns `true` if the SDP is within the size limit.
/// Logs a warning and returns `false` if oversize.
pub fn check_sdp_size(sdp: &str) -> bool {
    if sdp.len() > MAX_SDP_SIZE {
        warn!(
            "dropping oversize SDP: {} bytes (max {})",
            sdp.len(),
            MAX_SDP_SIZE
        );
        return false;
    }
    true
}

/// Returns `true` if the ICE candidate is within the size limit.
/// Logs a warning and returns `false` if oversize.
pub fn check_ice_candidate_size(candidate: &IceCandidate) -> bool {
    if candidate.candidate.len() > MAX_ICE_CANDIDATE_SIZE {
        warn!(
            "dropping oversize ICE candidate: {} bytes (max {})",
            candidate.candidate.len(),
            MAX_ICE_CANDIDATE_SIZE
        );
        return false;
    }
    true
}

#[cfg(test)]
mod tests {
    use super::*;
    use proptest::prelude::*;
    use proptest::proptest;

    #[test]
    fn sdp_within_limit_returns_true() {
        let sdp = "v=0\r\no=- 123 456 IN IP4 0.0.0.0\r\n";
        assert!(check_sdp_size(sdp));
    }

    #[test]
    fn sdp_at_exact_limit_returns_true() {
        let sdp = "x".repeat(MAX_SDP_SIZE);
        assert!(check_sdp_size(&sdp));
    }

    #[test]
    fn sdp_one_byte_over_returns_false() {
        let sdp = "x".repeat(MAX_SDP_SIZE + 1);
        assert!(!check_sdp_size(&sdp));
    }

    #[test]
    fn sdp_empty_returns_true() {
        assert!(check_sdp_size(""));
    }

    #[test]
    fn ice_within_limit_returns_true() {
        let candidate = IceCandidate {
            candidate: "candidate:1 1 UDP 2130706431 192.168.1.1 5000 typ host".into(),
            sdp_mid: "0".into(),
            sdp_mline_index: 0,
        };
        assert!(check_ice_candidate_size(&candidate));
    }

    #[test]
    fn ice_at_exact_limit_returns_true() {
        let candidate = IceCandidate {
            candidate: "x".repeat(MAX_ICE_CANDIDATE_SIZE),
            sdp_mid: "0".into(),
            sdp_mline_index: 0,
        };
        assert!(check_ice_candidate_size(&candidate));
    }

    #[test]
    fn ice_one_byte_over_returns_false() {
        let candidate = IceCandidate {
            candidate: "x".repeat(MAX_ICE_CANDIDATE_SIZE + 1),
            sdp_mid: "0".into(),
            sdp_mline_index: 0,
        };
        assert!(!check_ice_candidate_size(&candidate));
    }

    #[test]
    fn ice_empty_candidate_returns_true() {
        let candidate = IceCandidate {
            candidate: String::new(),
            sdp_mid: "0".into(),
            sdp_mline_index: 0,
        };
        assert!(check_ice_candidate_size(&candidate));
    }

    // -----------------------------------------------------------------------
    // Property 4: SDP size guard
    // For any SDP string, check_sdp_size returns false iff len > MAX_SDP_SIZE.
    // **Validates: Requirements 4.1, 4.2, 4.4**
    // -----------------------------------------------------------------------

    proptest! {
        #![proptest_config(ProptestConfig { cases: 100, .. ProptestConfig::default() })]

        #[test]
        fn prop_sdp_size_guard(len in 0usize..=(MAX_SDP_SIZE + 4096)) {
            let sdp = "a".repeat(len);
            let result = check_sdp_size(&sdp);
            if len > MAX_SDP_SIZE {
                prop_assert!(!result, "expected false for SDP len {}", len);
            } else {
                prop_assert!(result, "expected true for SDP len {}", len);
            }
        }
    }

    // -----------------------------------------------------------------------
    // Property 5: ICE candidate size guard
    // For any ICE candidate, check_ice_candidate_size returns false iff
    // candidate.len() > MAX_ICE_CANDIDATE_SIZE.
    // **Validates: Requirements 5.1, 5.2, 5.4**
    // -----------------------------------------------------------------------

    proptest! {
        #![proptest_config(ProptestConfig { cases: 100, .. ProptestConfig::default() })]

        #[test]
        fn prop_ice_candidate_size_guard(
            len in 0usize..=(MAX_ICE_CANDIDATE_SIZE + 4096),
            sdp_mid in "[a-z]{0,4}",
            sdp_mline_index in 0u16..=10,
        ) {
            let candidate = IceCandidate {
                candidate: "b".repeat(len),
                sdp_mid,
                sdp_mline_index,
            };
            let result = check_ice_candidate_size(&candidate);
            if len > MAX_ICE_CANDIDATE_SIZE {
                prop_assert!(!result, "expected false for ICE candidate len {}", len);
            } else {
                prop_assert!(result, "expected true for ICE candidate len {}", len);
            }
        }
    }
}
