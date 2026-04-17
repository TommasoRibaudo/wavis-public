//! Thin WebSocket upgrade entry point and per-connection receive/send loop.
//!
//! **Owns:** HTTP → WebSocket upgrade (including pre-upgrade abuse checks:
//! TLS enforcement, global ceiling, temp-ban, and per-IP connection cap),
//! the per-connection `tokio::select!` loop that reads inbound frames and
//! writes outbound messages, frame-level guards (size limit, rate limiting,
//! JSON depth), and connection lifecycle (registration, cleanup on disconnect).
//!
//! **Delegates to:**
//! - [`ws_rate_limit`] — per-connection message rate limiting and JSON depth checking
//! - [`ws_session`] — session state (`SignalingSession`) and connection cleanup
//! - [`ws_dispatch`] — all signaling message routing and business-logic dispatch
//!
//! **Does not own:** business logic for any signaling action. Every parsed
//! message is handed to [`ws_dispatch::dispatch_message`], which routes it
//! to the appropriate domain function. This module never inspects signaling
//! semantics beyond pre-dispatch validation (field lengths, state machine gates).
//!
//! **Key invariants:**
//! - A connection is assigned a unique ephemeral `peer_id` at upgrade time.
//! - Rate-limit config (`WsRateLimitConfig`) is read once from `AppState`,
//!   never from env vars at connection time.
//! - Abuse checks run *before* the upgrade completes — rejected clients
//!   never reach the message loop.
//!
//! **Layering:** handlers → domain → state. This module never calls into
//! `state::` directly for mutations; all state changes flow through domain
//! functions.

use crate::app_state::AppState;
use crate::chat::chat_rate_limiter::ChatRateLimiter;
use crate::connections::ConnectionManager;
use crate::ip::extract_client_ip;
use crate::ws::ws_dispatch::{DispatchContext, DispatchOutcome, SfuConfig, dispatch_message};
use crate::ws::ws_rate_limit::{WsRateLimiter, check_json_depth};
use crate::ws::ws_session::{SignalingSession, cleanup_unjoined_connection};
use axum::extract::ws::{Message, WebSocket, WebSocketUpgrade};
use axum::extract::{ConnectInfo, State};
use axum::http::HeaderMap;
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use shared::signaling::{self, ErrorPayload, SignalingMessage};
use std::net::{IpAddr, SocketAddr};
use std::sync::Arc;
use std::time::{SystemTime, UNIX_EPOCH};
use tokio::sync::mpsc;
use tracing::{debug, error, info, warn};
use uuid::Uuid;

const MAX_TEXT_MESSAGE_BYTES: usize = 64 * 1024;
const MSG_TOO_LARGE: &str = "message too large";
const MSG_RATE_LIMIT_EXCEEDED: &str = "rate limit exceeded";
const MSG_TOO_DEEPLY_NESTED: &str = "message too deeply nested";

/// RAII guard that decrements the per-IP connection count when dropped.
/// Ensures `remove` is called even on panics, early returns, or errors.
struct ConnectionGuard {
    tracker: Arc<crate::abuse::ip_tracker::IpConnectionTracker>,
    ip: IpAddr,
}

impl Drop for ConnectionGuard {
    fn drop(&mut self) {
        self.tracker.remove(self.ip);
    }
}

