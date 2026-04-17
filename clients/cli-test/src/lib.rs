//! Testable library surface for the Wavis CLI test client.
//!
//! Exposes `TungsteniteWs`, `wait_for_joined`, and `parse_args` so they
//! can be unit-tested without spawning the full binary.

use futures_util::{SinkExt, StreamExt};
use shared::signaling::{self, JoinedPayload, RoomCreatedPayload, SignalingMessage};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::mpsc::{self, error::TrySendError};
use wavis_client_shared::signaling::WebSocketConnection;

// ---------------------------------------------------------------------------
// TLS helpers
// ---------------------------------------------------------------------------

/// Build a `tokio_tungstenite::Connector` that skips certificate validation.
///
/// Only used when `--danger-insecure-tls` is passed.
pub fn insecure_tls_connector() -> tokio_tungstenite::Connector {
    let tls = native_tls::TlsConnector::builder()
        .danger_accept_invalid_certs(true)
        .build()
        .expect("failed to build insecure TLS connector");
    tokio_tungstenite::Connector::NativeTls(tls)
}

// ---------------------------------------------------------------------------
// WebSocket channel constants
// ---------------------------------------------------------------------------

/// Channel capacity for WS read and write paths.
const WS_CHANNEL_CAPACITY: usize = 512;
/// Max allowed WebSocket text frame length in bytes.
const MAX_WS_FRAME_LEN: usize = 128 * 1024; // 128 KB

// ---------------------------------------------------------------------------
// TungsteniteWs
// ---------------------------------------------------------------------------

/// WebSocket adapter backed by `tokio-tungstenite`.
///
/// Splits the stream into a write task and a read task. The write task owns
/// the sink and forwards messages from `write_tx`. The read task forwards
/// incoming text frames to the returned `Receiver<String>`.
#[derive(Clone)]
pub struct TungsteniteWs {
    pub write_tx: mpsc::Sender<String>,
    pub writer_alive: Arc<AtomicBool>,
    /// Frames dropped on the read path (oversize or channel full).
    pub dropped_read: Arc<AtomicU64>,
    /// Frames dropped on the write path (channel full).
    pub dropped_write: Arc<AtomicU64>,
}

impl TungsteniteWs {
    pub fn new(
        ws_stream: tokio_tungstenite::WebSocketStream<
            tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>,
        >,
    ) -> (Self, mpsc::Receiver<String>) {
        let (mut ws_write, mut ws_read) = ws_stream.split();
        let (write_tx, mut write_rx) = mpsc::channel::<String>(WS_CHANNEL_CAPACITY);
        let (read_tx, read_rx) = mpsc::channel::<String>(WS_CHANNEL_CAPACITY);

        let writer_alive = Arc::new(AtomicBool::new(true));
        let dropped_read = Arc::new(AtomicU64::new(0));
        let dropped_write = Arc::new(AtomicU64::new(0));
        let writer_alive_clone = Arc::clone(&writer_alive);
        tokio::spawn(async move {
            while let Some(text) = write_rx.recv().await {
                if ws_write
                    .send(tokio_tungstenite::tungstenite::Message::Text(text))
                    .await
                    .is_err()
                {
                    writer_alive_clone.store(false, Ordering::Release);
                    break;
                }
            }
            writer_alive_clone.store(false, Ordering::Release);
        });

        let dropped_read_clone = Arc::clone(&dropped_read);
        tokio::spawn(async move {
            while let Some(result) = ws_read.next().await {
                match result {
                    Ok(tokio_tungstenite::tungstenite::Message::Text(text)) => {
                        if text.len() > MAX_WS_FRAME_LEN {
                            dropped_read_clone.fetch_add(1, Ordering::Relaxed);
                            log::warn!("dropped oversize WS frame: {} bytes", text.len());
                            continue;
                        }
                        match read_tx.try_send(text) {
                            Ok(()) => {}
                            Err(TrySendError::Full(_)) => {
                                dropped_read_clone.fetch_add(1, Ordering::Relaxed);
                                log::warn!("dropped WS frame: read channel full");
                            }
                            Err(TrySendError::Closed(_)) => break,
                        }
                    }
                    Ok(tokio_tungstenite::tungstenite::Message::Close(_)) => break,
                    Ok(tokio_tungstenite::tungstenite::Message::Binary(_)) => {
                        log::warn!("unexpected binary WebSocket frame, ignoring");
                    }
                    Ok(_) => {}
                    Err(_) => break,
                }
            }
        });

        (
            Self {
                write_tx,
                writer_alive,
                dropped_read,
                dropped_write,
            },
            read_rx,
        )
    }
}

