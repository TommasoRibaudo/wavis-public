use shared::signaling::{InviteCreatePayload, SignalingMessage};

/// A parsed user command.
#[derive(Debug, Clone, PartialEq)]
pub enum Command {
    Create {
        room_id: String,
    },
    Join {
        room_id: String,
        invite_code: String,
    },
    Invite {
        max_uses: Option<u32>,
    },
    Revoke {
        invite_code: String,
    },
    Leave,
    Status,
    Name {
        new_name: String,
    },
    /// Master volume: `volume <0-100>`
    Volume {
        level: u8,
    },
    /// Per-peer volume: `volume <peer-name> <0-100>`
    PeerVolume {
        peer: String,
        level: u8,
    },
    Help,
    Quit,
}

/// Result of parsing a line of user input.
#[derive(Debug, Clone, PartialEq)]
pub enum ParseResult {
    Ok(Command),
    UnknownCommand(String),
    WrongArgCount { command: String, usage: String },
    EmptyInput,
}

/// Parse a single line of user input into a Command.
///
/// Splits on whitespace, matches the first token case-insensitively,
/// and validates argument count. Pure function with no side effects.
pub fn parse_command(input: &str) -> ParseResult {
    let tokens: Vec<&str> = input.split_whitespace().collect();

    if tokens.is_empty() {
        return ParseResult::EmptyInput;
    }

    let raw = tokens[0].strip_prefix('/').unwrap_or(tokens[0]);
    let cmd = raw.to_lowercase();
    let args = &tokens[1..];

    match cmd.as_str() {
        "create" => {
            if args.len() != 1 {
                return ParseResult::WrongArgCount {
                    command: "create".to_string(),
                    usage: "create <room-id>".to_string(),
                };
            }
            ParseResult::Ok(Command::Create {
                room_id: args[0].to_string(),
            })
        }
        "join" => {
            if args.len() != 2 {
                return ParseResult::WrongArgCount {
                    command: "join".to_string(),
                    usage: "join <room-id> <invite-code>".to_string(),
                };
            }
            ParseResult::Ok(Command::Join {
                room_id: args[0].to_string(),
                invite_code: args[1].to_string(),
            })
        }
        "invite" => {
            if args.len() > 1 {
                return ParseResult::WrongArgCount {
                    command: "invite".to_string(),
                    usage: "invite [max-uses]".to_string(),
                };
            }
            let max_uses = if args.len() == 1 {
                match args[0].parse::<u32>() {
                    Ok(n) => Some(n),
                    Err(_) => {
                        return ParseResult::WrongArgCount {
                            command: "invite".to_string(),
                            usage: "invite [max-uses]  (max-uses must be a positive integer)"
                                .to_string(),
                        };
                    }
                }
            } else {
                None
            };
            ParseResult::Ok(Command::Invite { max_uses })
        }
        "revoke" => {
            if args.len() != 1 {
                return ParseResult::WrongArgCount {
                    command: "revoke".to_string(),
                    usage: "revoke <invite-code>".to_string(),
                };
            }
            ParseResult::Ok(Command::Revoke {
                invite_code: args[0].to_string(),
            })
        }
        "leave" => {
            if !args.is_empty() {
                return ParseResult::WrongArgCount {
                    command: "leave".to_string(),
                    usage: "leave".to_string(),
                };
            }
            ParseResult::Ok(Command::Leave)
        }
        "status" => {
            if !args.is_empty() {
                return ParseResult::WrongArgCount {
                    command: "status".to_string(),
                    usage: "status".to_string(),
                };
            }
            ParseResult::Ok(Command::Status)
        }
        "name" => {
            if args.len() != 1 {
                return ParseResult::WrongArgCount {
                    command: "name".to_string(),
                    usage: "name <display-name>".to_string(),
                };
            }
            ParseResult::Ok(Command::Name {
                new_name: args[0].to_string(),
            })
        }
        "volume" => {
            if args.is_empty() || args.len() > 2 {
                return ParseResult::WrongArgCount {
                    command: "volume".to_string(),
                    usage: "volume <0-100> OR volume <peer> <0-100>".to_string(),
                };
            }
            if args.len() == 1 {
                // Master volume: volume <level>
                match args[0].parse::<u8>() {
                    Ok(n) if n <= 100 => ParseResult::Ok(Command::Volume { level: n }),
                    _ => ParseResult::WrongArgCount {
                        command: "volume".to_string(),
                        usage: "volume <0-100>  (must be an integer 0–100)".to_string(),
                    },
                }
            } else {
                // Per-peer volume: volume <peer> <level>
                match args[1].parse::<u8>() {
                    Ok(n) if n <= 100 => ParseResult::Ok(Command::PeerVolume {
                        peer: args[0].to_string(),
                        level: n,
                    }),
                    _ => ParseResult::WrongArgCount {
                        command: "volume".to_string(),
                        usage: "volume <peer> <0-100>  (level must be an integer 0–100)"
                            .to_string(),
                    },
                }
            }
        }
        "help" => {
            if !args.is_empty() {
                return ParseResult::WrongArgCount {
                    command: "help".to_string(),
                    usage: "help".to_string(),
                };
            }
            ParseResult::Ok(Command::Help)
        }
        "quit" => {
            if !args.is_empty() {
                return ParseResult::WrongArgCount {
                    command: "quit".to_string(),
                    usage: "quit".to_string(),
                };
            }
            ParseResult::Ok(Command::Quit)
        }
        _ => ParseResult::UnknownCommand(tokens[0].to_string()),
    }
}

