//! Per-message routing for WebSocket signaling.
//!
//! **Owns:** the `match message { ... }` dispatch block that routes each parsed
//! `SignalingMessage` to the appropriate domain function and translates the
//! result into outbound signals.
//!
//! **Does not own:** WebSocket transport (upgrade, framing, send/recv loop),
//! rate limiting preamble, JSON parsing, field validation, or state machine
//! validation. Those remain in [`super::ws`].
//!
//! **Key types:**
//! - [`DispatchContext`] — bundles mutable/shared state needed by dispatch arms.
//! - [`DispatchOutcome`] — tells the caller whether to continue looping or break.
//! - [`SfuConfig`] — SFU configuration read once per connection.

use crate::app_state::AppState;
use crate::channel::invite::InviteRevokeError;
use crate::chat::chat;
use crate::chat::chat_persistence;
use crate::chat::chat_rate_limiter::ChatRateLimiter;
use crate::connections::ConnectionManager;
use crate::ec2_control::Ec2InstanceState;
use crate::state::{InMemoryRoomState, RoomType};
use crate::voice::relay::{self, P2PJoinResult, RelayResult, RoomState, handle_p2p_join};
use crate::voice::screen_share::{
    ShareResult, handle_set_share_permission, handle_start_share, handle_stop_all_shares,
    handle_stop_share,
};
use crate::voice::sfu_bridge::SfuHealth;
use crate::voice::sfu_relay::{
    OutboundSignal, ParticipantRole, SignalTarget, determine_room_type, handle_create_room,
    handle_kick, handle_mute, handle_sfu_ice, handle_sfu_join, handle_sfu_offer, handle_unmute,
};
use crate::voice::turn_cred::{build_ice_config_payload, generate_turn_credentials};
use crate::voice::voice_orchestrator;
use crate::voice::voice_orchestrator::VoiceJoinError;
use crate::ws::ws_rate_limit::WsRateLimiter;
use crate::ws::ws_session::{SignalingSession, close_socket};
use axum::extract::ws::WebSocket;
use shared::signaling::{
    self, AnswerPayload, ErrorPayload, IceCandidatePayload, JoinRejectedPayload,
    ParticipantColorUpdatedPayload, ParticipantDeafenedPayload, ParticipantUndeafenedPayload,
    SessionDescription, SfuColdStartingPayload, SignalingMessage, ViewerJoinedPayload,
};
use std::env;
use std::net::IpAddr;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};
use tracing::{debug, error, info, warn};
use uuid::Uuid;

pub(crate) const MAX_SDP_BYTES: usize = 64 * 1024; // 64 KB
pub(crate) const MAX_ICE_CANDIDATE_BYTES: usize = 2 * 1024; // 2 KB
const MSG_ACTION_RATE_LIMIT_EXCEEDED: &str = "action rate limit exceeded";
const COLD_START_ESTIMATED_WAIT_SECS: u32 = 120;

/// SFU config — jwt_secret and jwt_issuer come from AppState; remaining fields from env vars.
pub(crate) struct SfuConfig {
    jwt_secret: Vec<u8>,
    jwt_issuer: String,
    token_ttl_secs: u64,
    max_participants: u8,
    livekit_api_key: Option<String>,
    livekit_api_secret: Option<String>,
}

impl SfuConfig {
    /// Build SFU config from AppState (jwt_secret, jwt_issuer) + env vars (remaining fields).
    /// The JWT secret and issuer are centralized in AppState — no env var re-reads.
    pub(crate) fn from_app_state(jwt_secret: &[u8], jwt_issuer: &str) -> Self {
        let token_ttl_secs = env::var("SFU_TOKEN_TTL_SECS")
            .ok()
            .and_then(|v| v.parse::<u64>().ok())
            .unwrap_or(crate::auth::jwt::DEFAULT_TOKEN_TTL_SECS);
        let max_participants = env::var("MAX_ROOM_PARTICIPANTS")
            .ok()
            .and_then(|v| v.parse::<u8>().ok())
            .unwrap_or(6)
            .clamp(3, 6);
        let livekit_api_key = env::var("LIVEKIT_API_KEY").ok();
        let livekit_api_secret = env::var("LIVEKIT_API_SECRET").ok();
        Self {
            jwt_secret: jwt_secret.to_vec(),
            jwt_issuer: jwt_issuer.to_string(),
            token_ttl_secs,
            max_participants,
            livekit_api_key,
            livekit_api_secret,
        }
    }
}

/// Inject TURN credentials into the `Joined` signal targeted at `peer_id`.
/// Called after domain join functions return signals, before dispatching.
/// No-op if `turn_config` is None.
pub(crate) fn inject_turn_credentials(
    signals: &mut [OutboundSignal],
    peer_id: &str,
    turn_config: Option<&crate::voice::turn_cred::TurnConfig>,
) {
    let config = match turn_config {
        Some(c) => c,
        None => return,
    };
    let now_unix = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs();
    let creds = generate_turn_credentials(peer_id, config, now_unix);
    let ice_payload = build_ice_config_payload(config, &creds);

    for signal in signals.iter_mut() {
        if let SignalingMessage::Joined(ref mut joined) = signal.msg {
            // Only inject into the Joined signal addressed to this peer
            if matches!(&signal.target, SignalTarget::Peer(pid) if pid == peer_id) {
                joined.ice_config = Some(ice_payload.clone());
            }
        }
    }
}

/// Dispatch a list of `OutboundSignal`s to the appropriate connections.
pub fn dispatch_signals(
    signals: Vec<OutboundSignal>,
    room_id: &str,
    state: &InMemoryRoomState,
    connections: &dyn ConnectionManager,
) {
    for signal in signals {
        match signal.target {
            SignalTarget::Peer(peer_id) => {
                connections.send_to(&peer_id, &signal.msg);
            }
            SignalTarget::Broadcast { exclude } => {
                let peers = state.get_peers_in_room(&room_id.to_string());
                for peer in peers {
                    if peer != exclude {
                        connections.send_to(&peer, &signal.msg);
                    }
                }
            }
            SignalTarget::BroadcastAll => {
                let peers = state.get_peers_in_room(&room_id.to_string());
                for peer in peers {
                    connections.send_to(&peer, &signal.msg);
                }
            }
        }
    }
}

pub(crate) fn schedule_sub_room_expiry(
    app_state: &AppState,
    room_id: &str,
    sub_room_id: &str,
    delete_at: Instant,
) {
    let app_state = app_state.clone();
    let room_id = room_id.to_string();
    let sub_room_id = sub_room_id.to_string();
    tokio::spawn(async move {
        tokio::time::sleep_until(tokio::time::Instant::from_std(delete_at)).await;
        let result = voice_orchestrator::expire_sub_room(
            app_state.room_state.as_ref(),
            &room_id,
            &sub_room_id,
            delete_at,
        );
        if !result.signals.is_empty() {
            dispatch_signals(
                result.signals,
                &room_id,
                app_state.room_state.as_ref(),
                app_state.connections.as_ref(),
            );
        }
    });
}

/// Bundles references to all the mutable/shared state that dispatch arms need.
pub(crate) struct DispatchContext<'a> {
    pub app_state: &'a AppState,
    pub peer_id: &'a str,
    pub issuer_id: &'a str,
    pub session: &'a mut Option<SignalingSession>,
    pub authenticated_user_id: &'a mut Option<String>,
    pub authenticated_device_id: &'a mut Option<Uuid>,
    pub rate_limiter: &'a mut WsRateLimiter,
    pub chat_rate_limiter: &'a mut ChatRateLimiter,
    pub sfu_config: &'a SfuConfig,
    pub client_ip: IpAddr,
    pub raw_text: &'a str,
    pub socket: &'a mut WebSocket,
}

/// Outcome of dispatching a single message — tells the caller whether to keep looping.
pub(crate) enum DispatchOutcome {
    /// Keep the receive loop running.
    Continue,
    /// Close the connection (e.g. Leave, or fatal dispatch error).
    Break,
}

