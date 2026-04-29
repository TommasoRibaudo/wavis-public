/**
 * Wavis Voice Room Service
 *
 * State machine, signaling message dispatch, participant tracking,
 * and event log management for channel-based voice sessions.
 * Connects via SignalingClient (websocket.ts) and bridges volume
 * controls to the Rust audio backend via Tauri IPC.
 */

import { SignalingClient } from '@shared/websocket';
import type { ChannelRole } from '@features/channels/channels';
import { getServerUrl, getDisplayName, refreshTokens, onTokensRefreshed } from '@features/auth/auth';
import { PROFILE_COLORS } from '@shared/colors';
import { toWsUrl } from '@shared/helpers';
import { LiveKitModule, type MediaState, type MediaCallbacks, type ShareQualityInfo, type ShareStats, type VideoReceiveStats } from './livekit-media';
import { NativeMediaModule } from './native-media';
import { setActiveLiveKitModule } from './audio-devices';
import { getDefaultVolume, getReconnectConfig, getMuteHotkey, getProfileColor, getChannelVolumes, setChannelVolumes } from '@features/settings/settings-store';
import type { ChannelVolumePrefs } from '@features/settings/settings-store';
import type { ShareMode, ShareSelection, EnumerationResult, FallbackReason, AudioShareStartResult } from '@features/screen-share/share-types';
import type { ShareSessionLeakSummary } from './share-leak-diagnostics';
import { registerMuteHotkey, unregisterMuteHotkey } from '@shared/hotkey-bridge';
import { playNotificationSound } from './notification-sounds';
import { toast } from 'sonner';
import { invoke } from '@tauri-apps/api/core';
import { WebviewWindow } from '@tauri-apps/api/webviewWindow';
import { emit, listen, type UnlistenFn } from '@tauri-apps/api/event';

const LOG = '[wavis:voice-room]';
const DEBUG_WASAPI = import.meta.env.VITE_DEBUG_WASAPI === 'true';
const DEBUG_SHARE_AUDIO = import.meta.env.VITE_DEBUG_SHARE_AUDIO === 'true';

/* ─── Types ─────────────────────────────────────────────────────── */

export type VoiceRoomMachineState =
  | 'idle'
  | 'connecting'
  | 'authenticated'
  | 'joining'
  | 'server_starting'
  | 'active'
  | 'reconnecting';

export type ParticipantRole = 'host' | 'guest';

export interface RoomParticipant {
  id: string;
  userId?: string;
  displayName: string;
  color: string;
  role: ParticipantRole;
  isSpeaking: boolean;
  isMuted: boolean;
  isHostMuted: boolean;
  isDeafened: boolean;
  isSharing: boolean;
  shareType?: string;
  rmsLevel: number;
  volume: number;
}

export type SubRoomMembershipSource = 'explicit' | 'legacy_room_one';

export interface VoiceSubRoom {
  id: string;
  roomNumber: number;
  isDefault: boolean;
  participantIds: string[];
  deleteAtMs: number | null;
}

export interface VoicePassthroughState {
  sourceSubRoomId: string;
  targetSubRoomId: string;
  label: string;
}

export type RoomEventType =
  | 'join'
  | 'leave'
  | 'kicked'
  | 'muted'
  | 'unmuted'
  | 'host-mute'
  | 'host-unmute'
  | 'deafen'
  | 'undeafen'
  | 'share-start'
  | 'share-stop'
  | 'share-permission'
  | 'system';
export interface ChatMessage {
  id: string;
  messageId?: string;
  timestamp: string;
  participantId: string;
  displayName: string;
  color: string;
  text: string;
  isHistory?: boolean;
  isDivider?: boolean;
}


export interface RoomEvent {
  id: string;
  timestamp: string;
  type: RoomEventType;
  message: string;
  participantId?: string;
}

export interface NetworkStats {
  rttMs: number;
  packetLossPercent: number;
  jitterMs: number;
  /** Current jitter buffer target delay in ms (0 = unavailable). */
  jitterBufferDelayMs: number;
  /** Concealment events (PLC activations) since the previous stats poll. */
  concealmentEventsPerInterval: number;
  /** ICE candidate type of the active connection. */
  candidateType: 'host' | 'srflx' | 'relay' | 'unknown';
  /** Estimated available outgoing bandwidth in kbps (0 = unavailable). */
  availableBandwidthKbps: number;
}

export interface VoiceRoomState {
  machineState: VoiceRoomMachineState;
  roomId: string | null;
  channelId: string;
  channelName: string;
  selfParticipantId: string | null;
  selfIsHost: boolean;
  participants: RoomParticipant[];
  masterVolume: number;
  networkStats: NetworkStats;
  mediaState: MediaState;
  mediaError: string | null;
  screenShareStreams: Map<string, MediaStream | null>;
  events: RoomEvent[];
  chatMessages: ChatMessage[];
  error: string | null;
  rejectionReason: string | null;
  serverStartingEstimatedWaitSecs: number | null;
  /** Actual captured screen share quality (null when not sharing). */
  shareQualityInfo: ShareQualityInfo | null;
  /** Live screen-share sender stats from the 5s polling loop (null when not sharing). */
  shareStats: ShareStats | null;
  /** Live video receiver stats for an incoming screen share (null when not watching). */
  videoReceiveStats: VideoReceiveStats | null;
  /** Connection mode: 'livekit' (JS SDK) or 'native' (Rust IPC). Debug-only badge. */
  connectionMode: 'livekit' | 'native' | undefined;
  /** Share permission from server: 'anyone' or 'host_only'. */
  sharePermission: 'anyone' | 'host_only';
  /** Default volume loaded from store, used for new participants. */
  defaultVolume: number;
  /** Consecutive media reconnect failure count. */
  mediaReconnectFailures: number;
  /** Active video share slot (screen or window). Null when no video share. */
  activeVideoShare: { mode: 'screen_audio' | 'window'; sourceName: string; withAudio: boolean } | null;
  /** Active standalone audio share slot. Null when no audio-only share. */
  activeAudioShare: { sourceId: string; sourceName: string } | null;
  /** Transient error from the last `error` signaling message (for chat panel display). */
  lastChatError: string | null;
  historyLoaded: boolean;
  /** Whether the local user is deafened (muted + volume 0). */
  isDeafened: boolean;
  /** Latest closed native video share leak summary for bug reports and repro analysis. */
  latestClosedShareLeakSummary: ShareSessionLeakSummary | null;
  /** True when the Windows native mic bridge (Rust WASAPI + DenoiseFilter) is active. */
  nativeMicBridgeActive: boolean;
  /** True when the active JS LiveKit microphone track has the denoise processor attached. */
  noiseSuppressionActive: boolean;
  /** Ordered synchronized sub-rooms inside the current channel voice session. */
  subRooms: VoiceSubRoom[];
  /** Actual sub-room currently joined by the local participant, or null when in no sub-room. */
  joinedSubRoomId: string | null;
  /** Session-scoped preferred sub-room used to restore room membership after reconnects. */
  desiredSubRoomId: string | null;
  /** Derived reverse index of participant id -> synchronized sub-room id. */
  participantSubRoomById: Record<string, string>;
  /** Authoritative active passthrough pair, if any. */
  passthrough: VoicePassthroughState | null;
}

/* ─── Constants ─────────────────────────────────────────────────── */

export const TERMINAL_COLORS = PROFILE_COLORS;

export const RMS_START_THRESHOLD = 0.06;
export const RMS_STOP_THRESHOLD = 0.03;
export const MAX_EVENTS = 100;
export const MAX_PARTICIPANTS = 6;
export const MAX_CHAT_MESSAGES = 200;

/**
 * EMA smoothing factor for RMS levels. Lower = smoother but more latent.
 * 0.4 at 50ms polling ≈ 125ms effective time constant — fast enough to
 * track low-volume speech without excessive flicker.
 */
export const RMS_EMA_ALPHA = 0.4;

/**
 * Number of consecutive frames the hysteresis decision must agree before
 * the speaking state actually transitions. At 50ms polling, 3 frames = 150ms.
 * Prevents single-frame spikes/dips from flipping the indicator.
 */
export const SPEAKING_DEBOUNCE_FRAMES = 3;

/** Per-participant smoothing + debounce state for the speaking indicator. */
interface SpeakingTrackerEntry {
  smoothedRms: number;
  /** How many consecutive frames the hysteresis output has been the *opposite* of current isSpeaking. */
  pendingFrames: number;
  /** The hysteresis output that pendingFrames is counting toward. */
  pendingState: boolean;
}

/** Module-level map — keyed by participant id. Cleared on leaveRoom. */
const speakingTracker = new Map<string, SpeakingTrackerEntry>();

/**
 * Update the smoothed RMS for a participant and return the debounced speaking state.
 * Exported for testability.
 */
export function updateSpeakingTracker(
  participantId: string,
  rawRms: number,
  currentlySpeaking: boolean,
  isMuted: boolean,
): boolean {
  // Muted is an immediate override — no debouncing needed.
  if (isMuted) return false;

  let entry = speakingTracker.get(participantId);
  if (!entry) {
    entry = { smoothedRms: rawRms, pendingFrames: 0, pendingState: currentlySpeaking };
    speakingTracker.set(participantId, entry);
  }

  // EMA smooth the raw RMS
  entry.smoothedRms = RMS_EMA_ALPHA * rawRms + (1 - RMS_EMA_ALPHA) * entry.smoothedRms;

  // Run hysteresis on the smoothed value
  const hysteresisResult = computeSpeaking(currentlySpeaking, entry.smoothedRms, false);

  if (hysteresisResult !== currentlySpeaking) {
    // Hysteresis wants to flip — count consecutive agreeing frames
    if (hysteresisResult === entry.pendingState) {
      entry.pendingFrames++;
    } else {
      // Direction changed — reset counter
      entry.pendingState = hysteresisResult;
      entry.pendingFrames = 1;
    }
    if (entry.pendingFrames >= SPEAKING_DEBOUNCE_FRAMES) {
      // Stable for enough frames — commit the transition
      entry.pendingFrames = 0;
      return hysteresisResult;
    }
    // Not stable yet — hold current state
    return currentlySpeaking;
  }

  // Hysteresis agrees with current state — reset pending counter
  entry.pendingFrames = 0;
  entry.pendingState = currentlySpeaking;
  return currentlySpeaking;
}

/* ─── Helpers (private) ─────────────────────────────────────────── */

let eventCounter = 0;

function makeEventId(): string {
  eventCounter += 1;
  return `evt-${Date.now()}-${eventCounter}`;
}

function timestamp(): string {
  return new Date().toISOString();
}

function makeShareSessionId(): string {
  if (typeof crypto !== 'undefined' && typeof crypto.randomUUID === 'function') {
    return crypto.randomUUID();
  }
  return `share-${Date.now()}-${Math.random().toString(36).slice(2, 10)}`;
}

/** Resolve the local user's display name for event log messages. */
function selfName(): string {
  const self = state.participants.find((p) => p.id === state.selfParticipantId);
  return self?.displayName ?? (state.selfParticipantId ? displayNameCache.get(state.selfParticipantId) : undefined) ?? 'You';
}

/**
 * Stable hash-based color: same userId/participantId always gets the same color.
 * Uses FNV-1a 32-bit hash.
 */
export function colorFor(participant: { userId?: string; id: string }): string {
  const key = participant.userId ?? participant.id;
  let h = 2166136261; // FNV-1a 32-bit offset basis
  for (let i = 0; i < key.length; i++) {
    h ^= key.charCodeAt(i);
    h = Math.imul(h, 16777619);
  }
  return TERMINAL_COLORS[Math.abs(h) % TERMINAL_COLORS.length];
}

/**
 * Pure function: compute next speaking state given current state and new RMS level.
 * Uses hysteresis — start threshold (0.06) is higher than stop threshold (0.03)
 * to prevent indicator flicker while remaining sensitive to low-volume speech.
 */
export function computeSpeaking(
  currentlySpeaking: boolean,
  rmsLevel: number,
  isMuted: boolean,
): boolean {
  if (isMuted) return false;
  if (currentlySpeaking) return rmsLevel >= RMS_STOP_THRESHOLD;
  return rmsLevel >= RMS_START_THRESHOLD;
}

/**
 * Pure function: determine if screen sharing is enabled for the current user.
 * Requires all three conditions: permission allows it (anyone OR host),
 * room is active, and LiveKit media connection is established.
 */
export function isShareEnabled(sharePermission: 'anyone' | 'host_only', selfIsHost: boolean, machineState: VoiceRoomMachineState, mediaState: MediaState): boolean {
  return (sharePermission === 'anyone' || selfIsHost) && machineState === 'active' && mediaState === 'connected'
}

/**
 * Compute the text label for the share/stopshare button.
 * "host only" is only shown when the permission policy is the actual reason
 * sharing is disabled — not when the room is reconnecting or media is not ready.
 */
export function shareButtonLabel(
  shareEnabled: boolean,
  selfSharing: boolean,
  sharePermission: 'anyone' | 'host_only',
  selfIsHost: boolean,
): string {
  if (selfSharing) return '/stopshare';
  if (shareEnabled) return '/share';
  if (sharePermission === 'host_only' && !selfIsHost) return '/share (host only)';
  return '/share';
}

/**
 * Resolve a participant's volume from persisted channel prefs.
 * Falls back to defaultVolume if no persisted value exists.
 */
function resolvePersistedVolume(userId: string | undefined, defaultVolume: number): number {
  if (!channelVolumePrefs || !userId) return defaultVolume;
  return channelVolumePrefs.participants[userId] ?? defaultVolume;
}