/// Build an InviteCreate signaling message.
pub fn build_invite_create(max_uses: Option<u32>) -> SignalingMessage {
    SignalingMessage::InviteCreate(InviteCreatePayload { max_uses })
}

#[cfg(test)]
mod tests {
    use super::*;

    // --- parse_command basic tests ---

    #[test]
    fn test_empty_input() {
        assert_eq!(parse_command(""), ParseResult::EmptyInput);
        assert_eq!(parse_command("   "), ParseResult::EmptyInput);
    }

    #[test]
    fn test_create_valid() {
        assert_eq!(
            parse_command("create my-room"),
            ParseResult::Ok(Command::Create {
                room_id: "my-room".to_string()
            })
        );
    }

    #[test]
    fn test_create_case_insensitive() {
        assert_eq!(
            parse_command("CREATE my-room"),
            ParseResult::Ok(Command::Create {
                room_id: "my-room".to_string()
            })
        );
        assert_eq!(
            parse_command("Create my-room"),
            ParseResult::Ok(Command::Create {
                room_id: "my-room".to_string()
            })
        );
    }

    #[test]
    fn test_create_wrong_args() {
        match parse_command("create") {
            ParseResult::WrongArgCount { command, .. } => assert_eq!(command, "create"),
            other => panic!("expected WrongArgCount, got {:?}", other),
        }
        match parse_command("create a b") {
            ParseResult::WrongArgCount { command, .. } => assert_eq!(command, "create"),
            other => panic!("expected WrongArgCount, got {:?}", other),
        }
    }

    #[test]
    fn test_join_valid() {
        assert_eq!(
            parse_command("join room1 abc123"),
            ParseResult::Ok(Command::Join {
                room_id: "room1".to_string(),
                invite_code: "abc123".to_string(),
            })
        );
    }

    #[test]
    fn test_join_wrong_args() {
        match parse_command("join") {
            ParseResult::WrongArgCount { command, .. } => assert_eq!(command, "join"),
            other => panic!("expected WrongArgCount, got {:?}", other),
        }
        match parse_command("join room1") {
            ParseResult::WrongArgCount { command, .. } => assert_eq!(command, "join"),
            other => panic!("expected WrongArgCount, got {:?}", other),
        }
    }

    #[test]
    fn test_invite_no_args() {
        assert_eq!(
            parse_command("invite"),
            ParseResult::Ok(Command::Invite { max_uses: None })
        );
    }

    #[test]
    fn test_invite_with_max_uses() {
        assert_eq!(
            parse_command("invite 5"),
            ParseResult::Ok(Command::Invite { max_uses: Some(5) })
        );
    }