/// Route a parsed `SignalingMessage` to the appropriate domain function.
///
/// Contains the entire `match message { ... }` block previously in `handle_socket`.
/// Each arm: parse payload → call domain function → return result.
pub(crate) async fn dispatch_message(
    ctx: &mut DispatchContext<'_>,
    message: SignalingMessage,
) -> DispatchOutcome {
    match message {
        SignalingMessage::Auth(payload) => {
            // Auth message dispatch (Req 6.1, 6.2, 6.3, 15.1, 15.2, 15.3)
            // validate_state_transition already checked auth is allowed
            match crate::auth::jwt::validate_access_token_with_rotation(
                &payload.access_token,
                &ctx.app_state.auth_jwt_secret,
                ctx.app_state
                    .auth_jwt_secret_previous
                    .as_ref()
                    .map(|s| s.as_slice()),
            ) {
                Ok((user_id, device_id, token_epoch)) => {
                    // Verify session epoch against DB
                    match crate::auth::auth::check_session_epoch(
                        &ctx.app_state.db_pool,
                        &user_id,
                        token_epoch,
                    )
                    .await
                    {
                        Ok(()) => {
                            // Verify the device has not been revoked (Req 15.1, 15.2)
                            let revoked_at: Option<Option<chrono::DateTime<chrono::Utc>>> =
                                sqlx::query_scalar(
                                    "SELECT revoked_at FROM devices WHERE device_id = $1",
                                )
                                .bind(device_id)
                                .fetch_optional(&ctx.app_state.db_pool)
                                .await
                                .unwrap_or(None);

                            match revoked_at {
                                Some(None) => {
                                    // Device exists and is not revoked — auth success
                                    *ctx.authenticated_user_id = Some(user_id.to_string());
                                    #[allow(unused_assignments)]
                                    {
                                        *ctx.authenticated_device_id = Some(device_id);
                                    }
                                    info!(peer_id = %ctx.peer_id, user_id = %user_id, device_id = %device_id, "ws auth succeeded");
                                    ctx.app_state.connections.send_to(
                                        ctx.peer_id,
                                        &SignalingMessage::AuthSuccess(
                                            shared::signaling::AuthSuccessPayload {
                                                user_id: user_id.to_string(),
                                            },
                                        ),
                                    );
                                }
                                _ => {
                                    // Device not found or revoked (Req 15.2)
                                    warn!(peer_id = %ctx.peer_id, user_id = %user_id, device_id = %device_id, "ws auth failed: device revoked or not found");
                                    ctx.app_state.connections.send_to(
                                        ctx.peer_id,
                                        &SignalingMessage::AuthFailed(
                                            shared::signaling::AuthFailedPayload {
                                                reason: "authentication failed".to_string(),
                                            },
                                        ),
                                    );
                                }
                            }
                        }
                        Err(err) => {
                            warn!(peer_id = %ctx.peer_id, user_id = %user_id, error = %err, "ws auth epoch check failed");
                            ctx.app_state.connections.send_to(
                                ctx.peer_id,
                                &SignalingMessage::AuthFailed(
                                    shared::signaling::AuthFailedPayload {
                                        reason: "authentication failed".to_string(),
                                    },
                                ),
                            );
                        }
                    }
                }
                Err(err) => {
                    warn!(peer_id = %ctx.peer_id, error = %err, "ws auth failed");
                    ctx.app_state.connections.send_to(
                        ctx.peer_id,
                        &SignalingMessage::AuthFailed(shared::signaling::AuthFailedPayload {
                            reason: "authentication failed".to_string(),
                        }),
                    );
                }
            }
            DispatchOutcome::Continue
        }
        SignalingMessage::Join(payload) => {
            // Reject re-Join on already-joined connections (Req 2.2)
            if ctx.session.is_some() {
                ctx.app_state.connections.send_to(
                    ctx.peer_id,
                    &SignalingMessage::Error(ErrorPayload {
                        message: "already joined".to_string(),
                    }),
                );
                return DispatchOutcome::Continue;
            }

            let room_id = payload.room_id.trim().to_string();
            if room_id.is_empty() {
                ctx.app_state.connections.send_to(
                    ctx.peer_id,
                    &SignalingMessage::Error(ErrorPayload {
                        message: "invalid room ID".to_string(),
                    }),
                );
                return DispatchOutcome::Continue;
            }

            let now = Instant::now();

            // --- Join rejection pipeline (short-circuit on first failure) ---

            let require_invite = ctx.app_state.require_invite_code;

            let invite_code = if require_invite {
                // Step 1: Check invite_code presence (Req 4.1)
                match payload.invite_code {
                    Some(ref code) if !code.is_empty() => code.clone(),
                    _ => {
                        ctx.app_state.join_rate_limiter.record_attempt(
                            ctx.client_ip,
                            None,
                            &room_id,
                            ctx.peer_id,
                            true,
                            now,
                        );
                        if let Some(count) = ctx
                            .app_state
                            .ip_failed_join_tracker
                            .record_failure(ctx.client_ip, now)
                        {
                            warn!(
                                client_ip = %ctx.client_ip,
                                failure_count = count,
                                window_seconds = ctx.app_state.ip_failed_join_tracker.window_secs(),
                                event = "ip_abuse_threshold_exceeded",
                                "per-IP failed join threshold exceeded"
                            );
                            ctx.app_state
                                .abuse_metrics
                                .increment(&ctx.app_state.abuse_metrics.invite_usage_anomalies);
                        }
                        ctx.app_state.connections.send_to(
                            ctx.peer_id,
                            &SignalingMessage::JoinRejected(JoinRejectedPayload {
                                reason: shared::signaling::JoinRejectionReason::InviteRequired,
                            }),
                        );
                        return DispatchOutcome::Continue;
                    }
                }
            } else {
                // Bypass mode: use provided code or empty string
                payload.invite_code.clone().unwrap_or_default()
            };

            if require_invite {
                // Step 1.5: global join ceiling (Req 15.1, 15.2, 15.3)
                {
                    let now_unix = SystemTime::now()
                        .duration_since(UNIX_EPOCH)
                        .unwrap_or_default()
                        .as_secs();
                    if !ctx.app_state.global_join_limiter.allow(now_unix) {
                        ctx.app_state
                            .abuse_metrics
                            .increment(&ctx.app_state.abuse_metrics.global_join_ceiling_rejections);
                        warn!(peer_id = %ctx.peer_id, "join rejected: global join ceiling exceeded");
                        ctx.app_state.connections.send_to(
                            ctx.peer_id,
                            &SignalingMessage::Error(ErrorPayload {
                                message: "server busy, try again later".to_string(),
                            }),
                        );
                        return DispatchOutcome::Continue;
                    }
                }

                // Step 2: check_join — all rate limit dimensions (Req 6.1–6.8)
                if let Err(reason) = ctx.app_state.join_rate_limiter.check_join(
                    ctx.client_ip,
                    Some(invite_code.as_str()),
                    &room_id,
                    ctx.peer_id,
                    now,
                ) {
                    ctx.app_state.join_rate_limiter.record_attempt(
                        ctx.client_ip,
                        Some(invite_code.as_str()),
                        &room_id,
                        ctx.peer_id,
                        true,
                        now,
                    );
                    ctx.app_state
                        .abuse_metrics
                        .increment(&ctx.app_state.abuse_metrics.join_rate_limit_rejections);
                    if let Some(count) = ctx
                        .app_state
                        .ip_failed_join_tracker
                        .record_failure(ctx.client_ip, now)
                    {
                        warn!(
                            client_ip = %ctx.client_ip,
                            failure_count = count,
                            window_seconds = ctx.app_state.ip_failed_join_tracker.window_secs(),
                            event = "ip_abuse_threshold_exceeded",
                            "per-IP failed join threshold exceeded"
                        );
                        ctx.app_state
                            .abuse_metrics
                            .increment(&ctx.app_state.abuse_metrics.invite_usage_anomalies);
                    }
                    ctx.app_state.connections.send_to(
                        ctx.peer_id,
                        &SignalingMessage::JoinRejected(JoinRejectedPayload { reason }),
                    );
                    return DispatchOutcome::Continue;
                }

                // Step 3: validate invite (Req 4.2–4.4, 2.2, 3.2, 5.3)
                if let Err(reason) =
                    ctx.app_state
                        .invite_store
                        .validate(&invite_code, &room_id, now)
                {
                    ctx.app_state.join_rate_limiter.record_attempt(
                        ctx.client_ip,
                        Some(invite_code.as_str()),
                        &room_id,
                        ctx.peer_id,
                        true,
                        now,
                    );
                    ctx.app_state
                        .abuse_metrics
                        .increment(&ctx.app_state.abuse_metrics.join_invite_rejections);
                    if let Some(count) = ctx
                        .app_state
                        .ip_failed_join_tracker
                        .record_failure(ctx.client_ip, now)
                    {
                        warn!(
                            client_ip = %ctx.client_ip,
                            failure_count = count,
                            window_seconds = ctx.app_state.ip_failed_join_tracker.window_secs(),
                            event = "ip_abuse_threshold_exceeded",
                            "per-IP failed join threshold exceeded"
                        );
                        ctx.app_state
                            .abuse_metrics
                            .increment(&ctx.app_state.abuse_metrics.invite_usage_anomalies);
                    }
                    ctx.app_state.connections.send_to(
                        ctx.peer_id,
                        &SignalingMessage::JoinRejected(JoinRejectedPayload { reason }),
                    );
                    return DispatchOutcome::Continue;
                }
            }

            // Step 4: dispatch to room-type-specific join (atomic join + validate_and_consume)
            let room_type = determine_room_type(
                payload.room_type.as_deref(),
                ctx.sfu_config.max_participants,
            );

            // Pass invite code to domain only when invite is required;
            // the domain function will atomically validate + consume
            // inside the per-room lock.
            let invite_code_opt = if require_invite {
                Some(invite_code.as_str())
            } else {
                None
            };

            let join_result: Result<(), shared::signaling::JoinRejectionReason> = match room_type {
                RoomType::P2P => {
                    match handle_p2p_join(
                        ctx.app_state.room_state.as_ref(),
                        &room_id,
                        ctx.peer_id,
                        ctx.app_state.invite_store.as_ref(),
                        invite_code_opt,
                    ) {
                        P2PJoinResult::Joined(mut signals) => {
                            inject_turn_credentials(
                                &mut signals,
                                ctx.peer_id,
                                ctx.app_state.turn_config.as_deref(),
                            );
                            dispatch_signals(
                                signals,
                                &room_id,
                                ctx.app_state.room_state.as_ref(),
                                ctx.app_state.connections.as_ref(),
                            );
                            // Determine role: first joiner is Host, subsequent joiners are Guest
                            let role = if let Some(info) =
                                ctx.app_state.room_state.get_room_info(&room_id)
                            {
                                if info.participants.len() == 1 {
                                    ParticipantRole::Host
                                } else {
                                    ParticipantRole::Guest
                                }
                            } else {
                                ParticipantRole::Guest
                            };
                            // Create SignalingSession after successful join (Req 2.1)
                            *ctx.session = Some(SignalingSession::new(
                                ctx.peer_id.to_string(),
                                room_id.clone(),
                                role,
                                ctx.authenticated_user_id.clone(),
                                None,
                            ));
                            Ok(())
                        }
                        P2PJoinResult::RoomFull => {
                            Err(shared::signaling::JoinRejectionReason::RoomFull)
                        }
                        P2PJoinResult::InviteRejected(reason) => Err(reason),
                    }
                }
                RoomType::Sfu => {
                    if !ctx.app_state.is_sfu_available().await {
                        ctx.app_state.connections.send_to(
                            ctx.peer_id,
                            &SignalingMessage::Error(ErrorPayload {
                                message: "SFU unavailable".to_string(),
                            }),
                        );
                        // record as failed attempt before continuing
                        ctx.app_state.join_rate_limiter.record_attempt(
                            ctx.client_ip,
                            Some(invite_code.as_str()),
                            &room_id,
                            ctx.peer_id,
                            true,
                            now,
                        );
                        return DispatchOutcome::Continue;
                    }

                    let display_name = payload
                        .display_name
                        .as_deref()
                        .filter(|s| !s.is_empty())
                        .unwrap_or(ctx.peer_id)
                        .to_string();
                    let token_mode = if ctx.app_state.sfu_signaling_proxy.is_none() {
                        match (
                            &ctx.sfu_config.livekit_api_key,
                            &ctx.sfu_config.livekit_api_secret,
                        ) {
                            (Some(key), Some(secret)) => {
                                crate::voice::sfu_relay::TokenMode::LiveKit {
                                    api_key: key.as_str(),
                                    api_secret: secret.as_str(),
                                    ttl_secs: crate::auth::jwt::LIVEKIT_TOKEN_TTL_SECS,
                                }
                            }
                            _ => crate::voice::sfu_relay::TokenMode::Custom {
                                jwt_secret: &ctx.sfu_config.jwt_secret,
                                issuer: &ctx.sfu_config.jwt_issuer,
                                ttl_secs: ctx.sfu_config.token_ttl_secs,
                            },
                        }
                    } else {
                        crate::voice::sfu_relay::TokenMode::Custom {
                            jwt_secret: &ctx.sfu_config.jwt_secret,
                            issuer: &ctx.sfu_config.jwt_issuer,
                            ttl_secs: ctx.sfu_config.token_ttl_secs,
                        }
                    };

                    match handle_sfu_join(
                        ctx.app_state.sfu_room_manager.as_ref(),
                        ctx.app_state.room_state.as_ref(),
                        &room_id,
                        ctx.peer_id,
                        &display_name,
                        payload.profile_color.as_deref(),
                        &token_mode,
                        &ctx.app_state.sfu_url,
                        ctx.sfu_config.max_participants,
                        ctx.app_state.invite_store.as_ref(),
                        invite_code_opt,
                    )
                    .await
                    {
                        Ok(mut signals) => {
                            inject_turn_credentials(
                                &mut signals,
                                ctx.peer_id,
                                ctx.app_state.turn_config.as_deref(),
                            );
                            dispatch_signals(
                                signals,
                                &room_id,
                                ctx.app_state.room_state.as_ref(),
                                ctx.app_state.connections.as_ref(),
                            );
                            // Spawn ICE candidate polling for this peer
                            if let Some(info) = ctx.app_state.room_state.get_room_info(&room_id)
                                && let Some(sfu_handle) = info.sfu_handle
                                && let Some(ref proxy) = ctx.app_state.sfu_signaling_proxy
                            {
                                let poll_bridge = proxy.clone();
                                let poll_connections = ctx.app_state.connections.clone();
                                let poll_peer_id = ctx.peer_id.to_string();
                                let poll_handle = sfu_handle;
                                tokio::spawn(async move {
                                    let mut interval =
                                        tokio::time::interval(Duration::from_millis(100));
                                    loop {
                                        interval.tick().await;
                                        match poll_bridge
                                            .poll_sfu_ice_candidates(&poll_handle, &poll_peer_id)
                                            .await
                                        {
                                            Ok(candidates) => {
                                                for candidate in candidates {
                                                    poll_connections.send_to(
                                                        &poll_peer_id,
                                                        &SignalingMessage::IceCandidate(
                                                            IceCandidatePayload { candidate },
                                                        ),
                                                    );
                                                }
                                            }
                                            Err(_) => break,
                                        }
                                    }
                                });
                            }
                            // Determine role: first joiner is Host, subsequent joiners are Guest
                            let role = if let Some(info) =
                                ctx.app_state.room_state.get_room_info(&room_id)
                            {
                                if info.participants.len() == 1 {
                                    ParticipantRole::Host
                                } else {
                                    ParticipantRole::Guest
                                }
                            } else {
                                ParticipantRole::Guest
                            };
                            // Create SignalingSession after successful join (Req 2.1)
                            *ctx.session = Some(SignalingSession::new(
                                ctx.peer_id.to_string(),
                                room_id.clone(),
                                role,
                                ctx.authenticated_user_id.clone(),
                                None,
                            ));
                            Ok(())
                        }
                        Err(crate::voice::sfu_bridge::SfuError::RoomFull) => {
                            Err(shared::signaling::JoinRejectionReason::RoomFull)
                        }
                        Err(crate::voice::sfu_bridge::SfuError::InviteExhausted) => {
                            Err(shared::signaling::JoinRejectionReason::InviteExhausted)
                        }
                        Err(e) => {
                            ctx.app_state.connections.send_to(
                                ctx.peer_id,
                                &SignalingMessage::Error(ErrorPayload {
                                    message: format!("join failed: {e}"),
                                }),
                            );
                            // record as failed, then continue (non-rejection error)
                            ctx.app_state.join_rate_limiter.record_attempt(
                                ctx.client_ip,
                                Some(invite_code.as_str()),
                                &room_id,
                                ctx.peer_id,
                                true,
                                now,
                            );
                            return DispatchOutcome::Continue;
                        }
                    }
                }
            };

            // Step 5: record_attempt exactly once (Req 6.1–6.8)
            let failed = join_result.is_err();
            ctx.app_state.join_rate_limiter.record_attempt(
                ctx.client_ip,
                Some(invite_code.as_str()),
                &room_id,
                ctx.peer_id,
                failed,
                now,
            );

            // Step 6: send JoinRejected on failure (Req 10.1, 10.2)
            if let Err(reason) = join_result {
                if let Some(count) = ctx
                    .app_state
                    .ip_failed_join_tracker
                    .record_failure(ctx.client_ip, now)
                {
                    warn!(
                        client_ip = %ctx.client_ip,
                        failure_count = count,
                        window_seconds = ctx.app_state.ip_failed_join_tracker.window_secs(),
                        event = "ip_abuse_threshold_exceeded",
                        "per-IP failed join threshold exceeded"
                    );
                    ctx.app_state
                        .abuse_metrics
                        .increment(&ctx.app_state.abuse_metrics.invite_usage_anomalies);
                }
                ctx.app_state.connections.send_to(
                    ctx.peer_id,
                    &SignalingMessage::JoinRejected(JoinRejectedPayload { reason }),
                );
            }
            DispatchOutcome::Continue
        }
        SignalingMessage::CreateRoom(payload) => {
            // State machine already rejects CreateRoom with active session,
            // but double-check as defense-in-depth.
            if ctx.session.is_some() {
                ctx.app_state.connections.send_to(
                    ctx.peer_id,
                    &SignalingMessage::Error(ErrorPayload {
                        message: "already joined".to_string(),
                    }),
                );
                return DispatchOutcome::Continue;
            }

            // Build token mode (same logic as Join SFU path)
            let token_mode = if ctx.app_state.sfu_signaling_proxy.is_none() {
                match (
                    &ctx.sfu_config.livekit_api_key,
                    &ctx.sfu_config.livekit_api_secret,
                ) {
                    (Some(key), Some(secret)) => crate::voice::sfu_relay::TokenMode::LiveKit {
                        api_key: key.as_str(),
                        api_secret: secret.as_str(),
                        ttl_secs: crate::auth::jwt::LIVEKIT_TOKEN_TTL_SECS,
                    },
                    _ => crate::voice::sfu_relay::TokenMode::Custom {
                        jwt_secret: &ctx.sfu_config.jwt_secret,
                        issuer: &ctx.sfu_config.jwt_issuer,
                        ttl_secs: ctx.sfu_config.token_ttl_secs,
                    },
                }
            } else {
                crate::voice::sfu_relay::TokenMode::Custom {
                    jwt_secret: &ctx.sfu_config.jwt_secret,
                    issuer: &ctx.sfu_config.jwt_issuer,
                    ttl_secs: ctx.sfu_config.token_ttl_secs,
                }
            };

            let sfu_available = ctx.app_state.is_sfu_available().await;

            match handle_create_room(
                ctx.app_state.sfu_room_manager.as_ref(),
                ctx.app_state.room_state.as_ref(),
                &payload.room_id,
                ctx.peer_id,
                payload
                    .display_name
                    .as_deref()
                    .filter(|s| !s.is_empty())
                    .unwrap_or(ctx.peer_id),
                payload.profile_color.as_deref(),
                payload.room_type.as_deref(),
                ctx.sfu_config.max_participants,
                &ctx.app_state.invite_store,
                ctx.issuer_id,
                &token_mode,
                &ctx.app_state.sfu_url,
                sfu_available,
            )
            .await
            {
                Ok(signals) => {
                    let room_id = payload.room_id.trim().to_string();

                    // Inject TURN credentials into the RoomCreated signal
                    let signals = if ctx.app_state.turn_config.is_some() {
                        signals
                            .into_iter()
                            .map(|mut sig| {
                                if let SignalingMessage::RoomCreated(ref mut p) = sig.msg
                                    && let Some(tc) = ctx.app_state.turn_config.as_deref()
                                {
                                    let now_unix = SystemTime::now()
                                        .duration_since(UNIX_EPOCH)
                                        .unwrap_or_default()
                                        .as_secs();
                                    let creds =
                                        generate_turn_credentials(ctx.peer_id, tc, now_unix);
                                    p.ice_config = Some(build_ice_config_payload(tc, &creds));
                                }
                                sig
                            })
                            .collect::<Vec<_>>()
                    } else {
                        signals
                    };

                    // Create session as Host
                    *ctx.session = Some(SignalingSession::new(
                        ctx.peer_id.to_string(),
                        room_id.clone(),
                        ParticipantRole::Host,
                        ctx.authenticated_user_id.clone(),
                        None,
                    ));

                    debug!(peer_id = %ctx.peer_id, room_id = %room_id, "room created");
                    dispatch_signals(
                        signals,
                        &room_id,
                        &ctx.app_state.room_state,
                        ctx.app_state.connections.as_ref(),
                    );
                }
                Err(e) => {
                    warn!(peer_id = %ctx.peer_id, "CreateRoom failed: {e}");
                    ctx.app_state.connections.send_to(
                        ctx.peer_id,
                        &SignalingMessage::Error(ErrorPayload {
                            message: e.to_string(),
                        }),
                    );
                }
            }
            DispatchOutcome::Continue
        }
        SignalingMessage::Leave => {
            if let Some(session_ref) = ctx.session.as_mut() {
                session_ref.handle_leave(ctx.app_state).await;
            }
            close_socket(ctx.socket).await;
            DispatchOutcome::Break
        }
        SignalingMessage::Offer(ref offer_payload) => {
            // Use session identity for sender (Req 2.3, 2.4)
            let sender_id = ctx
                .session
                .as_ref()
                .map(|s| s.participant_id.as_str())
                .unwrap_or(ctx.peer_id);
            // Req 3.5, 3.8: reject oversized SDP before any forwarding
            if offer_payload.session_description.sdp.len() > MAX_SDP_BYTES {
                ctx.app_state
                    .abuse_metrics
                    .increment(&ctx.app_state.abuse_metrics.payload_size_violations);
                ctx.app_state.connections.send_to(
                    ctx.peer_id,
                    &SignalingMessage::Error(ErrorPayload {
                        message: "sdp too large".to_string(),
                    }),
                );
                return DispatchOutcome::Continue;
            }
            // Check room type — SFU rooms forward to SFU, P2P rooms relay
            let room_id_opt = ctx.app_state.room_state.get_room_for_peer(sender_id);
            if let Some(ref room_id) = room_id_opt {
                let room_type = ctx
                    .app_state
                    .room_state
                    .get_room_info(room_id)
                    .map(|info| info.room_type);
                if room_type == Some(RoomType::Sfu)
                    && let Some(info) = ctx.app_state.room_state.get_room_info(room_id)
                    && let Some(ref sfu_handle) = info.sfu_handle
                {
                    let sdp = offer_payload.session_description.sdp.clone();
                    if let Some(ref proxy) = ctx.app_state.sfu_signaling_proxy {
                        let result =
                            handle_sfu_offer(proxy.as_ref(), sfu_handle, sender_id, &sdp).await;
                        match result {
                            crate::voice::sfu_relay::SfuRelayResult::SdpAnswer {
                                peer_id: pid,
                                answer_sdp,
                            } => {
                                ctx.app_state.connections.send_to(
                                    &pid,
                                    &SignalingMessage::Answer(AnswerPayload {
                                        session_description: SessionDescription {
                                            sdp: answer_sdp,
                                            sdp_type: "answer".to_string(),
                                        },
                                    }),
                                );
                            }
                            crate::voice::sfu_relay::SfuRelayResult::Error {
                                peer_id: pid,
                                error,
                            } => {
                                ctx.app_state.connections.send_to(&pid, &error);
                            }
                            crate::voice::sfu_relay::SfuRelayResult::IceForwarded => {}
                        }
                    } else {
                        debug!(peer_id = %sender_id, "SFU Offer ignored in LiveKit mode (no signaling proxy)");
                    }
                    return DispatchOutcome::Continue;
                }
            }
            // P2P fallback
            handle_signaling_message(
                ctx.app_state.room_state.as_ref(),
                sender_id,
                ctx.raw_text,
                ctx.app_state.connections.as_ref(),
            );
            DispatchOutcome::Continue
        }
        SignalingMessage::IceCandidate(ref ice_payload) => {
            // Use session identity for sender (Req 2.3, 2.4)
            let sender_id = ctx
                .session
                .as_ref()
                .map(|s| s.participant_id.as_str())
                .unwrap_or(ctx.peer_id);
            // Req 3.5, 3.8: reject oversized ICE candidate before any forwarding
            if ice_payload.candidate.candidate.len() > MAX_ICE_CANDIDATE_BYTES {
                ctx.app_state
                    .abuse_metrics
                    .increment(&ctx.app_state.abuse_metrics.payload_size_violations);
                ctx.app_state.connections.send_to(
                    ctx.peer_id,
                    &SignalingMessage::Error(ErrorPayload {
                        message: "ice candidate too large".to_string(),
                    }),
                );
                return DispatchOutcome::Continue;
            }
            let room_id_opt = ctx.app_state.room_state.get_room_for_peer(sender_id);
            if let Some(ref room_id) = room_id_opt {
                let room_type = ctx
                    .app_state
                    .room_state
                    .get_room_info(room_id)
                    .map(|info| info.room_type);
                if room_type == Some(RoomType::Sfu)
                    && let Some(info) = ctx.app_state.room_state.get_room_info(room_id)
                    && let Some(ref sfu_handle) = info.sfu_handle
                {
                    let candidate = ice_payload.candidate.clone();
                    if let Some(ref proxy) = ctx.app_state.sfu_signaling_proxy {
                        let _ =
                            handle_sfu_ice(proxy.as_ref(), sfu_handle, sender_id, &candidate).await;
                    } else {
                        debug!(peer_id = %sender_id, "SFU IceCandidate ignored in LiveKit mode (no signaling proxy)");
                    }
                    return DispatchOutcome::Continue;
                }
            }
            // P2P fallback
            handle_signaling_message(
                ctx.app_state.room_state.as_ref(),
                sender_id,
                ctx.raw_text,
                ctx.app_state.connections.as_ref(),
            );
            DispatchOutcome::Continue
        }
        SignalingMessage::Answer(ref answer_payload) => {
            // Use session identity for sender (Req 2.3, 2.4)
            let sender_id = ctx
                .session
                .as_ref()
                .map(|s| s.participant_id.as_str())
                .unwrap_or(ctx.peer_id);
            // Req 3.5, 3.8: reject oversized SDP before any forwarding
            if answer_payload.session_description.sdp.len() > MAX_SDP_BYTES {
                ctx.app_state
                    .abuse_metrics
                    .increment(&ctx.app_state.abuse_metrics.payload_size_violations);
                ctx.app_state.connections.send_to(
                    ctx.peer_id,
                    &SignalingMessage::Error(ErrorPayload {
                        message: "sdp too large".to_string(),
                    }),
                );
                return DispatchOutcome::Continue;
            }
            // In SFU rooms, Answer comes FROM the SFU back to the client —
            // clients should not send Answer messages in SFU rooms.
            // In P2P rooms, relay normally.
            let room_id_opt = ctx.app_state.room_state.get_room_for_peer(sender_id);
            let is_sfu = room_id_opt
                .as_ref()
                .and_then(|rid| ctx.app_state.room_state.get_room_info(rid))
                .map(|info| info.room_type == RoomType::Sfu)
                .unwrap_or(false);

            if is_sfu {
                if ctx.app_state.sfu_signaling_proxy.is_none() {
                    debug!(peer_id = %sender_id, "SFU Answer ignored in LiveKit mode");
                }
                // SFU rooms: ignore client-sent Answer
            } else {
                handle_signaling_message(
                    ctx.app_state.room_state.as_ref(),
                    sender_id,
                    ctx.raw_text,
                    ctx.app_state.connections.as_ref(),
                );
            }
            DispatchOutcome::Continue
        }
        SignalingMessage::InviteCreate(payload) => {
            // Use session identity for sender (Req 2.3, 2.4)
            let sender_id = ctx
                .session
                .as_ref()
                .map(|s| s.participant_id.as_str())
                .unwrap_or(ctx.peer_id);
            // Req 1.2, 1.3, 11.1–11.4: generate invite for the room the peer is in
            let room_id_opt = ctx.app_state.room_state.get_room_for_peer(sender_id);
            match room_id_opt {
                None => {
                    ctx.app_state.connections.send_to(
                        sender_id,
                        &SignalingMessage::Error(ErrorPayload {
                            message: "not in a room".to_string(),
                        }),
                    );
                }
                Some(room_id) => {
                    let now = Instant::now();
                    match ctx.app_state.invite_store.generate(
                        &room_id,
                        ctx.issuer_id,
                        payload.max_uses,
                        now,
                    ) {
                        Ok(record) => {
                            let expires_in_secs = ctx.app_state.invite_store.default_ttl_secs();
                            // Req 13.3: do NOT log the invite code value
                            debug!(peer_id = %sender_id, room_id = %room_id, "invite code generated");
                            ctx.app_state.connections.send_to(
                                sender_id,
                                &SignalingMessage::InviteCreated(
                                    shared::signaling::InviteCreatedPayload {
                                        invite_code: record.code,
                                        expires_in_secs,
                                        max_uses: record.remaining_uses,
                                    },
                                ),
                            );
                        }
                        Err(crate::channel::invite::InviteError::RoomLimitReached { .. }) => {
                            ctx.app_state.connections.send_to(
                                sender_id,
                                &SignalingMessage::Error(ErrorPayload {
                                    message: "invite limit reached for room".to_string(),
                                }),
                            );
                        }
                        Err(crate::channel::invite::InviteError::GlobalLimitReached { .. }) => {
                            ctx.app_state.connections.send_to(
                                sender_id,
                                &SignalingMessage::Error(ErrorPayload {
                                    message: "global invite limit reached".to_string(),
                                }),
                            );
                        }
                        Err(crate::channel::invite::InviteError::NotFound) => {
                            // unreachable from generate(), defensive
                            ctx.app_state.connections.send_to(
                                sender_id,
                                &SignalingMessage::Error(ErrorPayload {
                                    message: "invite code not found".to_string(),
                                }),
                            );
                        }
                    }
                }
            }
            DispatchOutcome::Continue
        }
        SignalingMessage::InviteRevoke(payload) => {
            // Session is guaranteed Some here (pre-join gate already handled above)
            let session_ref = ctx.session.as_ref().unwrap();
            let sender_id = session_ref.participant_id.as_str();
            let room_id = session_ref.room_id.as_str();

            // Call authorized revoke — domain function checks role + room match
            match ctx.app_state.invite_store.revoke_authorized(
                &payload.invite_code,
                room_id,
                sender_id,
                session_ref.role,
            ) {
                Ok(()) => {
                    // Req 13.3: do NOT log the invite code value
                    debug!(peer_id = %sender_id, "invite code revoked");
                    ctx.app_state.connections.send_to(
                        sender_id,
                        &SignalingMessage::InviteRevoked(shared::signaling::InviteRevokedPayload {
                            invite_code: payload.invite_code,
                        }),
                    );
                }
                Err(InviteRevokeError::Unauthorized) => {
                    ctx.app_state
                        .abuse_metrics
                        .increment(&ctx.app_state.abuse_metrics.revoke_authorization_rejections);
                    ctx.app_state.connections.send_to(
                        sender_id,
                        &SignalingMessage::Error(ErrorPayload {
                            message: "unauthorized".to_string(),
                        }),
                    );
                }
                Err(InviteRevokeError::NotFound) => {
                    ctx.app_state.connections.send_to(
                        sender_id,
                        &SignalingMessage::Error(ErrorPayload {
                            message: "invite code not found".to_string(),
                        }),
                    );
                }
            }
            DispatchOutcome::Continue
        }
        SignalingMessage::KickParticipant(ref payload) => {
            // Session is guaranteed Some here (pre-join gate already handled above)
            let session_ref = ctx.session.as_ref().unwrap();
            let kicker_id = session_ref.participant_id.as_str();

            // Action rate limit check (Req 3.1, 3.3)
            if !ctx.rate_limiter.action_allow() {
                warn!(peer_id = %ctx.peer_id, "ws peer exceeded action rate limit");
                ctx.app_state
                    .abuse_metrics
                    .increment(&ctx.app_state.abuse_metrics.action_rate_limit_rejections);
                ctx.app_state.connections.send_to(
                    kicker_id,
                    &SignalingMessage::Error(ErrorPayload {
                        message: MSG_ACTION_RATE_LIMIT_EXCEEDED.to_string(),
                    }),
                );
                return DispatchOutcome::Continue;
            }

            // Reject action messages in P2P rooms (Req 3.3)
            let room_id_opt = ctx.app_state.room_state.get_room_for_peer(kicker_id);
            let room_id = match room_id_opt {
                Some(ref r) => r.clone(),
                None => {
                    ctx.app_state.connections.send_to(
                        kicker_id,
                        &SignalingMessage::Error(ErrorPayload {
                            message: "not in a room".to_string(),
                        }),
                    );
                    return DispatchOutcome::Continue;
                }
            };
            let room_info = ctx.app_state.room_state.get_room_info(&room_id);
            let room_type = room_info.as_ref().map(|i| i.room_type);
            if room_type != Some(RoomType::Sfu) {
                ctx.app_state.connections.send_to(
                    kicker_id,
                    &SignalingMessage::Error(ErrorPayload {
                        message: "action not supported in P2P mode".to_string(),
                    }),
                );
                return DispatchOutcome::Continue;
            }

            // Determine effective role: lazy enforcement for channel sessions (Req 6.2, 6.5)
            let effective_role = if let Some(ref channel_id) = session_ref.channel_id {
                // Channel-based session: re-query current role from DB
                let user_id_str = session_ref
                    .user_id
                    .as_ref()
                    .expect("channel session always has user_id");
                let user_id_uuid =
                    Uuid::parse_str(user_id_str).expect("session user_id is always a valid UUID");
                match voice_orchestrator::get_current_channel_role(
                    &ctx.app_state.db_pool,
                    channel_id,
                    &user_id_uuid,
                )
                .await
                {
                    Ok(Some(channel_role)) => voice_orchestrator::map_channel_role(channel_role),
                    Ok(None) => {
                        // User no longer a member — reject action
                        warn!(peer_id = %ctx.peer_id, channel_id = %channel_id, "kick rejected: user no longer channel member");
                        ctx.app_state.connections.send_to(
                            kicker_id,
                            &SignalingMessage::Error(ErrorPayload {
                                message: "not authorized".to_string(),
                            }),
                        );
                        return DispatchOutcome::Continue;
                    }
                    Err(e) => {
                        // DB error — fail-closed, reject action
                        error!(peer_id = %ctx.peer_id, channel_id = %channel_id, error = %e, "kick rejected: DB error during lazy role check");
                        ctx.app_state.connections.send_to(
                            kicker_id,
                            &SignalingMessage::Error(ErrorPayload {
                                message: "internal error".to_string(),
                            }),
                        );
                        return DispatchOutcome::Continue;
                    }
                }
            } else {
                // Legacy room-based session: use cached session.role
                session_ref.role
            };

            // Check role is Host (Req 3.2, 3.3)
            if effective_role != ParticipantRole::Host {
                ctx.app_state.connections.send_to(
                    kicker_id,
                    &SignalingMessage::Error(ErrorPayload {
                        message: "unauthorized".to_string(),
                    }),
                );
                return DispatchOutcome::Continue;
            }

            // Check target is in the same room (Req 3.6)
            let target_id = payload.target_participant_id.clone();
            let target_in_room = room_info
                .as_ref()
                .map(|i| i.participants.iter().any(|p| p.participant_id == target_id))
                .unwrap_or(false);
            if !target_in_room {
                ctx.app_state.connections.send_to(
                    kicker_id,
                    &SignalingMessage::Error(ErrorPayload {
                        message: "target not in room".to_string(),
                    }),
                );
                return DispatchOutcome::Continue;
            }

            // Call domain function (Req 3.4)
            match handle_kick(
                ctx.app_state.sfu_room_manager.as_ref(),
                ctx.app_state.room_state.as_ref(),
                &room_id,
                kicker_id,
                effective_role,
                &target_id,
            )
            .await
            {
                Ok(mut signals) => {
                    let sub_room_result = voice_orchestrator::remove_participant_from_sub_room(
                        ctx.app_state.room_state.as_ref(),
                        &room_id,
                        &target_id,
                    );
                    signals.extend(sub_room_result.signals);
                    dispatch_signals(
                        signals,
                        &room_id,
                        ctx.app_state.room_state.as_ref(),
                        ctx.app_state.connections.as_ref(),
                    );
                    if let Some(expiry) = sub_room_result.expiry {
                        schedule_sub_room_expiry(
                            ctx.app_state,
                            &room_id,
                            &expiry.sub_room_id,
                            expiry.delete_at,
                        );
                    }
                }
                Err(e) => {
                    ctx.app_state.connections.send_to(
                        kicker_id,
                        &SignalingMessage::Error(ErrorPayload {
                            message: format!("kick failed: {e}"),
                        }),
                    );
                }
            }
            DispatchOutcome::Continue
        }
        SignalingMessage::MuteParticipant(ref payload) => {
            // Session is guaranteed Some here (pre-join gate already handled above)
            let session_ref = ctx.session.as_ref().unwrap();
            let muter_id = session_ref.participant_id.as_str();

            // Action rate limit check (Req 3.1, 3.3)
            if !ctx.rate_limiter.action_allow() {
                warn!(peer_id = %ctx.peer_id, "ws peer exceeded action rate limit");
                ctx.app_state
                    .abuse_metrics
                    .increment(&ctx.app_state.abuse_metrics.action_rate_limit_rejections);
                ctx.app_state.connections.send_to(
                    muter_id,
                    &SignalingMessage::Error(ErrorPayload {
                        message: MSG_ACTION_RATE_LIMIT_EXCEEDED.to_string(),
                    }),
                );
                return DispatchOutcome::Continue;
            }

            // Reject action messages in P2P rooms (Req 3.3)
            let room_id_opt = ctx.app_state.room_state.get_room_for_peer(muter_id);
            let room_id = match room_id_opt {
                Some(ref r) => r.clone(),
                None => {
                    ctx.app_state.connections.send_to(
                        muter_id,
                        &SignalingMessage::Error(ErrorPayload {
                            message: "not in a room".to_string(),
                        }),
                    );
                    return DispatchOutcome::Continue;
                }
            };
            let room_info = ctx.app_state.room_state.get_room_info(&room_id);
            let room_type = room_info.as_ref().map(|i| i.room_type);
            if room_type != Some(RoomType::Sfu) {
                ctx.app_state.connections.send_to(
                    muter_id,
                    &SignalingMessage::Error(ErrorPayload {
                        message: "action not supported in P2P mode".to_string(),
                    }),
                );
                return DispatchOutcome::Continue;
            }

            // Determine effective role: lazy enforcement for channel sessions (Req 6.2, 6.5)
            let effective_role = if let Some(ref channel_id) = session_ref.channel_id {
                // Channel-based session: re-query current role from DB
                let user_id_str = session_ref
                    .user_id
                    .as_ref()
                    .expect("channel session always has user_id");
                let user_id_uuid =
                    Uuid::parse_str(user_id_str).expect("session user_id is always a valid UUID");
                match voice_orchestrator::get_current_channel_role(
                    &ctx.app_state.db_pool,
                    channel_id,
                    &user_id_uuid,
                )
                .await
                {
                    Ok(Some(channel_role)) => voice_orchestrator::map_channel_role(channel_role),
                    Ok(None) => {
                        // User no longer a member — reject action
                        warn!(peer_id = %ctx.peer_id, channel_id = %channel_id, "mute rejected: user no longer channel member");
                        ctx.app_state.connections.send_to(
                            muter_id,
                            &SignalingMessage::Error(ErrorPayload {
                                message: "not authorized".to_string(),
                            }),
                        );
                        return DispatchOutcome::Continue;
                    }
                    Err(e) => {
                        // DB error — fail-closed, reject action
                        error!(peer_id = %ctx.peer_id, channel_id = %channel_id, error = %e, "mute rejected: DB error during lazy role check");
                        ctx.app_state.connections.send_to(
                            muter_id,
                            &SignalingMessage::Error(ErrorPayload {
                                message: "internal error".to_string(),
                            }),
                        );
                        return DispatchOutcome::Continue;
                    }
                }
            } else {
                // Legacy room-based session: use cached session.role
                session_ref.role
            };

            // Check role is Host (Req 3.2, 3.3)
            if effective_role != ParticipantRole::Host {
                ctx.app_state.connections.send_to(
                    muter_id,
                    &SignalingMessage::Error(ErrorPayload {
                        message: "unauthorized".to_string(),
                    }),
                );
                return DispatchOutcome::Continue;
            }

            // Check target is in the same room (Req 3.6)
            let target_id = &payload.target_participant_id;
            let target_in_room = room_info
                .as_ref()
                .map(|i| {
                    i.participants
                        .iter()
                        .any(|p| &p.participant_id == target_id)
                })
                .unwrap_or(false);
            if !target_in_room {
                ctx.app_state.connections.send_to(
                    muter_id,
                    &SignalingMessage::Error(ErrorPayload {
                        message: "target not in room".to_string(),
                    }),
                );
                return DispatchOutcome::Continue;
            }

            // Call domain function (Req 4.1)
            let target_id_owned = target_id.clone();
            match handle_mute(
                ctx.app_state.room_state.as_ref(),
                &room_id,
                muter_id,
                effective_role,
                &target_id_owned,
            )
            .await
            {
                Ok(signals) => {
                    dispatch_signals(
                        signals,
                        &room_id,
                        ctx.app_state.room_state.as_ref(),
                        ctx.app_state.connections.as_ref(),
                    );
                }
                Err(e) => {
                    ctx.app_state.connections.send_to(
                        muter_id,
                        &SignalingMessage::Error(ErrorPayload {
                            message: format!("mute failed: {e}"),
                        }),
                    );
                }
            }
            DispatchOutcome::Continue
        }
        SignalingMessage::UnmuteParticipant(ref payload) => {
            // Session is guaranteed Some here (pre-join gate already handled above)
            let session_ref = ctx.session.as_ref().unwrap();
            let unmuter_id = session_ref.participant_id.as_str();

            // Action rate limit check
            if !ctx.rate_limiter.action_allow() {
                warn!(peer_id = %ctx.peer_id, "ws peer exceeded action rate limit");
                ctx.app_state
                    .abuse_metrics
                    .increment(&ctx.app_state.abuse_metrics.action_rate_limit_rejections);
                ctx.app_state.connections.send_to(
                    unmuter_id,
                    &SignalingMessage::Error(ErrorPayload {
                        message: MSG_ACTION_RATE_LIMIT_EXCEEDED.to_string(),
                    }),
                );
                return DispatchOutcome::Continue;
            }

            // Reject action messages in P2P rooms
            let room_id_opt = ctx.app_state.room_state.get_room_for_peer(unmuter_id);
            let room_id = match room_id_opt {
                Some(ref r) => r.clone(),
                None => {
                    ctx.app_state.connections.send_to(
                        unmuter_id,
                        &SignalingMessage::Error(ErrorPayload {
                            message: "not in a room".to_string(),
                        }),
                    );
                    return DispatchOutcome::Continue;
                }
            };
            let room_info = ctx.app_state.room_state.get_room_info(&room_id);
            let room_type = room_info.as_ref().map(|i| i.room_type);
            if room_type != Some(RoomType::Sfu) {
                ctx.app_state.connections.send_to(
                    unmuter_id,
                    &SignalingMessage::Error(ErrorPayload {
                        message: "action not supported in P2P mode".to_string(),
                    }),
                );
                return DispatchOutcome::Continue;
            }

            // Determine effective role: lazy enforcement for channel sessions
            let effective_role = if let Some(ref channel_id) = session_ref.channel_id {
                let user_id_str = session_ref
                    .user_id
                    .as_ref()
                    .expect("channel session always has user_id");
                let user_id_uuid =
                    Uuid::parse_str(user_id_str).expect("session user_id is always a valid UUID");
                match voice_orchestrator::get_current_channel_role(
                    &ctx.app_state.db_pool,
                    channel_id,
                    &user_id_uuid,
                )
                .await
                {
                    Ok(Some(channel_role)) => voice_orchestrator::map_channel_role(channel_role),
                    Ok(None) => {
                        warn!(peer_id = %ctx.peer_id, channel_id = %channel_id, "unmute rejected: user no longer channel member");
                        ctx.app_state.connections.send_to(
                            unmuter_id,
                            &SignalingMessage::Error(ErrorPayload {
                                message: "not authorized".to_string(),
                            }),
                        );
                        return DispatchOutcome::Continue;
                    }
                    Err(e) => {
                        error!(peer_id = %ctx.peer_id, channel_id = %channel_id, error = %e, "unmute rejected: DB error during lazy role check");
                        ctx.app_state.connections.send_to(
                            unmuter_id,
                            &SignalingMessage::Error(ErrorPayload {
                                message: "internal error".to_string(),
                            }),
                        );
                        return DispatchOutcome::Continue;
                    }
                }
            } else {
                session_ref.role
            };

            // Check role is Host
            if effective_role != ParticipantRole::Host {
                ctx.app_state.connections.send_to(
                    unmuter_id,
                    &SignalingMessage::Error(ErrorPayload {
                        message: "unauthorized".to_string(),
                    }),
                );
                return DispatchOutcome::Continue;
            }

            // Check target is in the same room
            let target_id = &payload.target_participant_id;
            let target_in_room = room_info
                .as_ref()
                .map(|i| {
                    i.participants
                        .iter()
                        .any(|p| &p.participant_id == target_id)
                })
                .unwrap_or(false);
            if !target_in_room {
                ctx.app_state.connections.send_to(
                    unmuter_id,
                    &SignalingMessage::Error(ErrorPayload {
                        message: "target not in room".to_string(),
                    }),
                );
                return DispatchOutcome::Continue;
            }

            // Call domain function
            let target_id_owned = target_id.clone();
            match handle_unmute(
                ctx.app_state.room_state.as_ref(),
                &room_id,
                unmuter_id,
                effective_role,
                &target_id_owned,
            )
            .await
            {
                Ok(signals) => {
                    dispatch_signals(
                        signals,
                        &room_id,
                        ctx.app_state.room_state.as_ref(),
                        ctx.app_state.connections.as_ref(),
                    );
                }
                Err(e) => {
                    ctx.app_state.connections.send_to(
                        unmuter_id,
                        &SignalingMessage::Error(ErrorPayload {
                            message: format!("unmute failed: {e}"),
                        }),
                    );
                }
            }
            DispatchOutcome::Continue
        }
        SignalingMessage::SelfDeafen => {
            let session_ref = ctx.session.as_ref().unwrap();
            let sender_id = session_ref.participant_id.as_str();

            if !ctx.rate_limiter.deafen_allow() {
                warn!(peer_id = %ctx.peer_id, "ws peer exceeded deafen rate limit on SelfDeafen");
                ctx.app_state
                    .abuse_metrics
                    .increment(&ctx.app_state.abuse_metrics.action_rate_limit_rejections);
                ctx.app_state.connections.send_to(
                    sender_id,
                    &SignalingMessage::Error(ErrorPayload {
                        message: MSG_ACTION_RATE_LIMIT_EXCEEDED.to_string(),
                    }),
                );
                return DispatchOutcome::Continue;
            }

            let room_id_opt = ctx.app_state.room_state.get_room_for_peer(sender_id);
            let room_id = match room_id_opt {
                Some(ref r) => r.clone(),
                None => {
                    ctx.app_state.connections.send_to(
                        sender_id,
                        &SignalingMessage::Error(ErrorPayload {
                            message: "not in a room".to_string(),
                        }),
                    );
                    return DispatchOutcome::Continue;
                }
            };

            dispatch_signals(
                vec![OutboundSignal::broadcast_all(
                    SignalingMessage::ParticipantDeafened(ParticipantDeafenedPayload {
                        participant_id: sender_id.to_string(),
                    }),
                )],
                &room_id,
                ctx.app_state.room_state.as_ref(),
                ctx.app_state.connections.as_ref(),
            );
            DispatchOutcome::Continue
        }
        SignalingMessage::SelfUndeafen => {
            let session_ref = ctx.session.as_ref().unwrap();
            let sender_id = session_ref.participant_id.as_str();

            if !ctx.rate_limiter.deafen_allow() {
                warn!(peer_id = %ctx.peer_id, "ws peer exceeded deafen rate limit on SelfUndeafen");
                ctx.app_state
                    .abuse_metrics
                    .increment(&ctx.app_state.abuse_metrics.action_rate_limit_rejections);
                ctx.app_state.connections.send_to(
                    sender_id,
                    &SignalingMessage::Error(ErrorPayload {
                        message: MSG_ACTION_RATE_LIMIT_EXCEEDED.to_string(),
                    }),
                );
                return DispatchOutcome::Continue;
            }

            let room_id_opt = ctx.app_state.room_state.get_room_for_peer(sender_id);
            let room_id = match room_id_opt {
                Some(ref r) => r.clone(),
                None => {
                    ctx.app_state.connections.send_to(
                        sender_id,
                        &SignalingMessage::Error(ErrorPayload {
                            message: "not in a room".to_string(),
                        }),
                    );
                    return DispatchOutcome::Continue;
                }
            };

            dispatch_signals(
                vec![OutboundSignal::broadcast_all(
                    SignalingMessage::ParticipantUndeafened(ParticipantUndeafenedPayload {
                        participant_id: sender_id.to_string(),
                    }),
                )],
                &room_id,
                ctx.app_state.room_state.as_ref(),
                ctx.app_state.connections.as_ref(),
            );
            DispatchOutcome::Continue
        }
        SignalingMessage::UpdateProfileColor(payload) => {
            let session_ref = ctx.session.as_ref().unwrap();
            let sender_id = session_ref.participant_id.as_str();

            if !ctx.rate_limiter.action_allow() {
                warn!(peer_id = %ctx.peer_id, "ws peer exceeded action rate limit on UpdateProfileColor");
                ctx.app_state
                    .abuse_metrics
                    .increment(&ctx.app_state.abuse_metrics.action_rate_limit_rejections);
                ctx.app_state.connections.send_to(
                    sender_id,
                    &SignalingMessage::Error(ErrorPayload {
                        message: MSG_ACTION_RATE_LIMIT_EXCEEDED.to_string(),
                    }),
                );
                return DispatchOutcome::Continue;
            }

            let room_id_opt = ctx.app_state.room_state.get_room_for_peer(sender_id);
            let room_id = match room_id_opt {
                Some(ref r) => r.clone(),
                None => {
                    ctx.app_state.connections.send_to(
                        sender_id,
                        &SignalingMessage::Error(ErrorPayload {
                            message: "not in a room".to_string(),
                        }),
                    );
                    return DispatchOutcome::Continue;
                }
            };

            dispatch_signals(
                vec![OutboundSignal::broadcast_all(
                    SignalingMessage::ParticipantColorUpdated(ParticipantColorUpdatedPayload {
                        participant_id: sender_id.to_string(),
                        profile_color: payload.profile_color,
                    }),
                )],
                &room_id,
                ctx.app_state.room_state.as_ref(),
                ctx.app_state.connections.as_ref(),
            );
            DispatchOutcome::Continue
        }
        SignalingMessage::StartShare => {
            // Session is guaranteed Some here (pre-join gate already handled above)
            let session_ref = ctx.session.as_ref().unwrap();
            let sender_id = session_ref.participant_id.as_str();
            let sender_role = session_ref.role;

            // Action rate limit check (Req 4.2)
            if !ctx.rate_limiter.action_allow() {
                warn!(peer_id = %ctx.peer_id, "ws peer exceeded action rate limit on StartShare");
                ctx.app_state
                    .abuse_metrics
                    .increment(&ctx.app_state.abuse_metrics.action_rate_limit_rejections);
                ctx.app_state
                    .abuse_metrics
                    .increment(&ctx.app_state.abuse_metrics.screen_share_rejections);
                ctx.app_state.connections.send_to(
                    sender_id,
                    &SignalingMessage::Error(ErrorPayload {
                        message: MSG_ACTION_RATE_LIMIT_EXCEEDED.to_string(),
                    }),
                );
                return DispatchOutcome::Continue;
            }

            let room_id_opt = ctx.app_state.room_state.get_room_for_peer(sender_id);
            let room_id = match room_id_opt {
                Some(ref r) => r.clone(),
                None => {
                    ctx.app_state.connections.send_to(
                        sender_id,
                        &SignalingMessage::Error(ErrorPayload {
                            message: "not in a room".to_string(),
                        }),
                    );
                    return DispatchOutcome::Continue;
                }
            };

            match handle_start_share(
                ctx.app_state.room_state.as_ref(),
                &room_id,
                sender_id,
                sender_role,
            ) {
                ShareResult::Ok(signals) => {
                    dispatch_signals(
                        signals,
                        &room_id,
                        ctx.app_state.room_state.as_ref(),
                        ctx.app_state.connections.as_ref(),
                    );
                }
                ShareResult::Noop => {
                    // Idempotent — already sharing, nothing to do.
                }
                ShareResult::Error(err) => {
                    ctx.app_state
                        .abuse_metrics
                        .increment(&ctx.app_state.abuse_metrics.screen_share_rejections);
                    ctx.app_state.connections.send_to(sender_id, &err);
                }
            }
            DispatchOutcome::Continue
        }
        SignalingMessage::StopShare(_payload) => {
            // Session is guaranteed Some here (pre-join gate already handled above)
            let session_ref = ctx.session.as_ref().unwrap();
            let sender_id = session_ref.participant_id.as_str();
            let sender_role = session_ref.role;

            // Action rate limit check (Req 5.1)
            if !ctx.rate_limiter.action_allow() {
                warn!(peer_id = %ctx.peer_id, "ws peer exceeded action rate limit on StopShare");
                ctx.app_state
                    .abuse_metrics
                    .increment(&ctx.app_state.abuse_metrics.action_rate_limit_rejections);
                return DispatchOutcome::Continue;
            }

            let room_id_opt = ctx.app_state.room_state.get_room_for_peer(sender_id);
            let room_id = match room_id_opt {
                Some(ref r) => r.clone(),
                None => {
                    ctx.app_state.connections.send_to(
                        sender_id,
                        &SignalingMessage::Error(ErrorPayload {
                            message: "not in a room".to_string(),
                        }),
                    );
                    return DispatchOutcome::Continue;
                }
            };

            match handle_stop_share(
                ctx.app_state.room_state.as_ref(),
                &room_id,
                sender_id,
                _payload.target_participant_id.as_deref(),
                sender_role,
            ) {
                ShareResult::Ok(signals) => {
                    dispatch_signals(
                        signals,
                        &room_id,
                        ctx.app_state.room_state.as_ref(),
                        ctx.app_state.connections.as_ref(),
                    );
                }
                ShareResult::Noop => {
                    // Idempotent — non-owner/non-host attempt or no active share.
                    // Increment rejection metric only if there IS an active share
                    // (non-owner trying to stop someone else's share).
                    let has_share = ctx
                        .app_state
                        .room_state
                        .get_room_info(&room_id)
                        .map(|i| !i.active_shares.is_empty())
                        .unwrap_or(false);
                    if has_share {
                        ctx.app_state
                            .abuse_metrics
                            .increment(&ctx.app_state.abuse_metrics.screen_share_rejections);
                    }
                }
                ShareResult::Error(err) => {
                    ctx.app_state
                        .abuse_metrics
                        .increment(&ctx.app_state.abuse_metrics.screen_share_rejections);
                    ctx.app_state.connections.send_to(sender_id, &err);
                }
            }
            DispatchOutcome::Continue
        }
        SignalingMessage::StopAllShares => {
            // Session is guaranteed Some here (pre-join gate already handled above)
            let session_ref = ctx.session.as_ref().unwrap();
            let sender_id = session_ref.participant_id.as_str();
            let sender_role = session_ref.role;

            // Action rate limit check (Req 4.1)
            if !ctx.rate_limiter.action_allow() {
                warn!(peer_id = %ctx.peer_id, "ws peer exceeded action rate limit on StopAllShares");
                ctx.app_state
                    .abuse_metrics
                    .increment(&ctx.app_state.abuse_metrics.action_rate_limit_rejections);
                return DispatchOutcome::Continue;
            }

            let room_id_opt = ctx.app_state.room_state.get_room_for_peer(sender_id);
            let room_id = match room_id_opt {
                Some(ref r) => r.clone(),
                None => {
                    ctx.app_state.connections.send_to(
                        sender_id,
                        &SignalingMessage::Error(ErrorPayload {
                            message: "not in a room".to_string(),
                        }),
                    );
                    return DispatchOutcome::Continue;
                }
            };

            match handle_stop_all_shares(
                ctx.app_state.room_state.as_ref(),
                &room_id,
                sender_id,
                sender_role,
            ) {
                ShareResult::Ok(signals) => {
                    dispatch_signals(
                        signals,
                        &room_id,
                        ctx.app_state.room_state.as_ref(),
                        ctx.app_state.connections.as_ref(),
                    );
                }
                ShareResult::Noop => {
                    // Idempotent — no active shares, nothing to do.
                }
                ShareResult::Error(err) => {
                    ctx.app_state
                        .abuse_metrics
                        .increment(&ctx.app_state.abuse_metrics.screen_share_rejections);
                    ctx.app_state.connections.send_to(sender_id, &err);
                }
            }
            DispatchOutcome::Continue
        }
        SignalingMessage::SetSharePermission(ref payload) => {
            let session_ref = ctx.session.as_ref().unwrap();
            let sender_id = session_ref.participant_id.as_str();
            let sender_role = session_ref.role;

            if !ctx.rate_limiter.action_allow() {
                warn!(peer_id = %ctx.peer_id, "ws peer exceeded action rate limit on SetSharePermission");
                ctx.app_state
                    .abuse_metrics
                    .increment(&ctx.app_state.abuse_metrics.action_rate_limit_rejections);
                ctx.app_state.connections.send_to(
                    sender_id,
                    &SignalingMessage::Error(ErrorPayload {
                        message: MSG_ACTION_RATE_LIMIT_EXCEEDED.to_string(),
                    }),
                );
                return DispatchOutcome::Continue;
            }

            let room_id_opt = ctx.app_state.room_state.get_room_for_peer(sender_id);
            let room_id = match room_id_opt {
                Some(ref r) => r.clone(),
                None => {
                    ctx.app_state.connections.send_to(
                        sender_id,
                        &SignalingMessage::Error(ErrorPayload {
                            message: "not in a room".to_string(),
                        }),
                    );
                    return DispatchOutcome::Continue;
                }
            };

            // Lazy role enforcement for channel sessions
            let effective_role = if let Some(ref channel_id) = session_ref.channel_id {
                let user_id_str = session_ref
                    .user_id
                    .as_ref()
                    .expect("channel session always has user_id");
                let user_id_uuid =
                    Uuid::parse_str(user_id_str).expect("session user_id is always a valid UUID");
                match voice_orchestrator::get_current_channel_role(
                    &ctx.app_state.db_pool,
                    channel_id,
                    &user_id_uuid,
                )
                .await
                {
                    Ok(Some(channel_role)) => voice_orchestrator::map_channel_role(channel_role),
                    Ok(None) => {
                        ctx.app_state.connections.send_to(
                            sender_id,
                            &SignalingMessage::Error(ErrorPayload {
                                message: "not authorized".to_string(),
                            }),
                        );
                        return DispatchOutcome::Continue;
                    }
                    Err(e) => {
                        error!(peer_id = %ctx.peer_id, error = %e, "SetSharePermission rejected: DB error during lazy role check");
                        ctx.app_state.connections.send_to(
                            sender_id,
                            &SignalingMessage::Error(ErrorPayload {
                                message: "internal error".to_string(),
                            }),
                        );
                        return DispatchOutcome::Continue;
                    }
                }
            } else {
                sender_role
            };

            match handle_set_share_permission(
                ctx.app_state.room_state.as_ref(),
                &room_id,
                sender_id,
                effective_role,
                payload.permission,
            ) {
                ShareResult::Ok(signals) => {
                    dispatch_signals(
                        signals,
                        &room_id,
                        ctx.app_state.room_state.as_ref(),
                        ctx.app_state.connections.as_ref(),
                    );
                }
                ShareResult::Noop => {}
                ShareResult::Error(err) => {
                    ctx.app_state.connections.send_to(sender_id, &err);
                }
            }
            DispatchOutcome::Continue
        }
        SignalingMessage::JoinVoice(payload) => {
            // Channel-based voice join dispatch (Req 2.1, 5.2, 5.3, 5.5, 5.6, 7.6, 10.2, 10.4, 10.5)
            // Auth required — enforced by validate_state_transition (JoinVoice requires authenticated == true)
            let user_id_str = ctx
                .authenticated_user_id
                .as_ref()
                .expect("JoinVoice requires auth — enforced by state machine");
            let user_id_uuid =
                Uuid::parse_str(user_id_str).expect("authenticated_user_id is always a valid UUID");

            let cold_start_response: Option<u32> = {
                let mut health_w = ctx.app_state.sfu_health_status.write().await;
                match &*health_w {
                    SfuHealth::Available => None,
                    SfuHealth::Starting { since } => {
                        let elapsed = since.elapsed().as_secs() as u32;
                        Some(
                            COLD_START_ESTIMATED_WAIT_SECS
                                .saturating_sub(elapsed)
                                .max(15),
                        )
                    }
                    SfuHealth::Unavailable(_) => {
                        if let Some(ec2) = ctx.app_state.ec2_controller.clone() {
                            *health_w = SfuHealth::Starting {
                                since: Instant::now(),
                            };
                            drop(health_w);

                            let health_arc = ctx.app_state.sfu_health_status.clone();
                            tokio::spawn(async move {
                                match ec2.describe_state().await {
                                    Ok(Ec2InstanceState::Stopped) => {
                                        info!("Cold-start: issuing StartInstances");
                                        if let Err(error) = ec2.start_instance().await {
                                            error!(%error, "Cold-start: StartInstances failed");
                                            *health_arc.write().await = SfuHealth::Unavailable(
                                                format!("StartInstances failed: {error}"),
                                            );
                                        }
                                    }
                                    Ok(Ec2InstanceState::Running) => {
                                        warn!(
                                            "Cold-start: EC2 already running but SFU unhealthy; awaiting recovery"
                                        );
                                    }
                                    Ok(other) => {
                                        warn!("Cold-start: EC2 in state {other:?}; aborting start");
                                        *health_arc.write().await = SfuHealth::Unavailable(
                                            format!("EC2 in unexpected state: {other:?}"),
                                        );
                                    }
                                    Err(error) => {
                                        error!(%error, "Cold-start: describe_state failed");
                                        *health_arc.write().await =
                                            SfuHealth::Unavailable(error.to_string());
                                    }
                                }
                            });

                            Some(COLD_START_ESTIMATED_WAIT_SECS)
                        } else {
                            None
                        }
                    }
                }
            };

            if let Some(wait_secs) = cold_start_response {
                ctx.app_state.connections.send_to(
                    ctx.peer_id,
                    &SignalingMessage::SfuColdStarting(SfuColdStartingPayload {
                        estimated_wait_secs: wait_secs,
                    }),
                );
                return DispatchOutcome::Continue;
            }

            let now = Instant::now();

            // Global join ceiling check (Req 7.6)
            {
                let now_unix = SystemTime::now()
                    .duration_since(UNIX_EPOCH)
                    .unwrap_or_default()
                    .as_secs();
                if !ctx.app_state.global_join_limiter.allow(now_unix) {
                    ctx.app_state
                        .abuse_metrics
                        .increment(&ctx.app_state.abuse_metrics.global_join_ceiling_rejections);
                    warn!(peer_id = %ctx.peer_id, "voice join rejected: global join ceiling exceeded");
                    ctx.app_state.connections.send_to(
                        ctx.peer_id,
                        &SignalingMessage::Error(ErrorPayload {
                            message: "server busy, try again later".to_string(),
                        }),
                    );
                    return DispatchOutcome::Continue;
                }
            }

            // Per-connection + per-IP rate limit check (Req 10.4)
            // Use channel_id as room_id dimension, no invite code
            if let Err(reason) = ctx.app_state.join_rate_limiter.check_join(
                ctx.client_ip,
                None,
                &payload.channel_id,
                ctx.peer_id,
                now,
            ) {
                ctx.app_state.join_rate_limiter.record_attempt(
                    ctx.client_ip,
                    None,
                    &payload.channel_id,
                    ctx.peer_id,
                    true,
                    now,
                );
                ctx.app_state
                    .abuse_metrics
                    .increment(&ctx.app_state.abuse_metrics.join_rate_limit_rejections);
                if let Some(count) = ctx
                    .app_state
                    .ip_failed_join_tracker
                    .record_failure(ctx.client_ip, now)
                {
                    warn!(
                        client_ip = %ctx.client_ip,
                        failure_count = count,
                        window_seconds = ctx.app_state.ip_failed_join_tracker.window_secs(),
                        event = "ip_abuse_threshold_exceeded",
                        "per-IP failed join threshold exceeded"
                    );
                    ctx.app_state
                        .abuse_metrics
                        .increment(&ctx.app_state.abuse_metrics.invite_usage_anomalies);
                }
                ctx.app_state.connections.send_to(
                    ctx.peer_id,
                    &SignalingMessage::JoinRejected(JoinRejectedPayload { reason }),
                );
                return DispatchOutcome::Continue;
            }

            // Resolve display_name (fall back to peer_id if absent/empty)
            let display_name = payload
                .display_name
                .as_deref()
                .filter(|n| !n.trim().is_empty())
                .unwrap_or(ctx.peer_id)
                .to_string();

            // Build token mode (same logic as Join SFU path)
            let token_mode = if ctx.app_state.sfu_signaling_proxy.is_none() {
                match (
                    &ctx.sfu_config.livekit_api_key,
                    &ctx.sfu_config.livekit_api_secret,
                ) {
                    (Some(key), Some(secret)) => crate::voice::sfu_relay::TokenMode::LiveKit {
                        api_key: key.as_str(),
                        api_secret: secret.as_str(),
                        ttl_secs: crate::auth::jwt::LIVEKIT_TOKEN_TTL_SECS,
                    },
                    _ => crate::voice::sfu_relay::TokenMode::Custom {
                        jwt_secret: &ctx.sfu_config.jwt_secret,
                        issuer: &ctx.sfu_config.jwt_issuer,
                        ttl_secs: ctx.sfu_config.token_ttl_secs,
                    },
                }
            } else {
                crate::voice::sfu_relay::TokenMode::Custom {
                    jwt_secret: &ctx.sfu_config.jwt_secret,
                    issuer: &ctx.sfu_config.jwt_issuer,
                    ttl_secs: ctx.sfu_config.token_ttl_secs,
                }
            };

            // Call voice orchestrator (Req 2.1)
            let result = voice_orchestrator::join_voice(
                &ctx.app_state.db_pool,
                ctx.app_state.room_state.as_ref(),
                &ctx.app_state.active_room_map,
                ctx.app_state.sfu_room_manager.as_ref(),
                &token_mode,
                &ctx.app_state.sfu_url,
                &payload.channel_id,
                &user_id_uuid,
                ctx.peer_id,
                &display_name,
                payload.profile_color.as_deref(),
                payload.supports_sub_rooms == Some(true),
                ctx.sfu_config.max_participants,
            )
            .await;

            match result {
                Ok(join_result) => {
                    let mut signals = join_result.signals;

                    // Inject TURN credentials into Joined signal (Req 7.6)
                    inject_turn_credentials(
                        &mut signals,
                        ctx.peer_id,
                        ctx.app_state.turn_config.as_deref(),
                    );

                    // Create SignalingSession with channel_id (Req 4.5)
                    *ctx.session = Some(SignalingSession::new(
                        ctx.peer_id.to_string(),
                        join_result.room_id.clone(),
                        join_result.participant_role,
                        Some(user_id_str.clone()),
                        Some(join_result.channel_id),
                    ));

                    // Record successful join attempt (Req 10.4)
                    ctx.app_state.join_rate_limiter.record_attempt(
                        ctx.client_ip,
                        None,
                        &payload.channel_id,
                        ctx.peer_id,
                        false,
                        now,
                    );

                    // Dispatch signals (eviction ParticipantLeft + join signals)
                    dispatch_signals(
                        signals,
                        &join_result.room_id,
                        ctx.app_state.room_state.as_ref(),
                        ctx.app_state.connections.as_ref(),
                    );
                    if let Some(expiry) = join_result.sub_room_expiry {
                        schedule_sub_room_expiry(
                            ctx.app_state,
                            &join_result.room_id,
                            &expiry.sub_room_id,
                            expiry.delete_at,
                        );
                    }

                    // Close the stale WebSocket connection if a ghost session was evicted.
                    // Send a SessionDisplaced message first so the evicted client
                    // knows not to reconnect (prevents infinite reconnect loop).
                    // Unregistering the sender drops the mpsc channel, causing the
                    // stale connection's outbound_rx.recv() to return None and break
                    // its event loop — triggering graceful cleanup on that task.
                    if let Some(ref evicted_id) = join_result.evicted_peer_id {
                        ctx.app_state.connections.send_to(
                            evicted_id,
                            &SignalingMessage::SessionDisplaced(
                                shared::signaling::SessionDisplacedPayload {
                                    reason: "another session connected".to_string(),
                                },
                            ),
                        );
                        ctx.app_state.connections.unregister(evicted_id);
                    }
                }
                Err(e) => {
                    // Map VoiceJoinError to wire JoinRejectionReason (Req 5.4, 10.1)
                    let reason = match &e {
                        VoiceJoinError::RoomFull => {
                            shared::signaling::JoinRejectionReason::RoomFull
                        }
                        VoiceJoinError::NotChannelMember
                        | VoiceJoinError::ChannelBanned
                        | VoiceJoinError::InvalidChannelId
                        | VoiceJoinError::DatabaseError(_)
                        | VoiceJoinError::SfuError(_)
                        | VoiceJoinError::InternalError(_) => {
                            shared::signaling::JoinRejectionReason::NotAuthorized
                        }
                    };

                    // Log with differentiated internal reason (Req 10.2, 10.5)
                    match &e {
                        VoiceJoinError::NotChannelMember => {
                            warn!(
                                user_id = %user_id_str,
                                channel_id = %payload.channel_id,
                                reason = "not_channel_member",
                                "voice join rejected"
                            );
                        }
                        VoiceJoinError::ChannelBanned => {
                            warn!(
                                user_id = %user_id_str,
                                channel_id = %payload.channel_id,
                                reason = "channel_banned",
                                "voice join rejected"
                            );
                        }
                        VoiceJoinError::DatabaseError(msg) => {
                            error!(
                                user_id = %user_id_str,
                                channel_id = %payload.channel_id,
                                error = %msg,
                                "voice join DB error"
                            );
                        }
                        VoiceJoinError::InvalidChannelId => {
                            warn!(
                                user_id = %user_id_str,
                                channel_id = %payload.channel_id,
                                reason = "invalid_channel_id",
                                "voice join rejected"
                            );
                        }
                        VoiceJoinError::RoomFull => {
                            warn!(
                                user_id = %user_id_str,
                                channel_id = %payload.channel_id,
                                reason = "room_full",
                                "voice join rejected"
                            );
                        }
                        VoiceJoinError::SfuError(msg) => {
                            error!(
                                user_id = %user_id_str,
                                channel_id = %payload.channel_id,
                                error = %msg,
                                "voice join SFU error"
                            );
                        }
                        VoiceJoinError::InternalError(msg) => {
                            error!(
                                user_id = %user_id_str,
                                channel_id = %payload.channel_id,
                                error = %msg,
                                "voice join internal error"
                            );
                        }
                    }

                    // Record failed join attempt (Req 10.4)
                    ctx.app_state.join_rate_limiter.record_attempt(
                        ctx.client_ip,
                        None,
                        &payload.channel_id,
                        ctx.peer_id,
                        true,
                        now,
                    );
                    ctx.app_state
                        .ip_failed_join_tracker
                        .record_failure(ctx.client_ip, now);

                    // Send JoinRejected (Req 5.4)
                    ctx.app_state.connections.send_to(
                        ctx.peer_id,
                        &SignalingMessage::JoinRejected(JoinRejectedPayload { reason }),
                    );
                }
            }
            DispatchOutcome::Continue
        }
        SignalingMessage::CreateSubRoom(_) => {
            let Some(session_ref) = ctx.session.as_ref() else {
                return DispatchOutcome::Continue;
            };
            if session_ref.channel_id.is_none() {
                ctx.app_state.connections.send_to(
                    ctx.peer_id,
                    &SignalingMessage::Error(ErrorPayload {
                        message: "sub-rooms require a channel voice session".to_string(),
                    }),
                );
                return DispatchOutcome::Continue;
            }
            if !ctx.rate_limiter.action_allow() {
                return DispatchOutcome::Continue;
            }

            match voice_orchestrator::create_sub_room(
                ctx.app_state.room_state.as_ref(),
                &session_ref.room_id,
            ) {
                Ok(result) => {
                    dispatch_signals(
                        result.signals,
                        &session_ref.room_id,
                        ctx.app_state.room_state.as_ref(),
                        ctx.app_state.connections.as_ref(),
                    );
                    if let Some(expiry) = result.expiry {
                        schedule_sub_room_expiry(
                            ctx.app_state,
                            &session_ref.room_id,
                            &expiry.sub_room_id,
                            expiry.delete_at,
                        );
                    }
                }
                Err(message) => {
                    ctx.app_state.connections.send_to(
                        ctx.peer_id,
                        &SignalingMessage::Error(ErrorPayload { message }),
                    );
                }
            }
            DispatchOutcome::Continue
        }
        SignalingMessage::JoinSubRoom(payload) => {
            let Some(session_ref) = ctx.session.as_ref() else {
                return DispatchOutcome::Continue;
            };
            if session_ref.channel_id.is_none() {
                ctx.app_state.connections.send_to(
                    ctx.peer_id,
                    &SignalingMessage::Error(ErrorPayload {
                        message: "sub-rooms require a channel voice session".to_string(),
                    }),
                );
                return DispatchOutcome::Continue;
            }
            if !ctx.rate_limiter.action_allow() {
                return DispatchOutcome::Continue;
            }

            match voice_orchestrator::join_sub_room(
                ctx.app_state.room_state.as_ref(),
                &session_ref.room_id,
                &payload.sub_room_id,
                &session_ref.participant_id,
            ) {
                Ok(result) => {
                    dispatch_signals(
                        result.signals,
                        &session_ref.room_id,
                        ctx.app_state.room_state.as_ref(),
                        ctx.app_state.connections.as_ref(),
                    );
                    if let Some(expiry) = result.expiry {
                        schedule_sub_room_expiry(
                            ctx.app_state,
                            &session_ref.room_id,
                            &expiry.sub_room_id,
                            expiry.delete_at,
                        );
                    }
                }
                Err(message) => {
                    ctx.app_state.connections.send_to(
                        ctx.peer_id,
                        &SignalingMessage::Error(ErrorPayload { message }),
                    );
                }
            }
            DispatchOutcome::Continue
        }
        SignalingMessage::LeaveSubRoom(_) => {
            let Some(session_ref) = ctx.session.as_ref() else {
                return DispatchOutcome::Continue;
            };
            if session_ref.channel_id.is_none() {
                ctx.app_state.connections.send_to(
                    ctx.peer_id,
                    &SignalingMessage::Error(ErrorPayload {
                        message: "sub-rooms require a channel voice session".to_string(),
                    }),
                );
                return DispatchOutcome::Continue;
            }
            if !ctx.rate_limiter.action_allow() {
                return DispatchOutcome::Continue;
            }

            match voice_orchestrator::leave_sub_room(
                ctx.app_state.room_state.as_ref(),
                &session_ref.room_id,
                &session_ref.participant_id,
            ) {
                Ok(result) => {
                    dispatch_signals(
                        result.signals,
                        &session_ref.room_id,
                        ctx.app_state.room_state.as_ref(),
                        ctx.app_state.connections.as_ref(),
                    );
                    if let Some(expiry) = result.expiry {
                        schedule_sub_room_expiry(
                            ctx.app_state,
                            &session_ref.room_id,
                            &expiry.sub_room_id,
                            expiry.delete_at,
                        );
                    }
                }
                Err(message) => {
                    ctx.app_state.connections.send_to(
                        ctx.peer_id,
                        &SignalingMessage::Error(ErrorPayload { message }),
                    );
                }
            }
            DispatchOutcome::Continue
        }
        SignalingMessage::SetPassthrough(payload) => {
            let Some(session_ref) = ctx.session.as_ref() else {
                return DispatchOutcome::Continue;
            };
            if session_ref.channel_id.is_none() {
                ctx.app_state.connections.send_to(
                    ctx.peer_id,
                    &SignalingMessage::Error(ErrorPayload {
                        message: "passthrough requires a channel voice session".to_string(),
                    }),
                );
                return DispatchOutcome::Continue;
            }
            if !ctx.rate_limiter.action_allow() {
                return DispatchOutcome::Continue;
            }

            match voice_orchestrator::set_passthrough(
                ctx.app_state.room_state.as_ref(),
                &session_ref.room_id,
                &session_ref.participant_id,
                &payload.target_sub_room_id,
            ) {
                Ok(result) => {
                    dispatch_signals(
                        result.signals,
                        &session_ref.room_id,
                        ctx.app_state.room_state.as_ref(),
                        ctx.app_state.connections.as_ref(),
                    );
                }
                Err(message) => {
                    ctx.app_state.connections.send_to(
                        ctx.peer_id,
                        &SignalingMessage::Error(ErrorPayload { message }),
                    );
                }
            }
            DispatchOutcome::Continue
        }
        SignalingMessage::ClearPassthrough(_) => {
            let Some(session_ref) = ctx.session.as_ref() else {
                return DispatchOutcome::Continue;
            };
            if session_ref.channel_id.is_none() {
                ctx.app_state.connections.send_to(
                    ctx.peer_id,
                    &SignalingMessage::Error(ErrorPayload {
                        message: "passthrough requires a channel voice session".to_string(),
                    }),
                );
                return DispatchOutcome::Continue;
            }
            if !ctx.rate_limiter.action_allow() {
                return DispatchOutcome::Continue;
            }

            match voice_orchestrator::clear_passthrough(
                ctx.app_state.room_state.as_ref(),
                &session_ref.room_id,
                &session_ref.participant_id,
            ) {
                Ok(result) => {
                    dispatch_signals(
                        result.signals,
                        &session_ref.room_id,
                        ctx.app_state.room_state.as_ref(),
                        ctx.app_state.connections.as_ref(),
                    );
                }
                Err(message) => {
                    ctx.app_state.connections.send_to(
                        ctx.peer_id,
                        &SignalingMessage::Error(ErrorPayload { message }),
                    );
                }
            }
            DispatchOutcome::Continue
        }
        SignalingMessage::SubRoomState(_)
        | SignalingMessage::SubRoomCreated(_)
        | SignalingMessage::SubRoomJoined(_)
        | SignalingMessage::SubRoomLeft(_)
        | SignalingMessage::SubRoomDeleted(_) => {
            ctx.app_state.connections.send_to(
                ctx.peer_id,
                &SignalingMessage::Error(ErrorPayload {
                    message: "unexpected server-generated sub-room message".to_string(),
                }),
            );
            DispatchOutcome::Continue
        }
        SignalingMessage::ChatSend(payload) => {
            // Chat-specific rate limit (independent of global WS rate limiter)
            if !ctx.chat_rate_limiter.allow() {
                // Refund the global rate-limiter tokens so that
                // non-fatal chat rejection doesn't push the
                // connection toward the fatal global limit.
                ctx.rate_limiter.refund();
                ctx.app_state.connections.send_to(
                    ctx.peer_id,
                    &SignalingMessage::Error(ErrorPayload {
                        message: "chat rate limit exceeded".to_string(),
                    }),
                );
                return DispatchOutcome::Continue;
            }

            let session_ref = ctx.session.as_ref().unwrap(); // guaranteed by state machine

            // Resolve display name from room state, fallback to participant_id
            let display_name = ctx
                .app_state
                .room_state
                .get_room_info(&session_ref.room_id)
                .and_then(|info| {
                    info.participants
                        .iter()
                        .find(|p| p.participant_id == session_ref.participant_id)
                        .map(|p| p.display_name.clone())
                })
                .filter(|name| !name.is_empty())
                .unwrap_or_else(|| session_ref.participant_id.clone());

            let timestamp = chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Secs, true);

            // Generate message_id before broadcast so it's included in
            // both the real-time relay and the DB insert (Req 3.1, 3.5)
            let message_id = Uuid::new_v4();

            let signals = chat::handle_chat_send(
                &payload.text,
                &session_ref.participant_id,
                &display_name,
                &timestamp,
                &message_id.to_string(),
            );
            dispatch_signals(
                signals,
                &session_ref.room_id,
                ctx.app_state.room_state.as_ref(),
                ctx.app_state.connections.as_ref(),
            );

            // Best-effort async persistence (Req 1.2, 1.3)
            let db_pool = ctx.app_state.db_pool.clone();
            let channel_id_uuid: Option<Uuid> = session_ref
                .channel_id
                .as_ref()
                .and_then(|cid| cid.parse::<Uuid>().ok());
            let room_id_clone = session_ref.room_id.clone();
            let participant_id_clone = session_ref.participant_id.clone();
            let display_name_clone = display_name.clone();
            let text_clone = payload.text.clone();
            tokio::spawn(async move {
                if let Err(e) = chat_persistence::insert_chat_message(
                    &db_pool,
                    message_id,
                    channel_id_uuid,
                    &room_id_clone,
                    &participant_id_clone,
                    &display_name_clone,
                    &text_clone,
                )
                .await
                {
                    tracing::warn!(error = %e, "chat message persistence failed (best-effort)");
                }
            });
            DispatchOutcome::Continue
        }
        SignalingMessage::ChatHistoryRequest(payload) => {
            let session_ref = ctx.session.as_ref().unwrap(); // guaranteed by state machine

            // Resolve scope from session (Req 2.2)
            let channel_id_uuid: Option<Uuid> = session_ref
                .channel_id
                .as_ref()
                .and_then(|cid| cid.parse::<Uuid>().ok());

            // Parse since cursor (Req 3.2) — malformed = treat as None
            let since: Option<chrono::DateTime<chrono::Utc>> = payload
                .since
                .as_ref()
                .and_then(|s| chrono::DateTime::parse_from_rfc3339(s).ok())
                .map(|dt| dt.with_timezone(&chrono::Utc));

            let db_pool = ctx.app_state.db_pool.clone();
            let room_id = session_ref.room_id.clone();
            let peer_id_clone = ctx.peer_id.to_string();
            let connections = ctx.app_state.connections.clone();

            tokio::spawn(async move {
                let result = if let Some(cid) = channel_id_uuid {
                    chat_persistence::fetch_history_by_channel(&db_pool, cid, since, 200).await
                } else {
                    chat_persistence::fetch_history_by_room(&db_pool, &room_id, since, 200).await
                };

                match result {
                    Ok(rows) => {
                        let response = chat::build_history_response(rows);
                        connections.send_to(&peer_id_clone, &response);
                    }
                    Err(e) => {
                        tracing::warn!(error = %e, "failed to load chat history");
                        connections.send_to(
                            &peer_id_clone,
                            &SignalingMessage::Error(ErrorPayload {
                                message: "failed to load chat history".to_string(),
                            }),
                        );
                    }
                }
            });
            DispatchOutcome::Continue
        }
        SignalingMessage::ViewerSubscribed(ref payload) => {
            let session_ref = ctx.session.as_ref().unwrap();
            let sender_id = session_ref.participant_id.clone();
            let room_id = session_ref.room_id.clone();

            if !ctx.rate_limiter.action_allow() {
                return DispatchOutcome::Continue;
            }

            // Verify target is in the same room
            let peers = ctx.app_state.room_state.get_peers_in_room(&room_id);
            if !peers.contains(&payload.target_id) {
                return DispatchOutcome::Continue;
            }

            // Resolve viewer's display name (same pattern as ChatSend)
            let display_name = ctx
                .app_state
                .room_state
                .get_room_info(&room_id)
                .and_then(|info| {
                    info.participants
                        .iter()
                        .find(|p| p.participant_id == sender_id)
                        .map(|p| p.display_name.clone())
                })
                .filter(|name| !name.is_empty())
                .unwrap_or_else(|| sender_id.clone());

            // Send ViewerJoined to the sharer only
            ctx.app_state.connections.send_to(
                &payload.target_id,
                &SignalingMessage::ViewerJoined(ViewerJoinedPayload {
                    viewer_id: sender_id,
                    display_name,
                }),
            );
            DispatchOutcome::Continue
        }
        SignalingMessage::Ping => {
            /* keepalive — no action */
            DispatchOutcome::Continue
        }
        _ => {
            // Use session identity for sender (Req 2.3, 2.4)
            let sender_id = ctx
                .session
                .as_ref()
                .map(|s| s.participant_id.as_str())
                .unwrap_or(ctx.peer_id);
            handle_signaling_message(
                ctx.app_state.room_state.as_ref(),
                sender_id,
                ctx.raw_text,
                ctx.app_state.connections.as_ref(),
            );
            DispatchOutcome::Continue
        }
    }
}

