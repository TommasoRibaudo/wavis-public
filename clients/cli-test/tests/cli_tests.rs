//! Tests for the CLI wiring layer: arg parsing, join-wait logic, and
//! TungsteniteWs send/receive behavior via an in-process WebSocket server.
//!
//! Covers the gaps identified after the cli-networked-calls spec:
//! - `parse_args` — all branching paths
//! - `wait_for_joined_result` — joined, server error, channel close, timeout
//! - `TungsteniteWs` — send delivers text frames, receive delivers text frames,
//!   broken connection returns error from send_text

use futures_util::{SinkExt, StreamExt};
use shared::signaling::{
    self, ErrorPayload, JoinedPayload, OfferPayload, SessionDescription, SignalingMessage,
};
use std::net::SocketAddr;
use std::time::Duration;
use tokio::net::TcpListener;
use tokio::sync::mpsc;
use tokio_tungstenite::accept_async;
use tokio_tungstenite::tungstenite::Message;
use wavis_cli_test::{parse_args, wait_for_joined_result, CliArgs, JoinResult, TungsteniteWs};
use wavis_client_shared::signaling::WebSocketConnection;

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

/// Spin up a minimal in-process WebSocket server on a random port.
/// Returns the bound address and a channel that yields one
/// `(sink, stream)` per accepted connection.
async fn start_ws_server() -> (
    SocketAddr,
    mpsc::UnboundedReceiver<
        futures_util::stream::SplitSink<
            tokio_tungstenite::WebSocketStream<tokio::net::TcpStream>,
            Message,
        >,
    >,
    mpsc::UnboundedReceiver<
        futures_util::stream::SplitStream<
            tokio_tungstenite::WebSocketStream<tokio::net::TcpStream>,
        >,
    >,
) {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();

    let (sink_tx, sink_rx) = mpsc::unbounded_channel();
    let (stream_tx, stream_rx) = mpsc::unbounded_channel();

    tokio::spawn(async move {
        while let Ok((tcp, _)) = listener.accept().await {
            let ws = accept_async(tcp).await.unwrap();
            let (sink, stream) = ws.split();
            let _ = sink_tx.send(sink);
            let _ = stream_tx.send(stream);
        }
    });

    (addr, sink_rx, stream_rx)
}

/// Connect a `TungsteniteWs` to the given address and return it along with
/// its incoming-message receiver.
async fn connect_tungstenite(
    addr: SocketAddr,
) -> (TungsteniteWs, tokio::sync::mpsc::Receiver<String>) {
    let url = format!("ws://{addr}");
    let (ws_stream, _) = tokio_tungstenite::connect_async(&url).await.unwrap();
    TungsteniteWs::new(ws_stream)
}

// ---------------------------------------------------------------------------
// parse_args tests
// ---------------------------------------------------------------------------

#[test]
fn parse_args_loopback() {
    let args = vec!["--loopback".to_string()];
    assert_eq!(parse_args(&args), CliArgs::Loopback);
}

#[test]
fn parse_args_server_mode() {
    let args = vec![
        "--server".to_string(),
        "ws://127.0.0.1:3000/ws".to_string(),
        "--room".to_string(),
        "my-room".to_string(),
    ];
    assert_eq!(
        parse_args(&args),
        CliArgs::ServerMode {
            server_url: "ws://127.0.0.1:3000/ws".to_string(),
            room_id: "my-room".to_string(),
            invite_code: None,
            danger_insecure_tls: false,
        }
    );
}

#[test]
fn parse_args_server_without_room() {
    let args = vec!["--server".to_string(), "ws://127.0.0.1:3000/ws".to_string()];
    assert_eq!(
        parse_args(&args),
        CliArgs::MissingRoom {
            server_url: "ws://127.0.0.1:3000/ws".to_string(),
        }
    );
}

#[test]
fn parse_args_no_args_shows_usage() {
    assert_eq!(parse_args(&[]), CliArgs::ShowUsage);
}

#[test]
fn parse_args_unknown_flag_shows_usage() {
    let args = vec!["--unknown".to_string()];
    assert_eq!(parse_args(&args), CliArgs::ShowUsage);
}

#[test]
fn parse_args_room_and_server_order_independent() {
    // --room before --server
    let args = vec![
        "--room".to_string(),
        "r1".to_string(),
        "--server".to_string(),
        "ws://host/ws".to_string(),
    ];
    assert_eq!(
        parse_args(&args),
        CliArgs::ServerMode {
            server_url: "ws://host/ws".to_string(),
            room_id: "r1".to_string(),
            invite_code: None,
            danger_insecure_tls: false,
        }
    );
}

