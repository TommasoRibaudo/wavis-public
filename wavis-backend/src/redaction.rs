use sha2::{Digest, Sha256};
use std::fmt;

pub struct Sensitive<T>(pub T);

impl<T> fmt::Debug for Sensitive<T> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "[REDACTED]")
    }
}

impl<T> fmt::Display for Sensitive<T> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "[REDACTED]")
    }
}

impl<T> Sensitive<T> {
    pub fn inner(&self) -> &T {
        &self.0
    }
}

#[allow(dead_code)]
pub fn redact_token(token: &str) -> String {
    let prefix = token
        .char_indices()
        .nth(4)
        .map(|(i, _)| &token[..i])
        .unwrap_or(token);
    let hash = Sha256::digest(token.as_bytes());
    let hash_prefix = hex::encode(&hash[..4]);
    format!("{prefix}...{hash_prefix}")
}

#[allow(dead_code)]
pub fn redact_sdp_summary(sdp: &str) -> String {
    let hash = Sha256::digest(sdp.as_bytes());
    let hash_prefix = hex::encode(&hash[..4]);
    format!("sdp({} bytes, hash={hash_prefix})", sdp.len())
}

#[allow(dead_code)]
pub fn redact_ice_summary(candidate: &str) -> String {
    if let Some(pos) = candidate.find("typ ") {
        let rest = &candidate[pos + 4..];
        let typ = rest.split_whitespace().next().unwrap_or("unknown");
        match typ {
            "host" | "srflx" | "relay" | "prflx" => format!("ice(type={typ})"),
            _ => "ice(type=unknown)".to_string(),
        }
    } else {
        "ice(type=unknown)".to_string()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use proptest::prelude::*;

    #[test]
    fn sensitive_debug_redacts() {
        let s = Sensitive("secret");
        assert_eq!(format!("{:?}", s), "[REDACTED]");
    }

    #[test]
    fn sensitive_display_redacts() {
        let s = Sensitive(42u32);
        assert_eq!(format!("{}", s), "[REDACTED]");
    }

    #[test]
    fn sensitive_inner_returns_value() {
        let s = Sensitive("hello");
        assert_eq!(*s.inner(), "hello");
    }

    #[test]
    fn redact_token_long() {
        let result = redact_token("abcdefgh");
        assert!(result.starts_with("abcd..."));
        assert_eq!(result.len(), "abcd...".len() + 8);
    }

    #[test]
    fn redact_token_short() {
        let result = redact_token("ab");
        assert!(result.starts_with("ab..."));
    }

    #[test]
    fn redact_sdp_summary_format() {
        let sdp = "v=0\r\no=- 0 0 IN IP4 127.0.0.1\r\n";
        let result = redact_sdp_summary(sdp);
        assert!(result.starts_with(&format!("sdp({} bytes, hash=", sdp.len())));
    }

    #[test]
    fn redact_ice_known_types() {
        assert_eq!(
            redact_ice_summary("candidate:1 1 UDP 2122252543 192.168.1.1 54321 typ host"),
            "ice(type=host)"
        );
        assert_eq!(
            redact_ice_summary(
                "candidate:2 1 UDP 1685987071 1.2.3.4 54321 typ srflx raddr 0.0.0.0"
            ),
            "ice(type=srflx)"
        );
        assert_eq!(
            redact_ice_summary("candidate:3 1 UDP 33562623 5.6.7.8 3478 typ relay"),
            "ice(type=relay)"
        );
        assert_eq!(
            redact_ice_summary("candidate:4 1 UDP 1000 10.0.0.1 5000 typ prflx"),
            "ice(type=prflx)"
        );
    }

    #[test]
    fn redact_ice_unknown_type() {
        assert_eq!(
            redact_ice_summary("candidate:5 1 UDP 100 1.2.3.4 5000 typ bogus"),
            "ice(type=unknown)"
        );
    }

    #[test]
    fn redact_ice_no_typ() {
        assert_eq!(redact_ice_summary("no type info here"), "ice(type=unknown)");
    }

    // Feature: signaling-auth-and-abuse-controls, Property 12: Sensitive wrapper always redacts
    // Validates: Requirements 7.1, 7.5
    proptest! {
        #![proptest_config(ProptestConfig::with_cases(100))]
        #[test]
        fn prop_sensitive_debug_always_redacts(s in ".+") {
            let wrapped = Sensitive(s.clone());
            let debug_output = format!("{:?}", wrapped);
            prop_assert_eq!(&debug_output, "[REDACTED]");
        }

        #[test]
        fn prop_sensitive_display_always_redacts(s in ".+") {
            let wrapped = Sensitive(s.clone());
            let display_output = format!("{}", wrapped);
            prop_assert_eq!(&display_output, "[REDACTED]");
        }
    }

    // Feature: signaling-auth-and-abuse-controls, Property 13: Redaction helpers never leak raw input
    // Validates: Requirements 7.2, 7.3, 7.4
    proptest! {
        #![proptest_config(ProptestConfig::with_cases(100))]
        #[test]
        fn prop_redact_token_never_leaks(s in ".{5,}") {
            let output = redact_token(&s);
            // Output must be exactly: <first_4_chars>...<8_hex_chars>
            // This fixed-length structure cannot reproduce a token longer than 4 chars.
            let prefix: String = s.chars().take(4).collect();
            let expected_prefix = format!("{prefix}...");
            prop_assert!(output.starts_with(&expected_prefix),
                "output {:?} should start with {:?}", output, expected_prefix);
            // The hash suffix must be exactly 8 lowercase hex chars
            let hash_part = &output[expected_prefix.len()..];
            prop_assert_eq!(hash_part.len(), 8);
            prop_assert!(hash_part.chars().all(|c| c.is_ascii_hexdigit()),
                "hash part {:?} should be hex", hash_part);
            // Output length is fixed: 4-char prefix + "..." (3) + 8-char hash = prefix_bytes + 11
            // so it cannot equal the original input (which has >4 chars beyond the prefix)
            prop_assert_ne!(&output, &s);
        }

        #[test]
        fn prop_redact_sdp_never_leaks(s in ".{10,}") {
            let output = redact_sdp_summary(&s);
            prop_assert!(!output.contains(&s));
        }

        #[test]
        fn prop_redact_ice_never_leaks(s in ".{10,}") {
            let output = redact_ice_summary(&s);
            prop_assert!(!output.contains(&s));
        }
    }
}
