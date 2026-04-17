use std::sync::atomic::{AtomicBool, Ordering};

use shared::signaling::{
    InviteCreatedPayload, InviteRevokedPayload, JoinRejectedPayload, JoinedPayload,
    ParticipantInfo, ParticipantJoinedPayload, ParticipantLeftPayload, RoomCreatedPayload,
};
use wavis_client_shared::room_session::SfuConnectionMode;

// ── Invite code redaction ───────────────────────────────────────────

/// Controls whether invite codes are shown in full or redacted.
/// Default: false (codes are redacted).
static SHOW_SECRETS: AtomicBool = AtomicBool::new(false);

/// Set the SHOW_SECRETS flag. Called from `main.rs` when `--show-secrets`
/// is present, or after checking the `WAVIS_SHOW_SECRETS=1` env var.
pub fn set_show_secrets(enabled: bool) {
    SHOW_SECRETS.store(enabled, Ordering::Relaxed);
}

/// Initialise SHOW_SECRETS from the `WAVIS_SHOW_SECRETS` environment variable.
/// Call this early in startup as a fallback when the CLI flag is absent.
pub fn init_show_secrets() {
    if let Ok(val) = std::env::var("WAVIS_SHOW_SECRETS") {
        if val == "1" {
            set_show_secrets(true);
        }
    }
}

/// Redact an invite code: codes ≤ 8 chars become `"****"`,
/// longer codes show `first4…last4`.
pub fn redact_code(code: &str) -> String {
    if code.len() <= 8 {
        "****".to_string()
    } else {
        format!("{}…{}", &code[..4], &code[code.len() - 4..])
    }
}

/// Return the full code when SHOW_SECRETS is true, otherwise redact it.
pub fn display_code(code: &str) -> String {
    if SHOW_SECRETS.load(Ordering::Relaxed) {
        code.to_string()
    } else {
        redact_code(code)
    }
}

/// Print a success message: "OK: {message}"
pub fn ok(message: &str) {
    println!("OK: {message}");
}

/// Print an error message: "ERR: {message}"
pub fn err(message: &str) {
    println!("ERR: {message}");
}

/// Print an async event: "EVENT: {message}"
pub fn event(message: &str) {
    println!("EVENT: {message}");
}

/// Format a Joined response with room_id, peer_id, peer_count, and participants.
pub fn format_joined(payload: &JoinedPayload) -> String {
    let participants = format_participant_list(&payload.participants);
    format!(
        "Joined room {} as peer {} ({} peer(s)) [participants: {}]",
        payload.room_id, payload.peer_id, payload.peer_count, participants
    )
}

/// Format a JoinRejected response with the verbatim reason.
pub fn format_join_rejected(payload: &JoinRejectedPayload) -> String {
    format!("Join rejected: {:?}", payload.reason)
}

/// Format an InviteCreated response with invite_code, expires_in_secs, and max_uses.
pub fn format_invite_created(payload: &InviteCreatedPayload) -> String {
    format!(
        "Invite created: code={} expires_in_secs={} max_uses={}",
        display_code(&payload.invite_code),
        payload.expires_in_secs,
        payload.max_uses
    )
}

/// Format an InviteRevoked response with the invite_code.
pub fn format_invite_revoked(payload: &InviteRevokedPayload) -> String {
    format!("Invite revoked: code={}", payload.invite_code)
}

/// Format a RoomCreated response with room_id, peer_id, and initial invite code.
pub fn format_room_created(payload: &RoomCreatedPayload) -> String {
    format!(
        "Room '{}' created as peer {} — invite code: {} (expires_in_secs={} max_uses={})",
        payload.room_id,
        payload.peer_id,
        display_code(&payload.invite_code),
        payload.expires_in_secs,
        payload.max_uses
    )
}

/// Format a ParticipantJoined event with participant_id and display_name.
pub fn format_participant_joined(payload: &ParticipantJoinedPayload) -> String {
    format!(
        "Participant joined: {} ({})",
        payload.participant_id, payload.display_name
    )
}

/// Format a ParticipantLeft event with participant_id.
pub fn format_participant_left(payload: &ParticipantLeftPayload) -> String {
    format!("Participant left: {}", payload.participant_id)
}