// ---------------------------------------------------------------------------
// wait_for_joined_result tests
// ---------------------------------------------------------------------------

#[tokio::test]
async fn wait_for_joined_returns_payload_on_joined_message() {
    let (tx, mut rx) = tokio::sync::mpsc::channel::<String>(16);

    let joined_msg = SignalingMessage::Joined(JoinedPayload {
        room_id: "room-1".to_string(),
        peer_id: "peer-42".to_string(),
        peer_count: 1,
        participants: vec![],
        ice_config: None,
        share_permission: None,
    });
    tx.try_send(signaling::to_json(&joined_msg).unwrap())
        .unwrap();

    let result = wait_for_joined_result(&mut rx, Duration::from_secs(5)).await;
    assert_eq!(
        result,
        JoinResult::Joined(JoinedPayload {
            room_id: "room-1".to_string(),
            peer_id: "peer-42".to_string(),
            peer_count: 1,
            participants: vec![],
            ice_config: None,
            share_permission: None,
        })
    );
}

#[tokio::test]
async fn wait_for_joined_skips_unexpected_messages_before_joined() {
    let (tx, mut rx) = tokio::sync::mpsc::channel::<String>(16);

    // Send a non-Joined message first, then the real Joined
    let offer = SignalingMessage::Offer(OfferPayload {
        session_description: SessionDescription {
            sdp: "sdp".to_string(),
            sdp_type: "offer".to_string(),
        },
    });
    tx.try_send(signaling::to_json(&offer).unwrap()).unwrap();

    let joined_msg = SignalingMessage::Joined(JoinedPayload {
        room_id: "room-2".to_string(),
        peer_id: "peer-1".to_string(),
        peer_count: 2,
        participants: vec![],
        ice_config: None,
        share_permission: None,
    });
    tx.try_send(signaling::to_json(&joined_msg).unwrap())
        .unwrap();

    let result = wait_for_joined_result(&mut rx, Duration::from_secs(5)).await;
    assert_eq!(
        result,
        JoinResult::Joined(JoinedPayload {
            room_id: "room-2".to_string(),
            peer_id: "peer-1".to_string(),
            peer_count: 2,
            participants: vec![],
            ice_config: None,
            share_permission: None,
        })
    );
}

#[tokio::test]
async fn wait_for_joined_returns_server_error_on_error_message() {
    let (tx, mut rx) = tokio::sync::mpsc::channel::<String>(16);

    let err_msg = SignalingMessage::Error(ErrorPayload {
        message: "room is full".to_string(),
    });
    tx.try_send(signaling::to_json(&err_msg).unwrap()).unwrap();

    let result = wait_for_joined_result(&mut rx, Duration::from_secs(5)).await;
    assert_eq!(result, JoinResult::ServerError("room is full".to_string()));
}

#[tokio::test]
async fn wait_for_joined_returns_channel_closed_when_sender_drops() {
    let (tx, mut rx) = tokio::sync::mpsc::channel::<String>(16);
    drop(tx); // close the channel immediately

    let result = wait_for_joined_result(&mut rx, Duration::from_secs(5)).await;
    assert_eq!(result, JoinResult::ChannelClosed);
}

#[tokio::test]
async fn wait_for_joined_returns_timed_out_on_no_response() {
    let (_tx, mut rx) = tokio::sync::mpsc::channel::<String>(16);
    // tx kept alive but nothing sent — should time out

    let result = wait_for_joined_result(&mut rx, Duration::from_millis(50)).await;
    assert_eq!(result, JoinResult::TimedOut);
}

#[tokio::test]
async fn wait_for_joined_skips_invalid_json_and_continues() {
    let (tx, mut rx) = tokio::sync::mpsc::channel::<String>(16);

    tx.try_send("not valid json {{{{".to_string()).unwrap();
    let joined_msg = SignalingMessage::Joined(JoinedPayload {
        room_id: "r".to_string(),
        peer_id: "p".to_string(),
        peer_count: 1,
        participants: vec![],
        ice_config: None,
        share_permission: None,
    });
    tx.try_send(signaling::to_json(&joined_msg).unwrap())
        .unwrap();

    let result = wait_for_joined_result(&mut rx, Duration::from_secs(5)).await;
    assert!(matches!(result, JoinResult::Joined(_)));
}

// ---------------------------------------------------------------------------
// TungsteniteWs tests
// ---------------------------------------------------------------------------

