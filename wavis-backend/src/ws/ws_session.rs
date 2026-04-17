//! Per-connection signaling session and lifecycle management.
//!
//! **Owns:** the authenticated session context created after a successful room
//! join, and the lifecycle operations for that session: graceful leave,
//! ungraceful disconnect, connection cleanup, and room artifact teardown.
//!
//! **Does not own:** WebSocket transport, message parsing, or dispatch. Those
//! remain in [`super::ws`]. Business logic for signaling actions lives in
//! domain modules; this module only coordinates the teardown sequence.
//!
//! **Key invariants:**
//! - All leave/cleanup paths are idempotent (§4.3). Network disconnect races,
//!   duplicate notifications, and partial-setup failures can all trigger
//!   cleanup. Boolean guards ensure each step runs at most once.
//! - Lifecycle teardown performs direct state mutations (`remove_peer`,
//!   `unregister`, `add_revoked_participant`) rather than routing through
//!   domain functions. These are low-level resource release operations tied
//!   to connection lifecycle, not business policy decisions, so the direct
//!   calls are a deliberate pragmatic choice.

use crate::app_state::AppState;
use crate::connections::ConnectionManager;
use crate::state::RoomType;
use crate::voice::relay;
use crate::voice::screen_share::cleanup_share_on_disconnect;
use crate::voice::sfu_relay::{ParticipantRole, handle_sfu_leave};
use crate::voice::voice_orchestrator;
use crate::ws::ws_dispatch::{dispatch_signals, schedule_sub_room_expiry};
use axum::extract::ws::{Message, WebSocket};
use std::sync::atomic::Ordering;
use tracing::warn;
use uuid::Uuid;

/// Per-connection authenticated context created after a successful room join.
pub struct SignalingSession {
    pub participant_id: String,
    pub room_id: String,
    pub role: ParticipantRole,
    pub user_id: Option<String>,
    pub channel_id: Option<String>,
    leave_handled: bool,
    cleanup_complete: bool,
}

impl SignalingSession {
    pub fn new(
        participant_id: String,
        room_id: String,
        role: ParticipantRole,
        user_id: Option<String>,
        channel_id: Option<String>,
    ) -> Self {
        Self {
            participant_id,
            room_id,
            role,
            user_id,
            channel_id,
            leave_handled: false,
            cleanup_complete: false,
        }
    }

    /// Handle an explicit leave message from the client.
    ///
    /// Graceful leave does not revoke the participant or clean up shares — the
    /// client is expected to have stopped sharing before sending `Leave`.
    pub async fn handle_leave(&mut self, app_state: &AppState) {
        // Idempotency guarantee (§4.3): an explicit Leave followed by a socket close
        // will both try to run leave logic. The guard ensures the room-level leave
        // executes at most once; later calls become no-ops.
        if self.leave_handled {
            return;
        }

        self.leave_room(app_state, false).await;
    }

    /// Full connection cleanup: leave the room (if not already done), unregister
    /// the peer, and tear down any orphaned room artifacts.
    pub async fn cleanup_connection(&mut self, app_state: &AppState, peer_id: &str) {
        // Idempotency guarantee (§4.3): explicit leave, socket close, outbound channel drop,
        // and task teardown can all converge on this cleanup path. The guard makes repeat
        // invocations safe by turning later calls into no-ops after the first full cleanup.
        if self.cleanup_complete {
            return;
        }
        self.cleanup_complete = true;

        if !self.leave_handled {
            self.leave_room(app_state, true).await;
        }

        app_state.connections.unregister(peer_id);
        app_state.room_state.remove_peer(peer_id);
        self.cleanup_room_artifacts(app_state).await;
    }

    /// Unified room-leave logic for both graceful and ungraceful paths.
    ///
    /// When `is_disconnect` is true (ungraceful), additional cleanup runs:
    /// - SFU rooms: revoke the participant and clean up any active shares
    /// - P2P rooms: skip `remove_peer` (caller — `cleanup_connection` — handles it)
    async fn leave_room(&mut self, app_state: &AppState, is_disconnect: bool) {
        match app_state
            .room_state
            .get_room_info(&self.room_id)
            .map(|info| info.room_type)
        {
            Some(RoomType::Sfu) => {
                if is_disconnect {
                    app_state
                        .room_state
                        .add_revoked_participant(&self.room_id, &self.participant_id);
                    if let Some(share_signals) = cleanup_share_on_disconnect(
                        app_state.room_state.as_ref(),
                        &self.room_id,
                        &self.participant_id,
                    ) {
                        dispatch_signals(
                            share_signals,
                            &self.room_id,
                            app_state.room_state.as_ref(),
                            app_state.connections.as_ref(),
                        );
                    }
                }
                match handle_sfu_leave(
                    app_state.sfu_room_manager.as_ref(),
                    app_state.room_state.as_ref(),
                    &self.room_id,
                    &self.participant_id,
                )
                .await
                {
                    Ok(mut signals) => {
                        let sub_room_result = voice_orchestrator::remove_participant_from_sub_room(
                            app_state.room_state.as_ref(),
                            &self.room_id,
                            &self.participant_id,
                        );
                        signals.extend(sub_room_result.signals);
                        dispatch_signals(
                            signals,
                            &self.room_id,
                            app_state.room_state.as_ref(),
                            app_state.connections.as_ref(),
                        );
                        if let Some(expiry) = sub_room_result.expiry {
                            schedule_sub_room_expiry(
                                app_state,
                                &self.room_id,
                                &expiry.sub_room_id,
                                expiry.delete_at,
                            );
                        }
                    }
                    Err(err) => {
                        warn!(peer_id = %self.participant_id, "SFU leave error: {err}");
                    }
                }
            }
            _ => {
                if let Some((target_peer_id, message)) =
                    relay::handle_disconnect(app_state.room_state.as_ref(), &self.participant_id)
                {
                    app_state.connections.send_to(&target_peer_id, &message);
                }
                // Explicit leave owns peer removal; ungraceful disconnect delegates
                // to cleanup_connection which calls remove_peer after this method.
                if !is_disconnect {
                    app_state.room_state.remove_peer(&self.participant_id);
                }
            }
        }

        self.leave_handled = true;
    }

    async fn cleanup_room_artifacts(&self, app_state: &AppState) {
        if app_state.room_state.get_room_info(&self.room_id).is_some() {
            return;
        }

        app_state.invite_store.remove_room_invites(&self.room_id);

        if let Some(channel_id) = self.channel_id.as_ref()
            && let Ok(channel_uuid) = Uuid::parse_str(channel_id)
        {
            let mut map = app_state.active_room_map.write().await;
            if map.get(&channel_uuid).map(|room_id| room_id.as_str()) == Some(self.room_id.as_str())
            {
                map.remove(&channel_uuid);
            }
        }

        if app_state.pending_shutdown.load(Ordering::Acquire)
            && app_state.room_state.active_room_count() == 0
        {
            app_state.pending_shutdown.store(false, Ordering::Release);
            let app_state_clone = (*app_state).clone();
            tokio::spawn(async move {
                crate::ec2_control::trigger_ec2_stop(&app_state_clone).await;
            });
        }
    }
}

pub async fn cleanup_unjoined_connection(app_state: &AppState, peer_id: &str) {
    app_state.connections.unregister(peer_id);
    app_state.room_state.remove_peer(peer_id);
}

pub async fn close_socket(socket: &mut WebSocket) {
    let _ = socket.send(Message::Close(None)).await;
}
