/**
 * Server -> client signaling message types.
 *
 * Mirrors the server->client subset of `SignalingMessage` in
 * shared/src/signaling/mod.rs (`#[serde(tag = "type", rename_all =
 * "snake_case")]`; inner payload fields are camelCase via explicit
 * `#[serde(rename)]`). Field optionality here matches how voice-room.ts's
 * dispatchMessage actually trusts/guards each field today — not full Rust
 * nullability — so introducing this type does not change runtime behavior.
 * Fields the current code validates itself (typeof/Array.isArray checks)
 * are typed `unknown` so those guards stay meaningful to the type checker.
 */

export interface WireIceConfig {
  stunUrls: string[];
  turnUrls: string[];
  turnUsername?: string;
  turnCredential?: string;
}

export interface WireParticipant {
  participantId: string;
  displayName: string;
  userId?: string;
  profileColor?: string;
  isMuted?: boolean;
  isHostMuted?: boolean;
  isDeafened?: boolean;
}

export interface AuthSuccessMessage {
  type: 'auth_success';
}

export interface AuthFailedMessage {
  type: 'auth_failed';
  reason?: string;
}

export interface JoinedMessage {
  type: 'joined';
  peerId: string;
  roomId: string;
  iceConfig?: WireIceConfig;
  sharePermission?: string;
  participants?: WireParticipant[];
}

export interface SfuColdStartingMessage {
  type: 'sfu_cold_starting';
  estimatedWaitSecs?: unknown;
}

export interface JoinRejectedMessage {
  type: 'join_rejected';
  reason?: string;
}

export interface ParticipantJoinedMessage {
  type: 'participant_joined';
  participantId: string;
  displayName: string;
  userId?: string;
  profileColor?: string;
}

export interface ParticipantLeftMessage {
  type: 'participant_left';
  participantId: string;
}

export interface WireSubRoom {
  subRoomId: string;
  roomNumber: number;
  isDefault?: unknown;
  participantIds?: unknown;
  deleteAtMs?: unknown;
}

export interface SubRoomStateMessage {
  type: 'sub_room_state';
  rooms?: WireSubRoom[];
  passthrough?: Record<string, unknown> | null;
  passthroughEnabled?: unknown;
  passthroughVolumePercent?: unknown;
  passthroughFiltersEnabled?: unknown;
  passthroughFilterStrength?: unknown;
}

export interface SubRoomCreatedMessage {
  type: 'sub_room_created';
  room?: WireSubRoom;
}

export interface SubRoomJoinedMessage {
  type: 'sub_room_joined';
  participantId: string;
  subRoomId: string;
  source?: string;
}

export interface SubRoomLeftMessage {
  type: 'sub_room_left';
  participantId: string;
  subRoomId: string;
}

export interface SubRoomDeletedMessage {
  type: 'sub_room_deleted';
  subRoomId: string;
}

export interface RoomStateMessage {
  type: 'room_state';
  participants?: WireParticipant[];
}

export interface ParticipantKickedMessage {
  type: 'participant_kicked';
  participantId: string;
}

export interface SessionDisplacedMessage {
  type: 'session_displaced';
}

export interface ParticipantMutedMessage {
  type: 'participant_muted';
  participantId: string;
}

export interface ParticipantUnmutedMessage {
  type: 'participant_unmuted';
  participantId: string;
}

export interface ParticipantSelfMutedMessage {
  type: 'participant_self_muted';
  participantId: string;
}

export interface ParticipantSelfUnmutedMessage {
  type: 'participant_self_unmuted';
  participantId: string;
}

export interface ParticipantDeafenedMessage {
  type: 'participant_deafened';
  participantId: string;
}

export interface ParticipantUndeafenedMessage {
  type: 'participant_undeafened';
  participantId: string;
}

export interface ParticipantColorUpdatedMessage {
  type: 'participant_color_updated';
  participantId: string;
  profileColor: string;
}

export interface ParticipantUsernameUpdatedMessage {
  type: 'participant_username_updated';
  participantId: string;
  username: string;
}

