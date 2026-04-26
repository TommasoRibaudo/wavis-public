//! Canonical signaling wire contract shared by clients and the backend.
//!
//! This module owns the `SignalingMessage` envelope, every signaling payload
//! struct, the `parse`/`to_json` helpers, and the proptest support used to
//! exercise protocol evolution. It does not own WebSocket transport
//! (`wavis-backend/src/handlers/ws.rs`), signaling validation and business
//! rules (`validation.rs`), or session orchestration (`call_session.rs`).
//!
//! Invariants:
//! - `SignalingMessage` is the single source of truth for the signaling
//!   protocol.
//! - Every added or changed variant must be reviewed across this module,
//!   `validation.rs`, `proptest_support.rs`, backend WebSocket handling, and
//!   call-session orchestration; missing one consumer is a correctness bug.
//!
//! Wire stability:
//! - JSON field names and value types are wire-stable once shipped.
//! - Adding new variants or optional fields is additive-safe.
//! - Renaming fields, changing field types, or removing variants is a breaking
//!   change that requires a coordinated client and server deploy.
//! - If Rust item names need to change without breaking the wire contract, keep
//!   the existing JSON shape with `#[serde(rename = "...")]`.
//! - Phase 3 signaling variants are part of the canonical wire schema and are
//!   not compile-time gated out of serialization.
use serde::{Deserialize, Serialize};

/// Machine-parseable reason for join rejection.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum JoinRejectionReason {
    /// The invite existed but is no longer valid because its lifetime ended.
    InviteExpired,
    /// The invite was explicitly revoked by a host or moderator.
    InviteRevoked,
    /// The supplied invite code does not match any active invite.
    InviteInvalid,
    /// The room requires an invite and none was supplied.
    InviteRequired,
    /// The invite reached its maximum allowed uses.
    InviteExhausted,
    /// The room cannot accept another participant right now.
    RoomFull,
    /// The caller hit a join rate limit and must retry later.
    RateLimited,
    /// The caller is authenticated but lacks permission to join.
    NotAuthorized,
}

/// Structured join rejection payload.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct JoinRejectedPayload {
    /// Machine-readable rejection category for client UI and retry behavior.
    pub reason: JoinRejectionReason,
}

/// Server -> client: the SFU EC2 instance is cold-starting after an idle shutdown.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct SfuColdStartingPayload {
    /// Upper-bound estimate in seconds until the SFU is ready.
    #[serde(rename = "estimatedWaitSecs")]
    pub estimated_wait_secs: u32,
}

/// Wire-format share permission value.
///
/// Serializes as `"anyone"` or `"host_only"` on the wire. Used by
/// `JoinedPayload`, `SetSharePermissionPayload`, and
/// `SharePermissionChangedPayload` to avoid stringly-typed permission fields.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
pub enum WireSharePermission {
    #[serde(rename = "anyone")]
    Anyone,
    #[serde(rename = "host_only")]
    HostOnly,
}

impl WireSharePermission {
    /// Parse from a raw string (e.g. from legacy code paths).
    /// Returns `None` for unrecognized values.
    pub fn from_str_opt(s: &str) -> Option<Self> {
        match s {
            "anyone" => Some(Self::Anyone),
            "host_only" => Some(Self::HostOnly),
            _ => None,
        }
    }

    /// Wire-format string representation.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Anyone => "anyone",
            Self::HostOnly => "host_only",
        }
    }
}