#[tokio::test]
async fn tungstenite_ws_send_text_delivers_frame_to_server() {
    let (addr, mut sink_rx, mut stream_rx) = start_ws_server().await;
    let (ws, _rx) = connect_tungstenite(addr).await;

    // Give the server time to accept
    let mut server_stream = stream_rx.recv().await.unwrap();
    let _ = sink_rx.recv().await.unwrap(); // consume sink

    ws.send_text("hello from client").unwrap();

    // Server should receive the frame
    let msg = tokio::time::timeout(Duration::from_secs(2), server_stream.next())
        .await
        .expect("timed out")
        .unwrap()
        .unwrap();

    assert_eq!(msg, Message::Text("hello from client".to_string()));
}

#[tokio::test]
async fn tungstenite_ws_receive_delivers_server_frame_to_rx() {
    let (addr, mut sink_rx, mut stream_rx) = start_ws_server().await;
    let (_ws, mut client_rx) = connect_tungstenite(addr).await;

    let mut server_sink = sink_rx.recv().await.unwrap();
    let _ = stream_rx.recv().await.unwrap(); // consume stream

    // Server sends a text frame
    server_sink
        .send(Message::Text("hello from server".to_string()))
        .await
        .unwrap();

    let received = tokio::time::timeout(Duration::from_secs(2), client_rx.recv())
        .await
        .expect("timed out")
        .unwrap();

    assert_eq!(received, "hello from server");
}

#[tokio::test]
async fn tungstenite_ws_multiple_messages_delivered_in_order() {
    let (addr, mut sink_rx, mut stream_rx) = start_ws_server().await;
    let (_ws, mut client_rx) = connect_tungstenite(addr).await;

    let mut server_sink = sink_rx.recv().await.unwrap();
    let _ = stream_rx.recv().await.unwrap();

    for i in 0..5u32 {
        server_sink
            .send(Message::Text(format!("msg-{i}")))
            .await
            .unwrap();
    }

    for i in 0..5u32 {
        let received = tokio::time::timeout(Duration::from_secs(2), client_rx.recv())
            .await
            .expect("timed out")
            .unwrap();
        assert_eq!(received, format!("msg-{i}"));
    }
}

#[tokio::test]
async fn tungstenite_ws_send_text_fails_after_server_closes() {
    let (addr, mut sink_rx, mut stream_rx) = start_ws_server().await;
    let (ws, _rx) = connect_tungstenite(addr).await;

    let mut server_sink = sink_rx.recv().await.unwrap();
    let _ = stream_rx.recv().await.unwrap();

    // Drop the server side entirely — TCP connection goes away
    server_sink.send(Message::Close(None)).await.unwrap();
    drop(server_sink);

    // Pump sends until the write task detects the broken socket and sets
    // writer_alive = false. The channel itself stays open until the write
    // task errors on a real socket write, so we need to actually send.
    let failed = tokio::time::timeout(Duration::from_secs(2), async {
        loop {
            // Each send enqueues to the channel; the write task will error
            // on the actual socket write and flip writer_alive to false.
            let _ = ws.send_text("probe");
            tokio::time::sleep(Duration::from_millis(20)).await;
            if ws.send_text("check").is_err() {
                return true;
            }
        }
    })
    .await;

    assert!(
        failed.unwrap_or(false),
        "expected send_text to eventually return Err after server closed"
    );
}

#[tokio::test]
async fn tungstenite_ws_binary_frames_are_ignored() {
    let (addr, mut sink_rx, mut stream_rx) = start_ws_server().await;
    let (_ws, mut client_rx) = connect_tungstenite(addr).await;

    let mut server_sink = sink_rx.recv().await.unwrap();
    let _ = stream_rx.recv().await.unwrap();

    // Send binary frame (should be ignored) then a text frame
    server_sink
        .send(Message::Binary(vec![0x01, 0x02, 0x03]))
        .await
        .unwrap();
    server_sink
        .send(Message::Text("after-binary".to_string()))
        .await
        .unwrap();

    let received = tokio::time::timeout(Duration::from_secs(2), client_rx.recv())
        .await
        .expect("timed out")
        .unwrap();

    // Binary frame was dropped; only the text frame arrives
    assert_eq!(received, "after-binary");
}

#[tokio::test]
async fn tungstenite_ws_close_frame_stops_receive_channel() {
    let (addr, mut sink_rx, mut stream_rx) = start_ws_server().await;
    let (_ws, mut client_rx) = connect_tungstenite(addr).await;

    let mut server_sink = sink_rx.recv().await.unwrap();
    let _ = stream_rx.recv().await.unwrap();

    server_sink.send(Message::Close(None)).await.unwrap();
    drop(server_sink);

    // Channel should close (recv returns None)
    let result = tokio::time::timeout(Duration::from_secs(2), client_rx.recv())
        .await
        .expect("timed out");

    assert!(result.is_none());
}