export function computeEffectiveParticipantVolume(
  manualVolume: number,
  participantId: string,
  selfParticipantId: string | null,
  joinedSubRoomId: string | null,
  participantSubRoomById: Record<string, string>,
  passthrough: VoicePassthroughState | null,
): number {
  if (participantId === selfParticipantId) return manualVolume;
  if (!joinedSubRoomId) return 0;
  const participantSubRoomId = participantSubRoomById[participantId] ?? null;
  if (participantSubRoomId === joinedSubRoomId) return manualVolume;
  if (!participantSubRoomId || !passthrough) return 0;

  const pairedSubRoomId = passthrough.sourceSubRoomId === joinedSubRoomId
    ? passthrough.targetSubRoomId
    : passthrough.targetSubRoomId === joinedSubRoomId
      ? passthrough.sourceSubRoomId
      : null;
  if (participantSubRoomId !== pairedSubRoomId) return 0;
  return Math.round(manualVolume * 0.2);
}

/**
 * Pure function: merge old and new participant lists, preserving per-participant
 * volume settings across reconnects. Matched by id: present in both → keep old
 * volume; only in new → default volume; only in old → discarded.
 */
export function mergeParticipantsWithVolume(
  oldList: RoomParticipant[],
  newList: RoomParticipant[],
): RoomParticipant[] {
  const volumeMap = new Map(oldList.map((p) => [p.id, p.volume]));
  return newList.map((p) => ({
    ...p,
    volume: volumeMap.get(p.id) ?? p.volume,
  }));
}

function applyEffectiveParticipantVolume(participant: RoomParticipant): void {
  if (!lkModule || participant.id === state.selfParticipantId) return;
  lkModule.setParticipantVolume(
    participant.id,
    computeEffectiveParticipantVolume(
      participant.volume,
      participant.id,
      state.selfParticipantId,
      state.joinedSubRoomId,
      state.participantSubRoomById,
      state.passthrough,
    ),
  );
}

function applyEffectiveParticipantVolumes(): void {
  if (!lkModule) return;
  for (const participant of state.participants) {
    applyEffectiveParticipantVolume(participant);
  }
}

/**
 * Pure function: compute the `since` cursor for a ChatHistoryRequest.
 * Filters to non-history (real-time) messages, finds the earliest timestamp,
 * subtracts 1 second, and returns as ISO string. Returns undefined if no
 * real-time messages exist.
 */
export function computeSinceCursor(messages: ChatMessage[]): string | undefined {
  const realTime = messages.filter((m) => !m.isHistory);
  if (realTime.length === 0) return undefined;
  let earliest = realTime[0].timestamp;
  for (let i = 1; i < realTime.length; i++) {
    if (realTime[i].timestamp < earliest) {
      earliest = realTime[i].timestamp;
    }
  }
  const d = new Date(earliest);
  d.setTime(d.getTime() - 1000);
  return d.toISOString();
}

/**
 * Pure function: merge history messages with existing real-time messages.
 * Deduplicates by messageId, prepends history, inserts divider, enforces cap.
 * Exported for property testing.
 */
export function mergeHistoryMessages(
  historyPayload: Array<{ messageId: string; participantId: string; displayName: string; text: string; timestamp: string }>,
  existingMessages: ChatMessage[],
): ChatMessage[] {
  // Build set of existing messageIds for dedup (skip entries without messageId)
  const existingIds = new Set<string>();
  for (const m of existingMessages) {
    if (m.messageId) existingIds.add(m.messageId);
  }

  // Filter and convert history messages
  const historyMessages: ChatMessage[] = historyPayload
    .filter((h) => !existingIds.has(h.messageId))
    .map((h) => ({
      id: h.messageId,
      messageId: h.messageId,
      timestamp: h.timestamp,
      participantId: h.participantId,
      displayName: h.displayName,
      color: colorFor({ id: h.participantId }),
      text: h.text,
      isHistory: true,
    }));

  // Build merged array: history + divider (if history non-empty) + existing
  let merged: ChatMessage[];
  if (historyMessages.length > 0) {
    const divider: ChatMessage = {
      id: 'history-divider',
      messageId: undefined,
      timestamp: '',
      participantId: '',
      displayName: '',
      color: '',
      text: '',
      isHistory: false,
      isDivider: true,
    };
    merged = [...historyMessages, divider, ...existingMessages];
  } else {
    merged = [...existingMessages];
  }

  // Enforce cap — keep most recent MAX_CHAT_MESSAGES
  if (merged.length > MAX_CHAT_MESSAGES) {
    merged = merged.slice(merged.length - MAX_CHAT_MESSAGES);
  }

  return merged;
}

/* ─── Pure Share Helpers (exported for property testing) ─────────── */

/** Derive the legacy activeShareType from the two-slot model. */
export function activeShareType(
  videoShare: VoiceRoomState['activeVideoShare'],
  audioShare: VoiceRoomState['activeAudioShare'],
): ShareMode | null {
  // Video share takes precedence for display purposes
  if (videoShare) return videoShare.mode;
  if (audioShare) return 'audio_only';
  return null;
}

/** Whether any share is active (either slot occupied). */
export function isAnyShareActive(
  videoShare: VoiceRoomState['activeVideoShare'],
  audioShare: VoiceRoomState['activeAudioShare'],
): boolean {
  return videoShare !== null || audioShare !== null;
}

/** Check if a given share selection conflicts with current state. */
export function canStartShare(
  selection: ShareSelection,
  videoShare: VoiceRoomState['activeVideoShare'],
  audioShare: VoiceRoomState['activeAudioShare'],
): { allowed: boolean; reason?: string } {
  if (selection.mode === 'audio_only') {
    if (audioShare) return { allowed: false, reason: 'audio-only share already active' };
    return { allowed: true };
  }
  // screen_audio or window
  if (videoShare) return { allowed: false, reason: 'video share already active' };
  return { allowed: true };
}

/**
 * Pure routing logic for fallback share outcomes.
 * Given the boolean result of startScreenShare(), returns the action to take:
 * - 'send_start_share': capture succeeded → send signaling + notify
 * - 'no_op': user cancelled or capture failed silently → do nothing
 */
export function fallbackShareAction(startScreenShareResult: boolean): 'send_start_share' | 'no_op' {
  return startScreenShareResult ? 'send_start_share' : 'no_op';
}

/** Possible actions from the share routing decision. */
export type ShareRouteAction =
  | 'open_picker'
  | 'fallback_share'
  | 'error_toast'
  | 'no_sources_toast';

/**
 * Pure routing logic for handleStartShare.
 * Given the enumeration result (or null on error), whether an error occurred,
 * and the current connectionMode, returns the action to take.
 */
export function computeShareRoute(
  enumResult: { sources: { length: number }; fallback_reason: FallbackReason | null } | null,
  enumError: boolean,
  connectionMode: 'livekit' | 'native' | undefined,
): ShareRouteAction {
  if (enumError) {
    return connectionMode === 'livekit' ? 'fallback_share' : 'error_toast';
  }
  if (!enumResult) return 'error_toast';
  if (enumResult.sources.length > 0 || enumResult.fallback_reason === 'portal') {
    return 'open_picker';
  }
  if (enumResult.fallback_reason === 'get_display_media' && connectionMode === 'livekit') {
    return 'fallback_share';
  }
  return 'no_sources_toast';
}

/**
 * Pure routing logic for the stop button.
 * Given the current activeShareType and selfSharing flag, returns which stop
 * function to invoke:
 * - 'stop_custom': custom picker share is active → call stopCustomShare()
 * - 'stop_fallback': fallback (getDisplayMedia) share is active → call stopShare()
 * - 'none': not sharing → no-op
 */
export function computeStopRoute(
  activeShareType: ShareMode | null,
  selfSharing: boolean,
): 'stop_custom' | 'stop_fallback' | 'none' {
  if (activeShareType !== null) return 'stop_custom';
  if (selfSharing) return 'stop_fallback';
  return 'none';
}

/**
 * Pure logic for whether the share button should be disabled.
 * Disabled when any share is active — either custom or fallback.
 */
export function isShareButtonDisabled(
  activeShareType: ShareMode | null,
  selfSharing: boolean,
): boolean {
  return activeShareType !== null || selfSharing;
}

/**
 * Pure logic for whether the inline fallback share badge should be visible.
 * Visible when a fallback (getDisplayMedia) share is active — i.e., no custom
 * share type but the participant is sharing via the browser-native path.
 */
export function isFallbackBadgeVisible(
  activeShareType: ShareMode | null,
  selfSharing: boolean,
): boolean {
  return activeShareType === null && selfSharing;
}

/**
 * Pure logic for what share cleanup action leaveRoom should perform.
 * Returns which cleanup path to take:
 * - 'custom': custom share is active → stop captures + send stop_share
 * - 'fallback': fallback share is active → send stop_share only
 * - 'none': not sharing → no share cleanup needed
 */
export function computeLeaveShareCleanup(
  activeShareType: ShareMode | null,
  selfSharing: boolean,
): 'custom' | 'fallback' | 'none' {
  if (activeShareType !== null) return 'custom';
  if (selfSharing) return 'fallback';
  return 'none';
}

/**
 * Start a screen share via the browser-native getDisplayMedia() fallback.
 * Used when the custom share picker cannot enumerate sources (macOS, Windows
 * without Graphics Capture API, or any enumeration failure in livekit mode).
 * This is also the normal Windows share path from ActiveRoom, so logging or
 * leak instrumentation cannot live only in startCustomShare().
 *
 * startScreenShare() returns false for user-cancelled or silent failures.
 * It throws for platform-level permission denial so the caller can surface
 * a helpful OS-level message. The only other throw here is when lkModule is null.
 */
export async function startFallbackShare(): Promise<{ started: boolean; withAudio: boolean }> {
  console.log(LOG, `startFallbackShare: lkModule=${lkModule ? lkModule.constructor.name : 'null'}, mediaState=${state.mediaState}, machineState=${state.machineState}`);
  if (!lkModule) {
    throw new Error(`Screen sharing is not available (media module not initialized, mediaState=${state.mediaState})`);
  }

  const success = await lkModule.startScreenShare();

  if (success) {
    if (client) {
      // Include shareType so every start_share message carries origin metadata,
      // consistent with startCustomShare() which sends the native picker's mode.
      client.send({ type: 'start_share', shareType: 'browser' });
    }
    notify();
  }
  // Native share-audio platforms report an audio track only after the
  // separate audio bridge is started. Browser-managed paths report it here.
  const hasAudio = success && 'hasScreenShareAudio' in lkModule
    ? (lkModule as { hasScreenShareAudio(): boolean }).hasScreenShareAudio()
    : false;
  return { started: success, withAudio: hasAudio };
  // When success === false (user cancelled browser picker, or capture failed
  // silently), do nothing — no signaling, no error.
}


/** Plan which capture commands to invoke for a share selection. */
export function planShareCommands(selection: ShareSelection): {
  videoCommand: { name: string; sourceId: string } | null;
  audioCommand: { name: string; resolveMonitor: boolean } | null;
} {
  const needsVideo = selection.mode === 'screen_audio' || selection.mode === 'window';
  const needsAudio =
    selection.mode === 'audio_only' ||
    ((selection.mode === 'screen_audio' || selection.mode === 'window') && selection.withAudio);

  return {
    videoCommand: needsVideo
      ? { name: 'screen_share_start_source', sourceId: selection.sourceId }
      : null,
    audioCommand: needsAudio
      ? { name: 'audio_share_start', resolveMonitor: selection.mode !== 'audio_only' }
      : null,
  };
}

/** Plan which stop commands to invoke for a specific share slot. */
export function planStopCommands(
  target: 'video' | 'audio' | 'all',
  videoShare: VoiceRoomState['activeVideoShare'],
  audioShare: VoiceRoomState['activeAudioShare'],
): { stopVideo: boolean; stopCompanionAudio: boolean; stopAudioOnly: boolean } {
  if (target === 'video') {
    return {
      stopVideo: videoShare !== null,
      stopCompanionAudio: videoShare?.withAudio ?? false,
      stopAudioOnly: false,
    };
  }
  if (target === 'audio') {
    return {
      stopVideo: false,
      stopCompanionAudio: false,
      stopAudioOnly: audioShare !== null,
    };
  }
  // 'all'
  return {
    stopVideo: videoShare !== null,
    stopCompanionAudio: videoShare?.withAudio ?? false,
    stopAudioOnly: audioShare !== null,
  };
}

/** Build the start_share signaling message. */
export function buildStartShareMessage(mode: ShareMode): { type: string; shareType: ShareMode } {
  return { type: 'start_share', shareType: mode };
}

/* ─── Session State ─────────────────────────────────────────────── */

let client: SignalingClient | null = null;
let unsubscribe: (() => void) | null = null;
let onChange: ((state: VoiceRoomState) => void) | null = null;
let channelRole: ChannelRole | null = null;
let sessionDisplayName: string | null = null;
let sessionProfileColor: string | null = null;
let channelVolumePrefs: ChannelVolumePrefs | null = null;
let volumeSaveTimer: ReturnType<typeof setTimeout> | null = null;

const DEFAULT_STATE: VoiceRoomState = {
  machineState: 'idle',
  roomId: null,
  channelId: '',
  channelName: '',
  selfParticipantId: null,
  selfIsHost: false,
  participants: [],
  masterVolume: 70,
  networkStats: {
    rttMs: 0,
    packetLossPercent: 0,
    jitterMs: 0,
    jitterBufferDelayMs: 0,
    concealmentEventsPerInterval: 0,
    candidateType: 'unknown' as const,
    availableBandwidthKbps: 0,
  },
  mediaState: 'disconnected' as MediaState,
  mediaError: null,
  screenShareStreams: new Map(),
  events: [],
  chatMessages: [],
  error: null,
  rejectionReason: null,
  serverStartingEstimatedWaitSecs: null,
  shareQualityInfo: null,
  shareStats: null,
  videoReceiveStats: null,
  connectionMode: undefined,
  sharePermission: 'anyone',
  defaultVolume: 70,
  mediaReconnectFailures: 0,
  activeVideoShare: null,
  activeAudioShare: null,
  lastChatError: null,
  historyLoaded: false,
  isDeafened: false,
  latestClosedShareLeakSummary: null,
  nativeMicBridgeActive: false,
  noiseSuppressionActive: false,
  subRooms: [],
  joinedSubRoomId: null,
  desiredSubRoomId: null,
  participantSubRoomById: {},
  passthrough: null,
};

