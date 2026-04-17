use shared::signaling::{self, ParseError, SerializeError, SignalingMessage};
use std::sync::{Arc, Mutex};
use thiserror::Error;

/// Errors that can occur during signaling operations.
#[derive(Debug, Error)]
pub enum SignalingError {
    #[error("failed to serialize message: {0}")]
    Serialize(#[from] SerializeError),
    #[error("failed to parse message: {0}")]
    Parse(#[from] ParseError),
    #[error("WebSocket send failed: {0}")]
    SendFailed(String),
    #[error("JSON nesting depth exceeds limit")]
    JsonTooDeep,
}

/// Maximum allowed JSON nesting depth (braces + brackets).
pub const MAX_JSON_DEPTH: usize = 32;

/// Fast single-pass brace/bracket depth check. Returns `Err(SignalingError::JsonTooDeep)`
/// if the nesting depth of `{`/`[` outside JSON string literals exceeds `max_depth`.
///
/// This is a best-effort pre-filter; serde remains the authoritative JSON parser.
/// Handles `\"` escapes inside strings but not `\uXXXX` unicode escapes.
/// Runs in O(n) time with zero heap allocations.
pub fn check_json_depth(input: &str, max_depth: usize) -> Result<(), SignalingError> {
    let mut depth: usize = 0;
    let mut in_string = false;
    let mut escape_next = false;

    for ch in input.chars() {
        if escape_next {
            escape_next = false;
            continue;
        }
        if ch == '\\' && in_string {
            escape_next = true;
            continue;
        }
        if ch == '"' {
            in_string = !in_string;
            continue;
        }
        if in_string {
            continue;
        }
        match ch {
            '{' | '[' => {
                depth += 1;
                if depth > max_depth {
                    return Err(SignalingError::JsonTooDeep);
                }
            }
            '}' | ']' => {
                depth = depth.saturating_sub(1);
            }
            _ => {}
        }
    }
    Ok(())
}

/// Trait abstracting a WebSocket connection so the signaling client
/// is testable without a real network socket.
pub trait WebSocketConnection: Send + Sync {
    /// Send a text frame over the WebSocket.
    fn send_text(&self, text: &str) -> Result<(), String>;
}

/// Shared callback type for signaling message handlers.
type MessageHandler = Arc<Mutex<Option<Box<dyn Fn(SignalingMessage) + Send + 'static>>>>;

/// Client-side signaling module that sends and receives `SignalingMessage`
/// over a WebSocket connection. All message types come from the shared crate.
pub struct SignalingClient<W: WebSocketConnection> {
    ws: W,
    handler: MessageHandler,
}

impl<W: WebSocketConnection> SignalingClient<W> {
    /// Create a new `SignalingClient` wrapping the given WebSocket connection.
    pub fn new(ws: W) -> Self {
        Self {
            ws,
            handler: Arc::new(Mutex::new(None)),
        }
    }

    /// Serialize a `SignalingMessage` to JSON and send it over the WebSocket.
    pub fn send(&self, msg: &SignalingMessage) -> Result<(), SignalingError> {
        let json = signaling::to_json(msg)?;
        self.ws.send_text(&json).map_err(SignalingError::SendFailed)
    }

    /// Register a callback to be invoked when a signaling message is received.
    pub fn on_message(&self, cb: impl Fn(SignalingMessage) + Send + 'static) {
        let mut handler = self.handler.lock().unwrap();
        *handler = Some(Box::new(cb));
    }

    /// Feed a raw text frame received from the WebSocket into the client.
    /// Parses the JSON and dispatches to the registered handler.
    ///
    /// Runs the JSON depth guard before deserialization to reject deeply
    /// nested payloads that could cause CPU spikes in serde.
    pub fn handle_incoming(&self, text: &str) -> Result<(), SignalingError> {
        if let Err(e @ SignalingError::JsonTooDeep) = check_json_depth(text, MAX_JSON_DEPTH) {
            log::warn!("dropping message: {e}");
            return Err(e);
        }
        let msg = signaling::parse(text)?;
        let handler = self.handler.lock().unwrap();
        if let Some(cb) = handler.as_ref() {
            cb(msg);
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use proptest::prelude::*;

    // ── Reference implementation for property testing ──────────────────
    // Computes the maximum nesting depth of `{`/`[` outside JSON string
    // literals, using the same escape/string-tracking logic as
    // `check_json_depth` but returning the peak depth instead of erroring.
    fn reference_max_depth(input: &str) -> usize {
        let mut depth: usize = 0;
        let mut max: usize = 0;
        let mut in_string = false;
        let mut escape_next = false;

        for ch in input.chars() {
            if escape_next {
                escape_next = false;
                continue;
            }
            if ch == '\\' && in_string {
                escape_next = true;
                continue;
            }
            if ch == '"' {
                in_string = !in_string;
                continue;
            }
            if in_string {
                continue;
            }
            match ch {
                '{' | '[' => {
                    depth += 1;
                    if depth > max {
                        max = depth;
                    }
                }
                '}' | ']' => {
                    depth = depth.saturating_sub(1);
                }
                _ => {}
            }
        }
        max
    }

    // ── Strategy: generate strings from a JSON-relevant alphabet ──────
    fn json_like_string() -> impl Strategy<Value = String> {
        prop::collection::vec(
            prop::sample::select(vec!['{', '}', '[', ']', '"', '\\', 'a', ' ', ':', ',', '1']),
            0..120,
        )
        .prop_map(|chars| chars.into_iter().collect::<String>())
    }

    // ── Property 6: JSON depth guard correctness ──────────────────────
    // **Validates: Requirements 7.1, 7.2, 7.3**
    proptest! {
        #[test]
        fn prop_json_depth_guard_correctness(
            input in json_like_string(),
            max_depth in 0usize..64,
        ) {
            let actual_max = reference_max_depth(&input);
            let result = check_json_depth(&input, max_depth);
            if actual_max <= max_depth {
                prop_assert!(result.is_ok(),
                    "expected Ok for input {:?} (actual_max={}, max_depth={})",
                    input, actual_max, max_depth);
            } else {
                prop_assert!(result.is_err(),
                    "expected Err for input {:?} (actual_max={}, max_depth={})",
                    input, actual_max, max_depth);
            }
        }
    }

    // ── Unit tests: JSON depth guard ──────────────────────────────────
    // **Validates: Requirements 7.1, 7.2, 7.3, 7.4**

    #[test]
    fn depth_1_at_limit() {
        assert!(check_json_depth("{}", 1).is_ok());
    }

    #[test]
    fn depth_1_exceeds_0() {
        assert!(check_json_depth("{}", 0).is_err());
    }

    #[test]
    fn nested_31_ok_with_max_32() {
        let input = "{".repeat(31) + &"}".repeat(31);
        assert!(check_json_depth(&input, 32).is_ok());
    }

    #[test]
    fn nested_32_ok_with_max_32() {
        let input = "{".repeat(32) + &"}".repeat(32);
        assert!(check_json_depth(&input, 32).is_ok());
    }

    #[test]
    fn nested_33_err_with_max_32() {
        let input = "{".repeat(33) + &"}".repeat(33);
        assert!(check_json_depth(&input, 32).is_err());
    }

    #[test]
    fn braces_inside_string_ignored() {
        // 34 braces inside a JSON string literal — should not count toward depth
        let input = r#"{"data":"{{{{{{{{{{{{{{{{{{{{{{{{{{{{{{{{{{"}"#;
        assert!(check_json_depth(input, 32).is_ok());
    }

    #[test]
    fn escaped_quote_inside_string() {
        let input = r#"{"key":"value with \" escaped"}"#;
        assert!(check_json_depth(input, 32).is_ok());
    }

    #[test]
    fn empty_input_ok() {
        assert!(check_json_depth("", 0).is_ok());
        assert!(check_json_depth("", 32).is_ok());
    }

    #[test]
    fn brackets_depth_1() {
        assert!(check_json_depth("[]", 1).is_ok());
    }

    #[test]
    fn mixed_braces_and_brackets() {
        // {[]} is depth 2
        assert!(check_json_depth("{[]}", 2).is_ok());
        assert!(check_json_depth("{[]}", 1).is_err());
    }
}