// ---------------------------------------------------------------------------
// Property test: WebSocket frame size and channel bound (Property 3)
// ---------------------------------------------------------------------------
//
// **Validates: Requirements 3.2, 3.3, 3.4, 3.5**
//
// For any sequence of incoming text frames, no frame with len() > 128KB
// (MAX_WS_FRAME_LEN) is enqueued, and every discard increments dropped_read
// by exactly 1.

mod ws_frame_size_property {
    use super::*;
    use proptest::prelude::*;
    use std::sync::atomic::Ordering;

    /// The public-facing constant matching the private MAX_WS_FRAME_LEN in lib.rs.
    const MAX_WS_FRAME_LEN: usize = 128 * 1024;

    /// Strategy: generate a Vec of frame sizes. Each size is either:
    /// - "small" (1..=MAX_WS_FRAME_LEN) — should be accepted
    /// - "oversize" (MAX_WS_FRAME_LEN+1 ..= MAX_WS_FRAME_LEN+8192) — should be dropped
    ///
    /// We keep the total count small (1..=12) to keep test runtime reasonable
    /// since each case spins up a real WebSocket server.
    fn frame_sizes_strategy() -> impl Strategy<Value = Vec<usize>> {
        prop::collection::vec(
            prop_oneof![
                // Small frames: 1 byte up to exactly the limit
                (1usize..=MAX_WS_FRAME_LEN),
                // Oversize frames: 1 byte over up to 8KB over
                (MAX_WS_FRAME_LEN + 1..=MAX_WS_FRAME_LEN + 8192),
            ],
            1..=12,
        )
    }

    /// Build a text payload of exactly `size` bytes using a repeating character.
    fn make_payload(size: usize) -> String {
        "x".repeat(size)
    }

    /// Run the property check for a given set of frame sizes.
    /// Extracted so we can call `prop_assert!` in a non-async context
    /// (proptest macros return `Result<(), TestCaseError>`).
    fn check_frame_size_property(sizes: Vec<usize>) -> Result<(), TestCaseError> {
        let rt = tokio::runtime::Runtime::new().unwrap();
        let (received, actual_dropped, expected_accepted_excl_sentinel, expected_dropped) = rt
            .block_on(async {
                let (addr, mut sink_rx, mut _stream_rx) = start_ws_server().await;
                let (ws, mut client_rx) = connect_tungstenite(addr).await;

                let mut server_sink = sink_rx.recv().await.unwrap();
                let _ = _stream_rx.recv().await.unwrap();

                let mut expected_accepted = 0usize;
                let mut expected_dropped = 0u64;

                for &size in &sizes {
                    let payload = make_payload(size);
                    server_sink.send(Message::Text(payload)).await.unwrap();
                    if size > MAX_WS_FRAME_LEN {
                        expected_dropped += 1;
                    } else {
                        expected_accepted += 1;
                    }
                }

                // Sentinel to know when all frames have been processed.
                let sentinel = "__SENTINEL__".to_string();
                server_sink
                    .send(Message::Text(sentinel.clone()))
                    .await
                    .unwrap();

                let mut received: Vec<String> = Vec::new();
                let deadline = tokio::time::Instant::now() + Duration::from_secs(10);
                loop {
                    let remaining = deadline - tokio::time::Instant::now();
                    match tokio::time::timeout(remaining, client_rx.recv()).await {
                        Ok(Some(msg)) => {
                            if msg == sentinel {
                                break;
                            }
                            received.push(msg);
                        }
                        Ok(None) => break,
                        Err(_) => panic!("timed out waiting for sentinel"),
                    }
                }

                let actual_dropped = ws.dropped_read.load(Ordering::Relaxed);
                (
                    received,
                    actual_dropped,
                    expected_accepted,
                    expected_dropped,
                )
            });

        // PROPERTY: no received frame exceeds MAX_WS_FRAME_LEN
        for frame in &received {
            prop_assert!(
                frame.len() <= MAX_WS_FRAME_LEN,
                "received frame of {} bytes exceeds limit of {}",
                frame.len(),
                MAX_WS_FRAME_LEN
            );
        }

        // PROPERTY: received count matches expected accepted count
        prop_assert_eq!(
            received.len(),
            expected_accepted_excl_sentinel,
            "expected {} accepted frames (excl sentinel), got {}",
            expected_accepted_excl_sentinel,
            received.len()
        );

        // PROPERTY: dropped_read incremented exactly once per oversize frame
        prop_assert_eq!(
            actual_dropped,
            expected_dropped,
            "expected dropped_read={}, got {}",
            expected_dropped,
            actual_dropped
        );

        Ok(())
    }