let state: VoiceRoomState = { ...DEFAULT_STATE, events: [], chatMessages: [], participants: [], screenShareStreams: new Map() };

/** Common shape for both LiveKitModule (JS SDK) and NativeMediaModule (Rust IPC). */
type MediaModule = LiveKitModule | NativeMediaModule;

let lkModule: MediaModule | null = null;

/**
 * Platform detection: use the native Rust media path when the webview
 * lacks usable WebRTC support. Do not force Linux into the native path:
 * some Linux/Tauri environments expose working WebRTC + getDisplayMedia,
 * and that path is more stable than the Rust PipeWire backend on Hyprland.
 */
function shouldUseNativeMedia(): boolean {
  if (typeof window === 'undefined' || typeof navigator === 'undefined') return true;

  // On macOS and Windows, always use the LiveKit JS SDK (webview has full WebRTC support)
  const ua = navigator.userAgent || '';
  if (ua.includes('Macintosh') || ua.includes('Windows')) {
    return false;
  }

  const hasRtc = 'RTCPeerConnection' in window;
  const hasGetUserMedia =
    'mediaDevices' in navigator &&
    navigator.mediaDevices !== undefined &&
    typeof navigator.mediaDevices.getUserMedia === 'function';
  const hasGetDisplayMedia =
    'mediaDevices' in navigator &&
    navigator.mediaDevices !== undefined &&
    typeof navigator.mediaDevices.getDisplayMedia === 'function';

  return !(hasRtc && hasGetUserMedia && hasGetDisplayMedia);
}
let bufferedMediaToken: { sfuUrl: string; token: string } | null = null;
let desiredSubRoomIntent: string | null | undefined = undefined;
let lastReconnectMediaTime = 0;
/** Currently registered hotkey string (null when no hotkey is active). */
let registeredHotkey: string | null = null;
/** Volume before deafen, restored on undeafen. */
let preDeafenVolume: number | null = null;
let localStopShareSent = false;
let localSourceChanging = false;
let externalShareHelperActive = false;
const RECONNECT_MEDIA_COOLDOWN_MS = 3000;
/** Slow periodic media retry interval (ms) after fast retries are exhausted. */
const PERIODIC_MEDIA_RETRY_MS = 30_000;
let periodicMediaRetryTimer: ReturnType<typeof setInterval> | null = null;
const COLD_START_RETRY_MS = 30_000;
let coldStartRetryTimer: ReturnType<typeof setTimeout> | null = null;
const MAX_AUTH_REFRESH_RETRIES = 2;
let authRefreshRetries = 0;
let wasReconnecting = false;
let unsubTokenRefresh: (() => void) | null = null;
// TODO: proactiveReconnecting flag — wire into onStatusChange to suppress
// the "already reconnecting → connection lost" path during token-refresh reconnects.

/**
 * Cache of participantId → displayName. Survives participant_left so that
 * late-arriving events (share_stopped, etc.) can still resolve display names.
 */
const displayNameCache = new Map<string, string>();

/* ─── Share Picker Data (Tauri event handshake) ─────────────────── */

/** Data stored for the SharePicker window to retrieve via Tauri event. */
export interface PendingSharePickerData {
  enumResult: EnumerationResult;
  occupied: { videoOccupied: boolean; audioOccupied: boolean };
}

let pendingSharePickerData: PendingSharePickerData | null = null;
let unlistenSharePickerRequest: UnlistenFn | null = null;
let unlistenExternalShareStarted: UnlistenFn | null = null;
let unlistenExternalShareStopped: UnlistenFn | null = null;
let unlistenExternalShareError: UnlistenFn | null = null;
let unlistenViewerJoined: UnlistenFn | null = null;

export function setPendingSharePickerData(data: PendingSharePickerData | null): void {
  pendingSharePickerData = data;
}

export function getPendingSharePickerData(): PendingSharePickerData | null {
  return pendingSharePickerData;
}

function setupSharePickerListener(): void {
  if (unlistenSharePickerRequest) return; // already listening
  listen('share-picker:request-sources', () => {
    if (pendingSharePickerData) {
      emit('share-picker:sources', pendingSharePickerData);
    }
  }).then((unlisten) => {
    unlistenSharePickerRequest = unlisten;
  });
}

function teardownSharePickerListener(): void {
  if (unlistenSharePickerRequest) {
    unlistenSharePickerRequest();
    unlistenSharePickerRequest = null;
  }
  pendingSharePickerData = null;
}

function setupExternalShareHelperListeners(): void {
  if (unlistenExternalShareStarted || unlistenExternalShareStopped || unlistenExternalShareError) return;

  listen<{ sessionId: string }>('external-share-started', () => {
    externalShareHelperActive = true;
    localStopShareSent = false;
    const self = state.participants.find((p) => p.id === state.selfParticipantId);
    if (self) {
      self.isSharing = true;
    }
    if (client) {
      client.send({ type: 'start_share' });
    }
    notify();
  }).then((unlisten) => {
    unlistenExternalShareStarted = unlisten;
  });

  listen<{ sessionId: string }>('external-share-stopped', () => {
    externalShareHelperActive = false;
    state.shareQualityInfo = null;
    state.shareStats = null;
    const self = state.participants.find((p) => p.id === state.selfParticipantId);
    if (self) {
      self.isSharing = false;
    }
    if (!localStopShareSent && client) {
      client.send({ type: 'stop_share' });
    }
    localStopShareSent = false;
    notify();
  }).then((unlisten) => {
    unlistenExternalShareStopped = unlisten;
  });

  listen<{ sessionId: string; message: string }>('external-share-error', (event) => {
    externalShareHelperActive = false;
    state.shareQualityInfo = null;
    state.shareStats = null;
    const self = state.participants.find((p) => p.id === state.selfParticipantId);
    if (self) {
      self.isSharing = false;
    }
    appendEvent({
      id: makeEventId(),
      timestamp: timestamp(),
      type: 'system',
      message: `external share helper failed: ${event.payload.message}`,
    });
    localStopShareSent = false;
    notify();
  }).then((unlisten) => {
    unlistenExternalShareError = unlisten;
  });
}

function teardownExternalShareHelperListeners(): void {
  if (unlistenExternalShareStarted) {
    unlistenExternalShareStarted();
    unlistenExternalShareStarted = null;
  }
  if (unlistenExternalShareStopped) {
    unlistenExternalShareStopped();
    unlistenExternalShareStopped = null;
  }
  if (unlistenExternalShareError) {
    unlistenExternalShareError();
    unlistenExternalShareError = null;
  }
}

/* ─── Internal Helpers ──────────────────────────────────────────── */

function notify(): void {
  if (onChange) {
    onChange({
      ...state,
      participants: [...state.participants],
      events: [...state.events],
      chatMessages: [...state.chatMessages],
      subRooms: state.subRooms.map((room) => ({
        ...room,
        participantIds: [...room.participantIds],
      })),
      participantSubRoomById: { ...state.participantSubRoomById },
    });
  }
}

function appendEvent(event: RoomEvent): void {
  state.events.push(event);
  if (state.events.length > MAX_EVENTS) {
    state.events = state.events.slice(state.events.length - MAX_EVENTS);
  }
}

function deriveParticipantSubRoomById(subRooms: VoiceSubRoom[]): Record<string, string> {
  const assignments: Record<string, string> = {};
  for (const room of subRooms) {
    for (const participantId of room.participantIds) {
      assignments[participantId] = room.id;
    }
  }
  return assignments;
}

function syncDerivedSubRoomState(): void {
  state.participantSubRoomById = deriveParticipantSubRoomById(state.subRooms);
  if (!state.selfParticipantId) {
    state.joinedSubRoomId = null;
    return;
  }
  state.joinedSubRoomId = state.participantSubRoomById[state.selfParticipantId] ?? null;
}

function syncDesiredSubRoomPreference(): void {
  state.desiredSubRoomId = desiredSubRoomIntent === undefined
    ? state.joinedSubRoomId
    : desiredSubRoomIntent;
}

function playSubRoomMembershipSounds(
  previousParticipantSubRoomById: Record<string, string>,
  previousJoinedSubRoomId: string | null,
): void {
  const selfParticipantId = state.selfParticipantId;
  if (!selfParticipantId) return;

  const currentParticipantSubRoomById = state.participantSubRoomById;
  const currentJoinedSubRoomId = state.joinedSubRoomId;
  const participantIds = new Set([
    ...Object.keys(previousParticipantSubRoomById),
    ...Object.keys(currentParticipantSubRoomById),
  ]);

  for (const participantId of participantIds) {
    const previousRoomId = previousParticipantSubRoomById[participantId] ?? null;
    const currentRoomId = currentParticipantSubRoomById[participantId] ?? null;
    if (previousRoomId === currentRoomId) continue;

    if (participantId === selfParticipantId) {
      if (previousRoomId) {
        void playNotificationSound('leave');
      }
      if (currentRoomId) {
        void playNotificationSound('join');
      }
      continue;
    }

    if (previousJoinedSubRoomId && previousRoomId === previousJoinedSubRoomId) {
      void playNotificationSound('leave');
    }
    if (currentJoinedSubRoomId && currentRoomId === currentJoinedSubRoomId) {
      void playNotificationSound('join');
    }
  }
}

function reconcileDesiredSubRoomMembership(): void {
  syncDesiredSubRoomPreference();
  if (!client || client.status !== 'connected') return;

  const desiredSubRoomId = state.desiredSubRoomId;
  if (desiredSubRoomIntent === undefined || desiredSubRoomId === state.joinedSubRoomId) return;

  if (desiredSubRoomId === null) {
    if (state.joinedSubRoomId) {
      client.send({ type: 'leave_sub_room' });
    }
    return;
  }

  if (!state.subRooms.some((room) => room.id === desiredSubRoomId)) return;
  client.send({ type: 'join_sub_room', subRoomId: desiredSubRoomId });
}

function buildSyntheticSelfParticipant(): RoomParticipant | null {
  if (!state.selfParticipantId) return null;
  return {
    id: state.selfParticipantId,
    displayName: sessionDisplayName ?? displayNameCache.get(state.selfParticipantId) ?? 'You',
    color: sessionProfileColor ?? colorFor({ id: state.selfParticipantId }),
    role: state.selfIsHost ? 'host' : 'guest',
    isSpeaking: false,
    isMuted: false,
    isHostMuted: false,
    isDeafened: state.isDeafened,
    isSharing: false,
    rmsLevel: 0,
    volume: state.defaultVolume,
  };
}

function ensureSelfParticipant(reason: string): RoomParticipant | null {
  const existing = state.participants.find((p) => p.id === state.selfParticipantId);
  if (existing) return existing;

  const synthetic = buildSyntheticSelfParticipant();
  if (!synthetic) {
    console.warn(LOG, `ensureSelfParticipant: no selfParticipantId (${reason})`);
    return null;
  }

  displayNameCache.set(synthetic.id, synthetic.displayName);
  state.participants = [synthetic, ...state.participants].slice(0, MAX_PARTICIPANTS);
  console.warn(LOG, `restored missing self participant (${reason})`, { selfParticipantId: synthetic.id });
  appendEvent({
    id: makeEventId(),
    timestamp: timestamp(),
    type: 'system',
    message: 'local participant state was restored',
    participantId: synthetic.id,
  });
  return synthetic;
}

/** Debounced save of current volume state to the Tauri store. */
function saveVolumesDebounced(): void {
  if (!state.channelId) return;
  if (volumeSaveTimer) clearTimeout(volumeSaveTimer);
  volumeSaveTimer = setTimeout(() => {
    volumeSaveTimer = null;
    const participantVols: Record<string, number> = { ...(channelVolumePrefs?.participants ?? {}) };
    for (const p of state.participants) {
      if (p.userId && p.id !== state.selfParticipantId) {
        participantVols[p.userId] = p.volume;
      }
    }
    const prefs: ChannelVolumePrefs = { master: state.masterVolume, participants: participantVols };
    channelVolumePrefs = prefs;
    setChannelVolumes(state.channelId, prefs).catch((err) => {
      console.warn(LOG, 'failed to persist channel volumes:', err);
    });
  }, 300);
}

export function appendSystemEvent(message: string): void {
  appendEvent({
    id: `sys-${Date.now()}-${Math.random().toString(36).slice(2, 6)}`,
    timestamp: new Date().toISOString(),
    type: 'system',
    message,
  });
  notify();
}

/* ─── Media Lifecycle ────────────────────────────────────────────── */

function sendJoinVoiceRequest(): void {
  if (!client) return;
  const joinMsg: Record<string, unknown> = {
    type: 'join_voice',
    channelId: state.channelId,
    supportsSubRooms: true,
  };
  if (sessionDisplayName) joinMsg.displayName = sessionDisplayName;
  if (sessionProfileColor) joinMsg.profileColor = sessionProfileColor;
  client.send(joinMsg);
}

function stopColdStartRetry(): void {
  if (coldStartRetryTimer) {
    clearTimeout(coldStartRetryTimer);
    coldStartRetryTimer = null;
  }
}

function scheduleColdStartRetry(): void {
  stopColdStartRetry();
  coldStartRetryTimer = setTimeout(() => {
    coldStartRetryTimer = null;
    if (state.machineState !== 'server_starting' || !client) return;
    sendJoinVoiceRequest();
    scheduleColdStartRetry();
  }, COLD_START_RETRY_MS);
}