/// Format the status display with room_id, peer_id, participants, and sfu_mode.
pub fn format_status(
    room_id: &str,
    peer_id: &str,
    participants: &[ParticipantInfo],
    sfu_mode: &SfuConnectionMode,
) -> String {
    let mode_name = match sfu_mode {
        SfuConnectionMode::Proxy => "Proxy",
        SfuConnectionMode::LiveKit { .. } => "LiveKit",
    };
    let participant_list = format_participant_list(participants);
    format!(
        "Room: {} | Peer: {} | Participants: {} | Mode: {} | [{}]",
        room_id,
        peer_id,
        participants.len(),
        mode_name,
        participant_list
    )
}

/// Print the help text listing all commands.
pub fn print_help() {
    println!("Available commands:");
    println!("  create <room-id>            Create a new room (no invite needed, you become host)");
    println!("  join <room-id> <invite-code> Join an existing room with an invite code");
    println!("  invite [max-uses]           Generate an invite code (in-room only)");
    println!("  revoke <invite-code>        Revoke an invite code (host only)");
    println!("  leave                       Leave the current room");
    println!("  status                      Show current room status");
    println!("  name <display-name>         Set your display name (set before joining)");
    println!("  volume <0-100>              Set master playback volume (default: 70)");
    println!("  volume <peer> <0-100>       Set volume for a specific peer");
    println!("  help                        Show this help message");
    println!("  quit                        Leave room (if any) and exit");
}

/// Format a list of participants as a comma-separated string showing display names.
fn format_participant_list(participants: &[ParticipantInfo]) -> String {
    if participants.is_empty() {
        return "none".to_string();
    }
    participants
        .iter()
        .map(|p| {
            if p.display_name == p.participant_id || p.display_name.is_empty() {
                p.participant_id.clone()
            } else {
                format!("{} ({})", p.display_name, p.participant_id)
            }
        })
        .collect::<Vec<_>>()
        .join(", ")
}

#[cfg(test)]
mod tests {
    use super::*;
    use proptest::prelude::*;
    use shared::signaling::JoinRejectionReason;
    use std::sync::Mutex;

    /// Mutex to serialize tests that toggle the global SHOW_SECRETS flag.
    /// Without this, parallel test threads race on the AtomicBool and produce
    /// non-deterministic failures.
    static SECRETS_LOCK: Mutex<()> = Mutex::new(());

    // ── Strategies ──────────────────────────────────────────────────────

    /// Non-empty alphanumeric string (1..=20 chars) to avoid substring collisions.
    fn alnum_id() -> impl Strategy<Value = String> {
        "[a-zA-Z0-9]{1,20}"
    }

    fn participant_info_strategy() -> impl Strategy<Value = ParticipantInfo> {
        (alnum_id(), alnum_id()).prop_map(|(participant_id, display_name)| ParticipantInfo {
            participant_id,
            display_name,
            user_id: None,
            profile_color: None,
        })
    }

    fn sfu_mode_strategy() -> impl Strategy<Value = SfuConnectionMode> {
        prop_oneof![
            Just(SfuConnectionMode::Proxy),
            (alnum_id(), alnum_id()).prop_map(|(url, token)| SfuConnectionMode::LiveKit {
                livekit_url: url,
                token,
            }),
        ]
    }

    // ── Property Tests ──────────────────────────────────────────────────

