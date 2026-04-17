#![no_main]
// Fuzz target: signaling deserialization
// Requirements: 13.1
//
// Feeds arbitrary bytes to the signaling parser and asserts no panics.
// Run with: cargo fuzz run fuzz_signaling_parse (from shared/fuzz/)

use libfuzzer_sys::fuzz_target;
use shared::signaling::parse;

fuzz_target!(|data: &[u8]| {
    // Only attempt parse if input is valid UTF-8 — the real WS handler
    // receives text frames, so non-UTF-8 bytes are filtered before parse.
    if let Ok(text) = std::str::from_utf8(data) {
        // parse() must never panic regardless of input content.
        // Errors (unknown type, missing fields, etc.) are expected and fine.
        let _ = parse(text);
    }
});