function connectMedia(sfuUrl: string, token: string): void {
  // Tear down previous instance if any
  if (lkModule) {
    lkModule.disconnect();
    lkModule = null;
  }

  state.mediaState = 'connecting';
  state.nativeMicBridgeActive = false;
  state.noiseSuppressionActive = false;
  notify();

  const callbacks: MediaCallbacks = {
    onMediaConnected: () => {
      state.mediaState = 'connected';
      state.mediaReconnectFailures = 0;
      stopPeriodicMediaRetry();
      // Re-apply mute state after media reconnection — if the user was muted,
      // ensure the mic stays muted (LiveKit enables mic by default on connect).
      const self = state.participants.find((p) => p.id === state.selfParticipantId);
      if (self && self.isMuted && lkModule) {
        lkModule.setMicEnabled(false);
      }
      // Apply persisted master volume to the media layer
      if (lkModule) {
        lkModule.setMasterVolume(state.masterVolume);
        // Apply effective participant volumes after reconnect.
        applyEffectiveParticipantVolumes();
      }
      // Register global mute hotkey on media connect success (R22.1)
      getMuteHotkey().then((hotkey) => {
        registerMuteHotkey(hotkey, toggleSelfMute)
          .then(() => { registeredHotkey = hotkey; })
          .catch((err) => {
            console.warn(LOG, 'hotkey registration failed:', err);
          });
      }).catch(() => { });
      notify();
    },
    onMediaFailed: (reason) => {
      state.mediaReconnectFailures += 1;
      state.mediaState = 'failed';
      state.mediaError = reason;
      appendEvent({ id: makeEventId(), timestamp: timestamp(), type: 'system', message: `media failed: ${reason}` });
      notify();
    },
    onMediaDisconnected: () => {
      state.mediaState = 'disconnected';
      appendEvent({ id: makeEventId(), timestamp: timestamp(), type: 'system', message: 'media disconnected — attempting reconnect' });
      notify();
    },
    onAudioLevels: (levels) => {
      for (const [identity, data] of levels) {
        const p = state.participants.find((pp) => pp.id === identity);
        if (p) {
          p.rmsLevel = data.rmsLevel;
          p.isSpeaking = updateSpeakingTracker(p.id, data.rmsLevel, p.isSpeaking, p.isMuted);
        } else {
          console.warn(LOG, `audio level for unknown identity: ${identity}`);
        }
      }
      notify();
    },
    onLocalAudioLevel: (level) => {
      updateSelfRms(level);
    },
    onActiveSpeakers: (speakerIdentities) => {
      for (const p of state.participants) {
        const isSpeaker = speakerIdentities.includes(p.id);
        if (isSpeaker && !p.isMuted) {
          // Boost the smoothed RMS in the tracker so the debounce logic
          // converges to speaking within 1–2 frames instead of fighting
          // with onAudioLevels updates that arrive with real (lower) RMS.
          const entry = speakingTracker.get(p.id);
          if (entry) {
            entry.smoothedRms = Math.max(entry.smoothedRms, RMS_START_THRESHOLD + 0.05);
          }
          p.isSpeaking = updateSpeakingTracker(p.id, RMS_START_THRESHOLD + 0.05, p.isSpeaking, p.isMuted);
          p.rmsLevel = Math.max(p.rmsLevel, RMS_START_THRESHOLD + 0.05);
        } else if (!isSpeaker && p.isSpeaking) {
          // Let the tracker decay naturally — feed a zero-level sample
          // so the EMA + debounce handles the off-transition smoothly.
          p.isSpeaking = updateSpeakingTracker(p.id, 0, p.isSpeaking, p.isMuted);
        }
      }
      notify();
    },
    onConnectionQuality: (stats) => {
      state.networkStats = stats;
      notify();
    },
    onScreenShareSubscribed: (identity, stream) => {
      state.screenShareStreams = new Map(state.screenShareStreams);
      state.screenShareStreams.set(identity, stream);
      // Mark participant as sharing (handles late joiners where TrackSubscribed
      // arrives before share_state signaling message)
      const p = state.participants.find((pp) => pp.id === identity);
      if (p && !p.isSharing) {
        p.isSharing = true;
      }
      notify();
    },
    onScreenShareUnsubscribed: (identity) => {
      state.screenShareStreams = new Map(state.screenShareStreams);
      state.screenShareStreams.delete(identity);
      // Clear receiver stats when no remote shares remain
      if (state.screenShareStreams.size === 0) {
        state.videoReceiveStats = null;
      }
      notify();
    },
    onLocalScreenShareEnded: () => {
      // Clear quality info and stats when share ends
      state.shareQualityInfo = null;
      state.shareStats = null;
      // Only send stop_share if we didn't already send it from stopShare()
      // and we're not in the middle of changing the share source
      if (!localStopShareSent && !localSourceChanging && client) {
        client.send({ type: 'stop_share' });
      }
      localStopShareSent = false;
    },
    onParticipantMuteChanged: (identity, isMuted) => {
      const p = state.participants.find((pp) => pp.id === identity);
      if (!p) return;

      if (identity === state.selfParticipantId) {
        // Local participant mute state changed externally (e.g. LiveKit reconnect,
        // OS-level mute). Sync UI state to match actual mic state.
        if (p.isMuted !== isMuted) {
          p.isMuted = isMuted;
          if (isMuted) {
            p.isSpeaking = false;
            p.rmsLevel = 0;
          }
          appendEvent({
            id: makeEventId(),
            timestamp: timestamp(),
            type: isMuted ? 'muted' : 'unmuted',
            message: isMuted ? 'mic muted (system)' : 'mic unmuted (system)',
            participantId: identity,
          });
          notify();
        }
      } else {
        p.isMuted = isMuted;
        if (isMuted) {
          p.isSpeaking = false;
          p.rmsLevel = 0;
        }
        notify();
      }
    },
    onSystemEvent: (message) => {
      appendEvent({ id: makeEventId(), timestamp: timestamp(), type: 'system', message });
      notify();
    },
    onShareQualityInfo: (info) => {
      state.shareQualityInfo = info;
      notify();
    },
    onShareStats: (stats) => {
      state.shareStats = stats;
      notify();
    },
    onVideoReceiveStats: (stats) => {
      state.videoReceiveStats = stats;
      notify();
    },
    onShareLeakSummary: (summary) => {
      state.latestClosedShareLeakSummary = summary;
      notify();
    },
    onNativeMicBridgeState: (active) => {
      state.nativeMicBridgeActive = active;
      notify();
    },
    onNoiseSuppressionState: (active) => {
      state.noiseSuppressionActive = active;
      notify();
    },
  };

  // Platform detection: native Rust path on Linux (WebKitGTK lacks WebRTC),
  // JS SDK path on macOS/Windows where the webview has full WebRTC support.
  const useNative = shouldUseNativeMedia();
  state.connectionMode = useNative ? 'native' : 'livekit';
  if (useNative) {
    console.log(LOG, 'using native Rust media path');
    lkModule = new NativeMediaModule(callbacks);
  } else {
    lkModule = new LiveKitModule(callbacks);
  }

  lkModule.connect(sfuUrl, token).catch((err) => {
    state.mediaState = 'failed';
    state.mediaError = err instanceof Error ? err.message : 'Connection failed';
    notify();
  });

  // Wire audio-devices.ts delegation for both media backends.
  setActiveLiveKitModule(lkModule);
}

/* ─── Signaling Dispatcher ──────────────────────────────────────── */