    proptest! {
        /// Feature: interactive-cli-client, Property 2: Joined response output completeness
        ///
        /// *For any* `JoinedPayload` (with arbitrary room_id, peer_id, peer_count,
        /// and participants list), the formatted output string SHALL contain the
        /// room ID, peer ID, peer count value, and every participant ID.
        ///
        /// **Validates: Requirements 2.2, 5.2**
        #[test]
        fn prop_joined_output_completeness(
            room_id in alnum_id(),
            peer_id in alnum_id(),
            peer_count in 1u32..=6,
            participants in prop::collection::vec(participant_info_strategy(), 0..=6),
        ) {
            let payload = JoinedPayload {
                room_id: room_id.clone(),
                peer_id: peer_id.clone(),
                peer_count,
                participants: participants.clone(),
                ice_config: None,
                share_permission: None,
            };
            let out = format_joined(&payload);
            prop_assert!(out.contains(&room_id), "output missing room_id: {}", out);
            prop_assert!(out.contains(&peer_id), "output missing peer_id: {}", out);
            prop_assert!(out.contains(&peer_count.to_string()), "output missing peer_count: {}", out);
            for p in &participants {
                prop_assert!(out.contains(&p.participant_id), "output missing participant_id {}: {}", p.participant_id, out);
            }
        }

        /// Feature: interactive-cli-client, Property 3: JoinRejected verbatim output
        ///
        /// *For any* `JoinRejectedPayload` with an arbitrary rejection reason,
        /// the formatted output string SHALL contain the rejection reason's
        /// display string without modification.
        ///
        /// **Validates: Requirements 2.3, 5.3**
        #[test]
        fn prop_join_rejected_verbatim(reason in any::<JoinRejectionReason>()) {
            let payload = JoinRejectedPayload { reason };
            let out = format_join_rejected(&payload);
            let reason_str = format!("{:?}", payload.reason);
            prop_assert!(out.contains(&reason_str), "output missing reason {:?}: {}", reason_str, out);
        }

        /// Feature: interactive-cli-client, Property 5: InviteCreated output completeness
        ///
        /// *For any* `InviteCreatedPayload` (with arbitrary invite_code,
        /// expires_in_secs, and max_uses), the formatted output string SHALL
        /// contain the invite code (shown via display_code), the expiry duration,
        /// and the max uses value.
        ///
        /// **Validates: Requirements 3.3**
        #[test]
        fn prop_invite_created_completeness(
            invite_code in alnum_id(),
            expires_in_secs in 1u64..=86400,
            max_uses in 1u32..=100,
        ) {
            let _lock = SECRETS_LOCK.lock().unwrap_or_else(|e| e.into_inner());
            // Enable SHOW_SECRETS so the full code appears in output for assertion.
            set_show_secrets(true);
            let payload = InviteCreatedPayload {
                invite_code: invite_code.clone(),
                expires_in_secs,
                max_uses,
            };
            let out = format_invite_created(&payload);
            prop_assert!(out.contains(&invite_code), "output missing invite_code: {}", out);
            prop_assert!(out.contains(&expires_in_secs.to_string()), "output missing expires_in_secs: {}", out);
            prop_assert!(out.contains(&max_uses.to_string()), "output missing max_uses: {}", out);
            // Restore default.
            set_show_secrets(false);
        }

        /// Feature: interactive-cli-client, Property 6: InviteRevoked output completeness
        ///
        /// *For any* `InviteRevokedPayload` (with arbitrary invite_code),
        /// the formatted output string SHALL contain the revoked invite code.
        ///
        /// **Validates: Requirements 4.2**
        #[test]
        fn prop_invite_revoked_completeness(invite_code in alnum_id()) {
            let payload = InviteRevokedPayload { invite_code: invite_code.clone() };
            let out = format_invite_revoked(&payload);
            prop_assert!(out.contains(&invite_code), "output missing invite_code: {}", out);
        }

        /// Feature: interactive-cli-client, Property 7: Status output completeness
        ///
        /// *For any* `ClientState` with a non-None room_id, peer_id, a list of
        /// participants, and an SFU connection mode, the formatted status output
        /// SHALL contain the room ID, peer ID, participant count, SFU mode name,
        /// and every participant ID.
        ///
        /// **Validates: Requirements 7.1, 9.7**
        #[test]
        fn prop_status_output_completeness(
            room_id in alnum_id(),
            peer_id in alnum_id(),
            participants in prop::collection::vec(participant_info_strategy(), 0..=6),
            sfu_mode in sfu_mode_strategy(),
        ) {
            let out = format_status(&room_id, &peer_id, &participants, &sfu_mode);
            prop_assert!(out.contains(&room_id), "output missing room_id: {}", out);
            prop_assert!(out.contains(&peer_id), "output missing peer_id: {}", out);
            prop_assert!(out.contains(&participants.len().to_string()), "output missing participant count: {}", out);
            let mode_name = match &sfu_mode {
                SfuConnectionMode::Proxy => "Proxy",
                SfuConnectionMode::LiveKit { .. } => "LiveKit",
            };
            prop_assert!(out.contains(mode_name), "output missing mode name {}: {}", mode_name, out);
            for p in &participants {
                prop_assert!(out.contains(&p.participant_id), "output missing participant_id {}: {}", p.participant_id, out);
            }
        }

        /// Feature: client-security-hardening, Property 7: Invite code redaction safety
        ///
        /// *For any* invite code string with length > 8, when SHOW_SECRETS is false,
        /// `redact_code(code)` shall produce a string that does not contain the
        /// original code as a substring. For codes of length 8 or fewer,
        /// `redact_code` shall return `"****"`. When SHOW_SECRETS is true,
        /// `display_code(code)` shall return the original code unchanged.
        ///
        /// **Validates: Requirements 6.1, 6.2, 6.3, 6.6**
        #[test]
        fn prop_invite_code_redaction_safety(code in "[a-zA-Z0-9]{1,100}") {
            let _lock = SECRETS_LOCK.lock().unwrap_or_else(|e| e.into_inner());
            // Ensure SHOW_SECRETS is false for redaction tests.
            set_show_secrets(false);

            if code.len() > 8 {
                let redacted = redact_code(&code);
                prop_assert!(
                    !redacted.contains(&code),
                    "redacted output must not contain the original code as a substring: redacted={:?}, code={:?}",
                    redacted, code
                );
            } else {
                let redacted = redact_code(&code);
                prop_assert_eq!(redacted, "****".to_string());
            }

            // When SHOW_SECRETS is true, display_code returns the original.
            set_show_secrets(true);
            let displayed = display_code(&code);
            prop_assert_eq!(displayed, code.clone());

            // When SHOW_SECRETS is false, display_code returns redact_code.
            set_show_secrets(false);
            let displayed = display_code(&code);
            let expected = redact_code(&code);
            prop_assert_eq!(displayed, expected);
        }

        /// Feature: interactive-cli-client, Property 8: ParticipantJoined event output
        ///
        /// *For any* `ParticipantJoinedPayload` (with arbitrary participant_id
        /// and display_name), the formatted event output SHALL contain both the
        /// participant ID and the display name.
        ///
        /// **Validates: Requirements 8.1**
        #[test]
        fn prop_participant_joined_output(
            participant_id in alnum_id(),
            display_name in alnum_id(),
        ) {
            let payload = ParticipantJoinedPayload {
                participant_id: participant_id.clone(),
                display_name: display_name.clone(),
                user_id: None,
                profile_color: None,
            };
            let out = format_participant_joined(&payload);
            prop_assert!(out.contains(&participant_id), "output missing participant_id: {}", out);
            prop_assert!(out.contains(&display_name), "output missing display_name: {}", out);
        }

        /// Feature: interactive-cli-client, Property 9: ParticipantLeft event output
        ///
        /// *For any* `ParticipantLeftPayload` (with arbitrary participant_id),
        /// the formatted event output SHALL contain the participant ID.
        ///
        /// **Validates: Requirements 8.2**
        #[test]
        fn prop_participant_left_output(participant_id in alnum_id()) {
            let payload = ParticipantLeftPayload { participant_id: participant_id.clone() };
            let out = format_participant_left(&payload);
            prop_assert!(out.contains(&participant_id), "output missing participant_id: {}", out);
        }
    }