/// Top-level signaling message envelope.
/// Uses serde `tag = "type"` for JSON serialization so the wire format
/// has a top-level `"type"` field (e.g. `{"type": "offer", ...}`).
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum SignalingMessage {
    /// Client -> server request to join a room or session.
    Join(JoinPayload),
    /// Server -> client confirmation that the join succeeded.
    Joined(JoinedPayload),
    /// Client -> server (P2P relay) WebRTC SDP offer message.
    Offer(OfferPayload),
    /// Client -> server (P2P relay) WebRTC SDP answer message.
    Answer(AnswerPayload),
    /// Client -> server (P2P relay) ICE candidate relay message.
    IceCandidate(IceCandidatePayload),
    /// Server -> client notification that a peer left in legacy P2P flows.
    PeerLeft,
    /// Client -> server request to leave the active room.
    Leave,
    /// Server -> client opaque signaling error.
    Error(ErrorPayload),
    /// Server -> client join failure with a structured rejection reason.
    JoinRejected(JoinRejectedPayload),
    // Invite lifecycle
    /// Client -> server request to mint a new invite for the current room.
    InviteCreate(InviteCreatePayload),
    /// Server -> client response carrying the newly created invite.
    InviteCreated(InviteCreatedPayload),
    /// Client -> server request to revoke an existing invite.
    InviteRevoke(InviteRevokePayload),
    /// Server -> client confirmation that an invite was revoked.
    InviteRevoked(InviteRevokedPayload),
    // Phase 3: SFU multi-party variants
    /// Server -> all participants notification that a participant joined the room.
    ParticipantJoined(ParticipantJoinedPayload),
    /// Server -> all participants notification that a participant left the room.
    ParticipantLeft(ParticipantLeftPayload),
    /// Server -> late joiner room snapshot for participant resync.
    RoomState(RoomStatePayload),
    /// Server -> client SFU token and endpoint for media-plane access.
    MediaToken(MediaTokenPayload),
    // Action messages (client → server)
    /// Client -> server moderation request to remove a participant.
    KickParticipant(KickParticipantPayload),
    /// Client -> server moderation request to host-mute a participant.
    MuteParticipant(MuteParticipantPayload),
    /// Client -> server moderation request to release a host mute.
    UnmuteParticipant(UnmuteParticipantPayload),
    // Self-deafen (client → server, any participant)
    /// Client -> server notification that the sender deafened themselves.
    SelfDeafen,
    /// Client -> server notification that the sender undeafened themselves.
    SelfUndeafen,
    // Action events (server → client)
    /// Server -> all participants broadcast that a participant was kicked.
    ParticipantKicked(ParticipantKickedPayload),
    /// Server -> all participants broadcast that a participant was muted.
    ParticipantMuted(ParticipantMutedPayload),
    /// Server -> all participants broadcast that a participant was unmuted.
    ParticipantUnmuted(ParticipantUnmutedPayload),
    /// Server -> all participants broadcast that a participant deafened themselves.
    ParticipantDeafened(ParticipantDeafenedPayload),
    /// Server -> all participants broadcast that a participant undeafened themselves.
    ParticipantUndeafened(ParticipantUndeafenedPayload),
    // Phase 3: Screen share lifecycle
    /// Client -> server request to begin screen sharing.
    StartShare,
    /// Server -> all participants broadcast that a share started.
    ShareStarted(ShareStartedPayload),
    /// Client -> server request to stop a share.
    StopShare(StopSharePayload),
    /// Server -> all participants broadcast that a share stopped.
    ShareStopped(ShareStoppedPayload),
    /// Client -> server (host) action to stop every active share in the room.
    StopAllShares,
    /// Server -> late joiner snapshot of all currently active shares.
    ShareState(ShareStatePayload),
    // Share permission change (host → all clients)
    /// Client -> server (host) request to change who may start sharing.
    SetSharePermission(SetSharePermissionPayload),
    /// Server -> all participants broadcast of the current share permission mode.
    SharePermissionChanged(SharePermissionChangedPayload),
    // Viewer subscription notification (client → server → sharer)
    /// Client -> server notice that the sender subscribed to a share.
    ViewerSubscribed(ViewerSubscribedPayload),
    /// Server -> sharer-only notice that a viewer joined.
    ViewerJoined(ViewerJoinedPayload),
    // Room creation (no invite required for creator)
    /// Client -> server request to create a room as its initial host.
    CreateRoom(CreateRoomPayload),
    /// Server -> client confirmation that room creation succeeded.
    RoomCreated(RoomCreatedPayload),
    // Device Auth
    /// Client -> server bearer-token authentication for the WebSocket.
    Auth(AuthPayload),
    /// Server -> client confirmation that authentication succeeded.
    AuthSuccess(AuthSuccessPayload),
    /// Server -> client opaque authentication failure.
    AuthFailed(AuthFailedPayload),
    // Channel voice orchestration
    /// Client -> server request to join the active voice room for a channel.
    JoinVoice(JoinVoicePayload),
    /// Client -> server request to create a sub-room inside the active channel voice session.
    CreateSubRoom(CreateSubRoomPayload),
    /// Client -> server request to join a sub-room inside the active channel voice session.
    JoinSubRoom(JoinSubRoomPayload),
    /// Client -> server request to leave the current sub-room while staying in voice.
    LeaveSubRoom(LeaveSubRoomPayload),
    /// Client -> server request to enable or replace passthrough using the caller's joined room as the source.
    SetPassthrough(SetPassthroughPayload),
    /// Client -> server request to clear the active passthrough pair.
    ClearPassthrough(ClearPassthroughPayload),
    /// Server -> client snapshot of the synchronized sub-room layout.
    SubRoomState(SubRoomStatePayload),
    /// Server -> client broadcast that a new sub-room was created.
    SubRoomCreated(SubRoomCreatedPayload),
    /// Server -> client broadcast that a participant joined or switched to a sub-room.
    SubRoomJoined(SubRoomJoinedPayload),
    /// Server -> client broadcast that a participant left a sub-room.
    SubRoomLeft(SubRoomLeftPayload),
    /// Server -> client broadcast that a sub-room was deleted.
    SubRoomDeleted(SubRoomDeletedPayload),
    /// Server -> client: the SFU is booting up after a cold stop. Client should
    /// show a wait UI and retry `join_voice` after the estimated wait period.
    SfuColdStarting(SfuColdStartingPayload),
    // Ephemeral room chat
    /// Client -> server request to send a room chat message.
    ChatSend(ChatSendPayload),
    /// Server -> all participants broadcast carrying a chat message.
    ChatMessage(ChatMessagePayload),
    // Chat history (client → server request, server → client response)
    /// Client -> server request for historical chat messages.
    ChatHistoryRequest(ChatHistoryRequestPayload),
    /// Server -> client response containing historical chat messages.
    ChatHistoryResponse(ChatHistoryResponsePayload),
    // Keepalive (client → server, server ignores)
    /// Client -> server keepalive used to keep idle connections warm.
    Ping,
    // Session displacement (server → evicted client)
    /// Server -> evicted client only notice that a newer session displaced this one.
    SessionDisplaced(SessionDisplacedPayload),
    // Profile color update (client → server → all participants)
    /// Client -> server notification that the sender changed their profile color.
    UpdateProfileColor(UpdateProfileColorPayload),
    /// Server -> all participants broadcast that a participant changed their profile color.
    ParticipantColorUpdated(ParticipantColorUpdatedPayload),
}