function dispatchMessage(raw: unknown): void {
  const msg = raw as Record<string, unknown>;
  const type = msg.type as string;

  // Diagnostic: log media_token arrival
  if (type === 'media_token' || type === 'joined') {
    console.log(LOG, `dispatchMessage: type=${type}, machineState=${state.machineState}, mediaState=${state.mediaState}, lkModule=${lkModule ? 'set' : 'null'}`);
  }

  switch (type) {
    case 'auth_success': {
      if (state.machineState === 'connecting' || state.machineState === 'reconnecting') {
        authRefreshRetries = 0; // reset on successful auth
        // On reconnect: clear peer-bound state before re-joining
        if (state.machineState === 'reconnecting') {
          wasReconnecting = true;
          state.participants = [];
          state.subRooms = [];
          state.participantSubRoomById = {};
          state.joinedSubRoomId = null;
          state.passthrough = null;
          state.selfParticipantId = null;
          state.error = null;
          state.rejectionReason = null;
          state.sharePermission = 'anyone'; // reset stale permission; joined message will set authoritative value
        }
        state.machineState = 'authenticated';
        state.machineState = 'joining';
        sendJoinVoiceRequest();
        notify();
      }
      break;
    }

    case 'auth_failed': {
      // If we were connecting/reconnecting, attempt a token refresh before giving up —
      // the token may have expired between reconnect attempts.
      if (
        (state.machineState === 'connecting' || state.machineState === 'reconnecting') &&
        authRefreshRetries < MAX_AUTH_REFRESH_RETRIES
      ) {
        authRefreshRetries += 1;
        console.warn(LOG, `auth_failed — refreshing token (attempt ${authRefreshRetries}/${MAX_AUTH_REFRESH_RETRIES})`);
        refreshTokens().then((result) => {
          if (result.status !== 'success' || !client) {
            console.warn(LOG, 'token refresh failed after auth_failed:', result.status);
            state.error = (msg.reason as string) || 'Authentication failed';
            state.machineState = 'idle';
            notify();
            return;
          }
          getServerUrl().then((serverUrl) => {
            if (!serverUrl || !client) return;
            client.reconnectWithNewToken(toWsUrl(serverUrl)).catch((err) => {
              console.error(LOG, 'reconnect after refresh failed:', err);
              state.error = 'Authentication failed';
              state.machineState = 'idle';
              notify();
            });
          });
        });
        break;
      }

      state.error = (msg.reason as string) || 'Authentication failed';
      if (client) {
        client.disconnect();
      }
      state.machineState = 'idle';
      notify();
      break;
    }

    case 'joined': {
      stopColdStartRetry();
      state.serverStartingEstimatedWaitSecs = null;
      state.selfParticipantId = msg.peerId as string;
      state.roomId = msg.roomId as string;
      syncDerivedSubRoomState();
      syncDesiredSubRoomPreference();
      state.sharePermission = (msg.sharePermission as string) === 'host_only' ? 'host_only' : 'anyone';
      const participants = (msg.participants as Array<Record<string, unknown>>) || [];
      state.participants = participants.slice(0, MAX_PARTICIPANTS).map((p) => {
        displayNameCache.set(p.participantId as string, p.displayName as string);
        const isSelf = p.participantId === state.selfParticipantId;
        const pUserId = p.userId as string | undefined;
        return {
          id: p.participantId as string,
          userId: pUserId,
          displayName: p.displayName as string,
          color: isSelf && sessionProfileColor ? sessionProfileColor : (p.profileColor as string | undefined) ?? colorFor({ userId: pUserId, id: p.participantId as string }),
          role: (isSelf && state.selfIsHost) ? 'host' as ParticipantRole : 'guest' as ParticipantRole,
          isSpeaking: false,
          isMuted: false,
          isHostMuted: false,
          isDeafened: false,
          isSharing: false,
          rmsLevel: 0,
          volume: resolvePersistedVolume(pUserId, state.defaultVolume),
        };
      });
      ensureSelfParticipant('joined');
      state.machineState = 'active';

      // Reconcile: if TrackSubscribed already fired before the participant
      // list was built, the stream is in screenShareStreams but the
      // participant was just created with isSharing: false. Fix it.
      for (const p of state.participants) {
        if (!p.isSharing && state.screenShareStreams.has(p.id)) {
          p.isSharing = true;
        }
      }

      // Log reconnection success in the room event panel
      if (wasReconnecting) {
        wasReconnecting = false;
        appendEvent({ id: makeEventId(), timestamp: timestamp(), type: 'system', message: 'reconnected — back online' });
      }

      // Flush buffered media token if present
      if (bufferedMediaToken) {
        const { sfuUrl, token } = bufferedMediaToken;
        bufferedMediaToken = null;
        connectMedia(sfuUrl, token);
      }

      // Request chat history after successful join
      if (client) {
        const since = computeSinceCursor(state.chatMessages);
        const historyReq: Record<string, unknown> = { type: 'chat_history_request' };
        if (since) historyReq.since = since;
        client.send(historyReq);
      }
      reconcileDesiredSubRoomMembership();

      notify();
      break;
    }

    case 'sfu_cold_starting': {
      const wait = typeof msg.estimatedWaitSecs === 'number' ? msg.estimatedWaitSecs : 120;
      if (state.machineState === 'joining' || state.machineState === 'authenticated') {
        state.machineState = 'server_starting';
        state.serverStartingEstimatedWaitSecs = wait;
        scheduleColdStartRetry();
        notify();
      } else if (state.machineState === 'server_starting') {
        state.serverStartingEstimatedWaitSecs = wait;
        notify();
      }
      break;
    }

    case 'join_rejected': {
      const rawReason = (msg.reason as string) || 'unknown';
      console.warn(LOG, `join_rejected reason=${rawReason} channelId=${state.channelId}`);
      if (state.machineState === 'server_starting') {
        stopColdStartRetry();
        state.machineState = 'idle';
        state.serverStartingEstimatedWaitSecs = null;
        state.rejectionReason = (msg.reason as string) || 'Server failed to start';
        notify();
        break;
      }
      // Map wire reasons to user-friendly messages
      const friendlyMessages: Record<string, string> = {
        not_authorized: 'Unable to join voice. You may not be a member, or there was a server-side issue. Try again.',
        room_full: 'Room is full (max 6 participants).',
        invite_required: 'An invite code is required to join.',
        invite_exhausted: 'The invite code has been fully used.',
        invite_expired: 'The invite code has expired.',
      };
      state.rejectionReason = friendlyMessages[rawReason] || `Join rejected: ${rawReason}`;
      if (client) {
        client.disconnect();
      }
      client = null;
      state.machineState = 'idle';
      notify();
      break;
    }

    case 'participant_joined': {
      if (state.participants.length >= MAX_PARTICIPANTS) return;
      const pjId = msg.participantId as string;
      const pjName = msg.displayName as string;
      const pjUserId = msg.userId as string | undefined;
      displayNameCache.set(pjId, pjName);
      const newParticipant: RoomParticipant = {
        id: pjId,
        userId: pjUserId,
        displayName: pjName,
        color: (msg.profileColor as string | undefined) ?? colorFor({ userId: pjUserId, id: pjId }),
        role: 'guest',
        isSpeaking: false,
        isMuted: false,
        isHostMuted: false,
        isDeafened: false,
        isSharing: false,
        rmsLevel: 0,
        volume: resolvePersistedVolume(pjUserId, state.defaultVolume),
      };
      state.participants = [...state.participants, newParticipant];
      if (lkModule && state.mediaState === 'connected' && pjId !== state.selfParticipantId) {
        applyEffectiveParticipantVolume(newParticipant);
      }
      appendEvent({
        id: makeEventId(),
        timestamp: timestamp(),
        type: 'join',
        message: `${msg.displayName} joined`,
        participantId: msg.participantId as string,
      });
      notify();
      break;
    }

    case 'participant_left': {
      const previousParticipantSubRoomById = { ...state.participantSubRoomById };
      const previousJoinedSubRoomId = state.joinedSubRoomId;
      const leftId = msg.participantId as string;
      const leftP = state.participants.find((p) => p.id === leftId);
      state.participants = state.participants.filter((p) => p.id !== leftId);
      if (state.participantSubRoomById[leftId]) {
        const leftRoomId = state.participantSubRoomById[leftId];
        state.subRooms = state.subRooms.map((room) => (
          room.id === leftRoomId
            ? { ...room, participantIds: room.participantIds.filter((id) => id !== leftId) }
            : room
        ));
        syncDerivedSubRoomState();
        playSubRoomMembershipSounds(previousParticipantSubRoomById, previousJoinedSubRoomId);
        applyEffectiveParticipantVolumes();
      }
      speakingTracker.delete(leftId);
      const leftName = leftP?.displayName ?? displayNameCache.get(leftId) ?? leftId;
      appendEvent({
        id: makeEventId(),
        timestamp: timestamp(),
        type: 'leave',
        message: `${leftName} left`,
        participantId: leftId,
      });
      notify();
      break;
    }

    case 'sub_room_state': {
      const previousParticipantSubRoomById = { ...state.participantSubRoomById };
      const previousJoinedSubRoomId = state.joinedSubRoomId;
      const rooms = ((msg.rooms as Array<Record<string, unknown>>) || []).map((room) => ({
        id: room.subRoomId as string,
        roomNumber: room.roomNumber as number,
        isDefault: Boolean(room.isDefault),
        participantIds: Array.isArray(room.participantIds)
          ? (room.participantIds as string[]).slice()
          : [],
        deleteAtMs: typeof room.deleteAtMs === 'number' ? room.deleteAtMs : null,
      })).sort((a, b) => a.roomNumber - b.roomNumber);
      state.subRooms = rooms;
      const passthrough = msg.passthrough as Record<string, unknown> | null | undefined;
      state.passthrough = passthrough
        && typeof passthrough.sourceSubRoomId === 'string'
        && typeof passthrough.targetSubRoomId === 'string'
        && typeof passthrough.label === 'string'
        ? {
            sourceSubRoomId: passthrough.sourceSubRoomId,
            targetSubRoomId: passthrough.targetSubRoomId,
            label: passthrough.label,
          }
        : null;
      syncDerivedSubRoomState();
      playSubRoomMembershipSounds(previousParticipantSubRoomById, previousJoinedSubRoomId);
      applyEffectiveParticipantVolumes();
      reconcileDesiredSubRoomMembership();
      notify();
      break;
    }

    case 'sub_room_created': {
      const room = msg.room as Record<string, unknown> | undefined;
      if (!room) break;
      const createdRoom: VoiceSubRoom = {
        id: room.subRoomId as string,
        roomNumber: room.roomNumber as number,
        isDefault: Boolean(room.isDefault),
        participantIds: Array.isArray(room.participantIds)
          ? (room.participantIds as string[]).slice()
          : [],
        deleteAtMs: typeof room.deleteAtMs === 'number' ? room.deleteAtMs : null,
      };
      state.subRooms = [...state.subRooms.filter((existing) => existing.id !== createdRoom.id), createdRoom]
        .sort((a, b) => a.roomNumber - b.roomNumber);
      syncDerivedSubRoomState();
      applyEffectiveParticipantVolumes();
      reconcileDesiredSubRoomMembership();
      notify();
      break;
    }

    case 'sub_room_joined': {
      const previousParticipantSubRoomById = { ...state.participantSubRoomById };
      const previousJoinedSubRoomId = state.joinedSubRoomId;
      const participantId = msg.participantId as string;
      const subRoomId = msg.subRoomId as string;
      const source = ((msg.source as string) === 'legacy_room_one' ? 'legacy_room_one' : 'explicit') as SubRoomMembershipSource;
      state.subRooms = state.subRooms.map((room) => {
        if (room.id === subRoomId) {
          return room.participantIds.includes(participantId)
            ? { ...room, deleteAtMs: null }
            : { ...room, participantIds: [...room.participantIds, participantId], deleteAtMs: null };
        }
        return room.participantIds.includes(participantId)
          ? { ...room, participantIds: room.participantIds.filter((id) => id !== participantId) }
          : room;
      });
      syncDerivedSubRoomState();
      void source;
      playSubRoomMembershipSounds(previousParticipantSubRoomById, previousJoinedSubRoomId);
      applyEffectiveParticipantVolumes();
      reconcileDesiredSubRoomMembership();
      notify();
      break;
    }

    case 'sub_room_left': {
      const previousParticipantSubRoomById = { ...state.participantSubRoomById };
      const previousJoinedSubRoomId = state.joinedSubRoomId;
      const participantId = msg.participantId as string;
      const subRoomId = msg.subRoomId as string;
      state.subRooms = state.subRooms.map((room) => (
        room.id === subRoomId
          ? { ...room, participantIds: room.participantIds.filter((id) => id !== participantId) }
          : room
      ));
      syncDerivedSubRoomState();
      playSubRoomMembershipSounds(previousParticipantSubRoomById, previousJoinedSubRoomId);
      applyEffectiveParticipantVolumes();
      reconcileDesiredSubRoomMembership();
      notify();
      break;
    }

    case 'sub_room_deleted': {
      const subRoomId = msg.subRoomId as string;
      state.subRooms = state.subRooms.filter((room) => room.id !== subRoomId);
      syncDerivedSubRoomState();
      if (desiredSubRoomIntent === subRoomId) {
        desiredSubRoomIntent = undefined;
      }
      applyEffectiveParticipantVolumes();
      reconcileDesiredSubRoomMembership();
      notify();
      break;
    }

    case 'room_state': {
      // room_state contains only pre-join participants (excludes the joiner).
      // Merge with existing list to preserve self and any participants already
      // added via participant_joined that arrived before this snapshot.
      const rsParticipants = (msg.participants as Array<Record<string, unknown>>) || [];
      const incoming = rsParticipants.slice(0, MAX_PARTICIPANTS).map((p) => {
        displayNameCache.set(p.participantId as string, p.displayName as string);
        const isSelf = p.participantId === state.selfParticipantId;
        const rsUserId = p.userId as string | undefined;
        return {
          id: p.participantId as string,
          userId: rsUserId,
          displayName: p.displayName as string,
          color: isSelf && sessionProfileColor ? sessionProfileColor : (p.profileColor as string | undefined) ?? colorFor({ userId: rsUserId, id: p.participantId as string }),
          role: (isSelf && state.selfIsHost) ? 'host' as ParticipantRole : 'guest' as ParticipantRole,
          isSpeaking: false,
          isMuted: false,
          isHostMuted: false,
          isDeafened: false,
          isSharing: false,
          rmsLevel: 0,
          volume: resolvePersistedVolume(rsUserId, state.defaultVolume),
        };
      });
      const incomingIds = new Set(incoming.map((p) => p.id));
      // Keep participants already in state that aren't in the snapshot (self, late arrivals)
      const preserved = state.participants.filter((p) => !incomingIds.has(p.id));
      // Merge: preserve per-participant volume from old list across reconnects
      const merged = mergeParticipantsWithVolume(state.participants, [...incoming, ...preserved]);
      state.participants = merged.slice(0, MAX_PARTICIPANTS);
      ensureSelfParticipant('room_state');
      applyEffectiveParticipantVolumes();
      // No events appended — snapshot reconciliation

      // Reconcile: if TrackSubscribed already fired before room_state
      // rebuilt the participant list, mark them as sharing.
      for (const p of state.participants) {
        if (!p.isSharing && state.screenShareStreams.has(p.id)) {
          p.isSharing = true;
        }
      }

      // Request chat history on reconnect (room_state path)
      if (client) {
        const since = computeSinceCursor(state.chatMessages);
        const historyReq: Record<string, unknown> = { type: 'chat_history_request' };
        if (since) historyReq.since = since;
        client.send(historyReq);
      }

      notify();
      break;
    }

    case 'participant_kicked': {
      const kickedId = msg.participantId as string;
      const kickedP = state.participants.find((p) => p.id === kickedId);
      state.participants = state.participants.filter((p) => p.id !== kickedId);
      const kickedName = kickedP?.displayName ?? displayNameCache.get(kickedId) ?? kickedId;
      appendEvent({
        id: makeEventId(),
        timestamp: timestamp(),
        type: 'kicked',
        message: `${kickedName} was kicked`,
        participantId: kickedId,
      });
      if (kickedId === state.selfParticipantId) {
        state.error = 'You were kicked';
        // Unregister global mute hotkey on kick (R22.7)
        if (registeredHotkey) {
          unregisterMuteHotkey(registeredHotkey).catch(() => { });
          registeredHotkey = null;
        }
        if (client) {
          client.disconnect();
        }
        client = null;
        state.machineState = 'idle';
      }
      notify();
      break;
    }

    case 'session_displaced': {
      // Another client connected with the same account — this session was evicted.
      // Do NOT reconnect (prevents infinite reconnect loop).
      console.warn(LOG, 'session displaced by another client');
      state.error = 'Session taken over by another device';
      appendEvent({
        id: makeEventId(),
        timestamp: timestamp(),
        type: 'system',
        message: 'disconnected — session taken over by another device',
      });
      // Unregister global mute hotkey
      if (registeredHotkey) {
        unregisterMuteHotkey(registeredHotkey).catch(() => { });
        registeredHotkey = null;
      }
      // Intentional disconnect — suppress reconnect
      if (client) {
        client.disconnect();
      }
      client = null;
      state.machineState = 'idle';
      stopColdStartRetry();
      stopPeriodicMediaRetry();
      notify();
      break;
    }

    case 'participant_muted': {
      const mutedId = msg.participantId as string;
      const p = state.participants.find((pp) => pp.id === mutedId);
      if (p) {
        p.isMuted = true;
        p.isHostMuted = true;
      }
      if (mutedId === state.selfParticipantId) {
        if (p) {
          p.isSpeaking = false;
          p.rmsLevel = 0;
        }
        lkModule?.setMicEnabled(false);
        appendEvent({
          id: makeEventId(),
          timestamp: timestamp(),
          type: 'host-mute',
          message: 'you were muted by host',
          participantId: mutedId,
        });
      } else if (p) {
        appendEvent({
          id: makeEventId(),
          timestamp: timestamp(),
          type: 'host-mute',
          message: `${p.displayName} was muted by host`,
          participantId: mutedId,
        });
      }
      notify();
      break;
    }

    case 'participant_unmuted': {
      const unmutedId = msg.participantId as string;
      const up = state.participants.find((pp) => pp.id === unmutedId);
      if (up) {
        up.isHostMuted = false;
        // Don't auto-unmute the mic — just release the lock so the participant can self-unmute
      }
      if (unmutedId === state.selfParticipantId) {
        appendEvent({
          id: makeEventId(),
          timestamp: timestamp(),
          type: 'host-unmute',
          message: 'host released your mute',
          participantId: unmutedId,
        });
      } else if (up) {
        appendEvent({
          id: makeEventId(),
          timestamp: timestamp(),
          type: 'host-unmute',
          message: `${up.displayName} was unmuted by host`,
          participantId: unmutedId,
        });
      }
      notify();
      break;
    }

    case 'participant_deafened': {
      const deafId = msg.participantId as string;
      const dp = state.participants.find((pp) => pp.id === deafId);
      if (dp) {
        dp.isDeafened = true;
      }
      if (deafId === state.selfParticipantId) {
        // Server echo — local state already set by toggleSelfDeafen
      } else if (dp) {
        appendEvent({
          id: makeEventId(),
          timestamp: timestamp(),
          type: 'deafen',
          message: `${dp.displayName} deafened`,
          participantId: deafId,
        });
      }
      notify();
      break;
    }

    case 'participant_undeafened': {
      const undeafId = msg.participantId as string;
      const udp = state.participants.find((pp) => pp.id === undeafId);
      if (udp) {
        udp.isDeafened = false;
      }
      if (undeafId === state.selfParticipantId) {
        // Server echo — local state already set by toggleSelfDeafen
      } else if (udp) {
        appendEvent({
          id: makeEventId(),
          timestamp: timestamp(),
          type: 'undeafen',
          message: `${udp.displayName} undeafened`,
          participantId: undeafId,
        });
      }
      notify();
      break;
    }

    case 'participant_color_updated': {
      const coloredId = msg.participantId as string;
      const cp = state.participants.find((pp) => pp.id === coloredId);
      if (cp) {
        cp.color = msg.profileColor as string;
      }
      notify();
      break;
    }

    case 'share_started': {
      const shareStartId = msg.participantId as string;
      const shareStartName = msg.displayName as string | undefined;
      const sp = state.participants.find((pp) => pp.id === shareStartId);
      if (sp) {
        sp.isSharing = true;
        sp.shareType = (msg.shareType as string) || undefined;
      }
      // Cache the display name from the server payload
      if (shareStartName) {
        displayNameCache.set(shareStartId, shareStartName);
      }
      const resolvedStartName = sp?.displayName ?? shareStartName ?? displayNameCache.get(shareStartId) ?? shareStartId;
      // Skip event for self — startCustomShare already emitted a local event
      if (shareStartId !== state.selfParticipantId) {
        appendEvent({
          id: makeEventId(),
          timestamp: timestamp(),
          type: 'share-start',
          message: `${resolvedStartName} started screen share`,
          participantId: shareStartId,
        });
      }
      void playNotificationSound('share-start');
      notify();
      break;
    }

    case 'share_stopped': {
      const shareStopId = msg.participantId as string;
      const shareStopName = msg.displayName as string | undefined;
      const ssp = state.participants.find((pp) => pp.id === shareStopId);
      if (ssp) {
        ssp.isSharing = false;
        ssp.shareType = undefined;
      }
      // Cache the display name from the server payload
      if (shareStopName) {
        displayNameCache.set(shareStopId, shareStopName);
      }
      const resolvedStopName = ssp?.displayName ?? shareStopName ?? displayNameCache.get(shareStopId) ?? shareStopId;
      // Skip event for self — stopCustomShare already emitted a local event
      if (shareStopId !== state.selfParticipantId) {
        appendEvent({
          id: makeEventId(),
          timestamp: timestamp(),
          type: 'share-stop',
          message: `${resolvedStopName} stopped screen share`,
          participantId: shareStopId,
        });
      } else {
        void playNotificationSound('share-stop');
      }
      notify();
      break;
    }

    case 'share_state': {
      // share_state is an authoritative snapshot — reconcile all participants
      const shareIds = new Set((msg.participantIds as string[]) || []);
      for (const p of state.participants) {
        const shouldBeSharing = shareIds.has(p.id);
        if (p.isSharing && !shouldBeSharing) {
          // Server says they're not sharing — clear stale flag + stream
          p.isSharing = false;
          p.shareType = undefined;
          if (state.screenShareStreams.has(p.id)) {
            state.screenShareStreams = new Map(state.screenShareStreams);
            state.screenShareStreams.delete(p.id);
          }
        } else if (!p.isSharing && shouldBeSharing) {
          p.isSharing = true;
        }
      }
      notify();
      break;
    }

    case 'share_permission_changed': {
      const newPerm = (msg.permission as string) === 'host_only' ? 'host_only' : 'anyone';
      const oldPerm = state.sharePermission;
      state.sharePermission = newPerm;
      if (newPerm !== oldPerm) {
        const label = newPerm === 'host_only' ? 'host only' : 'anyone';
        appendEvent({
          id: makeEventId(),
          timestamp: timestamp(),
          type: 'share-permission',
          message: `share permission changed to ${label}`,
        });
      }
      notify();
      break;
    }

    case 'error': {
      const errorMessage = (msg.message as string) || 'Unknown error';
      appendEvent({
        id: makeEventId(),
        timestamp: timestamp(),
        type: 'system',
        message: errorMessage,
      });
      state.lastChatError = errorMessage;
      notify();
      break;
    }

    case 'peer_left': {
      const peerId = (msg.participantId as string) || (msg.peerId as string);
      if (peerId) {
        const peerP = state.participants.find((p) => p.id === peerId);
        state.participants = state.participants.filter((p) => p.id !== peerId);
        const peerName = peerP?.displayName ?? displayNameCache.get(peerId) ?? peerId;
        appendEvent({
          id: makeEventId(),
          timestamp: timestamp(),
          type: 'leave',
          message: `${peerName} left`,
          participantId: peerId,
        });
      } else {
        appendEvent({
          id: makeEventId(),
          timestamp: timestamp(),
          type: 'system',
          message: 'peer disconnected',
        });
      }
      notify();
      break;
    }

    case 'media_token': {
      const token = msg.token as string;
      const sfuUrl = msg.sfuUrl as string;

      if (!token || !sfuUrl) {
        appendEvent({
          id: makeEventId(), timestamp: timestamp(),
          type: 'system', message: 'media_token: empty token or sfuUrl',
        });
        notify();
        break;
      }

      if (state.machineState !== 'active') {
        bufferedMediaToken = { sfuUrl, token };
        break;
      }

      // Media already connected — this is a proactive refresh from the backend.
      // LiveKit SDK handles its own reconnection internally; tearing down and
      // rebuilding the Room would cause a visible audio/screenshare hiccup.
      if (state.mediaState === 'connected' || state.mediaState === 'connecting') {
        console.log(LOG, `media_token received while media ${state.mediaState} — ignoring (no reconnect needed)`);
        break;
      }

      // When media is in failed state, check if auto-reconnect retries remain
      if (state.mediaState === 'failed') {
        getReconnectConfig().then((config) => {
          if (state.mediaReconnectFailures < config.maxRetries) {
            connectMedia(sfuUrl, token);
          } else {
            startPeriodicMediaRetry();
            appendEvent({
              id: makeEventId(), timestamp: timestamp(),
              type: 'system', message: 'media_token ignored — retries exhausted, periodic retry active',
            });
            notify();
          }
        });
        break;
      }

      connectMedia(sfuUrl, token);
      break;
    }

    case 'chat_message': {
      const participant = state.participants.find(
        (p) => p.id === (msg.participantId as string)
      );
      const chatMsg: ChatMessage = {
        id: makeEventId(),
        messageId: (msg.messageId as string) || undefined,
        timestamp: msg.timestamp as string,
        participantId: msg.participantId as string,
        displayName: msg.displayName as string,
        color: participant?.color ?? '',
        text: msg.text as string,
      };
      state.chatMessages = [...state.chatMessages, chatMsg];
      if (state.chatMessages.length > MAX_CHAT_MESSAGES) {
        state.chatMessages = state.chatMessages.slice(-MAX_CHAT_MESSAGES);
      }
      notify();
      break;
    }

    case 'chat_history_response': {
      const messages = (msg.messages as Array<Record<string, unknown>>) || [];
      const historyPayload = messages.map((m) => ({
        messageId: m.messageId as string,
        participantId: m.participantId as string,
        displayName: m.displayName as string,
        text: m.text as string,
        timestamp: m.timestamp as string,
      }));
      state.chatMessages = mergeHistoryMessages(historyPayload, state.chatMessages);
      state.historyLoaded = true;
      notify();
      break;
    }

    case 'viewer_joined': {
      void playNotificationSound('viewer-joined');
      break;
    }

    default: {
      console.warn(LOG, 'unknown message type:', type);
      break;
    }
  }
}