    // ── Unit Tests ──────────────────────────────────────────────────────

    #[test]
    fn test_format_joined_with_participants() {
        let payload = JoinedPayload {
            room_id: "room-1".to_string(),
            peer_id: "peer-42".to_string(),
            peer_count: 3,
            participants: vec![
                ParticipantInfo {
                    participant_id: "alice".to_string(),
                    display_name: "Alice".to_string(),
                    user_id: None,
                    profile_color: None,
                },
                ParticipantInfo {
                    participant_id: "bob".to_string(),
                    display_name: "Bob".to_string(),
                    user_id: None,
                    profile_color: None,
                },
            ],
            ice_config: None,
            share_permission: None,
        };
        let out = format_joined(&payload);
        assert!(out.contains("room-1"));
        assert!(out.contains("peer-42"));
        assert!(out.contains("3"));
        assert!(out.contains("alice"));
        assert!(out.contains("bob"));
    }

    #[test]
    fn test_format_joined_empty_participants() {
        let payload = JoinedPayload {
            room_id: "r".to_string(),
            peer_id: "p".to_string(),
            peer_count: 1,
            participants: vec![],
            ice_config: None,
            share_permission: None,
        };
        let out = format_joined(&payload);
        assert!(out.contains("r"));
        assert!(out.contains("p"));
        assert!(out.contains("1"));
        assert!(out.contains("none"));
    }

    #[test]
    fn test_format_join_rejected() {
        let payload = JoinRejectedPayload {
            reason: JoinRejectionReason::RoomFull,
        };
        let out = format_join_rejected(&payload);
        assert!(out.contains("RoomFull"));
    }