    proptest! {
        #![proptest_config(ProptestConfig::with_cases(15))]

        #[test]
        fn oversize_frames_are_dropped_and_counted(sizes in frame_sizes_strategy()) {
            check_frame_size_property(sizes)?;
        }
    }
}

// ---------------------------------------------------------------------------
// Property test: drop counter monotonicity (Property 8)
// ---------------------------------------------------------------------------
//
// **Validates: Requirements 9.3**
//
// For any sequence of operations, all drop counter values are monotonically
// non-decreasing. We verify this by sending a mix of oversize and normal
// frames through a real WebSocket connection and observing that `dropped_read`
// never decreases between observations.

mod drop_counter_monotonicity_property {
    use super::*;
    use proptest::prelude::*;
    use std::sync::atomic::Ordering;

    /// The public-facing constant matching the private MAX_WS_FRAME_LEN in lib.rs.
    const MAX_WS_FRAME_LEN: usize = 128 * 1024;

    /// Strategy: generate a Vec of frame sizes, each either normal or oversize.
    /// We keep the count small (2..=10) since each case spins up a real WS server.
    fn frame_sizes_strategy() -> impl Strategy<Value = Vec<usize>> {
        prop::collection::vec(
            prop_oneof![
                // Normal frames
                (1usize..=1024),
                // Oversize frames
                (MAX_WS_FRAME_LEN + 1..=MAX_WS_FRAME_LEN + 4096),
            ],
            2..=10,
        )
    }

    fn check_drop_counter_monotonicity(sizes: Vec<usize>) -> Result<(), TestCaseError> {
        let rt = tokio::runtime::Runtime::new().unwrap();
        rt.block_on(async {
            let (addr, mut sink_rx, mut _stream_rx) = start_ws_server().await;
            let (ws, mut client_rx) = connect_tungstenite(addr).await;

            let mut server_sink = sink_rx.recv().await.unwrap();
            let _ = _stream_rx.recv().await.unwrap();

            let mut prev_dropped_read: u64 = 0;

            // Send frames one at a time and observe the counter after each
            for &size in &sizes {
                let payload = "x".repeat(size);
                server_sink
                    .send(tokio_tungstenite::tungstenite::Message::Text(payload))
                    .await
                    .unwrap();

                // Send a small sentinel after each frame so we know it's been processed
                let sentinel = format!("__MONO_SENTINEL_{size}__");
                server_sink
                    .send(tokio_tungstenite::tungstenite::Message::Text(
                        sentinel.clone(),
                    ))
                    .await
                    .unwrap();

                // Wait for the sentinel to arrive (or for the oversize frame to be
                // dropped and the sentinel to come through)
                let deadline = tokio::time::Instant::now() + Duration::from_secs(5);
                loop {
                    let remaining = deadline - tokio::time::Instant::now();
                    match tokio::time::timeout(remaining, client_rx.recv()).await {
                        Ok(Some(msg)) if msg == sentinel => break,
                        Ok(Some(_)) => continue, // normal frame, keep draining
                        Ok(None) => panic!("channel closed unexpectedly"),
                        Err(_) => panic!("timed out waiting for sentinel"),
                    }
                }

                // Observe the counter — it must be >= previous observation
                let current_dropped_read = ws.dropped_read.load(Ordering::Relaxed);
                prop_assert!(
                    current_dropped_read >= prev_dropped_read,
                    "dropped_read decreased from {} to {} after sending frame of size {}",
                    prev_dropped_read,
                    current_dropped_read,
                    size
                );
                prev_dropped_read = current_dropped_read;
            }

            // Verify dropped_write is also observable and consistent (should be 0
            // since we didn't overflow the write channel in this test)
            let final_dropped_write = ws.dropped_write.load(Ordering::Relaxed);
            prop_assert_eq!(
                final_dropped_write,
                0,
                "dropped_write should be 0 when write channel is not overflowed"
            );

            Ok(())
        })
    }

    proptest! {
        #![proptest_config(ProptestConfig::with_cases(15))]

        #[test]
        fn drop_counters_are_monotonically_non_decreasing(sizes in frame_sizes_strategy()) {
            check_drop_counter_monotonicity(sizes)?;
        }
    }
}
