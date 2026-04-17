use crate::chat::chat_persistence::ChatMessageRow;
use crate::voice::sfu_relay::OutboundSignal;
use shared::signaling::{
    ChatHistoryMessagePayload, ChatHistoryResponsePayload, ChatMessagePayload, SignalingMessage,
};

pub fn handle_chat_send(
    text: &str,
    participant_id: &str,
    display_name: &str,
    timestamp: &str,
    message_id: &str,
) -> Vec<OutboundSignal> {
    let msg = SignalingMessage::ChatMessage(ChatMessagePayload {
        participant_id: participant_id.to_string(),
        display_name: display_name.to_string(),
        text: text.to_string(),
        timestamp: timestamp.to_string(),
        message_id: Some(message_id.to_string()),
    });
    vec![OutboundSignal::broadcast_all(msg)]
}

/// Convert persistence rows into a `ChatHistoryResponse` signaling message.
pub fn build_history_response(rows: Vec<ChatMessageRow>) -> SignalingMessage {
    let messages = rows
        .into_iter()
        .map(|row| ChatHistoryMessagePayload {
            message_id: row.message_id.to_string(),
            participant_id: row.participant_id,
            display_name: row.display_name,
            text: row.text,
            timestamp: row.created_at.to_rfc3339(),
        })
        .collect();
    SignalingMessage::ChatHistoryResponse(ChatHistoryResponsePayload { messages })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::voice::sfu_relay::SignalTarget;
    use proptest::prelude::*;

    /// Generate a non-empty string of 1..=len arbitrary unicode chars.
    fn arb_nonempty_string(max_len: usize) -> impl Strategy<Value = String> {
        prop::collection::vec(any::<char>(), 1..=max_len)
            .prop_map(|chars| chars.into_iter().collect::<String>())
    }

    /// Generate a plausible ISO 8601 UTC timestamp string.
    fn arb_timestamp() -> impl Strategy<Value = String> {
        (
            2000u32..2100,
            1u32..=12,
            1u32..=28,
            0u32..24,
            0u32..60,
            0u32..60,
        )
            .prop_map(|(y, mo, d, h, mi, s)| {
                format!("{y:04}-{mo:02}-{d:02}T{h:02}:{mi:02}:{s:02}Z")
            })
    }

    // --- Unit tests for edge cases ---

    /// Empty display name is passed through as-is. The domain function does NOT
    /// perform fallback — that responsibility belongs to the handler (ws.rs).
    #[test]
    fn empty_display_name_passed_through() {
        let signals = handle_chat_send("hello", "peer-abc", "", "2025-01-15T10:30:00Z", "msg-001");
        assert_eq!(signals.len(), 1);
        match &signals[0].msg {
            SignalingMessage::ChatMessage(p) => {
                assert_eq!(
                    p.display_name, "",
                    "domain must pass empty display_name through unchanged"
                );
                assert_eq!(p.participant_id, "peer-abc");
            }
            other => panic!("expected ChatMessage, got {:?}", other),
        }
    }

    // Feature: ephemeral-room-chat, Property 4: Domain function produces correct broadcast signal
    // **Validates: Requirements 2.1, 2.2, 2.3**
    proptest! {
        #![proptest_config(ProptestConfig::with_cases(100))]
        #[test]
        fn prop_handle_chat_send_produces_correct_broadcast(
            text in arb_nonempty_string(2000),
            participant_id in arb_nonempty_string(64),
            display_name in ".*",  // any string, including empty
            timestamp in arb_timestamp(),
            message_id in arb_nonempty_string(64),
        ) {
            let signals = handle_chat_send(&text, &participant_id, &display_name, &timestamp, &message_id);

            // Exactly one signal returned.
            prop_assert_eq!(signals.len(), 1, "expected exactly 1 signal, got {}", signals.len());

            let signal = &signals[0];

            // Target must be BroadcastAll.
            prop_assert!(
                matches!(signal.target, SignalTarget::BroadcastAll),
                "expected BroadcastAll target, got {:?}", signal.target
            );

            // Inner message must be ChatMessage with all fields matching inputs.
            match &signal.msg {
                SignalingMessage::ChatMessage(payload) => {
                    prop_assert_eq!(&payload.participant_id, &participant_id);
                    prop_assert_eq!(&payload.display_name, &display_name);
                    prop_assert_eq!(&payload.text, &text);
                    prop_assert_eq!(&payload.timestamp, &timestamp);
                    prop_assert_eq!(payload.message_id.as_deref(), Some(message_id.as_str()),
                        "message_id must be included in the ChatMessage payload");
                }
                other => prop_assert!(false, "expected ChatMessage, got {:?}", other),
            }
        }
    }
}