/* ─── Session Lifecycle ─────────────────────────────────────────── */

export function initSession(
  channelId: string,
  channelName: string,
  channelRoleArg: ChannelRole,
  onChangeArg: (state: VoiceRoomState) => void,
): void {
  // Double-mount guard: tear down any existing session first
  if (client) {
    leaveRoom();
  }

  // Store callback and channel role
  onChange = onChangeArg;
  channelRole = channelRoleArg;

  // Derive host status from channel role
  const selfIsHost = channelRoleArg === 'owner' || channelRoleArg === 'admin';

  // Reset state to defaults, then set session-specific fields
  // Fresh arrays to avoid mutating DEFAULT_STATE via push()
  state = {
    ...DEFAULT_STATE,
    events: [],
    chatMessages: [],
    participants: [],
    channelId,
    channelName,
    selfIsHost,
    machineState: 'connecting',
    screenShareStreams: new Map(),
  };
  desiredSubRoomIntent = undefined;

  // Push initial state to the component
  notify();

  // Set up share picker event listener for Tauri event handshake
  setupSharePickerListener();
  setupExternalShareHelperListeners();

  // Forward viewer-subscribed events from WatchAllPage/ScreenSharePage to the signaling server
  if (!unlistenViewerJoined) {
    listen<{ targetId: string }>('viewer-subscribed', ({ payload }) => {
      if (client) {
        client.send({ type: 'viewer_subscribed', targetId: payload.targetId });
      }
    }).then((unlisten) => {
      unlistenViewerJoined = unlisten;
    });
  }

  // Create signaling client and wire handlers
  client = new SignalingClient();
  const thisClient = client; // capture for async guard
  unsubscribe = client.onMessage(dispatchMessage);

  // Detect disconnection during active session for reconnection flow
  client.onStatusChange((status) => {
    if (status === 'disconnected') {
      stopColdStartRetry();
      if (state.machineState === 'active') {
        // Connection dropped during active session — start reconnecting.
        // Do NOT tear down LiveKit media — it connects directly to the SFU,
        // independent of the signaling WS. Tearing it down causes an
        // unnecessary audio/screenshare interruption during WS reconnects
        // (e.g. CloudFront idle timeout drops the WS every ~10 min).
        state.machineState = 'reconnecting';
        appendEvent({ id: makeEventId(), timestamp: timestamp(), type: 'system', message: 'signaling connection lost — reconnecting' });
        notify();
      } else if (state.machineState === 'reconnecting') {
        // Already reconnecting and got another disconnect.
        // Only give up if the WS has exhausted all reconnect attempts
        // (both fast retries and periodic retry).
        if (client && client.status === 'disconnected' && !client['reconnectTimer'] && !client['periodicRetryTimer']) {
          // Unregister hotkey when giving up on reconnection (R22.7)
          if (registeredHotkey) {
            unregisterMuteHotkey(registeredHotkey).catch(() => { });
            registeredHotkey = null;
          }
          appendEvent({ id: makeEventId(), timestamp: timestamp(), type: 'system', message: 'signaling reconnect failed — session lost' });
          state.error = 'Connection lost';
          state.machineState = 'idle';
          notify();
        }
      }
    }
  });

  // Subscribe to proactive token refreshes from AuthGate.
  // The WS connection was already authenticated at connect time and the backend
  // does NOT re-validate the token on active connections — so we do NOT need to
  // tear down and reconnect the WS when the access token is refreshed.
  // The refresh only matters for REST API calls (apiFetch).
  // We keep the subscription to log it for diagnostics.
  unsubTokenRefresh = onTokensRefreshed(() => {
    if (!client || state.machineState !== 'active') return;
    console.log(LOG, 'token refreshed — WS connection stays active (no reconnect needed)');
  });

  // Connect with auth
  getServerUrl().then((serverUrl) => {
    if (!serverUrl || client !== thisClient) return;
    const wsUrl = toWsUrl(serverUrl);
    // Load display name, default volume, profile color, and persisted channel volumes before connecting
    Promise.all([getDisplayName(), getDefaultVolume(), getProfileColor(), getChannelVolumes(channelId)]).then(([name, vol, profileColor, savedVols]) => {
      if (client !== thisClient) return;
      sessionDisplayName = name;
      sessionProfileColor = profileColor;
      channelVolumePrefs = savedVols;
      state.defaultVolume = vol;
      state.masterVolume = savedVols?.master ?? vol;
      client.connectWithAuth(wsUrl).catch((err) => {
        console.error(LOG, 'connect failed:', err);
        if (client !== thisClient) return;
        state.error = 'Connection failed';
        state.machineState = 'idle';
        notify();
      });
    });
  });
}

export function leaveRoom(): void {
  // Clean up custom share captures (best-effort, fire-and-forget)
  if (state.activeVideoShare || state.activeAudioShare) {
    if (lkModule && lkModule instanceof LiveKitModule) {
      lkModule.stopWasapiAudioBridge().catch(() => { });
    }
    if (state.activeVideoShare) {
      // Stop native capture bridge (Windows LiveKit path)
      if (lkModule && lkModule instanceof LiveKitModule) {
        lkModule.stopNativeCapture().catch(() => { });
      }
      invoke('screen_share_stop').catch(() => { });
      if (state.activeVideoShare.withAudio) invoke('audio_share_stop').catch(() => { });
    }
    if (state.activeAudioShare) invoke('audio_share_stop').catch(() => { });
    // Close ShareIndicator window
    WebviewWindow.getByLabel('share-indicator')
      .then((win) => {
        if (win) win.close().catch(() => { });
      })
      .catch(() => { });
    // Send stop_share before leave for custom shares
    if (client && client.status === 'connected') {
      client.send({ type: 'stop_share' });
    }
    // Clear state synchronously
    state.activeVideoShare = null;
    state.activeAudioShare = null;
  }

  // Clean up fallback (getDisplayMedia) share if active
  const selfP = state.participants.find((p) => p.id === state.selfParticipantId);
  if (selfP?.isSharing && !state.activeVideoShare && !state.activeAudioShare) {
    invoke('external_share_stop').catch(() => { });
    externalShareHelperActive = false;
    // Fallback share is active — send stop_share signaling before leave
    if (client && client.status === 'connected') {
      client.send({ type: 'stop_share' });
    }
    selfP.isSharing = false;
    state.shareQualityInfo = null;
    state.shareStats = null;
  }

  // Notify all child windows (screen share pop-outs) that the session is ending.
  // This must fire before we tear down media/WS so child windows can self-close
  // even if the main window is being destroyed.
  emit('voice-session:ended', {}).catch(() => { });

  // Unregister global mute hotkey (R22.5, R22.7)
  if (registeredHotkey) {
    unregisterMuteHotkey(registeredHotkey).catch(() => { });
    registeredHotkey = null;
  }

  // Tear down LiveKit media
  if (lkModule) {
    lkModule.disconnect();
    lkModule = null;
  }
  bufferedMediaToken = null;
  externalShareHelperActive = false;
  stopColdStartRetry();
  stopPeriodicMediaRetry();
  setActiveLiveKitModule(null);

  // Send Leave if still connected
  if (client && client.status === 'connected') {
    client.send({ type: 'leave' });
  }

  // Remove message handler
  if (unsubscribe) {
    unsubscribe();
  }

  // Unsubscribe from token refresh notifications
  if (unsubTokenRefresh) {
    unsubTokenRefresh();
    unsubTokenRefresh = null;
  }
  authRefreshRetries = 0;
  wasReconnecting = false;

  // Disconnect the client
  if (client) {
    client.disconnect();
  }

  // Clear client references
  client = null;
  unsubscribe = null;

  // Reset state to defaults (fresh arrays to avoid mutating DEFAULT_STATE)
  state = { ...DEFAULT_STATE, events: [], chatMessages: [], participants: [], screenShareStreams: new Map() };
  desiredSubRoomIntent = undefined;

  // Reset reconnect cooldown timer
  lastReconnectMediaTime = 0;

  // Clear display name cache and speaking tracker
  displayNameCache.clear();
  speakingTracker.clear();
  preDeafenVolume = null;

  // Tear down share picker event listener
  teardownSharePickerListener();
  teardownExternalShareHelperListeners();
  if (unlistenViewerJoined) {
    unlistenViewerJoined();
    unlistenViewerJoined = null;
  }

  // Notify before clearing callback (so component gets the idle state)
  notify();

  // Clear session-scoped references
  onChange = null;
  channelRole = null;
  sessionDisplayName = null;
  channelVolumePrefs = null;
  if (volumeSaveTimer) {
    clearTimeout(volumeSaveTimer);
    volumeSaveTimer = null;
  }
}

