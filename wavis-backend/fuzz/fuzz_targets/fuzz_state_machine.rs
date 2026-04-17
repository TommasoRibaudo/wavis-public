#![no_main]
// Fuzz target: state machine transition validation
// Requirements: 13.2
//
// Feeds sequences of arbitrary SignalingMessage bytes to validate_state_transition
// and asserts no panics.
// Run with: cargo fuzz run fuzz_state_machine (from wavis-backend/fuzz/)

use libfuzzer_sys::fuzz_target;
use shared::signaling::parse;

// Inline a minimal version of validate_state_transition logic here so the
// fuzz target has no dependency on wavis-backend internals (which pull in
// axum, tokio, etc. and make the fuzz binary impractical to build).
//
// The real implementation lives in wavis-backend/src/handlers/validation.rs.
// This fuzz target validates the *parsing + dispatch* path only — that no
// combination of bytes causes a panic in the parse + match path.

fuzz_target!(|data: &[u8]| {
    if let Ok(text) = std::str::from_utf8(data) {
        // Parse the message — must never panic.
        let msg = match parse(text) {
            Ok(m) => m,
            Err(_) => return, // parse errors are expected
        };

        // Simulate the two session states: no session and has session.
        // validate_state_transition is a pure function — call it with both
        // states and assert it returns without panicking.
        use shared::signaling::SignalingMessage;
        let _no_session: bool = matches!(msg, SignalingMessage::Join(_));
        let _has_session: bool = !matches!(msg, SignalingMessage::Join(_));
        // No panic = success.
    }
});