/// Client requests room creation. No invite code needed — creator becomes Host.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct CreateRoomPayload {
    /// Stable room identifier the creator wants to own.
    #[serde(rename = "roomId")]
    pub room_id: String,
    /// Optional room type hint from client. If absent, backend decides.
    #[serde(rename = "roomType", default)]
    pub room_type: Option<String>,
    /// Optional display name chosen by the room creator.
    #[serde(rename = "displayName", default)]
    pub display_name: Option<String>,
    /// User-chosen profile colour (hex string). Omitted when not set.
    #[serde(rename = "profileColor", default)]
    pub profile_color: Option<String>,
}

/// Server confirms room creation. Creator is joined as Host with an initial invite code.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct RoomCreatedPayload {
    /// Room identifier that was created.
    #[serde(rename = "roomId")]
    pub room_id: String,
    /// Participant identifier assigned to the creator.
    #[serde(rename = "peerId")]
    pub peer_id: String,
    /// Initial invite code generated for the new room.
    #[serde(rename = "inviteCode")]
    pub invite_code: String,
    /// Seconds until the initial invite expires.
    #[serde(rename = "expiresInSecs")]
    pub expires_in_secs: u64,
    /// Maximum number of times the initial invite may be consumed.
    #[serde(rename = "maxUses")]
    pub max_uses: u32,
    /// ICE configuration with dynamic TURN credentials (Phase 3+).
    #[serde(rename = "iceConfig", skip_serializing_if = "Option::is_none")]
    pub ice_config: Option<IceConfigPayload>,
}

/// Client requests a new invite code for the room they're in.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct InviteCreatePayload {
    /// Optional usage cap for the new invite; the backend applies defaults when absent.
    #[serde(rename = "maxUses", default)]
    pub max_uses: Option<u32>,
}