/// Handle an incoming WebSocket text frame containing a signaling message.
///
/// # Requirements
/// - 1.1: Relay offer messages
/// - 1.2: Relay answer messages
/// - 1.3: Relay ice_candidate messages
/// - 1.4: Reject messages when no peer is available
/// - 2.3: Parse incoming messages as UTF-8 JSON
/// - 2.4: Send error for malformed messages
pub(crate) fn handle_signaling_message(
    state: &dyn RoomState,
    sender_peer_id: &str,
    text: &str,
    connections: &dyn ConnectionManager,
) {
    let message = match signaling::parse(text) {
        Ok(msg) => msg,
        Err(_) => {
            connections.send_to(
                sender_peer_id,
                &SignalingMessage::Error(ErrorPayload {
                    message: "invalid JSON".to_string(),
                }),
            );
            return;
        }
    };
    handle_signaling_event(state, sender_peer_id, message, connections);
}

pub(crate) fn handle_signaling_event(
    state: &dyn RoomState,
    sender_peer_id: &str,
    message: SignalingMessage,
    connections: &dyn ConnectionManager,
) {
    match message {
        SignalingMessage::Offer(_)
        | SignalingMessage::Answer(_)
        | SignalingMessage::IceCandidate(_) => {
            match relay::relay_signaling(state, sender_peer_id, message) {
                RelayResult::Relayed {
                    target_peer_id,
                    message,
                } => {
                    connections.send_to(&target_peer_id, &message);
                }
                RelayResult::NoPeer { error } => {
                    warn!(peer_id = %sender_peer_id, "relay failed: no peer available");
                    connections.send_to(sender_peer_id, &error);
                }
            }
        }
        SignalingMessage::Leave => {
            if let Some((target_peer_id, msg)) = relay::handle_disconnect(state, sender_peer_id) {
                connections.send_to(&target_peer_id, &msg);
            }
        }
        SignalingMessage::Join(_)
        | SignalingMessage::Joined(_)
        | SignalingMessage::PeerLeft
        | SignalingMessage::Error(_)
        | SignalingMessage::JoinRejected(_)
        | SignalingMessage::InviteCreate(_)
        | SignalingMessage::InviteCreated(_)
        | SignalingMessage::InviteRevoke(_)
        | SignalingMessage::InviteRevoked(_)
        | SignalingMessage::ParticipantJoined(_)
        | SignalingMessage::ParticipantLeft(_)
        | SignalingMessage::RoomState(_)
        | SignalingMessage::MediaToken(_)
        | SignalingMessage::KickParticipant(_)
        | SignalingMessage::MuteParticipant(_)
        | SignalingMessage::UnmuteParticipant(_)
        | SignalingMessage::ParticipantKicked(_)
        | SignalingMessage::ParticipantMuted(_)
        | SignalingMessage::ParticipantUnmuted(_)
        | SignalingMessage::SelfDeafen
        | SignalingMessage::SelfUndeafen
        | SignalingMessage::ParticipantDeafened(_)
        | SignalingMessage::ParticipantUndeafened(_)
        | SignalingMessage::StartShare
        | SignalingMessage::ShareStarted(_)
        | SignalingMessage::StopShare(_)
        | SignalingMessage::ShareStopped(_)
        | SignalingMessage::StopAllShares
        | SignalingMessage::ShareState(_)
        | SignalingMessage::SetSharePermission(_)
        | SignalingMessage::SharePermissionChanged(_)
        | SignalingMessage::CreateRoom(_)
        | SignalingMessage::RoomCreated(_)
        | SignalingMessage::Auth(_)
        | SignalingMessage::AuthSuccess(_)
        | SignalingMessage::AuthFailed(_)
        | SignalingMessage::JoinVoice(_)
        | SignalingMessage::CreateSubRoom(_)
        | SignalingMessage::JoinSubRoom(_)
        | SignalingMessage::LeaveSubRoom(_)
        | SignalingMessage::SetPassthrough(_)
        | SignalingMessage::ClearPassthrough(_)
        | SignalingMessage::SubRoomState(_)
        | SignalingMessage::SubRoomCreated(_)
        | SignalingMessage::SubRoomJoined(_)
        | SignalingMessage::SubRoomLeft(_)
        | SignalingMessage::SubRoomDeleted(_)
        | SignalingMessage::SfuColdStarting(_)
        | SignalingMessage::ChatSend(_)
        | SignalingMessage::ChatMessage(_)
        | SignalingMessage::ChatHistoryRequest(_)
        | SignalingMessage::ChatHistoryResponse(_)
        | SignalingMessage::SessionDisplaced(_)
        | SignalingMessage::ViewerSubscribed(_)
        | SignalingMessage::ViewerJoined(_)
        | SignalingMessage::UpdateProfileColor(_)
        | SignalingMessage::ParticipantColorUpdated(_)
        | SignalingMessage::Ping => {
            connections.send_to(
                sender_peer_id,
                &SignalingMessage::Error(ErrorPayload {
                    message: "invalid message type from client".to_string(),
                }),
            );
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::connections::ConnectionManager;
    use crate::voice::relay::{self, RoomState};
    use std::collections::HashMap;
    use std::sync::{Arc, Mutex};

    // --- Test helpers ---

    fn handle_disconnect(
        state: &dyn RoomState,
        peer_id: &str,
        connections: &dyn ConnectionManager,
    ) {
        if let Some((target_peer_id, message)) = relay::handle_disconnect(state, peer_id) {
            connections.send_to(&target_peer_id, &message);
        }
    }

    // --- Mock implementations for testing ---

    #[derive(Debug, Clone)]
    struct MockRoomState {
        peer_to_room: HashMap<String, String>,
        room_to_peers: HashMap<String, Vec<String>>,
    }

    impl MockRoomState {
        fn new() -> Self {
            Self {
                peer_to_room: HashMap::new(),
                room_to_peers: HashMap::new(),
            }
        }

        fn add_peer_to_room(&mut self, peer_id: String, room_id: String) {
            self.peer_to_room.insert(peer_id.clone(), room_id.clone());
            self.room_to_peers.entry(room_id).or_default().push(peer_id);
        }
    }

    impl RoomState for MockRoomState {
        fn get_room_for_peer(&self, peer_id: &str) -> Option<String> {
            self.peer_to_room.get(peer_id).cloned()
        }

        fn get_peers_in_room(&self, room_id: &String) -> Vec<String> {
            self.room_to_peers.get(room_id).cloned().unwrap_or_default()
        }
    }

    #[derive(Debug, Clone)]
    struct MockConnectionManager {
        sent_messages: Arc<Mutex<Vec<(String, SignalingMessage)>>>,
    }

    impl MockConnectionManager {
        fn new() -> Self {
            Self {
                sent_messages: Arc::new(Mutex::new(Vec::new())),
            }
        }

        fn get_sent_messages(&self) -> Vec<(String, SignalingMessage)> {
            self.sent_messages.lock().unwrap().clone()
        }
    }

    impl ConnectionManager for MockConnectionManager {
        fn send_to(&self, peer_id: &str, message: &SignalingMessage) {
            self.sent_messages
                .lock()
                .unwrap()
                .push((peer_id.to_string(), message.clone()));
        }
    }

    // --- Unit tests ---

    #[test]
    fn test_handle_offer_relays_to_peer() {
        let mut state = MockRoomState::new();
        state.add_peer_to_room("peer_a".to_string(), "room_1".to_string());
        state.add_peer_to_room("peer_b".to_string(), "room_1".to_string());

        let connections = MockConnectionManager::new();
        let offer_json = r#"{"type":"offer","sessionDescription":{"sdp":"test","type":"offer"}}"#;

        handle_signaling_message(&state, "peer_a", offer_json, &connections);

        let sent = connections.get_sent_messages();
        assert_eq!(sent.len(), 1);
        assert_eq!(sent[0].0, "peer_b");
        assert!(matches!(sent[0].1, SignalingMessage::Offer(_)));
    }

    #[test]
    fn test_handle_answer_relays_to_peer() {
        let mut state = MockRoomState::new();
        state.add_peer_to_room("peer_a".to_string(), "room_1".to_string());
        state.add_peer_to_room("peer_b".to_string(), "room_1".to_string());

        let connections = MockConnectionManager::new();
        let answer_json =
            r#"{"type":"answer","sessionDescription":{"sdp":"test","type":"answer"}}"#;

        handle_signaling_message(&state, "peer_b", answer_json, &connections);

        let sent = connections.get_sent_messages();
        assert_eq!(sent.len(), 1);
        assert_eq!(sent[0].0, "peer_a");
        assert!(matches!(sent[0].1, SignalingMessage::Answer(_)));
    }

    #[test]
    fn test_handle_ice_candidate_relays_to_peer() {
        let mut state = MockRoomState::new();
        state.add_peer_to_room("peer_a".to_string(), "room_1".to_string());
        state.add_peer_to_room("peer_b".to_string(), "room_1".to_string());

        let connections = MockConnectionManager::new();
        let ice_json = r#"{"type":"ice_candidate","candidate":{"candidate":"test","sdpMid":"0","sdpMLineIndex":0}}"#;

        handle_signaling_message(&state, "peer_a", ice_json, &connections);

        let sent = connections.get_sent_messages();
        assert_eq!(sent.len(), 1);
        assert_eq!(sent[0].0, "peer_b");
        assert!(matches!(sent[0].1, SignalingMessage::IceCandidate(_)));
    }

    #[test]
    fn test_handle_message_when_not_in_room_sends_error() {
        let state = MockRoomState::new();
        let connections = MockConnectionManager::new();
        let offer_json = r#"{"type":"offer","sessionDescription":{"sdp":"test","type":"offer"}}"#;

        handle_signaling_message(&state, "peer_a", offer_json, &connections);

        let sent = connections.get_sent_messages();
        assert_eq!(sent.len(), 1);
        assert_eq!(sent[0].0, "peer_a");
        match &sent[0].1 {
            SignalingMessage::Error(payload) => assert_eq!(payload.message, "not in a room"),
            _ => panic!("Expected Error message"),
        }
    }

    #[test]
    fn test_handle_message_when_alone_sends_error() {
        let mut state = MockRoomState::new();
        state.add_peer_to_room("peer_a".to_string(), "room_1".to_string());

        let connections = MockConnectionManager::new();
        let offer_json = r#"{"type":"offer","sessionDescription":{"sdp":"test","type":"offer"}}"#;

        handle_signaling_message(&state, "peer_a", offer_json, &connections);

        let sent = connections.get_sent_messages();
        assert_eq!(sent.len(), 1);
        assert_eq!(sent[0].0, "peer_a");
        match &sent[0].1 {
            SignalingMessage::Error(payload) => assert_eq!(payload.message, "no peer available"),
            _ => panic!("Expected Error message"),
        }
    }

    #[test]
    fn test_handle_malformed_json_sends_error() {
        let state = MockRoomState::new();
        let connections = MockConnectionManager::new();

        handle_signaling_message(&state, "peer_a", "not valid json", &connections);

        let sent = connections.get_sent_messages();
        assert_eq!(sent.len(), 1);
        assert_eq!(sent[0].0, "peer_a");
        match &sent[0].1 {
            SignalingMessage::Error(payload) => assert_eq!(payload.message, "invalid JSON"),
            _ => panic!("Expected Error message"),
        }
    }

    #[test]
    fn test_handle_peer_left_from_client_sends_error() {
        let mut state = MockRoomState::new();
        state.add_peer_to_room("peer_a".to_string(), "room_1".to_string());
        state.add_peer_to_room("peer_b".to_string(), "room_1".to_string());

        let connections = MockConnectionManager::new();
        let peer_left_json = r#"{"type":"peer_left"}"#;

        handle_signaling_message(&state, "peer_a", peer_left_json, &connections);

        let sent = connections.get_sent_messages();
        assert_eq!(sent.len(), 1);
        assert_eq!(sent[0].0, "peer_a");
        match &sent[0].1 {
            SignalingMessage::Error(payload) => {
                assert_eq!(payload.message, "invalid message type from client")
            }
            _ => panic!("Expected Error message"),
        }
    }

    // --- Task 6.2: Relay path rejects action message types (Requirement 3.7) ---

    #[test]
    fn test_kick_participant_rejected_on_relay_path() {
        let mut state = MockRoomState::new();
        state.add_peer_to_room("peer_a".to_string(), "room_1".to_string());
        state.add_peer_to_room("peer_b".to_string(), "room_1".to_string());

        let connections = MockConnectionManager::new();
        let msg = SignalingMessage::KickParticipant(shared::signaling::KickParticipantPayload {
            target_participant_id: "peer_b".to_string(),
        });
        handle_signaling_event(&state, "peer_a", msg, &connections);

        let sent = connections.get_sent_messages();
        assert_eq!(sent.len(), 1);
        assert_eq!(
            sent[0].0, "peer_a",
            "Error must be sent back to sender, not relayed"
        );
        match &sent[0].1 {
            SignalingMessage::Error(payload) => {
                assert_eq!(payload.message, "invalid message type from client");
            }
            _ => panic!("Expected Error message, got {:?}", sent[0].1),
        }
    }

    #[test]
    fn test_mute_participant_rejected_on_relay_path() {
        let mut state = MockRoomState::new();
        state.add_peer_to_room("peer_a".to_string(), "room_1".to_string());
        state.add_peer_to_room("peer_b".to_string(), "room_1".to_string());

        let connections = MockConnectionManager::new();
        let msg = SignalingMessage::MuteParticipant(shared::signaling::MuteParticipantPayload {
            target_participant_id: "peer_b".to_string(),
        });
        handle_signaling_event(&state, "peer_a", msg, &connections);

        let sent = connections.get_sent_messages();
        assert_eq!(sent.len(), 1);
        assert_eq!(
            sent[0].0, "peer_a",
            "Error must be sent back to sender, not relayed"
        );
        match &sent[0].1 {
            SignalingMessage::Error(payload) => {
                assert_eq!(payload.message, "invalid message type from client");
            }
            _ => panic!("Expected Error message, got {:?}", sent[0].1),
        }
    }

    #[test]
    fn test_handle_disconnect_notifies_remaining_peer() {
        let mut state = MockRoomState::new();
        state.add_peer_to_room("peer_a".to_string(), "room_1".to_string());
        state.add_peer_to_room("peer_b".to_string(), "room_1".to_string());

        let connections = MockConnectionManager::new();
        handle_disconnect(&state, "peer_a", &connections);

        let sent = connections.get_sent_messages();
        assert_eq!(sent.len(), 1);
        assert_eq!(sent[0].0, "peer_b");
        assert_eq!(sent[0].1, SignalingMessage::PeerLeft);
    }

    #[test]
    fn test_handle_disconnect_when_not_in_room_does_nothing() {
        let state = MockRoomState::new();
        let connections = MockConnectionManager::new();
        handle_disconnect(&state, "peer_a", &connections);
        assert_eq!(connections.get_sent_messages().len(), 0);
    }

    #[test]
    fn test_handle_disconnect_when_alone_does_nothing() {
        let mut state = MockRoomState::new();
        state.add_peer_to_room("peer_a".to_string(), "room_1".to_string());

        let connections = MockConnectionManager::new();
        handle_disconnect(&state, "peer_a", &connections);
        assert_eq!(connections.get_sent_messages().len(), 0);
    }

    #[test]
    fn test_session_identity_used_for_relay() {
        let mut state = MockRoomState::new();
        state.add_peer_to_room("session-peer-123".to_string(), "room-1".to_string());
        state.add_peer_to_room("peer-b".to_string(), "room-1".to_string());

        let connections = MockConnectionManager::new();

        let offer_json = r#"{"type":"offer","sessionDescription":{"sdp":"test","type":"offer"}}"#;
        handle_signaling_message(&state, "session-peer-123", offer_json, &connections);

        let sent = connections.get_sent_messages();
        assert_eq!(sent.len(), 1);
        assert_eq!(
            sent[0].0, "peer-b",
            "Message should be relayed to the other peer"
        );
        assert!(matches!(sent[0].1, SignalingMessage::Offer(_)));
    }

    // --- Task 9.5: Handler integration unit tests ---

    #[test]
    fn test_mute_in_p2p_room_rejected() {
        let mut state = MockRoomState::new();
        state.add_peer_to_room("peer_a".to_string(), "room_1".to_string());
        state.add_peer_to_room("peer_b".to_string(), "room_1".to_string());

        let connections = MockConnectionManager::new();
        let msg = SignalingMessage::MuteParticipant(shared::signaling::MuteParticipantPayload {
            target_participant_id: "peer_b".to_string(),
        });
        handle_signaling_event(&state, "peer_a", msg, &connections);

        let sent = connections.get_sent_messages();
        assert_eq!(sent.len(), 1);
        assert_eq!(sent[0].0, "peer_a");
        match &sent[0].1 {
            SignalingMessage::Error(payload) => {
                assert_eq!(payload.message, "invalid message type from client");
            }
            _ => panic!("Expected Error message"),
        }
    }

    // --- Property tests ---

    use proptest::prelude::*;

    // Feature: token-and-signaling-auth, Property 7: Negotiation message size validation
    // **Validates: Requirements 3.5, 3.8**

    proptest! {
        #![proptest_config(ProptestConfig::with_cases(100))]

        #[test]
        fn prop_p7_sdp_size_validation(
            sdp_len in 0usize..=(128 * 1024),
        ) {
            let within_limit = sdp_len <= MAX_SDP_BYTES;
            let sdp = "a".repeat(sdp_len);
            prop_assert_eq!(sdp.len() <= MAX_SDP_BYTES, within_limit);
            let would_reject = sdp.len() > MAX_SDP_BYTES;
            prop_assert_eq!(would_reject, !within_limit);
        }

        #[test]
        fn prop_p7_ice_candidate_size_validation(
            candidate_len in 0usize..=(4 * 1024),
        ) {
            let within_limit = candidate_len <= MAX_ICE_CANDIDATE_BYTES;
            let candidate = "a".repeat(candidate_len);
            prop_assert_eq!(candidate.len() <= MAX_ICE_CANDIDATE_BYTES, within_limit);
            let would_reject = candidate.len() > MAX_ICE_CANDIDATE_BYTES;
            prop_assert_eq!(would_reject, !within_limit);
        }
    }

    #[test]
    fn test_sdp_size_boundary() {
        let at_limit = "a".repeat(MAX_SDP_BYTES);
        assert!(
            at_limit.len() <= MAX_SDP_BYTES,
            "SDP at exactly MAX_SDP_BYTES must be allowed"
        );

        let over_limit = "a".repeat(MAX_SDP_BYTES + 1);
        assert!(
            over_limit.len() > MAX_SDP_BYTES,
            "SDP one byte over MAX_SDP_BYTES must be rejected"
        );
    }

    #[test]
    fn test_ice_candidate_size_boundary() {
        let at_limit = "a".repeat(MAX_ICE_CANDIDATE_BYTES);
        assert!(
            at_limit.len() <= MAX_ICE_CANDIDATE_BYTES,
            "ICE candidate at exactly MAX_ICE_CANDIDATE_BYTES must be allowed"
        );

        let over_limit = "a".repeat(MAX_ICE_CANDIDATE_BYTES + 1);
        assert!(
            over_limit.len() > MAX_ICE_CANDIDATE_BYTES,
            "ICE candidate one byte over MAX_ICE_CANDIDATE_BYTES must be rejected"
        );
    }

    // Feature: token-and-signaling-auth, Property 8: Relay type allowlist
    // **Validates: Requirements 3.7**

    proptest! {
        #![proptest_config(ProptestConfig::with_cases(100))]

        #[test]
        fn prop_p8_relay_type_allowlist(msg in any::<SignalingMessage>()) {
            let mut state = MockRoomState::new();
            state.add_peer_to_room("peer_a".to_string(), "room_1".to_string());
            state.add_peer_to_room("peer_b".to_string(), "room_1".to_string());

            let connections = MockConnectionManager::new();
            let is_negotiation = matches!(
                &msg,
                SignalingMessage::Offer(_) | SignalingMessage::Answer(_) | SignalingMessage::IceCandidate(_)
            );

            handle_signaling_event(&state, "peer_a", msg.clone(), &connections);

            let sent = connections.get_sent_messages();

            if is_negotiation {
                prop_assert_eq!(sent.len(), 1, "Negotiation message should produce exactly one outbound message");
                prop_assert_eq!(&sent[0].0, "peer_b", "Negotiation message must be relayed to peer_b");
                prop_assert!(
                    !matches!(sent[0].1, SignalingMessage::Error(_)),
                    "Relayed negotiation message must not be an error"
                );
            } else {
                let peer_b_got_raw_relay = sent.iter().any(|(id, received)| {
                    id == "peer_b" && received == &msg
                });
                prop_assert!(
                    !peer_b_got_raw_relay,
                    "Non-negotiation message must not be relayed verbatim to peer_b"
                );
            }
        }
    }
}