export interface ShareStartedMessage {
  type: 'share_started';
  participantId: string;
  displayName?: string;
  shareType?: unknown;
}

export interface ShareStoppedMessage {
  type: 'share_stopped';
  participantId: string;
  displayName?: string;
}

export interface WireActiveShare {
  participantId: string;
  shareType?: unknown;
}

export interface ShareStateMessage {
  type: 'share_state';
  participantIds?: string[];
  activeShares?: WireActiveShare[];
}

export interface SharePermissionChangedMessage {
  type: 'share_permission_changed';
  permission?: string;
}

export interface ErrorMessage {
  type: 'error';
  message?: string;
}

export interface PeerLeftMessage {
  type: 'peer_left';
  participantId?: string;
  peerId?: string;
}

export interface MediaTokenMessage {
  type: 'media_token';
  token: string;
  sfuUrl: string;
  iceConfig?: WireIceConfig;
}

export interface ViewerTokenMessage {
  type: 'viewer_token';
  windowId: string;
  token: string;
  sfuUrl: string;
  identity: string;
}

export interface ChatMessageMessage {
  type: 'chat_message';
  messageId?: string;
  timestamp: string;
  participantId: string;
  userId?: string;
  displayName: string;
  text: string;
}

export interface WireChatHistoryEntry {
  messageId: string;
  participantId: string;
  userId?: string;
  displayName: string;
  text: string;
  timestamp: string;
}

export interface ChatHistoryResponseMessage {
  type: 'chat_history_response';
  messages?: WireChatHistoryEntry[];
}

export interface ViewerJoinedMessage {
  type: 'viewer_joined';
}

/** Discriminated union of every server->client message dispatchMessage handles. */
export type ServerMessage =
  | AuthSuccessMessage
  | AuthFailedMessage
  | JoinedMessage
  | SfuColdStartingMessage
  | JoinRejectedMessage
  | ParticipantJoinedMessage
  | ParticipantLeftMessage
  | SubRoomStateMessage
  | SubRoomCreatedMessage
  | SubRoomJoinedMessage
  | SubRoomLeftMessage
  | SubRoomDeletedMessage
  | RoomStateMessage
  | ParticipantKickedMessage
  | SessionDisplacedMessage
  | ParticipantMutedMessage
  | ParticipantUnmutedMessage
  | ParticipantSelfMutedMessage
  | ParticipantSelfUnmutedMessage
  | ParticipantDeafenedMessage
  | ParticipantUndeafenedMessage
  | ParticipantColorUpdatedMessage
  | ParticipantUsernameUpdatedMessage
  | ShareStartedMessage
  | ShareStoppedMessage
  | ShareStateMessage
  | SharePermissionChangedMessage
  | ErrorMessage
  | PeerLeftMessage
  | MediaTokenMessage
  | ViewerTokenMessage
  | ChatMessageMessage
  | ChatHistoryResponseMessage
  | ViewerJoinedMessage;

/** Every `type` tag dispatchMessage's switch handles (mirrors the case list exactly). */
export const KNOWN_SERVER_MESSAGE_TYPES: ReadonlySet<ServerMessage['type']> = new Set([
  'auth_success',
  'auth_failed',
  'joined',
  'sfu_cold_starting',
  'join_rejected',
  'participant_joined',
  'participant_left',
  'sub_room_state',
  'sub_room_created',
  'sub_room_joined',
  'sub_room_left',
  'sub_room_deleted',
  'room_state',
  'participant_kicked',
  'session_displaced',
  'participant_muted',
  'participant_unmuted',
  'participant_self_muted',
  'participant_self_unmuted',
  'participant_deafened',
  'participant_undeafened',
  'participant_color_updated',
  'participant_username_updated',
  'share_started',
  'share_stopped',
  'share_state',
  'share_permission_changed',
  'error',
  'peer_left',
  'media_token',
  'viewer_token',
  'chat_message',
  'chat_history_response',
  'viewer_joined',
] satisfies ServerMessage['type'][]);