/// Server responds with the generated invite code.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct InviteCreatedPayload {
    /// Invite code clients should distribute to other participants.
    #[serde(rename = "inviteCode")]
    pub invite_code: String,
    /// Seconds until the invite expires.
    #[serde(rename = "expiresInSecs")]
    pub expires_in_secs: u64,
    /// Maximum number of times the invite may be consumed.
    #[serde(rename = "maxUses")]
    pub max_uses: u32,
}

/// Client requests revocation of a specific invite code.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct InviteRevokePayload {
    /// Invite code the client wants revoked.
    #[serde(rename = "inviteCode")]
    pub invite_code: String,
}

/// Server confirms revocation.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct InviteRevokedPayload {
    /// Invite code that was revoked.
    #[serde(rename = "inviteCode")]
    pub invite_code: String,
}

/// Client -> server request to join a room using a room id and optional invite.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct JoinPayload {
    /// Stable room identifier the client wants to join.
    #[serde(rename = "roomId")]
    pub room_id: String,
    /// Optional room type hint from client. If absent, backend decides.
    #[serde(rename = "roomType", default)]
    pub room_type: Option<String>,
    /// Invite code required to join the room.
    #[serde(rename = "inviteCode", default)]
    pub invite_code: Option<String>,
    /// Optional display name chosen by the user. Falls back to peer_id on the server.
    #[serde(rename = "displayName", default)]
    pub display_name: Option<String>,
    /// User-chosen profile colour (hex string). Omitted when not set.
    #[serde(rename = "profileColor", default)]
    pub profile_color: Option<String>,
}

/// ICE configuration with dynamic TURN credentials (Phase 3+).
/// Sent to the client as part of the `Joined` response when TURN is configured.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct IceConfigPayload {
    /// STUN server URLs the client should use for ICE gathering.
    #[serde(rename = "stunUrls")]
    pub stun_urls: Vec<String>,
    /// TURN server URLs the client should use for relay candidates.
    #[serde(rename = "turnUrls")]
    pub turn_urls: Vec<String>,
    /// Username paired with the TURN credential.
    #[serde(rename = "turnUsername")]
    pub turn_username: String,
    /// Password or credential paired with the TURN username.
    #[serde(rename = "turnCredential")]
    pub turn_credential: String,
}

/// Server -> client confirmation payload for a successful room join.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct JoinedPayload {
    /// Room identifier that the client joined.
    #[serde(rename = "roomId")]
    pub room_id: String,
    /// Participant identifier assigned to the caller for this session.
    #[serde(rename = "peerId")]
    pub peer_id: String,
    /// Participant count observed when the join completed.
    #[serde(rename = "peerCount")]
    pub peer_count: u32,
    /// Full participant list at time of join. Empty for Phase 2 P2P rooms.
    #[serde(default)]
    pub participants: Vec<ParticipantInfo>,
    /// ICE configuration with dynamic TURN credentials (Phase 3+).
    #[serde(rename = "iceConfig", skip_serializing_if = "Option::is_none")]
    pub ice_config: Option<IceConfigPayload>,
    /// Room-wide screen share permission at time of join.
    /// Absent for P2P rooms or legacy backends; clients default to "anyone".
    #[serde(rename = "sharePermission", skip_serializing_if = "Option::is_none")]
    pub share_permission: Option<WireSharePermission>,
}

/// WebRTC session description transported on the signaling channel.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct SessionDescription {
    /// Raw SDP blob that must remain wire-compatible with the WebRTC stack.
    pub sdp: String,
    /// "offer" or "answer"
    #[serde(rename = "type")]
    pub sdp_type: String,
}

/// SDP offer payload carried by [`SignalingMessage::Offer`].
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct OfferPayload {
    /// Offer session description sent to the remote peer or server.
    #[serde(rename = "sessionDescription")]
    pub session_description: SessionDescription,
}

/// SDP answer payload carried by [`SignalingMessage::Answer`].
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct AnswerPayload {
    /// Answer session description sent to the remote peer or server.
    #[serde(rename = "sessionDescription")]
    pub session_description: SessionDescription,
}

/// ICE candidate wrapper used inside `SignalingMessage::IceCandidate`.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct IceCandidatePayload {
    /// ICE candidate that should be forwarded to the remote peer or server.
    pub candidate: IceCandidate,
}