    #[test]
    fn test_invite_invalid_max_uses() {
        match parse_command("invite abc") {
            ParseResult::WrongArgCount { command, .. } => assert_eq!(command, "invite"),
            other => panic!("expected WrongArgCount, got {:?}", other),
        }
    }

    #[test]
    fn test_invite_too_many_args() {
        match parse_command("invite 5 extra") {
            ParseResult::WrongArgCount { command, .. } => assert_eq!(command, "invite"),
            other => panic!("expected WrongArgCount, got {:?}", other),
        }
    }

    #[test]
    fn test_revoke_valid() {
        assert_eq!(
            parse_command("revoke abc123"),
            ParseResult::Ok(Command::Revoke {
                invite_code: "abc123".to_string()
            })
        );
    }

    #[test]
    fn test_revoke_wrong_args() {
        match parse_command("revoke") {
            ParseResult::WrongArgCount { command, .. } => assert_eq!(command, "revoke"),
            other => panic!("expected WrongArgCount, got {:?}", other),
        }
    }

    #[test]
    fn test_no_arg_commands() {
        assert_eq!(parse_command("leave"), ParseResult::Ok(Command::Leave));
        assert_eq!(parse_command("status"), ParseResult::Ok(Command::Status));
        assert_eq!(parse_command("help"), ParseResult::Ok(Command::Help));
        assert_eq!(parse_command("quit"), ParseResult::Ok(Command::Quit));
    }

    #[test]
    fn test_no_arg_commands_with_extra_args() {
        for cmd in &["leave", "status", "help", "quit"] {
            let input = format!("{} extra", cmd);
            match parse_command(&input) {
                ParseResult::WrongArgCount { command, .. } => assert_eq!(command, *cmd),
                other => panic!("expected WrongArgCount for '{}', got {:?}", cmd, other),
            }
        }
    }

    #[test]
    fn test_unknown_command() {
        match parse_command("foobar") {
            ParseResult::UnknownCommand(token) => assert_eq!(token, "foobar"),
            other => panic!("expected UnknownCommand, got {:?}", other),
        }
    }

    #[test]
    fn test_unknown_command_preserves_case() {
        match parse_command("FooBar") {
            ParseResult::UnknownCommand(token) => assert_eq!(token, "FooBar"),
            other => panic!("expected UnknownCommand, got {:?}", other),
        }
    }

    #[test]
    fn test_whitespace_handling() {
        assert_eq!(
            parse_command("  create   my-room  "),
            ParseResult::Ok(Command::Create {
                room_id: "my-room".to_string()
            })
        );
        assert_eq!(
            parse_command("\tjoin\troom1\tabc123\t"),
            ParseResult::Ok(Command::Join {
                room_id: "room1".to_string(),
                invite_code: "abc123".to_string(),
            })
        );
    }

    // --- build_invite_create tests ---

    #[test]
    fn test_build_invite_create_with_max_uses() {
        let msg = build_invite_create(Some(10));
        assert_eq!(
            msg,
            SignalingMessage::InviteCreate(InviteCreatePayload { max_uses: Some(10) })
        );
    }

    #[test]
    fn test_build_invite_create_without_max_uses() {
        let msg = build_invite_create(None);
        assert_eq!(
            msg,
            SignalingMessage::InviteCreate(InviteCreatePayload { max_uses: None })
        );
    }

    // --- Property-based tests ---

    use proptest::prelude::*;

    /// Strategy that generates a whitespace string of spaces and tabs.
    fn whitespace(min_len: usize, max_len: usize) -> impl Strategy<Value = String> {
        prop::collection::vec(prop_oneof![Just(' '), Just('\t')], min_len..=max_len)
            .prop_map(|chars| chars.into_iter().collect())
    }

    /// Strategy for a non-empty token: alphanumeric + hyphens, 1-20 chars.
    fn token() -> impl Strategy<Value = String> {
        "[a-zA-Z0-9][a-zA-Z0-9\\-]{0,19}"
    }