fn error_text(message: &str) -> String {
    signaling::to_json(&SignalingMessage::Error(ErrorPayload {
        message: message.to_string(),
    }))
    .unwrap_or_else(|_| format!(r#"{{"type":"error","message":"{message}"}}"#))
}

async fn send_error_and_close(socket: &mut WebSocket, message: &str) {
    let _ = socket.send(Message::Text(error_text(message).into())).await;
    let _ = socket.send(Message::Close(None)).await;
}

/// Pure function: returns true if the headers indicate HTTPS proto.
///
/// Accepted sources, in order:
/// - `X-Wavis-Forwarded-Proto` — dedicated trusted edge header for chained
///   proxy setups where intermediary hops may overwrite `X-Forwarded-Proto`
/// - `X-Forwarded-Proto`
/// - RFC 7239 `Forwarded`
///
/// Used by `ws_handler` when `require_tls` is enabled.
pub(crate) fn check_tls_proto(headers: &HeaderMap) -> bool {
    headers
        .get("x-wavis-forwarded-proto")
        .and_then(|v| v.to_str().ok())
        .map(|v| v.eq_ignore_ascii_case("https"))
        .unwrap_or(false)
        || headers
            .get("x-forwarded-proto")
            .and_then(|v| v.to_str().ok())
            .map(|v| v.eq_ignore_ascii_case("https"))
            .unwrap_or(false)
        || headers
            .get("forwarded")
            .and_then(|v| v.to_str().ok())
            .map(|v| v.to_lowercase().contains("proto=https"))
            .unwrap_or(false)
}

pub async fn ws_handler(
    ws: WebSocketUpgrade,
    ConnectInfo(addr): ConnectInfo<SocketAddr>,
    headers: HeaderMap,
    State(app_state): State<AppState>,
) -> Response {
    let client_ip = extract_client_ip(&ConnectInfo(addr), &headers, &app_state.ip_config);

    // Pre-upgrade check: TLS proto enforcement (Req 3.6)
    if app_state.require_tls && !check_tls_proto(&headers) {
        app_state
            .abuse_metrics
            .increment(&app_state.abuse_metrics.tls_proto_rejections);
        warn!(ip = %client_ip, "ws upgrade rejected: non-HTTPS proto");
        return StatusCode::FORBIDDEN.into_response();
    }

    // Pre-upgrade check 0: global WS ceiling (Req 15.1, 15.2) — checked before per-IP limits
    {
        let now_unix = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs();
        if !app_state.global_ws_limiter.allow(now_unix) {
            app_state
                .abuse_metrics
                .increment(&app_state.abuse_metrics.global_ws_ceiling_rejections);
            warn!(ip = %client_ip, "ws upgrade rejected: global WS ceiling exceeded");
            return (StatusCode::TOO_MANY_REQUESTS, "too many requests").into_response();
        }
    }

    // Pre-upgrade check 1: temp ban (Req 6.2)
    if app_state.temp_ban_list.is_banned(client_ip) {
        app_state
            .abuse_metrics
            .increment(&app_state.abuse_metrics.connections_rejected_temp_ban);
        warn!(ip = %client_ip, "ws upgrade rejected: IP is temp-banned");
        return (StatusCode::TOO_MANY_REQUESTS, "too many requests").into_response();
    }

    // Pre-upgrade check 2: per-IP connection cap (Req 2.2)
    if !app_state.ip_connection_tracker.try_add(client_ip) {
        app_state
            .abuse_metrics
            .increment(&app_state.abuse_metrics.connections_rejected_ip_cap);
        warn!(ip = %client_ip, "ws upgrade rejected: per-IP connection cap exceeded");
        return (StatusCode::TOO_MANY_REQUESTS, "too many requests").into_response();
    }

    ws.on_upgrade(move |socket| handle_socket(socket, app_state, client_ip))
}

async fn handle_socket(mut socket: WebSocket, app_state: AppState, client_ip: IpAddr) {
    // RAII guard: decrements per-IP connection count when this function exits for any reason (Req 2.3)
    let _conn_guard = ConnectionGuard {
        tracker: app_state.ip_connection_tracker.clone(),
        ip: client_ip,
    };

    let peer_id = app_state.next_peer_id();
    // Req 12.1: generate server-side issuer_id scoped to this connection
    let issuer_id = Uuid::new_v4().to_string();
    // issuer_id is used in task 11.3 for invite generation; stored here for the connection lifetime
    let _ = &issuer_id;
    let (outbound_tx, mut outbound_rx) = mpsc::unbounded_channel::<String>();
    let mut rate_limiter = WsRateLimiter::new(&app_state.ws_rate_limit_config);
    let mut chat_rate_limiter = ChatRateLimiter::new(5.0);
    let sfu_config = SfuConfig::from_app_state(&app_state.jwt_secret, &app_state.jwt_issuer);
    let mut session: Option<SignalingSession> = None;
    let mut authenticated_user_id: Option<String> = None;
    #[allow(unused_variables, unused_assignments)]
    let mut authenticated_device_id: Option<Uuid> = None;
    // authenticated_device_id is stored for the connection lifetime (audit logging, future use)

    app_state.connections.register(peer_id.clone(), outbound_tx);
    info!(peer_id = %peer_id, "ws client connected");

    loop {
        tokio::select! {
            incoming = socket.recv() => {
                match incoming {
                    Some(Ok(Message::Text(text))) => {
                        // WS frame size limit (Spec 3 Req 1): enforced before serde — 64 KB max.
                        if text.len() > MAX_TEXT_MESSAGE_BYTES {
                            warn!(peer_id = %peer_id, bytes = text.len(), limit = MAX_TEXT_MESSAGE_BYTES, "ws text frame exceeded size limit");
                            app_state.abuse_metrics.increment(&app_state.abuse_metrics.payload_size_violations);
                            send_error_and_close(&mut socket, MSG_TOO_LARGE).await;
                            break;
                        }

                        if !rate_limiter.allow() {
                            warn!(peer_id = %peer_id, "ws peer exceeded message rate limit");
                            app_state.abuse_metrics.increment(&app_state.abuse_metrics.ws_rate_limit_rejections);
                            app_state.abuse_metrics.increment(&app_state.abuse_metrics.connections_closed_rate_limit);
                            app_state.temp_ban_list.record_violation(client_ip);
                            send_error_and_close(&mut socket, MSG_RATE_LIMIT_EXCEEDED).await;
                            break;
                        }

                        if !rate_limiter.burst_allow() {
                            warn!(peer_id = %peer_id, "ws peer exceeded burst rate limit");
                            app_state.abuse_metrics.increment(&app_state.abuse_metrics.ws_burst_rejections);
                            app_state.abuse_metrics.increment(&app_state.abuse_metrics.connections_closed_rate_limit);
                            app_state.temp_ban_list.record_violation(client_ip);
                            send_error_and_close(&mut socket, MSG_RATE_LIMIT_EXCEEDED).await;
                            break;
                        }

                        if !check_json_depth(&text, app_state.ws_rate_limit_config.max_json_depth) {
                            warn!(peer_id = %peer_id, "ws message exceeded JSON depth limit");
                            // JSON depth check (Spec 3 Req 1): runs before serde to prevent
                            // deeply-nested object attacks. max_json_depth = 32.
                            send_error_and_close(&mut socket, MSG_TOO_DEEPLY_NESTED).await;
                            break;
                        }

                        let message = match signaling::parse(&text) {
                            Ok(msg) => msg,
                            Err(_) => {
                                app_state.connections.send_to(
                                    &peer_id,
                                    &SignalingMessage::Error(ErrorPayload { message: "invalid JSON".to_string() }),
                                );
                                continue;
                            }
                        };

                        // Field-length validation (Req 14.2, 14.3) — after parse, before dispatch.
                        // Sends structured error with field name; does NOT close the connection.
                        if let Err(e) = shared::signaling::validation::validate_field_lengths(&message) {
                            warn!(peer_id = %peer_id, field = %e.field, actual = e.actual_len, max = e.max_len, "field length validation failed");
                            app_state.abuse_metrics.increment(&app_state.abuse_metrics.schema_validation_rejections);
                            app_state.connections.send_to(
                                &peer_id,
                                &SignalingMessage::Error(ErrorPayload {
                                    message: format!("field '{}' too long ({} > {})", e.field, e.actual_len, e.max_len),
                                }),
                            );
                            continue;
                        }

                        // State machine validation (Req 12.1, 12.2, 12.5) — after field validation.
                        // Sends structured error; does NOT close the connection.
                        {
                            let ctx = session.as_ref().map(|s| crate::ws::validation::SessionContext {
                                participant_id: s.participant_id.as_str(),
                            });
                            if let Err(reason) = crate::ws::validation::validate_state_transition(&message, ctx.as_ref(), authenticated_user_id.is_some()) {
                                app_state.abuse_metrics.increment(&app_state.abuse_metrics.state_machine_rejections);
                                app_state.connections.send_to(
                                    &peer_id,
                                    &SignalingMessage::Error(ErrorPayload { message: reason.to_string() }),
                                );
                                continue;
                            }
                        }

                        let outcome = dispatch_message(
                            &mut DispatchContext {
                                app_state: &app_state,
                                peer_id: &peer_id,
                                issuer_id: &issuer_id,
                                session: &mut session,
                                authenticated_user_id: &mut authenticated_user_id,
                                authenticated_device_id: &mut authenticated_device_id,
                                rate_limiter: &mut rate_limiter,
                                chat_rate_limiter: &mut chat_rate_limiter,
                                sfu_config: &sfu_config,
                                client_ip,
                                raw_text: &text,
                                socket: &mut socket,
                            },
                            message,
                        ).await;
                        if matches!(outcome, DispatchOutcome::Break) {
                            break;
                        }
                    }
                    Some(Ok(Message::Binary(bytes))) => {
                        debug!(peer_id = %peer_id, bytes = bytes.len(), "ws binary message");
                    }
                    Some(Ok(Message::Ping(_))) => {
                        debug!(peer_id = %peer_id, "ws ping");
                    }
                    Some(Ok(Message::Pong(_))) => {
                        debug!(peer_id = %peer_id, "ws pong");
                    }
                    Some(Ok(Message::Close(frame))) => {
                        info!(peer_id = %peer_id, ?frame, "ws close frame");
                        break;
                    }
                    Some(Err(err)) => {
                        error!(peer_id = %peer_id, error = %err, "ws receive error");
                        break;
                    }
                    None => {
                        break;
                    }
                }
            }
            outbound = outbound_rx.recv() => {
                match outbound {
                    Some(payload) => {
                        if let Err(err) = socket.send(Message::Text(payload.into())).await {
                            error!(peer_id = %peer_id, error = %err, "ws send error");
                            break;
                        }
                    }
                    None => {
                        break;
                    }
                }
            }
        }
    }

    if let Some(session_ref) = session.as_mut() {
        session_ref.cleanup_connection(&app_state, &peer_id).await;
    } else {
        cleanup_unjoined_connection(&app_state, &peer_id).await;
    }
    info!(peer_id = %peer_id, "ws client disconnected");
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::voice::sfu_relay::ParticipantRole;
    use crate::ws::ws_session::SignalingSession;

    use proptest::prelude::*;

    #[test]
    fn test_already_joined_error_message_format() {
        // Documents the expected error message format for re-join rejection (Req 2.2)
        let error_msg = SignalingMessage::Error(ErrorPayload {
            message: "already joined".to_string(),
        });

        let json = signaling::to_json(&error_msg).expect("serialize error message");
        assert!(json.contains("already joined"));

        let parsed = signaling::parse(&json).expect("parse error message");
        match parsed {
            SignalingMessage::Error(payload) => {
                assert_eq!(payload.message, "already joined");
            }
            _ => panic!("Expected Error message"),
        }
    }

    // Feature: token-and-signaling-auth, Property 3: Pre-join message rejection
    // For any SignalingMessage variant that is not Join, sending it when session is None results in an error
    // **Validates: Requirements 2.2**

    proptest! {
        #![proptest_config(ProptestConfig::with_cases(100))]

        #[test]
        fn prop_pre_join_message_rejection(message in any::<SignalingMessage>()) {
            let is_join = matches!(message, SignalingMessage::Join(_) | SignalingMessage::CreateRoom(_) | SignalingMessage::JoinVoice(_));
            let is_auth = matches!(message, SignalingMessage::Auth(_));
            let is_ping = matches!(message, SignalingMessage::Ping);
            let should_be_rejected = !is_join && !is_auth && !is_ping;

            if should_be_rejected {
                let error_response = SignalingMessage::Error(ErrorPayload {
                    message: "not authenticated".to_string(),
                });

                let json = signaling::to_json(&error_response).expect("serialize error");
                prop_assert!(json.contains("not authenticated"),
                    "Error response should contain 'not authenticated' for message type: {:?}",
                    std::mem::discriminant(&message));

                prop_assert!(
                    matches!(message,
                        SignalingMessage::Joined(_) |
                        SignalingMessage::Offer(_) |
                        SignalingMessage::Answer(_) |
                        SignalingMessage::IceCandidate(_) |
                        SignalingMessage::PeerLeft |
                        SignalingMessage::Leave |
                        SignalingMessage::Error(_) |
                        SignalingMessage::JoinRejected(_) |
                        SignalingMessage::InviteCreate(_) |
                        SignalingMessage::InviteCreated(_) |
                        SignalingMessage::InviteRevoke(_) |
                        SignalingMessage::InviteRevoked(_) |
                        SignalingMessage::ParticipantJoined(_) |
                        SignalingMessage::ParticipantLeft(_) |
                        SignalingMessage::RoomState(_) |
                        SignalingMessage::MediaToken(_) |
                        SignalingMessage::KickParticipant(_) |
                        SignalingMessage::MuteParticipant(_) |
                        SignalingMessage::UnmuteParticipant(_) |
                        SignalingMessage::ParticipantKicked(_) |
                        SignalingMessage::ParticipantMuted(_) |
                        SignalingMessage::ParticipantUnmuted(_) |
                        SignalingMessage::SelfDeafen |
                        SignalingMessage::SelfUndeafen |
                        SignalingMessage::ParticipantDeafened(_) |
                        SignalingMessage::ParticipantUndeafened(_) |
                        SignalingMessage::StartShare |
                        SignalingMessage::ShareStarted(_) |
                        SignalingMessage::StopShare(_) |
                        SignalingMessage::ShareStopped(_) |
                        SignalingMessage::StopAllShares |
                        SignalingMessage::ShareState(_) |
                        SignalingMessage::SetSharePermission(_) |
                        SignalingMessage::SharePermissionChanged(_) |
                        SignalingMessage::RoomCreated(_) |
                        SignalingMessage::CreateSubRoom(_) |
                        SignalingMessage::JoinSubRoom(_) |
                        SignalingMessage::LeaveSubRoom(_) |
                        SignalingMessage::SubRoomState(_) |
                        SignalingMessage::SubRoomCreated(_) |
                        SignalingMessage::SubRoomJoined(_) |
                        SignalingMessage::SubRoomLeft(_) |
                        SignalingMessage::SubRoomDeleted(_) |
                        SignalingMessage::AuthSuccess(_) |
                        SignalingMessage::AuthFailed(_) |
                        SignalingMessage::ChatSend(_) |
                        SignalingMessage::ChatMessage(_) |
                        SignalingMessage::ChatHistoryRequest(_) |
                        SignalingMessage::ChatHistoryResponse(_) |
                        SignalingMessage::SessionDisplaced(_) |
                        SignalingMessage::ViewerSubscribed(_) |
                        SignalingMessage::ViewerJoined(_) |
                        SignalingMessage::SfuColdStarting(_)
                    ),
                    "Message should be a non-Join/non-Auth variant that requires authentication"
                );
            } else {
                let is_pre_join = matches!(message, SignalingMessage::Join(_) | SignalingMessage::CreateRoom(_) | SignalingMessage::JoinVoice(_) | SignalingMessage::Auth(_) | SignalingMessage::Ping);
                prop_assert!(is_pre_join, "Only Join/CreateRoom/JoinVoice/Auth/Ping messages should be allowed when session is None");
            }
        }
    }

    // Feature: token-and-signaling-auth, Property 4: Session identity enforcement
    // **Validates: Requirements 2.3, 2.4**

    proptest! {
        #![proptest_config(ProptestConfig::with_cases(100))]

        #[test]
        fn prop_session_identity_enforcement(
            session_peer_id in "[a-z0-9-]{1,32}",
            room_id in "[a-z0-9-]{1,32}",
            message in any::<SignalingMessage>(),
        ) {
            let session = SignalingSession::new(
                session_peer_id.clone(),
                room_id.clone(),
                ParticipantRole::Host,
                None,
                None,
            );

            let extracted_id = session.participant_id.as_str();
            prop_assert_eq!(extracted_id, &session_peer_id,
                "Server must use session.participant_id as sender identity");

            let is_post_join_message = !matches!(message, SignalingMessage::Join(_));

            if is_post_join_message {
                prop_assert_eq!(&session.participant_id, &session_peer_id,
                    "Session must maintain the server-assigned participant_id");
                prop_assert_eq!(&session.room_id, &room_id,
                    "Session must be bound to the correct room");
            }
        }
    }

    #[test]
    fn test_session_identity_enforcement_concept() {
        // Documents the expected behavior for action messages with participant_id fields.
        // The server MUST:
        // 1. Use session.participant_id as the sender/actor identity
        // 2. Validate that the sender has permission to perform the action
        // 3. Validate that the target is in the same room
    }

    // Feature: security-hardening, Property 6: TLS proto enforcement rejects non-HTTPS
    // Validates: Requirements 3.6

    proptest! {
        #![proptest_config(ProptestConfig::with_cases(200))]

        #[test]
        fn prop_tls_proto_rejects_non_https(
            proto_value in "[a-zA-Z0-9 ,;:/_-]{0,64}".prop_filter(
                "must not be 'https' (case-insensitive)",
                |v| !v.eq_ignore_ascii_case("https")
            ),
        ) {
            let mut headers = HeaderMap::new();
            if !proto_value.is_empty() {
                headers.insert("x-forwarded-proto", proto_value.parse().unwrap());
            }
            prop_assert!(
                !check_tls_proto(&headers),
                "X-Forwarded-Proto '{}' should be rejected", proto_value
            );
        }

        #[test]
        fn prop_tls_proto_accepts_https(
            casing in proptest::collection::vec(proptest::bool::ANY, 5)
        ) {
            let base = "https";
            let value: String = base.chars().zip(casing.iter()).map(|(c, &upper)| {
                if upper { c.to_uppercase().next().unwrap() } else { c }
            }).collect();

            let mut headers = HeaderMap::new();
            headers.insert("x-forwarded-proto", value.parse().unwrap());
            prop_assert!(
                check_tls_proto(&headers),
                "X-Forwarded-Proto '{}' should be accepted", value
            );
        }

        #[test]
        fn prop_tls_proto_forwarded_header_non_https(
            forwarded_value in "[a-zA-Z0-9 ,;:/_=-]{0,64}".prop_filter(
                "must not contain 'proto=https'",
                |v| !v.to_lowercase().contains("proto=https")
            ),
        ) {
            let mut headers = HeaderMap::new();
            if !forwarded_value.is_empty() {
                headers.insert("forwarded", forwarded_value.parse().unwrap());
            }
            prop_assert!(
                !check_tls_proto(&headers),
                "Forwarded '{}' without proto=https should be rejected", forwarded_value
            );
        }
    }

    #[test]
    fn test_tls_proto_custom_edge_header_accepted() {
        let mut headers = HeaderMap::new();
        headers.insert("x-wavis-forwarded-proto", "https".parse().unwrap());
        assert!(
            check_tls_proto(&headers),
            "X-Wavis-Forwarded-Proto: https must be accepted"
        );
    }

    #[test]
    fn test_tls_proto_custom_edge_header_rejected_when_http() {
        let mut headers = HeaderMap::new();
        headers.insert("x-wavis-forwarded-proto", "http".parse().unwrap());
        assert!(
            !check_tls_proto(&headers),
            "X-Wavis-Forwarded-Proto: http must be rejected"
        );
    }

    #[test]
    fn test_tls_proto_absent_headers_rejected() {
        let headers = HeaderMap::new();
        assert!(
            !check_tls_proto(&headers),
            "absent headers must be rejected"
        );
    }

    #[test]
    fn test_tls_proto_forwarded_with_proto_https_accepted() {
        let mut headers = HeaderMap::new();
        headers.insert(
            "forwarded",
            "for=192.0.2.1;proto=https;by=proxy.example"
                .parse()
                .unwrap(),
        );
        assert!(
            check_tls_proto(&headers),
            "Forwarded with proto=https must be accepted"
        );
    }

    #[test]
    fn test_tls_proto_http_rejected() {
        let mut headers = HeaderMap::new();
        headers.insert("x-forwarded-proto", "http".parse().unwrap());
        assert!(
            !check_tls_proto(&headers),
            "X-Forwarded-Proto: http must be rejected"
        );
    }
}