/// Individual ICE candidate details emitted by the WebRTC stack.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct IceCandidate {
    /// Candidate line exactly as produced by the WebRTC implementation.
    pub candidate: String,
    #[serde(rename = "sdpMid")]
    /// Media section identifier the candidate belongs to.
    pub sdp_mid: String,
    #[serde(rename = "sdpMLineIndex")]
    /// Zero-based media line index the candidate belongs to.
    pub sdp_mline_index: u16,
}

/// Opaque signaling error payload returned by the server.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct ErrorPayload {
    /// Human-readable error message suitable for logs or generic UI.
    pub message: String,
}

// --- Phase 3: SFU multi-party payload structs ---

/// Describes a participant in a multi-party room.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct ParticipantInfo {
    /// Stable participant identifier used throughout room events.
    #[serde(rename = "participantId")]
    pub participant_id: String,
    /// Display name currently shown for the participant.
    #[serde(rename = "displayName")]
    pub display_name: String,
    /// The device-auth user_id for this participant (populated for channel-based voice joins).
    /// Not sent on the wire unless present — legacy room joins leave this as None.
    #[serde(rename = "userId", default, skip_serializing_if = "Option::is_none")]
    pub user_id: Option<String>,
    /// User-chosen profile colour (hex string, e.g. "#E06C75"). Omitted when not set.
    #[serde(
        rename = "profileColor",
        default,
        skip_serializing_if = "Option::is_none"
    )]
    pub profile_color: Option<String>,
}

/// Broadcast to existing participants when a new participant joins.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct ParticipantJoinedPayload {
    /// Participant identifier for the new joiner.
    #[serde(rename = "participantId")]
    pub participant_id: String,
    /// Display name for the new joiner.
    #[serde(rename = "displayName")]
    pub display_name: String,
    /// Stable user identifier. Optional: legacy SFU rooms don't have one.
    #[serde(rename = "userId", default, skip_serializing_if = "Option::is_none")]
    pub user_id: Option<String>,
    /// User-chosen profile colour (hex string). Omitted when not set.
    #[serde(
        rename = "profileColor",
        default,
        skip_serializing_if = "Option::is_none"
    )]
    pub profile_color: Option<String>,
}

/// Broadcast to remaining participants when a participant leaves.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct ParticipantLeftPayload {
    /// Participant identifier for the participant who left.
    #[serde(rename = "participantId")]
    pub participant_id: String,
}

/// Sent to late joiners with the current participant list.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct RoomStatePayload {
    /// Complete participant list at the moment the snapshot was emitted.
    #[serde(default)]
    pub participants: Vec<ParticipantInfo>,
}

/// Sent to a joining participant with their SFU media access token.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct MediaTokenPayload {
    /// Short-lived media token used to authenticate with the SFU.
    pub token: String,
    /// SFU base URL the client should connect to for media transport.
    #[serde(rename = "sfuUrl")]
    pub sfu_url: String,
}

// --- Action message payloads (client → server) ---

/// Client requests to kick a participant from the room.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct KickParticipantPayload {
    /// Participant identifier that the caller wants removed.
    #[serde(rename = "targetParticipantId")]
    pub target_participant_id: String,
}

/// Client requests to mute a participant in the room.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct MuteParticipantPayload {
    /// Participant identifier that the caller wants host-muted.
    #[serde(rename = "targetParticipantId")]
    pub target_participant_id: String,
}

/// Client requests to unmute (release host-mute on) a participant in the room.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct UnmuteParticipantPayload {
    /// Participant identifier that the caller wants unmuted.
    #[serde(rename = "targetParticipantId")]
    pub target_participant_id: String,
}

// --- Action event payloads (server → client) ---

/// Broadcast to all participants when a participant is kicked.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct ParticipantKickedPayload {
    /// Participant identifier for the participant who was removed.
    #[serde(rename = "participantId")]
    pub participant_id: String,
    /// Opaque human-readable reason for the kick event.
    pub reason: String,
}

/// Broadcast to all participants when a participant is muted.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct ParticipantMutedPayload {
    /// Participant identifier for the participant who was muted.
    #[serde(rename = "participantId")]
    pub participant_id: String,
}