    /// Feature: interactive-cli-client, Property 10: Command parsing — whitespace splitting
    /// Validates: Requirements 10.1
    mod prop_whitespace_splitting {
        use super::*;

        proptest! {
            #[test]
            fn create_with_arbitrary_whitespace(
                leading in whitespace(0, 10),
                mid in whitespace(1, 10),
                trailing in whitespace(0, 10),
                room_id in token(),
            ) {
                let input = format!("{leading}create{mid}{room_id}{trailing}");
                let result = parse_command(&input);
                prop_assert_eq!(
                    result,
                    ParseResult::Ok(Command::Create { room_id: room_id.clone() })
                );
            }

            #[test]
            fn join_with_arbitrary_whitespace(
                leading in whitespace(0, 10),
                mid1 in whitespace(1, 10),
                mid2 in whitespace(1, 10),
                trailing in whitespace(0, 10),
                room_id in token(),
                invite_code in token(),
            ) {
                let input = format!("{leading}join{mid1}{room_id}{mid2}{invite_code}{trailing}");
                let result = parse_command(&input);
                prop_assert_eq!(
                    result,
                    ParseResult::Ok(Command::Join {
                        room_id: room_id.clone(),
                        invite_code: invite_code.clone(),
                    })
                );
            }

            #[test]
            fn invite_with_arbitrary_whitespace(
                leading in whitespace(0, 10),
                mid in whitespace(1, 10),
                trailing in whitespace(0, 10),
                max_uses in 0u32..10000,
            ) {
                let input = format!("{leading}invite{mid}{max_uses}{trailing}");
                let result = parse_command(&input);
                prop_assert_eq!(
                    result,
                    ParseResult::Ok(Command::Invite { max_uses: Some(max_uses) })
                );
            }

            #[test]
            fn invite_no_args_with_whitespace(
                leading in whitespace(0, 10),
                trailing in whitespace(0, 10),
            ) {
                let input = format!("{leading}invite{trailing}");
                let result = parse_command(&input);
                prop_assert_eq!(
                    result,
                    ParseResult::Ok(Command::Invite { max_uses: None })
                );
            }

            #[test]
            fn revoke_with_arbitrary_whitespace(
                leading in whitespace(0, 10),
                mid in whitespace(1, 10),
                trailing in whitespace(0, 10),
                code in token(),
            ) {
                let input = format!("{leading}revoke{mid}{code}{trailing}");
                let result = parse_command(&input);
                prop_assert_eq!(
                    result,
                    ParseResult::Ok(Command::Revoke { invite_code: code.clone() })
                );
            }

            #[test]
            fn no_arg_commands_with_whitespace(
                leading in whitespace(0, 10),
                trailing in whitespace(0, 10),
                cmd_idx in 0usize..4,
            ) {
                let (cmd_name, expected) = match cmd_idx {
                    0 => ("leave", Command::Leave),
                    1 => ("status", Command::Status),
                    2 => ("help", Command::Help),
                    _ => ("quit", Command::Quit),
                };
                let input = format!("{leading}{cmd_name}{trailing}");
                let result = parse_command(&input);
                prop_assert_eq!(result, ParseResult::Ok(expected));
            }

            #[test]
            fn case_insensitive_with_whitespace(
                leading in whitespace(0, 10),
                mid in whitespace(1, 10),
                trailing in whitespace(0, 10),
                room_id in token(),
                upper in prop::bool::ANY,
            ) {
                let cmd = if upper { "CREATE" } else { "Create" };
                let input = format!("{leading}{cmd}{mid}{room_id}{trailing}");
                let result = parse_command(&input);
                prop_assert_eq!(
                    result,
                    ParseResult::Ok(Command::Create { room_id: room_id.clone() })
                );
            }
        }
    }

    /// Feature: interactive-cli-client, Property 11: Unknown command rejection
    /// Validates: Requirements 10.3
    mod prop_unknown_command_rejection {
        use super::*;

        const RECOGNIZED: &[&str] = &[
            "create", "join", "invite", "revoke", "leave", "status", "name", "volume", "help",
            "quit",
        ];