impl WebSocketConnection for TungsteniteWs {
    fn send_text(&self, text: &str) -> Result<(), String> {
        if !self.writer_alive.load(Ordering::Acquire) {
            return Err("WebSocket connection is broken".to_string());
        }
        match self.write_tx.try_send(text.to_string()) {
            Ok(()) => Ok(()),
            Err(TrySendError::Full(_)) => {
                self.dropped_write.fetch_add(1, Ordering::Relaxed);
                log::warn!("dropped outgoing WS message: write channel full");
                Ok(())
            }
            Err(TrySendError::Closed(_)) => Err("WebSocket send channel closed".to_string()),
        }
    }
}

// ---------------------------------------------------------------------------
// wait_for_joined
// ---------------------------------------------------------------------------

/// Outcome of the join-wait step, extracted for testability.
#[derive(Debug, PartialEq)]
pub enum JoinResult {
    Joined(JoinedPayload),
    ServerError(String),
    ChannelClosed,
    TimedOut,
}

/// Wait for a `Joined` response on `rx`, with a configurable timeout.
///
/// Returns a `JoinResult` instead of calling `process::exit` so tests can
/// assert on every outcome.
pub async fn wait_for_joined_result(
    rx: &mut mpsc::Receiver<String>,
    timeout: Duration,
) -> JoinResult {
    let result = tokio::time::timeout(timeout, async {
        while let Some(text) = rx.recv().await {
            match signaling::parse(&text) {
                Ok(SignalingMessage::Joined(payload)) => return JoinResult::Joined(payload),
                Ok(SignalingMessage::Error(e)) => return JoinResult::ServerError(e.message),
                Ok(_) => {}  // unexpected but non-fatal during join
                Err(_) => {} // parse error — skip
            }
        }
        JoinResult::ChannelClosed
    })
    .await;

    match result {
        Ok(r) => r,
        Err(_) => JoinResult::TimedOut,
    }
}

// ---------------------------------------------------------------------------
// wait_for_room_created
// ---------------------------------------------------------------------------

/// Outcome of the create-room-wait step, extracted for testability.
#[derive(Debug, PartialEq)]
pub enum CreateResult {
    Created(RoomCreatedPayload),
    ServerError(String),
    ChannelClosed,
    TimedOut,
}

/// Wait for a `RoomCreated` response on `rx`, with a configurable timeout.
pub async fn wait_for_room_created(
    rx: &mut mpsc::Receiver<String>,
    timeout: Duration,
) -> CreateResult {
    let result = tokio::time::timeout(timeout, async {
        while let Some(text) = rx.recv().await {
            match signaling::parse(&text) {
                Ok(SignalingMessage::RoomCreated(payload)) => {
                    return CreateResult::Created(payload)
                }
                Ok(SignalingMessage::Error(e)) => return CreateResult::ServerError(e.message),
                Ok(_) => {}
                Err(_) => {}
            }
        }
        CreateResult::ChannelClosed
    })
    .await;

    match result {
        Ok(r) => r,
        Err(_) => CreateResult::TimedOut,
    }
}

// ---------------------------------------------------------------------------
// Arg parsing
// ---------------------------------------------------------------------------

/// Parsed CLI arguments.
#[derive(Debug, PartialEq)]
pub enum CliArgs {
    Loopback,
    ServerMode {
        server_url: String,
        room_id: String,
        invite_code: Option<String>,
        danger_insecure_tls: bool,
    },
    SfuMode {
        server_url: String,
        room_id: String,
        invite_code: Option<String>,
        danger_insecure_tls: bool,
    },
    MissingRoom {
        server_url: String,
    },
    ShowUsage,
}

/// Parse raw CLI argument strings into a `CliArgs` variant.
pub fn parse_args(args: &[String]) -> CliArgs {
    let mut server_url: Option<String> = None;
    let mut room_id: Option<String> = None;
    let mut invite_code: Option<String> = None;
    let mut loopback = false;
    let mut sfu = false;
    let mut danger_insecure_tls = false;

    let mut i = 0;
    while i < args.len() {
        match args[i].as_str() {
            "--loopback" => loopback = true,
            "--sfu" => sfu = true,
            "--danger-insecure-tls" => danger_insecure_tls = true,
            "--server" => {
                i += 1;
                server_url = args.get(i).cloned();
            }
            "--room" => {
                i += 1;
                room_id = args.get(i).cloned();
            }
            "--invite" => {
                i += 1;
                let val = args.get(i).cloned();
                // Treat empty string as no invite code (from launch.json prompt)
                invite_code = val.filter(|s| !s.is_empty());
            }
            _ => {}
        }
        i += 1;
    }

    if loopback {
        return CliArgs::Loopback;
    }
    match server_url {
        Some(url) => match room_id {
            Some(room) => {
                if sfu {
                    CliArgs::SfuMode {
                        server_url: url,
                        room_id: room,
                        invite_code,
                        danger_insecure_tls,
                    }
                } else {
                    CliArgs::ServerMode {
                        server_url: url,
                        room_id: room,
                        invite_code,
                        danger_insecure_tls,
                    }
                }
            }
            None => CliArgs::MissingRoom { server_url: url },
        },
        None => CliArgs::ShowUsage,
    }
}