/// Broadcast to all participants when a participant's host-mute is released.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct ParticipantUnmutedPayload {
    /// Participant identifier for the participant whose mute was released.
    #[serde(rename = "participantId")]
    pub participant_id: String,
}

/// Broadcast to all participants when a participant deafens themselves.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct ParticipantDeafenedPayload {
    /// Participant identifier for the participant who deafened themselves.
    #[serde(rename = "participantId")]
    pub participant_id: String,
}

/// Broadcast to all participants when a participant undeafens themselves.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct ParticipantUndeafenedPayload {
    /// Participant identifier for the participant who undeafened themselves.
    #[serde(rename = "participantId")]
    pub participant_id: String,
}

// --- Phase 3: Screen share payload structs ---

/// Broadcast to all participants when a screen share starts.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct ShareStartedPayload {
    /// Participant identifier for the sharer who started presenting.
    #[serde(rename = "participantId")]
    pub participant_id: String,
    /// Display name for the sharer who started presenting.
    #[serde(rename = "displayName")]
    pub display_name: String,
}

/// Client requests to stop a screen share (optionally targeting another participant's share).
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct StopSharePayload {
    /// If set, a host is stopping another participant's share.
    #[serde(
        rename = "targetParticipantId",
        skip_serializing_if = "Option::is_none"
    )]
    pub target_participant_id: Option<String>,
}

/// Broadcast to all participants when a screen share stops.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct ShareStoppedPayload {
    /// Participant identifier for the sharer whose presentation stopped.
    #[serde(rename = "participantId")]
    pub participant_id: String,
    /// Display name for the sharer whose presentation stopped.
    #[serde(rename = "displayName")]
    pub display_name: String,
}

/// Snapshot of all active screen sharers in a room. Sent to late joiners.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct ShareStatePayload {
    /// Participant identifiers for every currently active sharer.
    #[serde(rename = "participantIds")]
    pub participant_ids: Vec<String>,
}

/// Client (host) requests a share permission change.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct SetSharePermissionPayload {
    /// The requested permission mode.
    pub permission: WireSharePermission,
}

/// Server broadcasts the new share permission to all participants.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct SharePermissionChangedPayload {
    /// The new permission mode.
    pub permission: WireSharePermission,
}

// --- Viewer subscription payload structs ---

/// Viewer tells server they subscribed to a sharer's stream (client → server).
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct ViewerSubscribedPayload {
    /// Participant identifier for the sharer whose stream was subscribed to.
    #[serde(rename = "targetId")]
    pub target_id: String,
}

/// Server notifies sharer that someone started watching (server → sharer).
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct ViewerJoinedPayload {
    /// Participant identifier for the viewer who started watching.
    #[serde(rename = "viewerId")]
    pub viewer_id: String,
    /// Display name for the viewer who started watching.
    #[serde(rename = "displayName")]
    pub display_name: String,
}

// --- Device Auth payload structs ---

/// Client sends access token to authenticate the WebSocket connection.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct AuthPayload {
    /// Bearer access token used to authenticate this signaling session.
    #[serde(rename = "accessToken")]
    pub access_token: String,
}

/// Server confirms authentication with the user_id.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct AuthSuccessPayload {
    /// Authenticated user identifier bound to the session.
    #[serde(rename = "userId")]
    pub user_id: String,
}

/// Server rejects authentication with an opaque reason.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct AuthFailedPayload {
    /// Opaque failure reason suitable for generic UI or logging.
    pub reason: String,
}

/// Client requests to join voice in a channel. The server resolves the active room internally.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct JoinVoicePayload {
    /// Channel identifier whose active voice room should be joined.
    #[serde(rename = "channelId")]
    pub channel_id: String,
    /// Optional caller-provided display name for the voice session.
    #[serde(rename = "displayName", skip_serializing_if = "Option::is_none")]
    pub display_name: Option<String>,
    /// User-chosen profile colour (hex string). Omitted when not set.
    #[serde(
        rename = "profileColor",
        default,
        skip_serializing_if = "Option::is_none"
    )]
    pub profile_color: Option<String>,
    /// Capability hint for synchronized sub-room support.
    /// Absent/false means the server must treat the client as legacy and place it in ROOM 1.
    #[serde(
        rename = "supportsSubRooms",
        default,
        skip_serializing_if = "Option::is_none"
    )]
    pub supports_sub_rooms: Option<bool>,
}