        /// Strategy that generates a non-empty token that is NOT a recognized command (case-insensitive).
        fn unknown_token() -> impl Strategy<Value = String> {
            "[a-zA-Z][a-zA-Z0-9\\-]{0,19}".prop_filter("must not be a recognized command", |s| {
                !RECOGNIZED.contains(&s.to_lowercase().as_str())
            })
        }

        /// Strategy for optional trailing arguments (0-3 space-separated tokens).
        fn optional_args() -> impl Strategy<Value = String> {
            prop::collection::vec(token(), 0..=3).prop_map(|tokens| {
                if tokens.is_empty() {
                    String::new()
                } else {
                    format!(" {}", tokens.join(" "))
                }
            })
        }

        proptest! {
            #[test]
            fn unknown_command_returns_unrecognized_token(
                cmd in unknown_token(),
                args in optional_args(),
            ) {
                let input = format!("{cmd}{args}");
                let result = parse_command(&input);
                prop_assert_eq!(result, ParseResult::UnknownCommand(cmd.clone()));
            }

            #[test]
            fn unknown_command_with_leading_trailing_whitespace(
                leading in whitespace(0, 10),
                cmd in unknown_token(),
                args in optional_args(),
                trailing in whitespace(0, 10),
            ) {
                let input = format!("{leading}{cmd}{args}{trailing}");
                let result = parse_command(&input);
                prop_assert_eq!(result, ParseResult::UnknownCommand(cmd.clone()));
            }
        }
    }

    /// Feature: interactive-cli-client, Property 12: Wrong argument count detection
    /// Validates: Requirements 10.5
    mod prop_wrong_arg_count {
        use super::*;

        /// Strategy that generates extra whitespace-separated tokens (1-5 tokens).
        fn extra_args(count: std::ops::RangeInclusive<usize>) -> impl Strategy<Value = String> {
            prop::collection::vec(token(), count).prop_map(|tokens| tokens.join(" "))
        }