function stopPeriodicMediaRetry(): void {
  if (periodicMediaRetryTimer) {
    clearInterval(periodicMediaRetryTimer);
    periodicMediaRetryTimer = null;
  }
}

function startPeriodicMediaRetry(): void {
  stopPeriodicMediaRetry();
  console.log(LOG, 'starting periodic media retry (30s interval)');
  periodicMediaRetryTimer = setInterval(() => {
    // Only retry if still in a failed/disconnected state and session is alive
    if (!client || state.machineState === 'idle') {
      stopPeriodicMediaRetry();
      return;
    }
    if (state.mediaState === 'connected' || state.mediaState === 'connecting') {
      stopPeriodicMediaRetry();
      return;
    }
    console.log(LOG, 'periodic media retry attempt');
    state.mediaReconnectFailures = 0;
    lastReconnectMediaTime = 0;
    reconnectMedia();
  }, PERIODIC_MEDIA_RETRY_MS);
}

export async function reconnectMedia(): Promise<void> {
  const now = Date.now();
  if (now - lastReconnectMediaTime < RECONNECT_MEDIA_COOLDOWN_MS) {
    appendEvent({
      id: makeEventId(), timestamp: timestamp(),
      type: 'system', message: 'reconnect cooldown active',
    });
    notify();
    return;
  }
  lastReconnectMediaTime = now;

  // Check retry budget (async — load config then proceed)
  const config = await getReconnectConfig();
  if (state.mediaReconnectFailures >= config.maxRetries) {
    state.mediaState = 'failed' as MediaState;
    appendEvent({
      id: makeEventId(), timestamp: timestamp(),
      type: 'system', message: `media reconnect retries exhausted (${config.maxRetries}) — periodic retry active`,
    });
    startPeriodicMediaRetry();
    notify();
    return;
  }

  // Tear down current media
  if (lkModule) {
    lkModule.disconnect();
    lkModule = null;
  }
  state.mediaState = 'disconnected';
  state.mediaError = null;
  notify();

  // Re-send JoinVoice to request new media_token
  sendJoinVoiceRequest();
}

export function resetMediaReconnectFailures(): void {
  state.mediaReconnectFailures = 0;
  notify();
}

/* ─── Self Actions ──────────────────────────────────────────────── */

export function toggleSelfMute(): void {
  const self = ensureSelfParticipant('toggleSelfMute');
  if (!self) {
    appendEvent({
      id: makeEventId(),
      timestamp: timestamp(),
      type: 'system',
      message: 'mute toggle ignored: local participant is unavailable',
    });
    notify();
    return;
  }
  if (self.isHostMuted) {
    appendEvent({
      id: makeEventId(),
      timestamp: timestamp(),
      type: 'system',
      message: 'mute toggle ignored: host mute is active',
      participantId: self.id,
    });
    notify();
    return;
  }

  // If deafened and trying to unmute, cancel deafen entirely
  if (state.isDeafened && self.isMuted) {
    toggleSelfDeafen();
    return;
  }

  self.isMuted = !self.isMuted;
  if (self.isMuted) {
    self.rmsLevel = 0;
    self.isSpeaking = false;
  }
  lkModule?.setMicEnabled(!self.isMuted);
  appendEvent({
    id: makeEventId(),
    timestamp: timestamp(),
    type: self.isMuted ? 'muted' : 'unmuted',
    message: self.isMuted ? 'you muted microphone' : 'you unmuted microphone',
    participantId: self.id,
  });
  void playNotificationSound(self.isMuted ? 'mute' : 'unmute');
  notify();
}

export function toggleSelfDeafen(): void {
  const self = state.participants.find((p) => p.id === state.selfParticipantId);
  if (!self) return;

  if (state.isDeafened) {
    // Undeafen: restore volume, unmute (unless host-muted)
    state.isDeafened = false;
    self.isDeafened = false;
    const restored = preDeafenVolume ?? 70;
    preDeafenVolume = null;
    state.masterVolume = restored;
    lkModule?.setMasterVolume(restored);
    if (!self.isHostMuted) {
      self.isMuted = false;
      lkModule?.setMicEnabled(true);
    }
    client?.send({ type: 'self_undeafen' });
    appendEvent({
      id: makeEventId(),
      timestamp: timestamp(),
      type: 'undeafen',
      message: 'you undeafened',
      participantId: self.id,
    });
    void playNotificationSound('undeafen');
  } else {
    // Deafen: save volume, set to 0, mute mic
    state.isDeafened = true;
    self.isDeafened = true;
    preDeafenVolume = state.masterVolume;
    state.masterVolume = 0;
    lkModule?.setMasterVolume(0);
    if (!self.isMuted) {
      self.isMuted = true;
      self.rmsLevel = 0;
      self.isSpeaking = false;
      lkModule?.setMicEnabled(false);
    }
    client?.send({ type: 'self_deafen' });
    appendEvent({
      id: makeEventId(),
      timestamp: timestamp(),
      type: 'deafen',
      message: 'you deafened',
      participantId: self.id,
    });
    void playNotificationSound('deafen');
  }
  notify();
}

// startShare() was removed (cleanup 2026-03).
// It was a thin wrapper around lkModule.startScreenShare() that sent
// { type: 'start_share' } without shareType metadata.
// Use startFallbackShare() (returns { started, withAudio } for the
// post-share audio prompt) or startCustomShare() instead.

export async function startExternalBrowserShare(): Promise<void> {
  await invoke('external_share_start');
}

/**
 * Start screen sharing via the native PipeWire/portal picker.
 * Uses the Rust-side `screen_share_start` IPC which goes through
 * `create_capture_backend()` → PipeWire portal → native source picker.
 * This is the primary path for Wayland compositors (Hyprland, GNOME, KDE).
 */
export async function startPortalShare(): Promise<boolean> {
  const result = await invoke<boolean>('screen_share_start');
  if (result) {
    // Mark self as sharing and notify peers (same as other share paths).
    const self = state.participants.find((p) => p.id === state.selfParticipantId);
    if (self) {
      self.isSharing = true;
      self.shareType = 'screen_audio';
    }
    if (client) {
      client.send({ type: 'start_share', shareType: 'screen_audio' });
    }
    notify();

    // Start system audio capture after portal video share is established.
    // Uses the pactl-based resolver to avoid PulseAudio API deadlocks.
    // Echo prevention is handled Rust-side via null sink + loopback routing.
    try {
      const audioSourceId = await invoke<string>('get_default_audio_monitor_fast');
      await invoke<AudioShareStartResult>('audio_share_start', { sourceId: audioSourceId });
    } catch (audioErr) {
      console.warn('[wavis] portal share: audio capture failed, continuing video-only:', audioErr);
    }
  }
  return result;
}

/**
 * Start a custom share from the share picker selection.
 * Routes to the correct capture commands based on mode, handles atomic
 * rollback on partial failure, sends signaling, and opens the indicator.
 */
export async function startCustomShare(selection: ShareSelection): Promise<void> {
  // 1. Check if the requested slot is available
  const check = canStartShare(selection, state.activeVideoShare, state.activeAudioShare);
  if (!check.allowed) {
    throw new Error(check.reason ?? 'share slot occupied');
  }

  // 2. Set state fields optimistically
  const isVideoShare = selection.mode === 'screen_audio' || selection.mode === 'window';
  if (isVideoShare) {
    state.activeVideoShare = {
      mode: selection.mode as 'screen_audio' | 'window',
      sourceName: selection.sourceName,
      withAudio: selection.withAudio,
    };
  } else {
    state.activeAudioShare = {
      sourceId: selection.sourceId,
      sourceName: selection.sourceName,
    };
  }
  notify();

  let videoStarted = false;
  const shareSessionId = isVideoShare ? makeShareSessionId() : null;

  try {
    const needsVideo = isVideoShare;
    const needsAudio =
      selection.mode === 'audio_only' ||
      (isVideoShare && selection.withAudio);
    console.log('[AUDIO-DEBUG] startCustomShare called:', selection.mode, 'withAudio:', selection.withAudio);
    console.log(LOG, '[wasapi-diag] startCustomShare: mode=%s withAudio=%s needsVideo=%s needsAudio=%s',
      selection.mode, selection.withAudio, needsVideo, needsAudio);

    // 3. Start video first (if needed)
    if (needsVideo) {
      // On Windows (LiveKit JS SDK path), install the frame buffering
      // handler BEFORE starting the Rust capture. prepareNativeCapture()
      // is synchronous — no async, no event listener, no HWND dependency.
      // This branch is only for the custom picker/native-source pipeline.
      // The normal Windows `/share` button goes through startFallbackShare()
      // -> lkModule.startScreenShare() instead.
      if (lkModule && lkModule instanceof LiveKitModule) {
        lkModule.beginNativeCaptureLeakSession({
          shareSessionId,
          mode: selection.mode as 'screen_audio' | 'window',
          sourceId: selection.sourceId,
          sourceName: selection.sourceName,
        });
        lkModule.prepareNativeCapture();
      }

      if (selection.sourceId === 'portal') {
        await invoke('screen_share_start');
      } else {
        await invoke('screen_share_start_source', {
          sourceId: selection.sourceId,
          shareSessionId,
        });
      }
      videoStarted = true;

      // On Windows (LiveKit JS SDK path), the Rust capture writes frames
      // to a shared buffer. startNativeCapture() starts a polling loop
      // via invoke('screen_share_poll_frame') that uses the ipc:// protocol
      // (HTTP-like), completely bypassing PostMessage/HWND. This is immune
      // to the HWND corruption caused by child windows (SharePicker).
      if (lkModule && lkModule instanceof LiveKitModule) {
        await lkModule.startNativeCapture();
      }
    }

    // 4. Start audio (if needed)
    if (needsAudio) {
      console.log('[AUDIO-DEBUG] needsAudio=true, resolving audio source...');
      try {
        let audioSourceId: string;
        if (selection.mode === 'audio_only') {
          audioSourceId = selection.sourceId;
        } else {
          if (DEBUG_WASAPI) console.log(LOG, '[wasapi] resolving default audio monitor via get_default_audio_monitor');
          audioSourceId = await invoke<string>('get_default_audio_monitor');
          if (DEBUG_WASAPI) console.log(LOG, '[wasapi] default audio monitor resolved:', audioSourceId);
        }
        if (DEBUG_WASAPI) console.log(LOG, '[wasapi] invoking audio_share_start, sourceId:', audioSourceId);
        const audioStartResult = await invoke<AudioShareStartResult>('audio_share_start', { sourceId: audioSourceId });
        if (DEBUG_WASAPI) console.log(LOG, '[wasapi] audio_share_start result:', audioStartResult);

        // On Windows, the Rust WASAPI capture thread streams PCM frames via
        // Tauri events. Start the JS-side AudioWorklet bridge to receive them
        // and publish as a LiveKit ScreenShareAudio track.
        if (DEBUG_WASAPI) console.log(LOG, '[wasapi] lkModule:', lkModule?.constructor?.name, 'is LiveKitModule:', lkModule instanceof LiveKitModule);
        if (lkModule && lkModule instanceof LiveKitModule) {
          const lk = lkModule; // capture narrowed type for closures
          try {
            if (DEBUG_WASAPI) console.log(LOG, '[wasapi] starting audio bridge');
            // startWasapiAudioBridge now owns the Tauri event listeners internally.
            await lk.startWasapiAudioBridge(audioStartResult.loopback_exclusion_available);
            if (DEBUG_WASAPI) console.log(LOG, '[wasapi] bridge fully active');
          } catch (bridgeErr) {
            console.warn(LOG, '[wasapi] WASAPI audio bridge failed:', bridgeErr);
          }
        } else {
          if (DEBUG_WASAPI) console.log(LOG, '[wasapi] skipping bridge — not a LiveKitModule or lkModule null');
        }
      } catch (audioErr) {
        if (videoStarted) {
          // Audio failed but video is already running. On Windows (JS SDK path),
          // system audio sharing may not be available yet — downgrade to video-only
          // instead of rolling back the entire share.
          console.warn(LOG, 'audio companion failed, continuing with video-only:', audioErr);
          if (isVideoShare && state.activeVideoShare) {
            state.activeVideoShare.withAudio = false;
          }
          appendEvent({
            id: makeEventId(),
            timestamp: timestamp(),
            type: 'system',
            message: `system audio unavailable: ${audioErr instanceof Error ? audioErr.message : String(audioErr)}`,
          });
        } else {
          // Audio-only mode failed — no video to keep, propagate error.
          toast.error(audioErr instanceof Error ? audioErr.message : String(audioErr));
          throw audioErr;
        }
      }
    }

    // 5. Send signaling on success
    if (client) {
      client.send({ type: 'start_share', shareType: selection.mode });
    }

    // 6. Open or update ShareIndicator window
    await updateShareIndicator();

    appendEvent({
      id: makeEventId(),
      timestamp: timestamp(),
      type: 'share-start',
      message: `${selfName()} started sharing (${selection.mode})`,
      participantId: state.selfParticipantId ?? undefined,
    });
    notify();
  } catch (err) {
    // Guarantee the affected slot returns to idle on failure
    console.error(LOG, 'startCustomShare failed:', err);
    // Clean up pre-registered listener if startNativeCapture never ran
    if (lkModule && lkModule instanceof LiveKitModule) {
      if (isVideoShare) {
        lkModule.markNativeCaptureFailure(err instanceof Error ? err.message : String(err));
      }
      await lkModule.stopNativeCapture();
    }
    if (isVideoShare) {
      state.activeVideoShare = null;
    } else {
      state.activeAudioShare = null;
    }
    notify();
    throw err;
  }
}