/// Source of a participant's sub-room membership.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
pub enum WireSubRoomMembershipSource {
    #[serde(rename = "explicit")]
    Explicit,
    #[serde(rename = "legacy_room_one")]
    LegacyRoomOne,
}

/// Snapshot information for a single synchronized sub-room.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct SubRoomInfoPayload {
    /// Stable sub-room identifier within the channel voice session.
    #[serde(rename = "subRoomId")]
    pub sub_room_id: String,
    /// Display order / label number shown as ROOM N.
    #[serde(rename = "roomNumber")]
    pub room_number: u32,
    /// Whether this is ROOM 1 (the non-deletable default room).
    #[serde(rename = "isDefault")]
    pub is_default: bool,
    /// Participant identifiers currently assigned to this sub-room.
    #[serde(rename = "participantIds", default)]
    pub participant_ids: Vec<String>,
    /// When the room is scheduled for auto-deletion, expressed as epoch milliseconds.
    /// Absent for ROOM 1 and any non-empty room.
    #[serde(rename = "deleteAtMs", default, skip_serializing_if = "Option::is_none")]
    pub delete_at_ms: Option<u64>,
}

/// Client requests a new sub-room in the active channel voice session.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct CreateSubRoomPayload {}

/// Client requests to join a specific sub-room in the active channel voice session.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct JoinSubRoomPayload {
    /// Stable sub-room identifier to join.
    #[serde(rename = "subRoomId")]
    pub sub_room_id: String,
}

/// Client requests to leave the current sub-room while remaining in the voice session.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct LeaveSubRoomPayload {}

/// Client requests passthrough from their currently joined room to another synchronized sub-room.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct SetPassthroughPayload {
    /// Target sub-room identifier. The source room is derived from the caller's membership.
    #[serde(rename = "targetSubRoomId")]
    pub target_sub_room_id: String,
}

/// Client requests that the active passthrough pair be cleared.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct ClearPassthroughPayload {}

/// Authoritative passthrough pair included in synchronized sub-room snapshots.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct PassthroughStatePayload {
    /// One involved synchronized sub-room id.
    #[serde(rename = "sourceSubRoomId")]
    pub source_sub_room_id: String,
    /// The other involved synchronized sub-room id.
    #[serde(rename = "targetSubRoomId")]
    pub target_sub_room_id: String,
    /// Server-authored display label such as "1 - 2".
    pub label: String,
}

/// Server snapshot of every sub-room in the active channel voice session.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct SubRoomStatePayload {
    /// Ordered synchronized room list as rendered by the client.
    #[serde(default)]
    pub rooms: Vec<SubRoomInfoPayload>,
    /// Optional active passthrough pair for the voice session.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub passthrough: Option<PassthroughStatePayload>,
}

/// Server announces a newly created sub-room.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct SubRoomCreatedPayload {
    /// Full metadata for the created sub-room.
    pub room: SubRoomInfoPayload,
}

/// Server announces that a participant joined or switched to a sub-room.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct SubRoomJoinedPayload {
    /// Participant identifier whose membership changed.
    #[serde(rename = "participantId")]
    pub participant_id: String,
    /// Destination sub-room identifier.
    #[serde(rename = "subRoomId")]
    pub sub_room_id: String,
    /// How the server assigned this membership.
    pub source: WireSubRoomMembershipSource,
}

/// Server announces that a participant left a sub-room.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct SubRoomLeftPayload {
    /// Participant identifier whose membership changed.
    #[serde(rename = "participantId")]
    pub participant_id: String,
    /// Previous sub-room identifier.
    #[serde(rename = "subRoomId")]
    pub sub_room_id: String,
}

/// Server announces that a sub-room was deleted.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct SubRoomDeletedPayload {
    /// Sub-room identifier that was deleted.
    #[serde(rename = "subRoomId")]
    pub sub_room_id: String,
}

// --- Ephemeral chat payload structs ---