        proptest! {
            // --- create: expects exactly 1 arg ---

            #[test]
            fn create_too_few_args(
                leading in whitespace(0, 5),
                trailing in whitespace(0, 5),
            ) {
                let input = format!("{leading}create{trailing}");
                let result = parse_command(&input);
                match result {
                    ParseResult::WrongArgCount { command, usage } => {
                        prop_assert_eq!(command, "create");
                        prop_assert!(!usage.is_empty());
                    }
                    other => prop_assert!(false, "expected WrongArgCount, got {:?}", other),
                }
            }

            #[test]
            fn create_too_many_args(
                leading in whitespace(0, 5),
                trailing in whitespace(0, 5),
                args in extra_args(2..=5),
            ) {
                let input = format!("{leading}create {args}{trailing}");
                let result = parse_command(&input);
                match result {
                    ParseResult::WrongArgCount { command, usage } => {
                        prop_assert_eq!(command, "create");
                        prop_assert!(!usage.is_empty());
                    }
                    other => prop_assert!(false, "expected WrongArgCount, got {:?}", other),
                }
            }

            // --- join: expects exactly 2 args ---

            #[test]
            fn join_too_few_zero_args(
                leading in whitespace(0, 5),
                trailing in whitespace(0, 5),
            ) {
                let input = format!("{leading}join{trailing}");
                let result = parse_command(&input);
                match result {
                    ParseResult::WrongArgCount { command, usage } => {
                        prop_assert_eq!(command, "join");
                        prop_assert!(!usage.is_empty());
                    }
                    other => prop_assert!(false, "expected WrongArgCount, got {:?}", other),
                }
            }

            #[test]
            fn join_too_few_one_arg(
                leading in whitespace(0, 5),
                trailing in whitespace(0, 5),
                arg in token(),
            ) {
                let input = format!("{leading}join {arg}{trailing}");
                let result = parse_command(&input);
                match result {
                    ParseResult::WrongArgCount { command, usage } => {
                        prop_assert_eq!(command, "join");
                        prop_assert!(!usage.is_empty());
                    }
                    other => prop_assert!(false, "expected WrongArgCount, got {:?}", other),
                }
            }

            #[test]
            fn join_too_many_args(
                leading in whitespace(0, 5),
                trailing in whitespace(0, 5),
                args in extra_args(3..=5),
            ) {
                let input = format!("{leading}join {args}{trailing}");
                let result = parse_command(&input);
                match result {
                    ParseResult::WrongArgCount { command, usage } => {
                        prop_assert_eq!(command, "join");
                        prop_assert!(!usage.is_empty());
                    }
                    other => prop_assert!(false, "expected WrongArgCount, got {:?}", other),
                }
            }

            // --- invite: expects 0 or 1 args, so wrong is 2+ ---

            #[test]
            fn invite_too_many_args(
                leading in whitespace(0, 5),
                trailing in whitespace(0, 5),
                args in extra_args(2..=5),
            ) {
                let input = format!("{leading}invite {args}{trailing}");
                let result = parse_command(&input);
                match result {
                    ParseResult::WrongArgCount { command, usage } => {
                        prop_assert_eq!(command, "invite");
                        prop_assert!(!usage.is_empty());
                    }
                    other => prop_assert!(false, "expected WrongArgCount, got {:?}", other),
                }
            }

            // --- revoke: expects exactly 1 arg ---

            #[test]
            fn revoke_too_few_args(
                leading in whitespace(0, 5),
                trailing in whitespace(0, 5),
            ) {
                let input = format!("{leading}revoke{trailing}");
                let result = parse_command(&input);
                match result {
                    ParseResult::WrongArgCount { command, usage } => {
                        prop_assert_eq!(command, "revoke");
                        prop_assert!(!usage.is_empty());
                    }
                    other => prop_assert!(false, "expected WrongArgCount, got {:?}", other),
                }
            }

            #[test]
            fn revoke_too_many_args(
                leading in whitespace(0, 5),
                trailing in whitespace(0, 5),
                args in extra_args(2..=5),
            ) {
                let input = format!("{leading}revoke {args}{trailing}");
                let result = parse_command(&input);
                match result {
                    ParseResult::WrongArgCount { command, usage } => {
                        prop_assert_eq!(command, "revoke");
                        prop_assert!(!usage.is_empty());
                    }
                    other => prop_assert!(false, "expected WrongArgCount, got {:?}", other),
                }
            }

            // --- leave, status, help, quit: expect exactly 0 args ---

            #[test]
            fn no_arg_commands_with_args(
                leading in whitespace(0, 5),
                trailing in whitespace(0, 5),
                args in extra_args(1..=5),
                cmd_idx in 0usize..4,
            ) {
                let cmd_name = match cmd_idx {
                    0 => "leave",
                    1 => "status",
                    2 => "help",
                    _ => "quit",
                };
                let input = format!("{leading}{cmd_name} {args}{trailing}");
                let result = parse_command(&input);
                match result {
                    ParseResult::WrongArgCount { command, usage } => {
                        prop_assert_eq!(command, cmd_name);
                        prop_assert!(!usage.is_empty());
                    }
                    other => prop_assert!(false, "expected WrongArgCount for '{}', got {:?}", cmd_name, other),
                }
            }
        }
    }

    /// Feature: interactive-cli-client, Property 4: Invite max-uses passthrough
    /// Validates: Requirements 3.1, 3.2
    mod prop_invite_max_uses_passthrough {
        use super::*;
        use shared::signaling::InviteCreatePayload;

        proptest! {
            #[test]
            fn some_max_uses_passes_through(n: u32) {
                let msg = build_invite_create(Some(n));
                prop_assert_eq!(
                    msg,
                    SignalingMessage::InviteCreate(InviteCreatePayload { max_uses: Some(n) })
                );
            }
        }

        #[test]
        fn none_max_uses_passes_through() {
            let msg = build_invite_create(None);
            assert_eq!(
                msg,
                SignalingMessage::InviteCreate(InviteCreatePayload { max_uses: None })
            );
        }
    }
}