/** Stop a specific share slot or all shares. */
export async function stopCustomShare(target: 'video' | 'audio' | 'all' = 'all'): Promise<void> {
  const plan = planStopCommands(target, state.activeVideoShare, state.activeAudioShare);

  // 1. Stop video capture (best-effort)
  if (plan.stopVideo) {
    // Stop the native capture bridge on Windows (LiveKit JS SDK path)
    if (lkModule && lkModule instanceof LiveKitModule) {
      try {
        await lkModule.stopNativeCapture();
      } catch (err) {
        console.error(LOG, 'best-effort stopNativeCapture failed:', err);
      }
    }
    try {
      await invoke('screen_share_stop');
    } catch (err) {
      console.error(LOG, 'best-effort screen_share_stop failed:', err);
    }
  }

  // 1b. Clean up WASAPI audio bridge (Windows) for any audio stop
  if (plan.stopCompanionAudio || plan.stopAudioOnly) {
    if (lkModule && lkModule instanceof LiveKitModule) {
      try { await lkModule.stopWasapiAudioBridge(); } catch { /* best-effort */ }
    }
  }

  // 2. Stop companion audio from video share (best-effort)
  if (plan.stopCompanionAudio) {
    try {
      await invoke('audio_share_stop');
    } catch (err) {
      console.error(LOG, 'best-effort audio_share_stop (companion) failed:', err);
    }
  }

  // 3. Stop standalone audio-only capture (best-effort)
  if (plan.stopAudioOnly) {
    try {
      await invoke('audio_share_stop');
    } catch (err) {
      console.error(LOG, 'best-effort audio_share_stop (standalone) failed:', err);
    }
  }

  // 4. Send stop_share signaling
  if (client) {
    client.send({ type: 'stop_share' });
  }

  // 5. Clear affected slot(s)
  if (target === 'video' || target === 'all') {
    state.activeVideoShare = null;
    state.shareQualityInfo = null;
    state.shareStats = null;
  }
  if (target === 'audio' || target === 'all') {
    state.activeAudioShare = null;
  }

  // 6. Update or close ShareIndicator
  if (state.activeVideoShare || state.activeAudioShare) {
    await updateShareIndicator();
  } else {
    try {
      const indicatorWin = await WebviewWindow.getByLabel('share-indicator');
      if (indicatorWin) await indicatorWin.close();
    } catch { /* best-effort */ }
  }

  notify();

  appendEvent({
    id: makeEventId(),
    timestamp: timestamp(),
    type: 'share-stop',
    message: `${selfName()} stopped sharing (${target})`,
    participantId: state.selfParticipantId ?? undefined,
  });
}

/** Open or update the ShareIndicator window to reflect current share state. */
async function updateShareIndicator(): Promise<void> {
  const shares: Array<{ mode: ShareMode; sourceName: string }> = [];
  if (state.activeVideoShare) {
    shares.push({ mode: state.activeVideoShare.mode, sourceName: state.activeVideoShare.sourceName });
  }
  if (state.activeAudioShare) {
    shares.push({ mode: 'audio_only', sourceName: state.activeAudioShare.sourceName });
  }
  if (shares.length === 0) return;

  const indicatorParams = { shares };
  const hash = encodeURIComponent(JSON.stringify(indicatorParams));

  // Close existing indicator first (it may have stale data)
  try {
    const existing = await WebviewWindow.getByLabel('share-indicator');
    if (existing) await existing.close();
  } catch { /* best-effort */ }

  // Small delay to let the old window fully close before creating a new one
  await new Promise((r) => setTimeout(r, 50));

  new WebviewWindow('share-indicator', {
    url: `/share-indicator#${hash}`,
    title: 'Wavis — Sharing',
    width: 280,
    height: shares.length > 1 ? 72 : 48,
    resizable: false,
    decorations: false,
    alwaysOnTop: true,
    skipTaskbar: true,
  });
}

export async function stopShare(): Promise<void> {
  // Clear local sharing flag immediately for responsive UI
  const self = state.participants.find((p) => p.id === state.selfParticipantId);
  if (self) {
    self.isSharing = false;
    notify();
  }
  state.shareQualityInfo = null;
  state.shareStats = null;
  localStopShareSent = true;
  if (externalShareHelperActive) {
    await invoke('external_share_stop').catch(() => { });
  }
  // Stop native PipeWire/portal capture if running (best-effort).
  await invoke('screen_share_stop').catch(() => { });
  // Stop system audio capture if running (portal audio share).
  await invoke('audio_share_stop').catch(() => { });
  await lkModule?.stopScreenShare();
  if (client) {
    client.send({ type: 'stop_share' });
  }
}

export type ShareQuality = 'low' | 'high' | 'max';

export type { ShareQualityInfo } from './livekit-media';

export async function setShareQuality(quality: ShareQuality): Promise<void> {
  if (lkModule && 'setScreenShareQuality' in lkModule) {
    await (lkModule as { setScreenShareQuality(q: ShareQuality): Promise<void> }).setScreenShareQuality(quality);
  }
}

export async function toggleShareAudio(withAudio: boolean): Promise<boolean> {
  if (DEBUG_SHARE_AUDIO) {
    console.log(LOG, '[share-audio] toggleShareAudio called', {
      withAudio,
      hasLkModule: !!lkModule,
      lkModuleType: lkModule?.constructor?.name,
      userActivationIsActive: (navigator as { userActivation?: { isActive: boolean } }).userActivation?.isActive,
    });
  }
  if (lkModule && 'restartScreenShareWithAudio' in lkModule) {
    localSourceChanging = true;
    const result = await (lkModule as LiveKitModule).restartScreenShareWithAudio(withAudio);
    localSourceChanging = false;
    if (DEBUG_SHARE_AUDIO) console.log(LOG, '[share-audio] toggleShareAudio result:', result);
    return result;
  }
  return false;
}

export async function changeShareSource(): Promise<boolean> {
  if (lkModule && 'changeScreenShareSource' in lkModule) {
    localSourceChanging = true;
    const result = await (lkModule as LiveKitModule).changeScreenShareSource();
    localSourceChanging = false;

    if (!result && client && !localStopShareSent) {
      // If the source change failed AND there is no active screen share
      // publication, reconcile backend state by sending stop_share. This
      // prevents the backend from staying stuck in a "sharing" state when
      // local media is already dead (e.g. replaceTrack threw after teardown).
      const stillActive = (lkModule as LiveKitModule).hasActiveScreenShareTrack();
      if (!stillActive) {
        client.send({ type: 'stop_share' });
        localStopShareSent = true;
      }
    }

    return result;
  }
  return false;
}

/* ─── Volume ────────────────────────────────────────────────────── */

export function updateSessionProfileColor(color: string): void {
  sessionProfileColor = color;
  if (state.selfParticipantId) {
    state.participants = state.participants.map((p) =>
      p.id === state.selfParticipantId ? { ...p, color } : p,
    );
    notify();
  }
  client?.send({ type: 'update_profile_color', profileColor: color });
}

export function setParticipantVolume(participantId: string, volume: number): void {
  const clamped = Math.max(0, Math.min(100, Math.round(volume)));
  const p = state.participants.find((pp) => pp.id === participantId);
  if (p) {
    p.volume = clamped;
    // Eagerly update in-memory prefs so the value survives a rapid leave/rejoin
    // even before the debounced storage write fires.
    if (p.userId) {
      if (!channelVolumePrefs) {
        channelVolumePrefs = { master: state.masterVolume, participants: {} };
      }
      channelVolumePrefs = {
        ...channelVolumePrefs,
        participants: { ...channelVolumePrefs.participants, [p.userId]: clamped },
      };
    }
    applyEffectiveParticipantVolume(p);
  } else if (lkModule) {
    lkModule.setParticipantVolume(
      participantId,
      computeEffectiveParticipantVolume(
        clamped,
        participantId,
        state.selfParticipantId,
        state.joinedSubRoomId,
        state.participantSubRoomById,
        state.passthrough,
      ),
    );
  }
  saveVolumesDebounced();
  notify();
}

export function setMasterVolume(volume: number): void {
  const clamped = Math.max(0, Math.min(100, Math.round(volume)));
  // Manual volume change cancels deafen state
  if (state.isDeafened && clamped > 0) {
    state.isDeafened = false;
    const self = state.participants.find((p) => p.id === state.selfParticipantId);
    if (self) self.isDeafened = false;
    preDeafenVolume = null;
    client?.send({ type: 'self_undeafen' });
  }
  state.masterVolume = clamped;
  lkModule?.setMasterVolume(clamped);
  saveVolumesDebounced();
  notify();
}

/* ─── Host Actions ──────────────────────────────────────────────── */

export function setScreenShareAudioVolume(participantId: string, volume: number): void {
  const clamped = Math.max(0, Math.min(100, Math.round(volume)));
  if (lkModule && 'setScreenShareAudioVolume' in lkModule) {
    (lkModule as LiveKitModule).setScreenShareAudioVolume(participantId, clamped);
  }
}

export function kickParticipant(participantId: string): void {
  if (!state.selfIsHost) return;
  if (!client) return;
  client.send({ type: 'kick_participant', targetParticipantId: participantId });
}

export function muteParticipant(participantId: string): void {
  if (!state.selfIsHost) return;
  if (!client) return;
  client.send({ type: 'mute_participant', targetParticipantId: participantId });
}

export function unmuteParticipant(participantId: string): void {
  if (!state.selfIsHost) return;
  if (!client) return;
  client.send({ type: 'unmute_participant', targetParticipantId: participantId });
}

export function stopParticipantShare(participantId: string): void {
  if (!state.selfIsHost) return;
  if (!client) return;
  client.send({ type: 'stop_share', targetParticipantId: participantId });
}

export function stopAllShares(): void {
  if (!state.selfIsHost) return;
  if (!client) return;
  client.send({ type: 'stop_all_shares' });
}

export function setSharePermission(permission: 'anyone' | 'host_only'): void {
  if (!state.selfIsHost) return;
  if (!client) return;
  client.send({ type: 'set_share_permission', permission });
}

export function createSubRoom(): void {
  if (!client || client.status !== 'connected') return;
  client.send({ type: 'create_sub_room' });
}

export function joinSubRoom(subRoomId: string): void {
  if (!subRoomId) return;
  desiredSubRoomIntent = subRoomId;
  state.desiredSubRoomId = subRoomId;
  notify();
  if (!client || client.status !== 'connected') return;
  client.send({ type: 'join_sub_room', subRoomId });
}

export function leaveSubRoom(): void {
  desiredSubRoomIntent = null;
  state.desiredSubRoomId = null;
  notify();
  if (!client || client.status !== 'connected') return;
  client.send({ type: 'leave_sub_room' });
}

export function setPassthrough(targetSubRoomId: string): void {
  if (!targetSubRoomId) return;
  if (!client || client.status !== 'connected') return;
  client.send({ type: 'set_passthrough', targetSubRoomId });
}

export function clearPassthrough(): void {
  if (!client || client.status !== 'connected') return;
  client.send({ type: 'clear_passthrough' });
}

/* ─── Chat ──────────────────────────────────────────────────────── */

export function sendChatMessage(text: string): void {
  const trimmed = text.trim();
  if (!trimmed) return;
  if (trimmed.length > 2000) return; // client-side guard
  if (!client || client.status !== 'connected') return;

  client.send({ type: 'chat_send', text: trimmed });
  // Do NOT append locally — wait for server echo (echo-only model)
}


/* ─── RMS ───────────────────────────────────────────────────────── */

export function updateSelfRms(level: number): void {
  const p = state.participants.find((pp) => pp.id === state.selfParticipantId);
  if (!p) return;
  p.rmsLevel = level;
  p.isSpeaking = updateSpeakingTracker(p.id, level, p.isSpeaking, p.isMuted);
  notify();
}

/* ─── Screen Share Audio ─────────────────────────────────────────── */

/** Attach deferred screen share audio when user opens the viewer. */
export function attachScreenShareAudio(participantId: string): void {
  if (lkModule && 'attachScreenShareAudio' in lkModule) {
    (lkModule as LiveKitModule).attachScreenShareAudio(participantId);
  }
}

/** Detach screen share audio when user closes the viewer. */
export function detachScreenShareAudio(participantId: string): void {
  if (lkModule && 'detachScreenShareAudio' in lkModule) {
    (lkModule as LiveKitModule).detachScreenShareAudio(participantId);
  }
}

/* ─── State Access ──────────────────────────────────────────────── */

export function getState(): VoiceRoomState {
  return {
    ...state,
    participants: [...state.participants],
    events: [...state.events],
    chatMessages: [...state.chatMessages],
    subRooms: state.subRooms.map((room) => ({
      ...room,
      participantIds: [...room.participantIds],
    })),
    participantSubRoomById: { ...state.participantSubRoomById },
    passthrough: state.passthrough ? { ...state.passthrough } : null,
  };
}

export function isSelfHost(): boolean {
  return state.selfIsHost;
}

/** Session-scoped channel role — survives reconnection. */
export function getChannelRole(): ChannelRole | null {
  return channelRole;
}

/** Returns the currently registered hotkey string, or null if none. */
export function getRegisteredHotkey(): string | null {
  return registeredHotkey;
}
