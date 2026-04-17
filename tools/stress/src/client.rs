use std::time::Duration;

use futures_util::stream::{SplitSink, SplitStream};
use futures_util::{SinkExt, StreamExt};
use tokio::net::TcpStream;
use tokio_tungstenite::{MaybeTlsStream, WebSocketStream, connect_async, tungstenite::Message};

pub struct StressClient {
    sink: SplitSink<WebSocketStream<MaybeTlsStream<TcpStream>>, Message>,
    stream: SplitStream<WebSocketStream<MaybeTlsStream<TcpStream>>>,
    pub peer_id: Option<String>,
    pub room_id: Option<String>,
}

pub struct JoinResult {
    pub peer_id: String,
    pub room_id: String,
    pub success: bool,
    pub rejection_reason: Option<String>,
}

#[derive(Debug, thiserror::Error)]
pub enum StressError {
    #[error("WebSocket error: {0}")]
    Ws(#[from] tokio_tungstenite::tungstenite::Error),
    #[error("JSON error: {0}")]
    Json(#[from] serde_json::Error),
    #[error("Timeout waiting for message type: {0}")]
    Timeout(String),
    #[error("Connection closed")]
    Closed,
    #[error("Unexpected message: {0}")]
    Unexpected(String),
}

impl StressClient {
    /// Connect to the backend WebSocket URL. Returns error if connection fails.
    pub async fn connect(url: &str) -> Result<Self, StressError> {
        let (ws_stream, _response) = connect_async(url).await?;
        let (sink, stream) = ws_stream.split();
        Ok(Self {
            sink,
            stream,
            peer_id: None,
            room_id: None,
        })
    }

    /// Try to connect — same as connect but returns StressError instead of panicking.
    pub async fn try_connect(url: &str) -> Result<Self, StressError> {
        Self::connect(url).await
    }

    /// Send a JSON-serializable value as a text WebSocket message.
    pub async fn send_json(&mut self, msg: &serde_json::Value) -> Result<(), StressError> {
        let text = serde_json::to_string(msg)?;
        self.sink.send(Message::Text(text)).await?;
        Ok(())
    }

    /// Send a raw text string (for malformed message tests).
    pub async fn send_raw(&mut self, text: &str) -> Result<(), StressError> {
        self.sink.send(Message::Text(text.to_owned())).await?;
        Ok(())
    }

    /// Wait for a message with the given "type" field, with timeout.
    /// Returns the full JSON value of the matching message.
    pub async fn recv_type(
        &mut self,
        msg_type: &str,
        timeout: Duration,
    ) -> Result<serde_json::Value, StressError> {
        let deadline = tokio::time::timeout(timeout, async {
            loop {
                match self.stream.next().await {
                    Some(Ok(Message::Text(text))) => {
                        let val: serde_json::Value = match serde_json::from_str(&text) {
                            Ok(v) => v,
                            Err(_) => continue,
                        };
                        if val.get("type").and_then(|t| t.as_str()) == Some(msg_type) {
                            return Ok(val);
                        }
                        // Not the type we want — keep looping
                    }
                    Some(Ok(Message::Close(_))) | None => {
                        return Err(StressError::Closed);
                    }
                    Some(Ok(_)) => {
                        // Ping/Pong/Binary — ignore
                    }
                    Some(Err(e)) => {
                        return Err(StressError::Ws(e));
                    }
                }
            }
        });

        match deadline.await {
            Ok(result) => result,
            Err(_elapsed) => Err(StressError::Timeout(msg_type.to_owned())),
        }
    }

    /// Send a Join message and wait for Joined or JoinRejected response.
    /// invite_code: None means no invite code (will get InviteRequired rejection)
    pub async fn join_room(
        &mut self,
        room_id: &str,
        room_type: &str,
        invite_code: Option<&str>,
    ) -> Result<JoinResult, StressError> {
        let msg = serde_json::json!({
            "type": "join",
            "roomId": room_id,
            "roomType": room_type,
            "inviteCode": invite_code,
        });
        self.send_json(&msg).await?;

        let timeout = Duration::from_secs(10);
        let deadline = tokio::time::timeout(timeout, async {
            loop {
                match self.stream.next().await {
                    Some(Ok(Message::Text(text))) => {
                        let val: serde_json::Value = match serde_json::from_str(&text) {
                            Ok(v) => v,
                            Err(_) => continue,
                        };
                        match val.get("type").and_then(|t| t.as_str()) {
                            Some("joined") => {
                                let peer_id = val
                                    .get("peerId")
                                    .and_then(|v| v.as_str())
                                    .unwrap_or("")
                                    .to_owned();
                                let room = val
                                    .get("roomId")
                                    .and_then(|v| v.as_str())
                                    .unwrap_or(room_id)
                                    .to_owned();
                                return Ok(JoinResult {
                                    peer_id,
                                    room_id: room,
                                    success: true,
                                    rejection_reason: None,
                                });
                            }
                            Some("join_rejected") => {
                                let reason = val
                                    .get("reason")
                                    .and_then(|v| v.as_str())
                                    .map(|s| s.to_owned());
                                return Ok(JoinResult {
                                    peer_id: String::new(),
                                    room_id: room_id.to_owned(),
                                    success: false,
                                    rejection_reason: reason,
                                });
                            }
                            // Treat server-side error messages (e.g. global rate
                            // limiter "server busy") as join rejections so the
                            // caller doesn't have to wait for a 10 s timeout.
                            Some("error") => {
                                let reason = val
                                    .get("message")
                                    .and_then(|v| v.as_str())
                                    .map(|s| format!("error: {s}"))
                                    .unwrap_or_else(|| "error".to_owned());
                                return Ok(JoinResult {
                                    peer_id: String::new(),
                                    room_id: room_id.to_owned(),
                                    success: false,
                                    rejection_reason: Some(reason),
                                });
                            }
                            _ => {
                                // Not a join response — keep looping
                            }
                        }
                    }
                    Some(Ok(Message::Close(_))) | None => {
                        return Err(StressError::Closed);
                    }
                    Some(Ok(_)) => {}
                    Some(Err(e)) => {
                        return Err(StressError::Ws(e));
                    }
                }
            }
        });

        match deadline.await {
            Ok(result) => {
                if let Ok(ref r) = result
                    && r.success
                {
                    self.peer_id = Some(r.peer_id.clone());
                    self.room_id = Some(r.room_id.clone());
                }
                result
            }
            Err(_elapsed) => Err(StressError::Timeout("joined/join_rejected".to_owned())),
        }
    }

    /// Wait for a message whose "type" field matches any of the given types, with timeout.
    /// Returns the first matching message.
    pub async fn recv_type_any_of(
        &mut self,
        msg_types: &[&str],
        timeout: Duration,
    ) -> Result<serde_json::Value, StressError> {
        let types: Vec<String> = msg_types.iter().map(|s| s.to_string()).collect();
        let deadline = tokio::time::timeout(timeout, async {
            loop {
                match self.stream.next().await {
                    Some(Ok(Message::Text(text))) => {
                        let val: serde_json::Value = match serde_json::from_str(&text) {
                            Ok(v) => v,
                            Err(_) => continue,
                        };
                        let t = val.get("type").and_then(|t| t.as_str()).unwrap_or("");
                        if types.iter().any(|expected| expected == t) {
                            return Ok(val);
                        }
                    }
                    Some(Ok(Message::Close(_))) | None => {
                        return Err(StressError::Closed);
                    }
                    Some(Ok(_)) => {}
                    Some(Err(e)) => {
                        return Err(StressError::Ws(e));
                    }
                }
            }
        });

        match deadline.await {
            Ok(result) => result,
            Err(_elapsed) => Err(StressError::Timeout(msg_types.join("|"))),
        }
    }

    /// Drain all pending messages until timeout, returning them all.
    pub async fn drain(&mut self, timeout: Duration) -> Vec<serde_json::Value> {
        let mut messages = Vec::new();
        loop {
            match tokio::time::timeout(timeout, self.stream.next()).await {
                Ok(Some(Ok(Message::Text(text)))) => {
                    if let Ok(val) = serde_json::from_str::<serde_json::Value>(&text) {
                        messages.push(val);
                    }
                }
                Ok(Some(Ok(Message::Close(_)))) | Ok(None) => break,
                Ok(Some(Ok(_))) => {}
                Ok(Some(Err(_))) => break,
                Err(_elapsed) => break, // timeout — no more messages
            }
        }
        messages
    }

    /// Close the WebSocket connection gracefully.
    /// Bounded to 2 seconds — on Windows, a graceful close on a forcibly-reset
    /// connection (OS error 10054) can block indefinitely.
    pub async fn close(mut self) {
        let _ = tokio::time::timeout(Duration::from_secs(2), async {
            let _ = self.sink.send(Message::Close(None)).await;
            let _ = self.sink.close().await;
        })
        .await;
    }
}