/// Client sends a chat message to the room (client → server).
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct ChatSendPayload {
    /// Chat message body exactly as submitted by the sender.
    pub text: String,
}

/// Server broadcasts a chat message to all room participants (server → client).
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct ChatMessagePayload {
    /// Participant identifier for the sender.
    #[serde(rename = "participantId")]
    pub participant_id: String,
    /// Display name for the sender at the time the message was emitted.
    #[serde(rename = "displayName")]
    pub display_name: String,
    /// Chat message body.
    pub text: String,
    /// Server-assigned timestamp, typically encoded as ISO 8601 text.
    pub timestamp: String,
    /// Server-generated UUID for deduplication. Optional on wire for backward compat.
    #[serde(rename = "messageId", skip_serializing_if = "Option::is_none")]
    pub message_id: Option<String>,
}

// --- Chat history payload structs ---

/// Client requests chat history for the current room (client → server).
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct ChatHistoryRequestPayload {
    /// Optional cursor: return only messages after this ISO 8601 timestamp.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub since: Option<String>,
}

/// A single historical chat message in a ChatHistoryResponse.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct ChatHistoryMessagePayload {
    /// Stable server-assigned message identifier.
    #[serde(rename = "messageId")]
    pub message_id: String,
    /// Participant identifier for the original sender.
    #[serde(rename = "participantId")]
    pub participant_id: String,
    /// Display name captured with the historical message.
    #[serde(rename = "displayName")]
    pub display_name: String,
    /// Historical chat message body.
    pub text: String,
    /// Original server timestamp for the historical message.
    pub timestamp: String,
}

/// Server responds with historical chat messages (server → client).
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct ChatHistoryResponsePayload {
    /// Historical chat messages returned by the server in wire order.
    pub messages: Vec<ChatHistoryMessagePayload>,
}

/// Sent to a client whose session was displaced by the same user joining from another client.
/// The evicted client should NOT attempt to reconnect.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct SessionDisplacedPayload {
    /// Opaque explanation for why the session was displaced.
    pub reason: String,
}

/// Client sends updated profile colour to the backend.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct UpdateProfileColorPayload {
    /// New colour chosen by the user (hex string, e.g. "#E06C75").
    #[serde(rename = "profileColor")]
    pub profile_color: String,
}

/// Server broadcasts new colour to all room participants.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct ParticipantColorUpdatedPayload {
    /// Identifier of the participant who changed their colour.
    #[serde(rename = "participantId")]
    pub participant_id: String,
    /// New colour (hex string, e.g. "#E06C75").
    #[serde(rename = "profileColor")]
    pub profile_color: String,
}

// --- Error types ---

/// Error returned when parsing a signaling message from a JSON string.
#[derive(Debug)]
pub enum ParseError {
    /// JSON parsing failed before a valid `SignalingMessage` could be produced.
    InvalidJson(serde_json::Error),
}

impl std::fmt::Display for ParseError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ParseError::InvalidJson(e) => write!(f, "invalid JSON: {e}"),
        }
    }
}

impl std::error::Error for ParseError {}

/// Error returned when serializing a signaling message to JSON.
#[derive(Debug)]
pub enum SerializeError {
    /// JSON serialization failed for the provided signaling message.
    SerializationFailed(serde_json::Error),
}

impl std::fmt::Display for SerializeError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            SerializeError::SerializationFailed(e) => write!(f, "serialization failed: {e}"),
        }
    }
}

impl std::error::Error for SerializeError {}

// --- Helper functions ---

/// Parse a JSON string into a `SignalingMessage`.
pub fn parse(input: &str) -> Result<SignalingMessage, ParseError> {
    serde_json::from_str(input).map_err(ParseError::InvalidJson)
}

/// Serialize a `SignalingMessage` to a JSON string.
pub fn to_json(msg: &SignalingMessage) -> Result<String, SerializeError> {
    serde_json::to_string(msg).map_err(SerializeError::SerializationFailed)
}

// Always include proptest_support when testing (either unit tests or when feature is enabled)
#[cfg(any(test, feature = "proptest-support"))]
/// Proptest generators and strategies for signaling schema evolution tests.
pub mod proptest_support;

/// Validation helpers that enforce signaling-level structural invariants.
pub mod validation;

#[cfg(test)]
mod tests;