    #[test]
    fn test_format_invite_created() {
        let _lock = SECRETS_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        // With SHOW_SECRETS=false (default), a 6-char code is redacted to "****".
        set_show_secrets(false);
        let payload = InviteCreatedPayload {
            invite_code: "ABC123".to_string(),
            expires_in_secs: 3600,
            max_uses: 5,
        };
        let out = format_invite_created(&payload);
        assert!(
            out.contains("****"),
            "expected redacted code in output: {}",
            out
        );
        assert!(
            !out.contains("ABC123"),
            "raw code should not appear when redacted: {}",
            out
        );
        assert!(out.contains("3600"));
        assert!(out.contains("5"));

        // With SHOW_SECRETS=true, the full code is shown.
        set_show_secrets(true);
        let out = format_invite_created(&payload);
        assert!(out.contains("ABC123"));
        assert!(out.contains("3600"));
        assert!(out.contains("5"));
        set_show_secrets(false);
    }

    #[test]
    fn test_format_invite_revoked() {
        let payload = InviteRevokedPayload {
            invite_code: "XYZ789".to_string(),
        };
        let out = format_invite_revoked(&payload);
        assert!(out.contains("XYZ789"));
    }

    #[test]
    fn test_format_participant_joined() {
        let payload = ParticipantJoinedPayload {
            participant_id: "user-1".to_string(),
            display_name: "Alice".to_string(),
            user_id: None,
            profile_color: None,
        };
        let out = format_participant_joined(&payload);
        assert!(out.contains("user-1"));
        assert!(out.contains("Alice"));
    }

    #[test]
    fn test_format_participant_left() {
        let payload = ParticipantLeftPayload {
            participant_id: "user-1".to_string(),
        };
        let out = format_participant_left(&payload);
        assert!(out.contains("user-1"));
    }

    #[test]
    fn test_format_status_proxy_mode() {
        let participants = vec![
            ParticipantInfo {
                participant_id: "a".to_string(),
                display_name: "A".to_string(),
                user_id: None,
                profile_color: None,
            },
            ParticipantInfo {
                participant_id: "b".to_string(),
                display_name: "B".to_string(),
                user_id: None,
                profile_color: None,
            },
        ];
        let out = format_status("room-x", "peer-y", &participants, &SfuConnectionMode::Proxy);
        assert!(out.contains("room-x"));
        assert!(out.contains("peer-y"));
        assert!(out.contains("2"));
        assert!(out.contains("Proxy"));
        assert!(out.contains("a"));
        assert!(out.contains("b"));
    }

    #[test]
    fn test_format_status_livekit_mode() {
        let out = format_status(
            "room-z",
            "peer-w",
            &[],
            &SfuConnectionMode::LiveKit {
                livekit_url: "wss://lk.example.com".to_string(),
                token: "tok".to_string(),
            },
        );
        assert!(out.contains("room-z"));
        assert!(out.contains("peer-w"));
        assert!(out.contains("0"));
        assert!(out.contains("LiveKit"));
        assert!(out.contains("none"));
    }

    // ── Invite Code Redaction Unit Tests ─────────────────────────────

    /// **Validates: Requirements 6.1, 6.2, 6.3, 6.6**
    #[test]
    fn test_redact_code_length_1() {
        // len=1 (≤ 8) → "****"
        assert_eq!(redact_code("A"), "****");
    }

    #[test]
    fn test_redact_code_length_8() {
        // len=8 (≤ 8) → "****"
        assert_eq!(redact_code("ABCDEFGH"), "****");
    }

    #[test]
    fn test_redact_code_length_9() {
        // len=9 (> 8) → first4…last4
        let redacted = redact_code("ABCDEFGHI");
        assert_eq!(redacted, "ABCD…FGHI");
        assert!(!redacted.contains("ABCDEFGHI"));
    }

    #[test]
    fn test_redact_code_length_20() {
        // len=20 (> 8) → first4…last4
        let code = "ABCDEFGHIJKLMNOPQRST";
        let redacted = redact_code(code);
        assert_eq!(redacted, "ABCD…QRST");
        assert!(!redacted.contains(code));
    }

    #[test]
    fn test_display_code_show_secrets_toggle() {
        let _lock = SECRETS_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        // With SHOW_SECRETS=true, full codes are returned.
        set_show_secrets(true);
        assert_eq!(display_code("ABCDEFGHIJKL"), "ABCDEFGHIJKL");
        assert_eq!(display_code("SHORT"), "SHORT");

        // With SHOW_SECRETS=false, codes are redacted.
        set_show_secrets(false);
        assert_eq!(display_code("ABCDEFGHIJKL"), "ABCD…IJKL");
        assert_eq!(display_code("SHORT"), "****");
    }
}
