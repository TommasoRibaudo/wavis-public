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
import { getServerUrl, getUsername, refreshTokens, onTokensRefreshed } from '@features/auth/auth';
import { PROFILE_COLORS } from '@shared/colors';
import { toWsUrl } from '@shared/helpers';
import {
  LiveKitModule,
  type MediaState,
  type MediaCallbacks,
  type ShareQualityInfo,
  type ShareStats,
  type VideoReceiveStats,
  type ShareProfileId,
} from './livekit-media';
import {
  type CameraQuality,
  type CameraStartError,
  type CameraStartWarning,
  type VideoTileViewModel,
} from './camera-types';
import { MotionDetector, DEFAULT_MOTION_DETECTOR_CONFIG } from './motion-detector';
import {
  startOffscreenCanvasSampling,
  isLeakDiagnosticsThumbnailingActive,
} from './motion-sample-source';
import { NativeMediaModule } from './native-media';
import { setActiveLiveKitModule } from './audio-devices';
import {
  getDefaultVolume,
  getNotificationVolume,
  getSoundVolumes,
  getReconnectConfig,
  getMuteHotkey,
  getProfileColor,
  getChannelVolumes,
  getWindowsSharePath,
  setChannelVolumes,
  getVideoInputDevice,
  setVideoInputDevice,
  DEFAULT_PASSTHROUGH_VOLUME,
} from '@features/settings/settings-store';
import type { ChannelVolumePrefs } from '@features/settings/settings-store';
import type {
  ShareSelection,
  EnumerationResult,
  AudioShareStartResult,
} from '@features/screen-share/share-types';
import type {
  ShareSessionLeakSummary,
  WindowsNativeCaptureDiagnostics,
} from './share-leak-diagnostics';
import { emitTelemetryEvent, type WindowsSharePath } from './telemetry';
import {
  MAX_CHAT_MESSAGES,
  colorFor,
  computeSinceCursor,
  mergeHistoryMessages,
  shouldPlayChatNotification,
  type ChatMessage,
  type RoomEvent,
} from './chat-display-model';
import {
  canStartShare,
  planStopCommands,
  type ActiveAudioShare,
  type ActiveVideoShare,
} from './share-slot-policy';
import {
  computeEffectiveParticipantVolume,
  deriveParticipantSubRoomById,
  mergeParticipantsWithVolume,
  pairedSubRoomId,
  type VoicePassthroughState,
  type VoiceSubRoom,
} from './participant-volume-model';
import {
  buildVideoTilesById,
  computeRoomPanelTab,
  desiredCameraQualityForState,
  type CameraPublicationState,
  type RemoteCameraTileRuntimeState,
} from './video-tile-model';
import { lookupLinuxCapability, type LinuxCapabilityRow } from './linux-capability-matrix';
import {
  buildUnknownLinuxCapabilityRow,
  normalizeLinuxCompositor,
  normalizeLinuxDesktopEnv,
} from './linux-capability-normalize';
import { parseSignalingMessage } from './signaling/parse';
import { registerMuteHotkey, unregisterMuteHotkey } from '@shared/hotkey-bridge';
import {
  playNotificationSound,
  prewarmAudioContext,
  updateCachedNotificationVolume,
  updateCachedSoundVolumes,
} from './notification-sounds';
import { toast } from 'sonner';
import { invoke } from '@tauri-apps/api/core';
import { emit, emitTo, listen, type UnlistenFn } from '@tauri-apps/api/event';

const LOG = '[wavis:voice-room]';
const DEBUG_WASAPI = import.meta.env.VITE_DEBUG_WASAPI === 'true';
const DEBUG_VIEWER_CONNECTION = import.meta.env.VITE_DEBUG_VIEWER_CONNECTION === 'true';
const DEBUG_SHARE_AUDIO = import.meta.env.VITE_DEBUG_SHARE_AUDIO === 'true';
const DEBUG_VIDEO_FEED = import.meta.env.VITE_DEBUG_VIDEO_FEED === 'true';
const windowsWgcFailedSourceKinds = new Set<'screen' | 'window'>();

export function resetWindowsWgcSessionBypassForTests(): void {
  windowsWgcFailedSourceKinds.clear();
}

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
  mediaConnected: boolean;
  rmsLevel: number;
  volume: number;
}

type RemoteShareType = 'screen_audio' | 'window' | 'audio_only' | 'browser';

function parseRemoteShareType(value: unknown): RemoteShareType | undefined {
  return value === 'screen_audio' ||
    value === 'window' ||
    value === 'audio_only' ||
    value === 'browser'
    ? value
    : undefined;
}

function updateRemoteShareType(participantId: string, shareType?: RemoteShareType): void {
  if (lkModule && 'setRemoteShareType' in lkModule) {
    (lkModule as LiveKitModule).setRemoteShareType(liveKitIdentityFor(participantId), shareType);
  }
}

function refreshRemoteScreenShare(participantId: string): void {
  if (lkModule && 'refreshRemoteScreenShare' in lkModule) {
    (
      lkModule as LiveKitModule & { refreshRemoteScreenShare(participantId: string): void }
    ).refreshRemoteScreenShare(liveKitIdentityFor(participantId));
  }
}

/**
 * Schedule retry attempts (at 1s, 3s, 6s, 12s, 24s) to call
 * refreshRemoteScreenShare for a participant whose share just started.
 * Handles the race where TrackSubscribed fires after share_started arrives,
 * or where the SFU treats a republish as a track resume and never fires
 * TrackPublished/TrackSubscribed at all. The 12s/24s long tail covers slower
 * recoveries (e.g. a delayed signaling/media identity realignment) beyond the
 * original 6s cap.
 */
/**
 * True when we hold a screen-share stream for the participant whose video
 * track is still live. A stale entry with an ended track (SFU treated a
 * republish as a resume, so TrackSubscribed never re-fired) must not count
 * as healthy — it lights the share icon while the viewer window waits for
 * frames forever.
 */
function hasLiveScreenShareStream(participantId: string): boolean {
  if (!state.screenShareStreams.has(participantId)) return false;
  const stream = state.screenShareStreams.get(participantId);
  // The native path (Linux) stores null — pixels render outside WebKit, so a
  // present entry is healthy by definition.
  if (!stream) return true;
  return stream.getVideoTracks().some((t) => t.readyState === 'live');
}

/** Cadence for retries beyond the initial escalating burst — see scheduleRefreshRetries. */
const SUSTAINED_REFRESH_RETRY_MS = 30000;

/**
 * One retry attempt: drop a stale (ended-track) stream entry and ask for a
 * fresh subscription. Returns true if the participant is still sharing
 * without a live stream — i.e. another attempt is warranted — and false if
 * this generation was superseded, the stream is now healthy, or sharing has
 * stopped, any of which means the retry loop should not continue.
 */
function attemptScreenShareRefresh(participantId: string, generation: number): boolean {
  if (refreshRetryGenerations.get(participantId) !== generation) return false;
  if (state.screenShareStreams.has(participantId)) {
    // Only a healthy entry skips the refresh. When the SFU treats a
    // republish as a track resume it never re-fires TrackSubscribed, so
    // the map can hold the previous share's stream with an ended video
    // track — the share icon lights up but the viewer window waits for
    // frames forever. Drop such a dead entry and fall through to the
    // refresh so a fresh subscription replaces it. (Native-path entries
    // are null and always count as healthy — see hasLiveScreenShareStream.)
    if (hasLiveScreenShareStream(participantId)) return false;
    console.warn(
      LOG,
      `share stream for ${participantId} has no live video track — dropping stale entry and refreshing`,
    );
    state.screenShareStreams = new Map(state.screenShareStreams);
    state.screenShareStreams.delete(participantId);
    notify();
  }
  const p = state.participants.find((pp) => pp.id === participantId);
  if (!p?.isSharing) return false;
  refreshRemoteScreenShare(participantId);
  return true;
}

/**
 * Schedule retry attempts (at 1s, 3s, 6s, 12s, 24s, then every 30s) to call
 * refreshRemoteScreenShare for a participant whose share just started.
 * Handles the race where TrackSubscribed fires after share_started arrives,
 * or where the SFU treats a republish as a track resume and never fires
 * TrackPublished/TrackSubscribed at all. The 12s/24s long tail covers slower
 * recoveries (e.g. a delayed signaling/media identity realignment) beyond the
 * original 6s cap; the sustained 30s cadence after that covers rarer, slower
 * recoveries (e.g. a LiveKit identity that only resolves once a delayed
 * participant-list update arrives) so the icon isn't stuck grey forever once
 * the fixed burst runs out — it keeps trying until the stream goes live or
 * sharing stops.
 */
function scheduleRefreshRetries(participantId: string): void {
  const generation = (refreshRetryGenerations.get(participantId) ?? 0) + 1;
  refreshRetryGenerations.set(participantId, generation);
  const delays = [1000, 3000, 6000, 12000, 24000];
  for (const delay of delays) {
    setTimeout(() => {
      attemptScreenShareRefresh(participantId, generation);
    }, delay);
  }
  const sustain = () => {
    setTimeout(() => {
      if (attemptScreenShareRefresh(participantId, generation)) sustain();
    }, SUSTAINED_REFRESH_RETRY_MS);
  };
  sustain();
}

function clearRemoteShareType(participantId: string): void {
  if (lkModule && 'clearRemoteShareType' in lkModule) {
    (lkModule as LiveKitModule).clearRemoteShareType(liveKitIdentityFor(participantId));
  }
}

export type SubRoomMembershipSource = 'explicit' | 'legacy_room_one';

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
  /** Passthrough volume (0–100): attenuates audio from paired sub-room. */
  passthroughVolume: number;
  /** Whether channel members may create new passthrough links. */
  passthroughEnabled: boolean;
  /** Whether paired sub-room participants are spectrally muffled. */
  passthroughFiltersEnabled: boolean;
  /** Passthrough muffle strength (0–100). */
  passthroughFilterStrength: number;
  /** Consecutive media reconnect failure count. */
  mediaReconnectFailures: number;
  /** Active video share slot (screen or window). Null when no video share. */
  activeVideoShare: ActiveVideoShare | null;
  /** Active standalone audio share slot. Null when no audio-only share. */
  activeAudioShare: ActiveAudioShare | null;
  /** Transient error from the last `error` signaling message (for chat panel display). */
  lastChatError: string | null;
  /** Friendly reconnect notice persisted across a WS rate-limit disconnect cycle. */
  lastRateLimitError: string | null;
  historyLoaded: boolean;
  /** Whether the local user is deafened (muted + volume 0). */
  isDeafened: boolean;
  /** Latest closed native video share leak summary for bug reports and repro analysis. */
  latestClosedShareLeakSummary: ShareSessionLeakSummary | null;
  /** True when the Windows native mic bridge (Rust WASAPI + DenoiseFilter) is active. */
  nativeMicBridgeActive: boolean;
  /** True when the active JS LiveKit microphone track has the denoise processor attached. */
  noiseSuppressionActive: boolean;
  /** Preferred Windows share path; reads as browser on non-Windows platforms. */
  windowsSharePath: WindowsSharePath;
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
  cameraIntent: boolean;
  cameraPublication: CameraPublicationState;
  cameraSelectedDeviceId: string | null;
  videoTilesById: Record<string, VideoTileViewModel>;
  roomPanelManualOverride: 'logs' | 'video' | null;
  roomPanelTab: 'logs' | 'video';
  /** Identities of remote participants currently in audio-only share mode. */
  audioOnlySharers: Set<string>;
  /**
   * Subset of audioOnlySharers whose membership came from an explicit
   * audio_only share_started/share_state entry (a real, independent
   * standalone-audio slot), as opposed to LiveKit's TrackSubscribed-based
   * guess (onAudioOnlySharerAdded) made before any signaling confirms it.
   * A later video-type share_started must not clear a confirmed entry (the
   * two slots are independent), but should still correct an unconfirmed
   * LiveKit guess once real signaling arrives.
   */
  confirmedAudioOnlySharers: Set<string>;
}

/* ─── Constants ─────────────────────────────────────────────────── */

export const TERMINAL_COLORS = PROFILE_COLORS;

export const RMS_START_THRESHOLD = 0.06;
export const RMS_STOP_THRESHOLD = 0.03;
export const MAX_EVENTS = 100;
export const MAX_PARTICIPANTS = 6;
const WS_RATE_LIMIT_ERROR_MESSAGE = 'rate limit exceeded';

const RATE_LIMIT_RECONNECT_MESSAGE =
  "Connection closed — you're sending messages too fast. Reconnecting";

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

interface MediaTokenPayload {
  sub?: unknown;
}

/** Decode the `sub` (LiveKit participant identity) claim from a media JWT without verifying it. */
function decodeMediaTokenIdentity(token: string): string | null {
  try {
    const parts = token.split('.');
    if (parts.length !== 3) return null;
    const b64 = parts[1].replace(/-/g, '+').replace(/_/g, '/');
    const padded = b64 + '='.repeat((4 - (b64.length % 4)) % 4);
    const payload = JSON.parse(atob(padded)) as MediaTokenPayload;
    return typeof payload.sub === 'string' ? payload.sub : null;
  } catch {
    return null;
  }
}

/**
 * LiveKit identities observed on the live media connection, recorded raw
 * (untranslated) from onRemoteParticipantConnected. Which identity scheme a
 * participant's media session uses depends on the backend version: newer
 * backends sign LiveKit tokens with the durable user_id, older ones with the
 * ephemeral peer_id. This set lets liveKitIdentityFor pick whichever form the
 * media plane is actually using, so a new client stays correct against an
 * old backend and vice versa.
 */
const knownLiveKitIdentities = new Set<string>();

/**
 * Translate a signaling participantId to the LiveKit identity used for that
 * participant's media session. Prefers whichever candidate (durable userId
 * or the participantId itself) has actually been observed on the media
 * connection; when neither has connected yet, defaults to userId (the
 * stable-identity scheme) and finally the participantId (anonymous ad-hoc
 * rooms, where identity == peer_id as before).
 */
function liveKitIdentityFor(participantId: string): string {
  const userId = state.participants.find((p) => p.id === participantId)?.userId;
  if (userId && knownLiveKitIdentities.has(userId)) return userId;
  if (knownLiveKitIdentities.has(participantId)) return participantId;
  return userId ?? participantId;
}

/**
 * Public wrapper of liveKitIdentityFor for callers outside this module
 * (e.g. ActiveRoom's native share viewer invokes and watch-all tile payloads,
 * whose Rust side keys frames by LiveKit identity, not signaling id).
 */
export function liveKitIdentityForParticipant(participantId: string): string {
  return liveKitIdentityFor(participantId);
}

/**
 * Reverse of liveKitIdentityFor: translate a LiveKit identity received from
 * livekit-media.ts back to the signaling participantId it corresponds to.
 * Falls back to treating the identity as the participantId itself, which is
 * correct for anonymous rooms and degrades gracefully during the brief race
 * before a remote participant's userId is known (scheduleRefreshRetries
 * covers that window for screen-share).
 */
function participantIdForLiveKitIdentity(identity: string): string {
  return state.participants.find((p) => p.userId === identity)?.id ?? identity;
}

/** Last time an unknown-identity warning was logged, per identity. */
const unknownIdentityWarnAt = new Map<string, number>();

/**
 * Warn (at most once per identity per 10s — audio levels arrive at ~20Hz and
 * would otherwise flood the diagnostics buffer) that a media-plane identity
 * could not be matched to any signaling participant, including a snapshot of
 * the participant list so a bug report shows exactly which ids/userIds the
 * client knew at that moment.
 */
function warnUnknownIdentity(identity: string): void {
  const now = Date.now();
  if ((unknownIdentityWarnAt.get(identity) ?? 0) > now - 10_000) return;
  unknownIdentityWarnAt.set(identity, now);
  const snapshot = state.participants.map((p) => `${p.id}→${p.userId ?? '(no userId)'}`).join(', ');
  console.warn(LOG, `audio level for unknown identity: ${identity} — participants: [${snapshot}]`);
}

/** Resolve the local user's display name for event log messages. */
function selfName(): string {
  const self = state.participants.find((p) => p.id === state.selfParticipantId);
  return (
    self?.displayName ??
    (state.selfParticipantId ? displayNameCache.get(state.selfParticipantId) : undefined) ??
    'You'
  );
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
 * Requires all four conditions: permission allows it (anyone OR host),
 * room is active, media is connected, and the user is in a synchronized room.
 */
export function isShareEnabled(
  sharePermission: 'anyone' | 'host_only',
  selfIsHost: boolean,
  machineState: VoiceRoomMachineState,
  mediaState: MediaState,
  joinedSubRoomId: string | null,
): boolean {
  return (
    joinedSubRoomId !== null &&
    (sharePermission === 'anyone' || selfIsHost) &&
    machineState === 'active' &&
    mediaState === 'connected'
  );
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
  if (selfSharing) return '/stop-share';
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

function isPassthroughParticipant(participantId: string): boolean {
  if (participantId === state.selfParticipantId || !state.joinedSubRoomId || !state.passthrough) {
    return false;
  }
  const participantSubRoomId = state.participantSubRoomById[participantId] ?? null;
  if (!participantSubRoomId || participantSubRoomId === state.joinedSubRoomId) return false;
  return participantSubRoomId === pairedSubRoomId(state.joinedSubRoomId, state.passthrough);
}

function applyPassthroughFilterSettings(): void {
  lkModule?.setPassthroughFilterSettings({
    enabled: state.passthroughFiltersEnabled,
    strength: state.passthroughFilterStrength,
  });
}

function applyEffectiveParticipantVolume(participant: RoomParticipant): void {
  if (!lkModule || participant.id === state.selfParticipantId) return;
  const identity = liveKitIdentityFor(participant.id);
  lkModule.setParticipantVolume(
    identity,
    computeEffectiveParticipantVolume(
      participant.volume,
      participant.id,
      state.selfParticipantId,
      state.joinedSubRoomId,
      state.participantSubRoomById,
      state.passthrough,
      state.passthroughVolume / 100,
    ),
  );
  lkModule.setParticipantPassthrough(identity, isPassthroughParticipant(participant.id));
}

function applyEffectiveParticipantVolumes(): void {
  if (lkModule) {
    for (const participant of state.participants) {
      applyEffectiveParticipantVolume(participant);
    }
  }
  // Runs after volumes are pushed to the still-live connection above —
  // reconcileLocalMicWithRoomMembership() defers the actual teardown here
  // rather than nulling lkModule out from under this function.
  flushPendingMediaDisconnectForNoRoom();
}

function selfParticipant(): RoomParticipant | undefined {
  return state.participants.find((p) => p.id === state.selfParticipantId);
}

function markParticipantMediaConnected(
  participantId: string,
  mediaConnected: boolean,
  options: { playJoinSound: boolean } = { playJoinSound: true },
): void {
  const participant = state.participants.find((p) => p.id === participantId);
  if (mediaConnected) {
    mediaConnectedParticipantIds.add(participantId);
  } else {
    mediaConnectedParticipantIds.delete(participantId);
  }
  if (!participant || participant.mediaConnected === mediaConnected) return;

  participant.mediaConnected = mediaConnected;
  if (!mediaConnected) {
    participant.isSpeaking = false;
    participant.rmsLevel = 0;
  }

  if (
    mediaConnected &&
    options.playJoinSound &&
    state.joinedSubRoomId !== null &&
    state.participantSubRoomById[participantId] === state.joinedSubRoomId
  ) {
    void playNotificationSound('join');
  }
}

function markAllRemoteParticipantsMediaConnected(
  mediaConnected: boolean,
  options: { playJoinSound: boolean } = { playJoinSound: true },
): void {
  for (const participant of state.participants) {
    if (participant.id === state.selfParticipantId) continue;
    markParticipantMediaConnected(participant.id, mediaConnected, options);
  }
}

function markAllParticipantsMediaConnecting(): void {
  for (const participant of state.participants) {
    markParticipantMediaConnected(participant.id, false, { playJoinSound: false });
  }
}

function clearSelfAudioActivity(self: RoomParticipant): void {
  self.isSpeaking = false;
  self.rmsLevel = 0;
}

function setLocalMicPublishing(enabled: boolean): void {
  void lkModule?.setMicEnabled(enabled);
}

function detachAllScreenShareAudioPlayback(): void {
  if (!lkModule || !('detachScreenShareAudio' in lkModule)) return;
  const detach = (
    lkModule as { detachScreenShareAudio(participantIdentity: string): void }
  ).detachScreenShareAudio.bind(lkModule);
  const participantIds = new Set<string>();
  for (const participant of state.participants) {
    if (participant.id !== state.selfParticipantId) {
      participantIds.add(participant.id);
    }
  }
  for (const participantId of state.screenShareStreams.keys()) {
    if (participantId !== state.selfParticipantId) {
      participantIds.add(participantId);
    }
  }
  for (const participantId of state.audioOnlySharers) {
    if (participantId !== state.selfParticipantId) {
      participantIds.add(participantId);
    }
  }
  for (const participantId of participantIds) {
    detach(participantId);
  }
}

function shouldPublishLocalMic(self: RoomParticipant | undefined): boolean {
  return (
    state.joinedSubRoomId !== null &&
    self !== undefined &&
    !self.isMuted &&
    !self.isHostMuted &&
    !state.isDeafened
  );
}

function reconcileLocalMicWithRoomMembership(previousJoinedSubRoomId: string | null): void {
  const currentJoinedSubRoomId = state.joinedSubRoomId;
  const self = selfParticipant();

  if (currentJoinedSubRoomId === null) {
    if (self) {
      clearSelfAudioActivity(self);
    }
    setLocalMicPublishing(false);
    detachAllScreenShareAudioPlayback();
    // While reconnecting, the server's rejoin handshake always reports the
    // participant as sub-room-less for a moment before the client's
    // automatic rejoin (reconcileDesiredSubRoomMembership) lands. Don't tear
    // down the still-live LiveKit session for that transient window — doing
    // so causes an unnecessary media disconnect/reconnect (and a stuck
    // 'reconnecting' state if the promotion race loses) on every WS
    // reconnect, exactly what machineState='reconnecting' keeps media alive
    // through elsewhere (see the WS disconnect handler in initSession).
    if (state.machineState !== 'reconnecting') {
      // Deferred to flushPendingMediaDisconnectForNoRoom(), called once the
      // caller has finished applying effective participant volumes on the
      // still-live connection — disconnecting here would null it out first.
      pendingMediaDisconnectForNoRoom = true;
    }
    return;
  }

  if (previousJoinedSubRoomId !== null) return;
  if (!self) {
    setLocalMicPublishing(false);
    return;
  }
  clearSelfAudioActivity(self);
  setLocalMicPublishing(shouldPublishLocalMic(self));
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
  ensureInSubRoomForShare();
  console.log(
    LOG,
    `startFallbackShare: lkModule=${lkModule ? lkModule.constructor.name : 'null'}, mediaState=${state.mediaState}, machineState=${state.machineState}`,
  );
  if (!lkModule) {
    throw new Error(
      `Screen sharing is not available (media module not initialized, mediaState=${state.mediaState})`,
    );
  }
  ensureReusePatchReadyForPublish();
  emitSharePathSelected(
    isWindowsPlatform() ? 'browser' : isMacPlatform() ? 'browser_mac' : 'linux_native',
    isWindowsPlatform()
      ? 'browser_display_media'
      : isMacPlatform()
        ? 'mac_browser_share'
        : 'linux_share_start',
  );

  const success = await lkModule.startScreenShare();

  if (success) {
    if (client) {
      // Include shareType so every start_share message carries origin metadata,
      // consistent with startCustomShare() which sends the native picker's mode.
      client.send({ type: 'start_share', shareType: 'browser' });
    }
    notify();
    // Start motion-sample source and profile-switch state machine (W5).
    if (lkModule instanceof LiveKitModule) {
      const track = lkModule.getLocalScreenShareTrack();
      if (track) selectMotionSampleSource(track);
    }
    await initializeProfileSwitchAfterShareStart();
  }
  // Native share-audio platforms report an audio track only after the
  // separate audio bridge is started. Browser-managed paths report it here.
  const hasAudio =
    success && 'hasScreenShareAudio' in lkModule
      ? (lkModule as { hasScreenShareAudio(): boolean }).hasScreenShareAudio()
      : false;
  return { started: success, withAudio: hasAudio };
  // When success === false (user cancelled browser picker, or capture failed
  // silently), do nothing — no signaling, no error.
}

/* ─── Session State ─────────────────────────────────────────────── */

let client: SignalingClient | null = null;
let unsubscribe: (() => void) | null = null;
let unsubStatusChange: (() => void) | null = null;
let onChange: ((state: VoiceRoomState) => void) | null = null;
let channelRole: ChannelRole | null = null;
let sessionUsername: string | null = null;
let sessionProfileColor: string | null = null;
let channelVolumePrefs: ChannelVolumePrefs | null = null;
let volumeSaveTimer: ReturnType<typeof setTimeout> | null = null;
let passthroughVolumeSendTimer: ReturnType<typeof setTimeout> | null = null;
let passthroughFilterSendTimer: ReturnType<typeof setTimeout> | null = null;

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
  mediaState: 'disconnected',
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
  passthroughVolume: DEFAULT_PASSTHROUGH_VOLUME,
  passthroughEnabled: false,
  passthroughFiltersEnabled: true,
  passthroughFilterStrength: 50,
  mediaReconnectFailures: 0,
  activeVideoShare: null,
  activeAudioShare: null,
  lastChatError: null,
  lastRateLimitError: null,
  historyLoaded: false,
  isDeafened: false,
  latestClosedShareLeakSummary: null,
  nativeMicBridgeActive: false,
  noiseSuppressionActive: false,
  windowsSharePath: 'browser',
  subRooms: [],
  joinedSubRoomId: null,
  desiredSubRoomId: null,
  participantSubRoomById: {},
  passthrough: null,
  cameraIntent: false,
  cameraPublication: 'idle',
  cameraSelectedDeviceId: null,
  videoTilesById: {},
  roomPanelManualOverride: null,
  roomPanelTab: 'logs',
  audioOnlySharers: new Set(),
  confirmedAudioOnlySharers: new Set(),
};

let state: VoiceRoomState = {
  ...DEFAULT_STATE,
  events: [],
  chatMessages: [],
  participants: [],
  screenShareStreams: new Map(),
};
let remoteCameraTilesById: Record<string, RemoteCameraTileRuntimeState> = {};
let roomPanelHadAnyVideoActive = false;
let cameraQualityRequestVersion = 0;
let cameraQualityRetryTimer: ReturnType<typeof setTimeout> | null = null;
let lastRequestedCameraQualityTier: CameraQuality['tier'] | null = null;
/** Serialises concurrent toggleCameraIntent calls — prevents intent/publication desync on rapid clicks. */
let cameraToggleChain: Promise<void> = Promise.resolve();

/** Common shape for both LiveKitModule (JS SDK) and NativeMediaModule (Rust IPC). */
type MediaModule = LiveKitModule | NativeMediaModule;

let lkModule: MediaModule | null = null;
// Settled promise of the most recent session-end media teardown. Awaited
// (bounded) by the updater so the installer never kills the process while the
// mic capture device is still open — see waitForMediaTeardown().
let lastMediaTeardown: Promise<void> = Promise.resolve();
let reusePatchGuardCheckedForSession = false;
let reusePatchGuardPassedForSession = false;
interface PendingWasapiResume {
  systemAudioEnabled: boolean;
  systemAudioMuted: boolean;
  systemAudioSourceId: string | null;
}
let pendingWasapiResume: PendingWasapiResume | null = null;

/* ─── Motion detection (W5) ─────────────────────────────────────── */

let _motionDetector: MotionDetector | null = null;
let _stopMotionSampling: (() => void) | null = null;

/** Current active motion detector, or null when no share is active. */
export function getMotionDetector(): MotionDetector | null {
  return _motionDetector;
}

/**
 * Selects and starts a motion-sample source for the given track.
 * Primary: Leak_Diagnostics thumbnail canvas (when active).
 * Fallback: OffscreenCanvas 4 Hz sampler (always available while share is active).
 * Re-call to re-select source on diagnostics-toggle events.
 */
function selectMotionSampleSource(track: MediaStreamTrack): void {
  _stopMotionSampling?.();
  _stopMotionSampling = null;
  if (!_motionDetector) {
    _motionDetector = new MotionDetector(DEFAULT_MOTION_DETECTOR_CONFIG);
  }
  if (isLeakDiagnosticsThumbnailingActive()) {
    // Primary source (thumbnailing) — not yet implemented; falls through to fallback.
  }
  _stopMotionSampling = startOffscreenCanvasSampling(track, _motionDetector);
}

function stopMotionDetection(): void {
  _stopMotionSampling?.();
  _stopMotionSampling = null;
  _motionDetector = null;
  _stopAutoSwitchPoll?.();
  _stopAutoSwitchPoll = null;
}

/* ─── Profile-switch state machine (W5 task 10.8) ───────────────── */

let _currentShareProfile: ShareProfileId = 'detail';
let _stopAutoSwitchPoll: (() => void) | null = null;
let selectedShareQuality: ShareQuality = 'high';

/** Minimum time between applied auto-switches, independent of detector dwell,
 *  to bound worst-case encoder churn regardless of content. */
const MIN_PROFILE_SWITCH_INTERVAL_MS = 5_000;
let _lastProfileSwitchAppliedAtMs = 0;

/** Maps a ShareProfileId to the legacy ShareQuality for the existing setScreenShareQuality path. */
function profileToQuality(profile: ShareProfileId): ShareQuality {
  return profile === 'motion' ? 'low' : 'high';
}

/**
 * Applies the given profile's encoding envelope to the active share.
 * Emits share.profile.switched telemetry.
 */
async function applyProfileSwitch(
  to: ShareProfileId,
  reason: 'init' | 'auto_in' | 'auto_out',
): Promise<void> {
  if (selectedShareQuality === 'max') {
    _currentShareProfile = 'detail';
    return;
  }
  const from = _currentShareProfile;
  _currentShareProfile = to;
  const quality = profileToQuality(to);
  if (lkModule && 'setScreenShareQuality' in lkModule) {
    await (lkModule as { setScreenShareQuality(q: ShareQuality): Promise<void> })
      .setScreenShareQuality(quality)
      .catch(() => {});
  }
  emitTelemetryEvent({ name: 'share.profile.switched', from, to, reason, ts: Date.now() });
}

/**
 * Starts the auto-switch polling loop. Polls _motionDetector.currentRecommendation()
 * every 1 second. Unconditional — no user gate (A1.5).
 * Returns a stop function.
 */
function startAutoSwitchPoll(): () => void {
  let stopped = false;
  const id = setInterval(() => {
    void (async () => {
      if (stopped || !_motionDetector) return;
      const recommendation = _motionDetector.currentRecommendation();
      if (recommendation === _currentShareProfile) return;
      const now = Date.now();
      if (now - _lastProfileSwitchAppliedAtMs < MIN_PROFILE_SWITCH_INTERVAL_MS) return;
      _lastProfileSwitchAppliedAtMs = now;
      const reason: 'auto_in' | 'auto_out' = recommendation === 'motion' ? 'auto_in' : 'auto_out';
      await applyProfileSwitch(recommendation, reason).catch(() => {});
    })();
  }, 1_000);
  return () => {
    stopped = true;
    clearInterval(id);
  };
}

async function initializeProfileSwitchAfterShareStart(): Promise<void> {
  _lastProfileSwitchAppliedAtMs = 0;
  if (selectedShareQuality === 'max') {
    const from = _currentShareProfile;
    _currentShareProfile = 'detail';
    _stopAutoSwitchPoll?.();
    _stopAutoSwitchPoll = null;
    emitTelemetryEvent({
      name: 'share.profile.switched',
      from,
      to: 'detail',
      reason: 'init',
      ts: Date.now(),
    });
    return;
  }
  await applyProfileSwitch('detail', 'init');
  _stopAutoSwitchPoll?.();
  _stopAutoSwitchPoll = startAutoSwitchPoll();
}

async function syncNativeScreenShareQualityBeforeCapture(path: string): Promise<void> {
  try {
    await invoke('media_set_screen_share_quality', { quality: selectedShareQuality });
  } catch (error) {
    console.warn(LOG, 'native screen share quality sync failed', {
      quality: selectedShareQuality,
      path,
      error,
    });
    throw error;
  }
}

/**
 * Platform detection: use the native Rust media path when the webview
 * lacks usable WebRTC support. Do not force Linux into the native path:
 * some Linux/Tauri environments expose working WebRTC + getUserMedia even
 * when getDisplayMedia is absent. Camera publish/receive only needs those
 * browser media APIs; Linux screen sharing still has the native portal path.
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
    // lib.dom.d.ts declares navigator.mediaDevices as always defined, but it is
    // genuinely undefined at runtime in non-secure contexts and older browsers.
    // eslint-disable-next-line @typescript-eslint/no-unnecessary-condition
    navigator.mediaDevices !== undefined &&
    typeof navigator.mediaDevices.getUserMedia === 'function';

  return !(hasRtc && hasGetUserMedia);
}

function isWindowsPlatform(): boolean {
  return typeof navigator !== 'undefined' && /Windows NT/i.test(navigator.userAgent || '');
}

function isMacPlatform(): boolean {
  return typeof navigator !== 'undefined' && /Macintosh/i.test(navigator.userAgent || '');
}

function isLinuxPlatform(): boolean {
  return typeof navigator !== 'undefined' && !isWindowsPlatform() && !isMacPlatform();
}

function getWindowsSharePathOverride(): WindowsSharePath | null {
  const value = import.meta.env.VITE_WAVIS_WINDOWS_SHARE_PATH as string | undefined;
  return value === 'browser' || value === 'native' ? value : null;
}

function resolveWindowsSharePathPreference(storedPath: WindowsSharePath): WindowsSharePath {
  if (!isWindowsPlatform()) {
    return 'browser';
  }
  return getWindowsSharePathOverride() ?? storedPath;
}

function emitSharePathSelected(
  path: 'browser' | 'native' | 'linux_native' | 'browser_mac',
  reason: string,
): void {
  emitTelemetryEvent({
    name: 'share.path.selected',
    path,
    reason,
    ts: Date.now(),
  });
}

function emitAudioCaptureSelection(result: AudioShareStartResult): void {
  if (!result.capture_path) {
    return;
  }

  const os =
    result.capture_path === 'wasapi'
      ? 'windows'
      : result.capture_path === 'pulse_audio'
        ? 'linux'
        : 'macos';
  emitTelemetryEvent({
    name: 'capture.path.selected',
    os,
    path: result.capture_path,
    ts: Date.now(),
  });

  if (result.capture_path === 'screen_capture_kit') {
    emitTelemetryEvent({
      name: 'capture.fallback.activated',
      os: 'macos',
      from: 'process_tap',
      to: 'sck',
      reason: result.fallback_reason ?? 'process tap isolation was unavailable',
      ts: Date.now(),
    });
  }
}

interface LinuxDesktopContext {
  desktopEnv: string;
  sessionType: string;
}

interface LinuxCaptureFallbackPayload {
  from: string;
  to: string;
  reason: string;
}

async function resolveLinuxCapabilityRow(): Promise<LinuxCapabilityRow> {
  const context = await invoke<LinuxDesktopContext>('get_linux_desktop_context');
  const desktopEnv = normalizeLinuxDesktopEnv(context.desktopEnv);
  const compositor = normalizeLinuxCompositor(context.sessionType);

  if (!compositor) {
    return buildUnknownLinuxCapabilityRow(
      'Linux native share on this desktop is not in the checked-in Wavis capability matrix yet; share will proceed with degraded expectations.',
    );
  }

  return lookupLinuxCapability(desktopEnv, compositor);
}

async function guardLinuxNativeShareStart(): Promise<void> {
  if (!isLinuxPlatform()) {
    return;
  }

  const row = await resolveLinuxCapabilityRow();
  if (row.combinedStatus === 'supported') {
    return;
  }

  if (row.userMessage) {
    appendEvent({
      id: makeEventId(),
      timestamp: timestamp(),
      type: 'system',
      message: row.userMessage,
    });
    notify();
  }

  if (row.combinedStatus === 'unsupported') {
    throw new Error(row.userMessage ?? 'Linux native share is not supported on this desktop.');
  }
}

function maybeNoticeMacCaptureFallback(result: AudioShareStartResult): void {
  if (result.capture_path !== 'screen_capture_kit') {
    return;
  }

  appendEvent({
    id: makeEventId(),
    timestamp: timestamp(),
    type: 'system',
    message: `macOS system audio is using the ScreenCaptureKit fallback with weaker isolation: ${result.fallback_reason ?? 'process tap isolation was unavailable'}`,
  });
}

function ensureReusePatchReadyForPublish(): void {
  if (!(lkModule instanceof LiveKitModule)) {
    return;
  }
  if (reusePatchGuardCheckedForSession && reusePatchGuardPassedForSession) {
    return;
  }

  reusePatchGuardCheckedForSession = true;
  reusePatchGuardPassedForSession =
    (globalThis as { __wavisSenderData?: unknown }).__wavisSenderData !== undefined;
  if (reusePatchGuardPassedForSession) {
    return;
  }

  emitTelemetryEvent({
    name: 'share.reuse_patch.missing',
    ts: Date.now(),
  });
  throw new Error(
    'Screen sharing is unavailable because the LiveKit transceiver reuse patch is missing.',
  );
}

function snapshotPendingWasapiResume(): void {
  if (!isWindowsPlatform()) {
    pendingWasapiResume = null;
    return;
  }

  const videoShareAudioEnabled = state.activeVideoShare?.withAudio === true;
  const audioSourceId =
    state.activeAudioShare?.sourceId ?? state.activeVideoShare?.audioSourceId ?? null;
  const systemAudioEnabled = state.activeAudioShare !== null || videoShareAudioEnabled;

  pendingWasapiResume = {
    systemAudioEnabled,
    systemAudioMuted: !systemAudioEnabled,
    systemAudioSourceId: audioSourceId,
  };
}

async function resumePendingWasapiCapture(): Promise<void> {
  const snapshot = pendingWasapiResume;
  if (!snapshot) {
    return;
  }
  if (
    !snapshot.systemAudioEnabled ||
    snapshot.systemAudioMuted ||
    snapshot.systemAudioSourceId === null ||
    !(lkModule instanceof LiveKitModule)
  ) {
    pendingWasapiResume = null;
    return;
  }

  try {
    const audioStartResult = await invoke<AudioShareStartResult>('audio_share_start', {
      sourceId: snapshot.systemAudioSourceId,
    });
    emitAudioCaptureSelection(audioStartResult);
    await lkModule.startWasapiAudioBridge(audioStartResult.loopback_exclusion_available);
  } catch (error) {
    const reason = error instanceof Error ? error.message : String(error);
    emitTelemetryEvent({
      name: 'capture.fallback.activated',
      os: 'windows',
      from: 'wasapi',
      to: 'none',
      reason,
      ts: Date.now(),
    });
    appendEvent({
      id: makeEventId(),
      timestamp: timestamp(),
      type: 'system',
      message: `system audio resume failed: ${reason}`,
    });
    notify();
  } finally {
    pendingWasapiResume = null;
  }
}

let bufferedMediaToken: {
  sfuUrl: string;
  token: string;
  iceConfig?: {
    stunUrls: string[];
    turnUrls: string[];
    turnUsername?: string;
    turnCredential?: string;
  };
} | null = null;
let latestMediaToken: {
  sfuUrl: string;
  token: string;
  iceConfig?: {
    stunUrls: string[];
    turnUrls: string[];
    turnUsername?: string;
    turnCredential?: string;
  };
} | null = null;
let bufferedIceConfig: {
  stunUrls: string[];
  turnUrls: string[];
  turnUsername?: string;
  turnCredential?: string;
} | null = null;
// LiveKit identity (JWT `sub`) that `lkModule` is currently connected/connecting with.
// Used to detect the ephemeral-peer_id drift that happens when a silent WS reconnect
// issues a new signaling participantId while the LiveKit session survives on its old identity.
let connectedMediaIdentity: string | null = null;
let desiredSubRoomIntent: string | null | undefined = undefined;
let lastReconnectMediaTime = 0;
let suppressMediaDisconnectedReconnect = false;
let pendingMediaDisconnectForNoRoom = false;
/** Currently registered hotkey string (null when no hotkey is active). */
let registeredHotkey: string | null = null;
/** Volume before deafen, restored on undeafen. */
let preDeafenVolume: number | null = null;
/** Self mute intent before deafen forced the mic silent. */
let preDeafenSelfMuted: boolean | null = null;
let localStopShareSent = false;
let localSourceChanging = false;
let externalShareHelperActive = false;
/** Generation counters per participant — incremented on each share_started to cancel stale retries. */
const refreshRetryGenerations = new Map<string, number>();
export const BACKGROUND_LEAVE_DISCONNECT_MS = 15 * 60_000;
const RECONNECT_MEDIA_COOLDOWN_MS = 3000;
/** Slow periodic media retry interval (ms) after fast retries are exhausted. */
const PERIODIC_MEDIA_RETRY_MS = 30_000;
let periodicMediaRetryTimer: ReturnType<typeof setInterval> | null = null;
const COLD_START_RETRY_MS = 30_000;
let coldStartRetryTimer: ReturnType<typeof setTimeout> | null = null;
/**
 * sendJoinVoiceRequest() is fire-and-forget — it has no built-in way to
 * notice a response never arrives (e.g. the backend can't reach its own
 * SFU dependency and never replies with media_token). Without this guard,
 * mediaReconnectFailures never increments past its current value and
 * reconnectMedia()'s retry-budget check never gets a second attempt to
 * run, let alone exhaust — media sits in 'disconnected' silently forever.
 */
const MEDIA_TOKEN_REQUEST_TIMEOUT_MS = 15_000;
let mediaTokenRequestTimer: ReturnType<typeof setTimeout> | null = null;
let backgroundLeaveTimer: ReturnType<typeof setTimeout> | null = null;
const MAX_AUTH_REFRESH_RETRIES = 2;
let authRefreshRetries = 0;
let wasReconnecting = false;
let unsubTokenRefresh: (() => void) | null = null;
let localSessionJoined = false;
let localDisconnectSoundPlayed = false;
// TODO: proactiveReconnecting flag — wire into onStatusChange to suppress
// the "already reconnecting → connection lost" path during token-refresh reconnects.

/**
 * Cache of participantId → displayName. Survives participant_left so that
 * late-arriving events (share_stopped, etc.) can still resolve display names.
 */
const displayNameCache = new Map<string, string>();
const mediaConnectedParticipantIds = new Set<string>();
let suppressReconnectJoinForParticipantIds = new Set<string>();

function playLocalDisconnectSoundOnce(): void {
  const wasInLocalSession =
    localSessionJoined ||
    !!state.selfParticipantId ||
    !!state.roomId ||
    state.machineState === 'active' ||
    state.machineState === 'reconnecting';

  if (!wasInLocalSession || localDisconnectSoundPlayed) return;
  localDisconnectSoundPlayed = true;
  void playNotificationSound('leave');
}

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
let unlistenLinuxCaptureFallback: UnlistenFn | null = null;
let unlistenViewerJoined: UnlistenFn | null = null;
let unlistenViewerTokenRequest: UnlistenFn | null = null;
/** Broker correlation for viewer-token requests: windowId → windowLabel. */
const pendingViewerTokenRequests = new Map<string, string>();

export function setPendingSharePickerData(data: PendingSharePickerData | null): void {
  pendingSharePickerData = data;
}

export function getPendingSharePickerData(): PendingSharePickerData | null {
  return pendingSharePickerData;
}

function setupSharePickerListener(): void {
  if (unlistenSharePickerRequest) return; // already listening
  void listen('share-picker:request-sources', () => {
    if (pendingSharePickerData) {
      void emit('share-picker:sources', pendingSharePickerData);
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

function setupLinuxCaptureFallbackListener(): void {
  if (unlistenLinuxCaptureFallback) return;
  void listen<LinuxCaptureFallbackPayload>('linux-capture-fallback-activated', ({ payload }) => {
    emitTelemetryEvent({
      name: 'capture.fallback.activated',
      os: 'linux',
      from: payload.from,
      to: payload.to,
      reason: payload.reason,
      ts: Date.now(),
    });
  }).then((unlisten) => {
    unlistenLinuxCaptureFallback = unlisten;
  });
}

function teardownLinuxCaptureFallbackListener(): void {
  if (unlistenLinuxCaptureFallback) {
    unlistenLinuxCaptureFallback();
    unlistenLinuxCaptureFallback = null;
  }
}

function setupExternalShareHelperListeners(): void {
  if (unlistenExternalShareStarted || unlistenExternalShareStopped || unlistenExternalShareError)
    return;

  void listen<{ sessionId: string }>('external-share-started', () => {
    if (state.joinedSubRoomId === null) {
      invoke('external_share_stop').catch(() => {});
      return;
    }
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

  void listen<{ sessionId: string }>('external-share-stopped', () => {
    externalShareHelperActive = false;
    state.shareQualityInfo = null;
    state.shareStats = null;
    const self = state.participants.find((p) => p.id === state.selfParticipantId);
    if (self) {
      self.isSharing = false;
    }
    if (!localStopShareSent && client) {
      client.send({ type: 'stop-share' });
    }
    localStopShareSent = false;
    notify();
  }).then((unlisten) => {
    unlistenExternalShareStopped = unlisten;
  });

  void listen<{ sessionId: string; message: string }>('external-share-error', (event) => {
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
      videoTilesById: { ...state.videoTilesById },
    });
  }
}

function getCameraMediaModule(): MediaModule | null {
  return lkModule;
}

function resetCameraQualityController(): void {
  cameraQualityRequestVersion = 0;
  lastRequestedCameraQualityTier = null;
  if (cameraQualityRetryTimer) {
    clearTimeout(cameraQualityRetryTimer);
    cameraQualityRetryTimer = null;
  }
}

function resetCameraRuntimeState(): void {
  remoteCameraTilesById = {};
  roomPanelHadAnyVideoActive = false;
  resetCameraQualityController();
  state.cameraIntent = false;
  state.cameraPublication = 'idle';
  state.cameraSelectedDeviceId = null;
  state.videoTilesById = {};
  state.roomPanelManualOverride = null;
  state.roomPanelTab = 'logs';
}

function cleanupPublishedMediaForSessionEnd(): void {
  let sentStopShare = false;

  if (state.activeVideoShare || state.activeAudioShare) {
    if (state.activeVideoShare) {
      invoke('screen_share_stop').catch(() => {});
      if (state.activeVideoShare.withAudio) invoke('audio_share_stop').catch(() => {});
    }
    if (state.activeAudioShare) invoke('audio_share_stop').catch(() => {});
    if (client && client.status === 'connected') {
      client.send({ type: 'stop-share' });
      sentStopShare = true;
    }
    state.activeVideoShare = null;
    state.activeAudioShare = null;
    state.shareQualityInfo = null;
    state.shareStats = null;
    stopMotionDetection();
  }

  const selfP = state.participants.find((p) => p.id === state.selfParticipantId);
  if (selfP?.isSharing) {
    invoke('external_share_stop').catch(() => {});
    invoke('screen_share_stop').catch(() => {});
    invoke('audio_share_stop').catch(() => {});
    externalShareHelperActive = false;
    if (!sentStopShare && client && client.status === 'connected') {
      client.send({ type: 'stop-share' });
    }
  }

  for (const participant of state.participants) {
    participant.isSharing = false;
  }
  state.screenShareStreams = new Map();
  state.shareQualityInfo = null;
  state.shareStats = null;

  if (lkModule) {
    if (state.cameraPublication === 'published' || state.cameraIntent) {
      void getCameraMediaModule()
        ?.unpublishCamera()
        .catch(() => {});
    }
    void lkModule.stopScreenShare().catch(() => {});
    suppressMediaDisconnectedReconnect = true;
    try {
      lkModule.disconnect();
    } finally {
      suppressMediaDisconnectedReconnect = false;
    }
    // Retain the teardown promise so waitForMediaTeardown() (the updater path)
    // can wait for the mic capture device to actually be released. The `in`
    // guard keeps NativeMediaModule and test doubles working without it.
    lastMediaTeardown =
      'waitForTeardown' in lkModule
        ? lkModule.waitForTeardown().catch(() => {})
        : Promise.resolve();
    lkModule = null;
  }
  resetCameraRuntimeState();

  bufferedMediaToken = null;
  latestMediaToken = null;
  bufferedIceConfig = null;
  pendingViewerTokenRequests.clear();
  externalShareHelperActive = false;
  reusePatchGuardCheckedForSession = false;
  reusePatchGuardPassedForSession = false;
  pendingWasapiResume = null;
  stopColdStartRetry();
  stopPeriodicMediaRetry();
  clearMediaTokenRequestTimeout();
  setActiveLiveKitModule(null);
}

/**
 * Wait for the most recent session-end media teardown to release its capture
 * devices, bounded by `timeoutMs`. The auto-updater awaits this after
 * leaveRoom(): installing an update kills the process, and killing it while
 * the mic capture is still open can wedge Bluetooth/USB headsets until they
 * are reconnected (issue #230).
 */
export async function waitForMediaTeardown(timeoutMs = 2000): Promise<void> {
  let timer: ReturnType<typeof setTimeout> | undefined;
  const timeout = new Promise<void>((resolve) => {
    timer = setTimeout(resolve, timeoutMs);
  });
  try {
    await Promise.race([lastMediaTeardown, timeout]);
  } finally {
    clearTimeout(timer);
  }
}

function visibleRemoteCameraSet(): {
  ids: Set<string>;
  tiles: Record<string, RemoteCameraTileRuntimeState>;
} {
  const joinedSubRoomId = state.joinedSubRoomId;
  if (!joinedSubRoomId) return { ids: new Set(), tiles: {} };
  const visibleSubRoomIds = new Set([joinedSubRoomId]);
  if (state.passthrough) {
    const { sourceSubRoomId, targetSubRoomId } = state.passthrough;
    if (sourceSubRoomId === joinedSubRoomId) visibleSubRoomIds.add(targetSubRoomId);
    else if (targetSubRoomId === joinedSubRoomId) visibleSubRoomIds.add(sourceSubRoomId);
  }
  const ids = new Set<string>();
  const tiles: Record<string, RemoteCameraTileRuntimeState> = {};
  for (const [id, entry] of Object.entries(remoteCameraTilesById)) {
    const participantSubRoomId = state.participantSubRoomById[id] ?? null;
    if (participantSubRoomId && visibleSubRoomIds.has(participantSubRoomId)) {
      ids.add(id);
      tiles[id] = entry;
    }
  }
  // Also include in-scope participants without an existing tile entry yet, so a camera
  // that publishes after we've already filtered (e.g. someone turns on camera in our
  // sub-room) is allowed to subscribe through applyRemoteCameraVisibility.
  for (const participant of state.participants) {
    const participantSubRoomId = state.participantSubRoomById[participant.id] ?? null;
    if (participantSubRoomId && visibleSubRoomIds.has(participantSubRoomId)) {
      ids.add(participant.id);
    }
  }
  return { ids, tiles };
}

function rebuildVideoTiles(): void {
  const module = getCameraMediaModule();
  const localTrack = module?.getLocalCameraTrack() ?? null;
  const { ids: visibleIds, tiles: visibleTiles } = visibleRemoteCameraSet();
  state.videoTilesById = buildVideoTilesById({
    participants: state.participants,
    selfParticipantId: state.selfParticipantId,
    cameraPublication: state.cameraPublication,
    localTrack,
    remoteTilesById: visibleTiles,
  });
  // Keep server-side subscriptions in sync with the visibility filter so cameras outside
  // the joined sub-room don't keep streaming bytes (TrackSubscribed force-enables every
  // camera publication; without this, hidden cameras would burn bandwidth indefinitely).
  module?.applyRemoteCameraVisibility(new Set([...visibleIds].map(liveKitIdentityFor)));
  syncRoomPanelTab();
}

function syncRoomPanelTab(): void {
  const anyVideoActive = Object.values(state.videoTilesById).some((tile) => !tile.isMuted);
  if (anyVideoActive) {
    roomPanelHadAnyVideoActive = true;
  } else if (roomPanelHadAnyVideoActive) {
    state.roomPanelManualOverride = null;
    roomPanelHadAnyVideoActive = false;
  }

  state.roomPanelTab = computeRoomPanelTab({
    anyVideoActive,
    manualOverride: state.roomPanelManualOverride,
    hadAnyVideoActive: roomPanelHadAnyVideoActive,
  });
}

function appendCameraSystemEvent(message: string): void {
  appendSystemEvent(message);
}

function cameraErrorMessage(error: CameraStartError): string {
  switch (error.kind) {
    case 'permission_denied':
      return isMacPlatform()
        ? 'Camera access denied. Enable it in System Settings > Privacy & Security > Camera, then try again.'
        : 'camera permission denied';
    case 'device_unavailable':
      return 'camera device unavailable';
    case 'device_in_use':
      return 'camera device is already in use';
    case 'timeout':
      return 'camera start timed out';
    case 'no_camera_configured':
      return 'no camera available';
    case 'publish_failed':
      return 'camera publish failed';
  }
}

function cameraWarningMessage(warning: CameraStartWarning): string {
  switch (warning.kind) {
    case 'device_not_found':
      return `camera warning (device_not_found): selected camera not found, using browser default (${warning.missingDeviceId})`;
    case 'permission_denied':
      return 'camera warning (permission_denied): camera change blocked by permission denial';
    case 'device_in_use':
      return 'camera warning (device_in_use): camera change failed because the device is in use';
    case 'hardware_error':
      return 'camera warning (hardware_error): camera change failed due to a hardware error';
  }
}

function surfaceCameraError(error: CameraStartError): void {
  const message = cameraErrorMessage(error);
  appendCameraSystemEvent(message);
  toast.error(message);
}

function surfaceCameraWarning(warning: CameraStartWarning): void {
  const message = cameraWarningMessage(warning);
  appendCameraSystemEvent(message);
  toast.error(message);
}

function classifyCameraReplaceWarning(
  error: unknown,
  requestedDeviceId: string | null,
): CameraStartWarning {
  const cameraError = error as CameraStartError | null;
  if (requestedDeviceId !== null && cameraError?.kind === 'device_unavailable') {
    return { kind: 'device_not_found', missingDeviceId: requestedDeviceId };
  }
  if (cameraError?.kind === 'permission_denied') {
    return { kind: 'permission_denied' };
  }
  if (cameraError?.kind === 'device_in_use') {
    return { kind: 'device_in_use' };
  }
  return { kind: 'hardware_error' };
}

async function enumerateVideoInputs(): Promise<MediaDeviceInfo[]> {
  if (
    typeof navigator === 'undefined' ||
    !('mediaDevices' in navigator) ||
    // lib.dom.d.ts declares navigator.mediaDevices as always defined, but it is
    // genuinely undefined at runtime in non-secure contexts and older browsers.
    // eslint-disable-next-line @typescript-eslint/no-unnecessary-condition
    navigator.mediaDevices === undefined ||
    typeof navigator.mediaDevices.enumerateDevices !== 'function'
  ) {
    return [];
  }

  const devices = await navigator.mediaDevices.enumerateDevices();
  return devices.filter((device) => device.kind === 'videoinput');
}

async function resolveCameraDeviceSelection(deviceId: string | null): Promise<{
  effectiveDeviceId: string | null;
  warning: CameraStartWarning | null;
}> {
  const videoInputs = await enumerateVideoInputs();
  if (videoInputs.length === 0) {
    throw { kind: 'no_camera_configured' } satisfies CameraStartError;
  }

  if (deviceId === null) {
    return { effectiveDeviceId: null, warning: null };
  }

  if (videoInputs.some((device) => device.deviceId === deviceId)) {
    return { effectiveDeviceId: deviceId, warning: null };
  }

  return {
    effectiveDeviceId: null,
    warning: { kind: 'device_not_found', missingDeviceId: deviceId },
  };
}

function appendEvent(event: RoomEvent): void {
  state.events.push(event);
  if (state.events.length > MAX_EVENTS) {
    state.events = state.events.slice(state.events.length - MAX_EVENTS);
  }
}

function isWsRateLimitError(message: string): boolean {
  return message.trim().toLowerCase() === WS_RATE_LIMIT_ERROR_MESSAGE;
}

function syncDerivedSubRoomState(): void {
  state.participantSubRoomById = deriveParticipantSubRoomById(state.subRooms);
  if (!state.selfParticipantId) {
    state.joinedSubRoomId = null;
    return;
  }
  state.joinedSubRoomId = state.participantSubRoomById[state.selfParticipantId] ?? null;
}

/**
 * If a media token is buffered and the local user is now in a sub-room,
 * consume the token and connect media. Called from every handler that can
 * advance joinedSubRoomId: joined, sub_room_joined, sub_room_state.
 */
function flushBufferedMediaTokenIfReady(): void {
  if (bufferedMediaToken && state.joinedSubRoomId !== null) {
    const { sfuUrl, token, iceConfig } = bufferedMediaToken;
    bufferedMediaToken = null;
    connectMedia(sfuUrl, token, iceConfig);
  }
}

function flushPendingMediaDisconnectForNoRoom(): void {
  if (!pendingMediaDisconnectForNoRoom) return;
  pendingMediaDisconnectForNoRoom = false;
  disconnectMediaForNoSubRoom();
}

function disconnectMediaForNoSubRoom(): void {
  if (!bufferedMediaToken && latestMediaToken) {
    bufferedMediaToken = latestMediaToken;
  }
  if (lkModule) {
    // Stop any local screen share still publishing on this connection immediately.
    // stopLocalShareAfterLeavingSubRoom() may have already kicked off its own
    // fire-and-forget stopShare(), but that call reaches lkModule.stopScreenShare()
    // only after several awaited native-capture invokes — well after this
    // synchronous disconnect nulls lkModule below.
    void lkModule.stopScreenShare().catch(() => {});
    suppressMediaDisconnectedReconnect = true;
    try {
      lkModule.disconnect();
    } finally {
      suppressMediaDisconnectedReconnect = false;
    }
    lkModule = null;
  }
  state.mediaState = 'disconnected';
  state.mediaError = null;
  state.nativeMicBridgeActive = false;
  state.noiseSuppressionActive = false;
  state.shareQualityInfo = null;
  state.shareStats = null;
  state.videoReceiveStats = null;
  state.screenShareStreams = new Map();
  mediaConnectedParticipantIds.clear();
  knownLiveKitIdentities.clear();
  markAllParticipantsMediaConnecting();
  remoteCameraTilesById = {};
  rebuildVideoTiles();
  stopPeriodicMediaRetry();
  setActiveLiveKitModule(null);
  if (registeredHotkey) {
    unregisterMuteHotkey(registeredHotkey).catch(() => {});
    registeredHotkey = null;
  }
}

function promoteReconnectedSessionIfReady(): void {
  if (
    state.machineState !== 'reconnecting' ||
    state.mediaState !== 'connected' ||
    state.selfParticipantId === null ||
    state.roomId === null ||
    state.joinedSubRoomId === null
  ) {
    return;
  }

  state.machineState = 'active';
  if (wasReconnecting) {
    wasReconnecting = false;
    appendEvent({
      id: makeEventId(),
      timestamp: timestamp(),
      type: 'system',
      message: 'reconnected - back online',
    });
  }
}

function ensureInSubRoomForShare(): void {
  if (state.joinedSubRoomId !== null) return;
  throw new Error('Join a room before sharing.');
}

function stopLocalShareAfterLeavingSubRoom(previousJoinedSubRoomId: string | null): void {
  if (!previousJoinedSubRoomId || state.joinedSubRoomId !== null) return;
  const self = state.participants.find((p) => p.id === state.selfParticipantId);
  const hasCustomShare = state.activeVideoShare !== null || state.activeAudioShare !== null;
  const hasFallbackShare =
    self?.isSharing === true ||
    externalShareHelperActive ||
    state.shareQualityInfo !== null ||
    state.shareStats !== null;

  if (hasCustomShare) {
    void stopCustomShare('all');
  } else if (hasFallbackShare) {
    void stopShare();
  }
}

function stopLocalCameraAfterLeavingSubRoom(previousJoinedSubRoomId: string | null): void {
  if (!previousJoinedSubRoomId || state.joinedSubRoomId !== null) return;
  if (!state.cameraIntent) return;
  state.cameraIntent = false;
  void unpublishLocalCamera();
}

function syncDesiredSubRoomPreference(): void {
  state.desiredSubRoomId =
    desiredSubRoomIntent === undefined ? state.joinedSubRoomId : desiredSubRoomIntent;
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
      if (currentRoomId && state.participants.find((p) => p.id === participantId)?.mediaConnected) {
        void playNotificationSound('join');
      }
      continue;
    }

    if (previousJoinedSubRoomId && previousRoomId === previousJoinedSubRoomId) {
      void playNotificationSound('leave');
    }
    if (
      currentJoinedSubRoomId &&
      currentRoomId === currentJoinedSubRoomId &&
      state.participants.find((p) => p.id === participantId)?.mediaConnected
    ) {
      void playNotificationSound('join');
    }
  }
}

function shouldToastJoinForLocalRoom(participantId: string, targetSubRoomId: string): boolean {
  return (
    participantId !== state.selfParticipantId &&
    state.joinedSubRoomId !== null &&
    targetSubRoomId === state.joinedSubRoomId
  );
}

function shouldToastLeaveFromLocalRoom(
  participantId: string,
  previousParticipantSubRoomById: Record<string, string>,
  previousJoinedSubRoomId: string | null,
): boolean {
  return (
    participantId !== state.selfParticipantId &&
    previousJoinedSubRoomId !== null &&
    previousParticipantSubRoomById[participantId] === previousJoinedSubRoomId
  );
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
    displayName: sessionUsername ?? displayNameCache.get(state.selfParticipantId) ?? 'You',
    color: sessionProfileColor ?? colorFor({ id: state.selfParticipantId }),
    role: state.selfIsHost ? 'host' : 'guest',
    isSpeaking: false,
    isMuted: false,
    isHostMuted: false,
    isDeafened: state.isDeafened,
    isSharing: false,
    mediaConnected: false,
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
  console.warn(LOG, `restored missing self participant (${reason})`, {
    selfParticipantId: synthetic.id,
  });
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
    const prefs: ChannelVolumePrefs = {
      master: state.masterVolume,
      participants: participantVols,
      streams: { ...(channelVolumePrefs?.streams ?? {}) },
      streamMutes: { ...(channelVolumePrefs?.streamMutes ?? {}) },
    };
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

const CAMERA_QUALITY_RETRY_INTERVAL_MS = 1_000;
const CAMERA_QUALITY_MAX_ATTEMPTS = 3;

// Exported for tests so retry-timing assertions stay in sync with the source
// of truth — never duplicate these literals.
export const __CAMERA_TEST_HOOKS__ = {
  CAMERA_QUALITY_RETRY_INTERVAL_MS,
  CAMERA_QUALITY_MAX_ATTEMPTS,
} as const;

async function attemptCameraQualityUpdate(
  version: number,
  quality: CameraQuality,
  attempt: number,
): Promise<void> {
  const cameraModule = getCameraMediaModule();
  if (!cameraModule || state.cameraPublication !== 'published') {
    return;
  }

  try {
    await cameraModule.setCameraQuality(quality);
    if (version !== cameraQualityRequestVersion) {
      return;
    }
    // Only record the tier after a successful apply so the dedupe check in
    // applyCameraQualityForShareState doesn't skip a retry after exhaustion.
    lastRequestedCameraQualityTier = quality.tier;
  } catch (error) {
    if (version !== cameraQualityRequestVersion) {
      return;
    }
    if (attempt < CAMERA_QUALITY_MAX_ATTEMPTS) {
      cameraQualityRetryTimer = setTimeout(() => {
        cameraQualityRetryTimer = null;
        void attemptCameraQualityUpdate(version, quality, attempt + 1);
      }, CAMERA_QUALITY_RETRY_INTERVAL_MS);
      return;
    }
    const message = `camera quality update failed after ${CAMERA_QUALITY_MAX_ATTEMPTS} attempts (${quality.tier})`;
    appendCameraSystemEvent(message);
    toast.error(message);
    // Always log retry exhaustion — it's a warning-level event regardless of the debug flag.
    console.warn(LOG, message, error);
  }
}

export async function applyCameraQualityForShareState(): Promise<void> {
  const cameraModule = getCameraMediaModule();
  if (!cameraModule || state.cameraPublication !== 'published') {
    resetCameraQualityController();
    return;
  }

  const quality = desiredCameraQualityForState(state);
  if (lastRequestedCameraQualityTier === quality.tier && cameraQualityRetryTimer === null) {
    return;
  }

  if (cameraQualityRetryTimer) {
    clearTimeout(cameraQualityRetryTimer);
    cameraQualityRetryTimer = null;
  }

  cameraQualityRequestVersion += 1;
  if (DEBUG_VIDEO_FEED) {
    console.log(LOG, '[video-feed] applying camera quality', quality.tier);
  }
  await attemptCameraQualityUpdate(cameraQualityRequestVersion, quality, 1);
}

async function publishLocalCamera(): Promise<void> {
  const cameraModule = getCameraMediaModule();
  if (!cameraModule) {
    state.cameraIntent = false;
    state.cameraPublication = 'idle';
    rebuildVideoTiles();
    notify();
    return;
  }

  const selectedDeviceId =
    isLinuxPlatform() && cameraModule instanceof NativeMediaModule
      ? null
      : await getVideoInputDevice();
  state.cameraSelectedDeviceId = selectedDeviceId;

  try {
    const { effectiveDeviceId, warning } =
      isLinuxPlatform() && cameraModule instanceof NativeMediaModule
        ? { effectiveDeviceId: null, warning: null }
        : await resolveCameraDeviceSelection(selectedDeviceId);
    if (warning) {
      surfaceCameraWarning(warning);
    }

    const quality = desiredCameraQualityForState(state);
    state.cameraPublication = 'publishing';
    notify();

    await cameraModule.publishCamera({
      deviceId: effectiveDeviceId,
      quality,
    });

    state.cameraPublication = 'published';
    lastRequestedCameraQualityTier = quality.tier;
    rebuildVideoTiles();
    notify();
  } catch (error) {
    const cameraError = (
      error && typeof error === 'object' && 'kind' in error ? error : { kind: 'publish_failed' }
    ) as CameraStartError;

    state.cameraIntent = false;
    state.cameraPublication = 'failing';
    surfaceCameraError(cameraError);
    await cameraModule.unpublishCamera().catch(() => {});
    state.cameraPublication = 'idle';
    resetCameraQualityController();
    rebuildVideoTiles();
    notify();
  }
}

async function unpublishLocalCamera(): Promise<void> {
  const cameraModule = getCameraMediaModule();
  try {
    await cameraModule?.unpublishCamera();
  } catch (error) {
    console.warn(LOG, 'unpublishCamera failed, classifying as publish_failed', error);
    surfaceCameraError({ kind: 'publish_failed' });
  } finally {
    state.cameraPublication = 'idle';
    resetCameraQualityController();
    rebuildVideoTiles();
    notify();
  }
}

async function restorePublishedCameraAfterReconnect(): Promise<void> {
  if (!state.cameraIntent || state.cameraPublication !== 'published') {
    return;
  }
  await publishLocalCamera();
}

export async function toggleCameraIntent(): Promise<void> {
  if (
    !getCameraMediaModule() ||
    state.mediaState === 'disconnected' ||
    state.mediaState === 'failed'
  ) {
    return;
  }

  // Serialise concurrent invocations so rapid double-clicks can't interleave
  // a publish and an unpublish, leaving intent and publication out of sync.
  cameraToggleChain = cameraToggleChain
    .then(async () => {
      // Re-check lkModule and mediaState inside the chain — state may have
      // changed while we were waiting for the previous toggle to finish.
      if (
        !getCameraMediaModule() ||
        state.mediaState === 'disconnected' ||
        state.mediaState === 'failed'
      ) {
        return;
      }

      if (state.cameraIntent) {
        state.cameraIntent = false;
        notify();
        await unpublishLocalCamera();
        return;
      }

      state.cameraIntent = true;
      state.cameraPublication = 'opening';
      notify();
      await publishLocalCamera();
    })
    .catch((err) => {
      // publishLocalCamera/unpublishLocalCamera surface their own errors via
      // toast + system event. Anything reaching this catch is unexpected
      // (programming error, lkModule torn down mid-flight, etc.) — log it so
      // it doesn't disappear silently, but keep the chain alive so the next
      // toggle can still run.
      console.warn(LOG, 'unexpected error in camera toggle chain', err);
    });

  return cameraToggleChain;
}

export function selectRoomPanelTab(tab: 'logs' | 'video'): void {
  state.roomPanelManualOverride = tab;
  syncRoomPanelTab();
  notify();
}

export async function changeSelectedCamera(deviceId: string | null): Promise<void> {
  const cameraModule = getCameraMediaModule();
  if (!cameraModule || state.cameraPublication !== 'published') {
    await setVideoInputDevice(deviceId);
    state.cameraSelectedDeviceId = deviceId;
    notify();
    return;
  }

  const previousDeviceId = state.cameraSelectedDeviceId;
  try {
    const { effectiveDeviceId, warning } = await resolveCameraDeviceSelection(deviceId);
    if (warning) {
      surfaceCameraWarning(warning);
    }
    await cameraModule.replaceCameraDevice(effectiveDeviceId);
    await setVideoInputDevice(deviceId);
    state.cameraSelectedDeviceId = deviceId;
    rebuildVideoTiles();
    notify();
  } catch (error) {
    state.cameraSelectedDeviceId = previousDeviceId;
    surfaceCameraWarning(classifyCameraReplaceWarning(error, deviceId));
    rebuildVideoTiles();
    notify();
  }
}

/* ─── Media Lifecycle ────────────────────────────────────────────── */

function sendJoinVoiceRequest(): void {
  if (!client) return;
  const joinMsg: Record<string, unknown> = {
    type: 'join_voice',
    channelId: state.channelId,
    supportsSubRooms: true,
  };
  if (sessionUsername) joinMsg.displayName = sessionUsername;
  if (sessionProfileColor) joinMsg.profileColor = sessionProfileColor;
  client.send(joinMsg);
}

function stopColdStartRetry(): void {
  if (coldStartRetryTimer) {
    clearTimeout(coldStartRetryTimer);
    coldStartRetryTimer = null;
  }
}

function clearMediaTokenRequestTimeout(): void {
  if (mediaTokenRequestTimer) {
    clearTimeout(mediaTokenRequestTimer);
    mediaTokenRequestTimer = null;
  }
}

/**
 * Starts (restarts) the "did the backend ever answer our join_voice" guard.
 * Call this right after sendJoinVoiceRequest(). If no media_token arrives
 * before this fires, the attempt counts as failed and reconnectMedia() runs
 * again — same retry budget/give-up path as an explicit onMediaFailed.
 */
function scheduleMediaTokenRequestTimeout(): void {
  clearMediaTokenRequestTimeout();
  mediaTokenRequestTimer = setTimeout(() => {
    mediaTokenRequestTimer = null;
    if (state.machineState === 'idle') return;
    console.warn(LOG, 'media_token request timed out — no response from backend');
    state.mediaReconnectFailures += 1;
    appendEvent({
      id: makeEventId(),
      timestamp: timestamp(),
      type: 'system',
      message: 'media reconnect timed out waiting for a fresh media token',
    });
    notify();
    void reconnectMedia();
  }, MEDIA_TOKEN_REQUEST_TIMEOUT_MS);
}

function clearBackgroundLeaveTimer(): void {
  if (backgroundLeaveTimer) {
    clearTimeout(backgroundLeaveTimer);
    backgroundLeaveTimer = null;
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

function connectMedia(
  sfuUrl: string,
  token: string,
  iceConfig?: {
    stunUrls: string[];
    turnUrls: string[];
    turnUsername?: string;
    turnCredential?: string;
  },
): void {
  connectedMediaIdentity = decodeMediaTokenIdentity(token);
  // Tear down previous instance if any
  if (lkModule) {
    suppressMediaDisconnectedReconnect = true;
    try {
      lkModule.disconnect();
    } finally {
      suppressMediaDisconnectedReconnect = false;
    }
    lkModule = null;
  }

  state.mediaState = 'connecting';
  state.mediaError = null;
  state.nativeMicBridgeActive = false;
  state.noiseSuppressionActive = false;
  markAllParticipantsMediaConnecting();
  suppressReconnectJoinForParticipantIds.clear();
  knownLiveKitIdentities.clear();
  // Pre-warm the notification-sounds AudioContext before the join sound fires.
  // Without this, the AudioContext is created lazily inside an async callback
  // where the browser may refuse to resume it (no active user gesture).
  prewarmAudioContext();
  notify();

  const restoreConnectedMediaState = (
    options: { playJoinSound: boolean } = { playJoinSound: true },
  ): void => {
    state.mediaState = 'connected';
    state.mediaError = null;
    state.mediaReconnectFailures = 0;
    stopPeriodicMediaRetry();
    clearMediaTokenRequestTimeout();
    const self = selfParticipant();
    if (self) {
      markParticipantMediaConnected(self.id, true, { playJoinSound: options.playJoinSound });
    }
    if (lkModule instanceof NativeMediaModule) {
      markAllRemoteParticipantsMediaConnected(true, { playJoinSound: options.playJoinSound });
    }
    setLocalMicPublishing(shouldPublishLocalMic(self));
    // Apply persisted master volume to the media layer
    if (lkModule) {
      lkModule.setMasterVolume(state.masterVolume);
      // Apply effective participant volumes after reconnect.
      applyEffectiveParticipantVolumes();
    }
    // Register global mute hotkey on media connect success (R22.1)
    getMuteHotkey()
      .then((hotkey) => {
        if (registeredHotkey === hotkey) return;
        if (registeredHotkey) {
          unregisterMuteHotkey(registeredHotkey).catch(() => {});
          registeredHotkey = null;
        }
        registerMuteHotkey(hotkey, toggleSelfMute)
          .then(() => {
            registeredHotkey = hotkey;
          })
          .catch((err) => {
            console.warn(LOG, 'hotkey registration failed:', err);
          });
      })
      .catch(() => {});
    void resumePendingWasapiCapture();
    promoteReconnectedSessionIfReady();
    notify();
  };

  const callbacks: MediaCallbacks = {
    onMediaConnected: () => {
      restoreConnectedMediaState({ playJoinSound: true });
      // Re-apply mute state after media reconnection — if the user was muted,
      // ensure the mic stays muted (LiveKit enables mic by default on connect).
    },
    onMediaReconnecting: () => {
      if (state.mediaState === 'failed' || state.mediaState === 'disconnected') return;
      state.mediaState = 'reconnecting';
      state.mediaError = null;
      markAllParticipantsMediaConnecting();
      suppressReconnectJoinForParticipantIds = new Set(
        state.participants.filter((p) => p.id !== state.selfParticipantId).map((p) => p.id),
      );
      notify();
    },
    onMediaReconnected: () => {
      restoreConnectedMediaState({ playJoinSound: false });
      void restorePublishedCameraAfterReconnect();
    },
    onMediaFailed: (reason) => {
      state.mediaReconnectFailures += 1;
      state.mediaState = 'failed';
      state.mediaError = reason;
      markAllParticipantsMediaConnecting();
      suppressReconnectJoinForParticipantIds.clear();
      appendEvent({
        id: makeEventId(),
        timestamp: timestamp(),
        type: 'system',
        message: `media failed: ${reason}`,
      });
      notify();
    },
    onMediaDisconnected: () => {
      const shouldReconnect =
        !suppressMediaDisconnectedReconnect &&
        state.machineState !== 'idle' &&
        state.mediaState !== 'disconnected' &&
        state.mediaState !== 'failed';
      state.mediaState = 'disconnected';
      markAllParticipantsMediaConnecting();
      suppressReconnectJoinForParticipantIds.clear();
      appendEvent({
        id: makeEventId(),
        timestamp: timestamp(),
        type: 'system',
        message: 'media disconnected — attempting reconnect',
      });
      notify();
      if (shouldReconnect) {
        void reconnectMedia();
      }
    },
    onAudioLevels: (levels) => {
      // Audio levels arrive at ~20Hz per participant, but only the isSpeaking
      // transition is rendered (see voiceIcon in ActiveRoom.tsx) — notifying
      // on every tick forces a full re-render of the room UI for no visible
      // change. Only broadcast when a participant's speaking state flips.
      let changed = false;
      for (const [identity, data] of levels) {
        const participantId = participantIdForLiveKitIdentity(identity);
        const p = state.participants.find((pp) => pp.id === participantId);
        if (p) {
          p.rmsLevel = data.rmsLevel;
          const wasSpeaking = p.isSpeaking;
          p.isSpeaking = updateSpeakingTracker(p.id, data.rmsLevel, p.isSpeaking, p.isMuted);
          if (p.isSpeaking !== wasSpeaking) changed = true;
        } else {
          warnUnknownIdentity(identity);
        }
      }
      if (changed) notify();
    },
    onLocalAudioLevel: (level) => {
      updateSelfRms(level);
    },
    onActiveSpeakers: (speakerIdentities) => {
      const speakerParticipantIds = speakerIdentities.map(participantIdForLiveKitIdentity);
      let changed = false;
      for (const p of state.participants) {
        const isSpeaker = speakerParticipantIds.includes(p.id);
        const wasSpeaking = p.isSpeaking;
        if (isSpeaker && !p.isMuted) {
          // Boost the smoothed RMS in the tracker so the debounce logic
          // converges to speaking within 1–2 frames instead of fighting
          // with onAudioLevels updates that arrive with real (lower) RMS.
          const entry = speakingTracker.get(p.id);
          if (entry) {
            entry.smoothedRms = Math.max(entry.smoothedRms, RMS_START_THRESHOLD + 0.05);
          }
          p.isSpeaking = updateSpeakingTracker(
            p.id,
            RMS_START_THRESHOLD + 0.05,
            p.isSpeaking,
            p.isMuted,
          );
          p.rmsLevel = Math.max(p.rmsLevel, RMS_START_THRESHOLD + 0.05);
        } else if (!isSpeaker) {
          if (p.isSpeaking) {
            // Let the tracker decay naturally — feed a zero-level sample
            // so the EMA + debounce handles the off-transition smoothly.
            p.isSpeaking = updateSpeakingTracker(p.id, 0, p.isSpeaking, p.isMuted);
          }
          // Always clear rmsLevel here, even when isSpeaking was already
          // false — the boost branch above can set rmsLevel via Math.max
          // without necessarily flipping isSpeaking through the debounce
          // (a hangover flap), which would otherwise freeze rmsLevel at
          // 0.11 forever (above the 0.03 threshold other code checks
          // against). Harmless: the analyser rewrites a real value within
          // 50ms whenever audio actually decodes.
          p.rmsLevel = 0;
        }
        if (p.isSpeaking !== wasSpeaking) changed = true;
      }
      if (changed) notify();
    },
    onConnectionQuality: (stats) => {
      state.networkStats = stats;
      notify();
    },
    onRemoteParticipantConnected: (identity) => {
      knownLiveKitIdentities.add(identity);
      const participantId = participantIdForLiveKitIdentity(identity);
      const suppressJoinSound = suppressReconnectJoinForParticipantIds.delete(participantId);
      markParticipantMediaConnected(participantId, true, { playJoinSound: !suppressJoinSound });
      notify();
    },
    onRemoteParticipantDisconnected: (identity) => {
      knownLiveKitIdentities.delete(identity);
      markParticipantMediaConnected(participantIdForLiveKitIdentity(identity), false, {
        playJoinSound: false,
      });
      notify();
    },
    onScreenShareSubscribed: (identity, stream) => {
      const participantId = participantIdForLiveKitIdentity(identity);
      state.screenShareStreams = new Map(state.screenShareStreams);
      state.screenShareStreams.set(participantId, stream);
      // Mark participant as sharing (handles late joiners where TrackSubscribed
      // arrives before share_state signaling message)
      const p = state.participants.find((pp) => pp.id === participantId);
      if (p) {
        if (!p.isSharing) {
          p.isSharing = true;
        }
      } else {
        // The LiveKit identity publishing this track has no matching signaling
        // participant — a sign of the ephemeral peer_id/LiveKit identity drift
        // (see media_token identity mismatch handling above). The stream is stored
        // under an id no one in the UI looks up, so the share icon stays grey.
        console.warn(LOG, `screen share stream subscribed for unknown identity: ${identity}`);
      }
      notify();
    },
    onScreenShareUnsubscribed: (identity) => {
      const participantId = participantIdForLiveKitIdentity(identity);
      state.screenShareStreams = new Map(state.screenShareStreams);
      state.screenShareStreams.delete(participantId);
      // Clear receiver stats when no remote shares remain
      if (state.screenShareStreams.size === 0) {
        state.videoReceiveStats = null;
      }
      // If the participant is still marked as sharing, schedule retries so that
      // a temporary track unsubscription (e.g. LiveKit media reconnect) doesn't
      // leave the icon stuck in "waiting for stream" forever when TrackSubscribed
      // doesn't re-fire (SFU treat-as-resume edge case).
      const unsub = state.participants.find((pp) => pp.id === participantId);
      if (unsub?.isSharing && participantId !== state.selfParticipantId) {
        scheduleRefreshRetries(participantId);
      }
      notify();
    },
    onLocalScreenShareEnded: () => {
      // Clear quality info and stats when share ends
      state.shareQualityInfo = null;
      state.shareStats = null;
      stopMotionDetection();
      // Only send stop_share if we didn't already send it from stopShare()
      // and we're not in the middle of changing the share source
      if (!localStopShareSent && !localSourceChanging && client) {
        client.send({ type: 'stop-share' });
      }
      localStopShareSent = false;
    },
    onParticipantMuteChanged: (identity, isMuted) => {
      const participantId = participantIdForLiveKitIdentity(identity);
      const p = state.participants.find((pp) => pp.id === participantId);
      if (!p) return;

      if (participantId === state.selfParticipantId) {
        if (state.joinedSubRoomId === null) {
          clearSelfAudioActivity(p);
          if (!isMuted) {
            void lkModule?.setMicEnabled(false);
          }
          notify();
          return;
        }
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
            participantId,
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
    onRemoteCameraPublished: (identity) => {
      if (DEBUG_VIDEO_FEED) {
        console.log(LOG, '[video-feed] remote camera published', identity);
      }
    },
    onRemoteCameraReady: (identity, track) => {
      const participantId = participantIdForLiveKitIdentity(identity);
      remoteCameraTilesById = {
        ...remoteCameraTilesById,
        [participantId]: {
          track,
          isMuted: false,
          hasError: false,
        },
      };
      rebuildVideoTiles();
      notify();
    },
    onRemoteCameraMutedChanged: (identity, muted) => {
      const participantId = participantIdForLiveKitIdentity(identity);
      const current = remoteCameraTilesById[participantId];
      // Without noUncheckedIndexedAccess, TS types Record index access as always
      // defined even though a missing key makes it genuinely undefined at runtime.
      // eslint-disable-next-line @typescript-eslint/no-unnecessary-condition
      if (!current) {
        return;
      }
      remoteCameraTilesById = {
        ...remoteCameraTilesById,
        [participantId]: {
          ...current,
          isMuted: muted,
        },
      };
      rebuildVideoTiles();
      notify();
    },
    onRemoteCameraUnpublished: (identity) => {
      const participantId = participantIdForLiveKitIdentity(identity);
      if (!(participantId in remoteCameraTilesById)) {
        return;
      }
      const nextTiles = { ...remoteCameraTilesById };
      delete nextTiles[participantId];
      remoteCameraTilesById = nextTiles;
      rebuildVideoTiles();
      notify();
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
    onAudioOnlySharerAdded: (identity) => {
      const participantId = participantIdForLiveKitIdentity(identity);
      state.audioOnlySharers = new Set(state.audioOnlySharers);
      state.audioOnlySharers.add(participantId);
      // Silence immediately so the gain node is created at 0 before any audio
      // plays. The viewer un-mutes by clicking the music icon to restore volume.
      setScreenShareAudioVolume(participantId, 0);
      notify();
    },
    onAudioOnlySharerRemoved: (identity) => {
      const participantId = participantIdForLiveKitIdentity(identity);
      state.audioOnlySharers = new Set(state.audioOnlySharers);
      state.audioOnlySharers.delete(participantId);
      state.confirmedAudioOnlySharers = new Set(state.confirmedAudioOnlySharers);
      state.confirmedAudioOnlySharers.delete(participantId);
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

  for (const participant of state.participants) {
    if (participant.isSharing) {
      const shareType = parseRemoteShareType(participant.shareType);
      if (shareType !== undefined || !state.audioOnlySharers.has(participant.id)) {
        updateRemoteShareType(participant.id, shareType);
      }
    }
  }

  applyPassthroughFilterSettings();
  applyEffectiveParticipantVolumes();
  lkModule.connect(sfuUrl, token, iceConfig).catch((err) => {
    state.mediaState = 'failed';
    state.mediaError = err instanceof Error ? err.message : 'Connection failed';
    notify();
  });

  // Wire audio-devices.ts delegation for both media backends.
  setActiveLiveKitModule(lkModule);
}

/* ─── Signaling Dispatcher ──────────────────────────────────────── */

function dispatchMessage(raw: unknown): void {
  const parsed = parseSignalingMessage(raw);
  if (parsed.kind === 'invalid') {
    console.warn(LOG, 'invalid message received:', raw);
    return;
  }
  if (parsed.kind === 'unknown') {
    console.warn(LOG, 'unknown message type:', parsed.type);
    return;
  }
  const msg = parsed.msg;
  const type = msg.type;

  // Diagnostic: log media_token arrival
  if (type === 'media_token' || type === 'joined') {
    console.log(
      LOG,
      `dispatchMessage: type=${type}, machineState=${state.machineState}, mediaState=${state.mediaState}, lkModule=${lkModule ? 'set' : 'null'}`,
    );
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
        console.warn(
          LOG,
          `auth_failed — refreshing token (attempt ${authRefreshRetries}/${MAX_AUTH_REFRESH_RETRIES})`,
        );
        void refreshTokens().then((result) => {
          if (result.status !== 'success' || !client) {
            console.warn(LOG, 'token refresh failed after auth_failed:', result.status);
            state.error = msg.reason || 'Authentication failed';
            playLocalDisconnectSoundOnce();
            cleanupPublishedMediaForSessionEnd();
            state.machineState = 'idle';
            notify();
            return;
          }
          void getServerUrl().then((serverUrl) => {
            if (!serverUrl || !client) return;
            client.reconnectWithNewToken(toWsUrl(serverUrl)).catch((err) => {
              console.error(LOG, 'reconnect after refresh failed:', err);
              state.error = 'Authentication failed';
              playLocalDisconnectSoundOnce();
              cleanupPublishedMediaForSessionEnd();
              state.machineState = 'idle';
              notify();
            });
          });
        });
        break;
      }

      state.error = msg.reason || 'Authentication failed';
      if (client) {
        client.disconnect();
      }
      playLocalDisconnectSoundOnce();
      cleanupPublishedMediaForSessionEnd();
      state.machineState = 'idle';
      notify();
      break;
    }

    case 'joined': {
      const joinedActiveSession = state.machineState !== 'idle';
      stopColdStartRetry();
      state.serverStartingEstimatedWaitSecs = null;
      state.lastRateLimitError = null;
      state.selfParticipantId = msg.peerId;
      state.roomId = msg.roomId;
      bufferedIceConfig = msg.iceConfig ?? null;
      if (joinedActiveSession) {
        localSessionJoined = true;
        localDisconnectSoundPlayed = false;
      }
      syncDerivedSubRoomState();
      syncDesiredSubRoomPreference();
      state.sharePermission = msg.sharePermission === 'host_only' ? 'host_only' : 'anyone';
      const participants = msg.participants || [];
      const incomingParticipants = participants.slice(0, MAX_PARTICIPANTS).map((p) => {
        displayNameCache.set(p.participantId, p.displayName);
        const isSelf = p.participantId === state.selfParticipantId;
        const pUserId = p.userId;
        // Annotated so the literal union doesn't widen to string in the
        // non-contextually-typed object literal below.
        const role: ParticipantRole = isSelf && state.selfIsHost ? 'host' : 'guest';
        return {
          id: p.participantId,
          userId: pUserId,
          displayName: p.displayName,
          color:
            isSelf && sessionProfileColor
              ? sessionProfileColor
              : (p.profileColor ?? colorFor({ userId: pUserId, id: p.participantId })),
          role,
          isSpeaking: false,
          isMuted: Boolean(p.isMuted),
          isHostMuted: Boolean(p.isHostMuted),
          isDeafened: Boolean(p.isDeafened),
          isSharing: false,
          mediaConnected: !isSelf && mediaConnectedParticipantIds.has(p.participantId),
          rmsLevel: 0,
          volume: resolvePersistedVolume(pUserId, state.defaultVolume),
        };
      });
      state.participants = mergeParticipantsWithVolume(state.participants, incomingParticipants);
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
        appendEvent({
          id: makeEventId(),
          timestamp: timestamp(),
          type: 'system',
          message: 'reconnected — back online',
        });
      }

      // Belt-and-suspenders: flush if the backend ever sends joined with the
      // user already in a sub-room. Normally fires on sub_room_joined/sub_room_state.
      flushBufferedMediaTokenIfReady();

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
      const rawReason = msg.reason || 'unknown';
      console.warn(LOG, `join_rejected reason=${rawReason} channelId=${state.channelId}`);
      if (state.machineState === 'server_starting') {
        stopColdStartRetry();
        playLocalDisconnectSoundOnce();
        cleanupPublishedMediaForSessionEnd();
        state.machineState = 'idle';
        state.serverStartingEstimatedWaitSecs = null;
        state.rejectionReason = msg.reason || 'Server failed to start';
        notify();
        break;
      }
      // Map wire reasons to user-friendly messages
      const friendlyMessages: Record<string, string> = {
        not_authorized:
          'Unable to join voice. You may not be a member, or there was a server-side issue. Try again.',
        room_full: 'Room is full (max 6 participants).',
        invite_required: 'An invite code is required to join.',
        invite_exhausted: 'The invite code has been fully used.',
        invite_expired: 'The invite code has expired.',
        rate_limited: 'Too many join attempts — please wait a moment before retrying.',
      };
      state.rejectionReason = friendlyMessages[rawReason] || `Join rejected: ${rawReason}`;
      if (client) {
        client.disconnect();
      }
      client = null;
      playLocalDisconnectSoundOnce();
      cleanupPublishedMediaForSessionEnd();
      state.machineState = 'idle';
      notify();
      break;
    }

    case 'participant_joined': {
      if (state.participants.length >= MAX_PARTICIPANTS) return;
      const pjId = msg.participantId;
      const pjName = msg.displayName;
      const pjUserId = msg.userId;
      displayNameCache.set(pjId, pjName);
      const newParticipant: RoomParticipant = {
        id: pjId,
        userId: pjUserId,
        displayName: pjName,
        color: msg.profileColor ?? colorFor({ userId: pjUserId, id: pjId }),
        role: 'guest',
        isSpeaking: false,
        isMuted: false,
        isHostMuted: false,
        isDeafened: false,
        isSharing: false,
        mediaConnected: pjId !== state.selfParticipantId && mediaConnectedParticipantIds.has(pjId),
        rmsLevel: 0,
        volume: resolvePersistedVolume(pjUserId, state.defaultVolume),
      };
      const withoutExisting = state.participants.filter((p) => p.id !== pjId);
      state.participants = mergeParticipantsWithVolume(state.participants, [
        ...withoutExisting,
        newParticipant,
      ]).slice(0, MAX_PARTICIPANTS);
      if (lkModule && state.mediaState === 'connected' && pjId !== state.selfParticipantId) {
        applyEffectiveParticipantVolume(newParticipant);
      }
      rebuildVideoTiles();
      notify();
      break;
    }

    case 'participant_left': {
      const previousParticipantSubRoomById = { ...state.participantSubRoomById };
      const previousJoinedSubRoomId = state.joinedSubRoomId;
      const leftId = msg.participantId;
      const leftP = state.participants.find((p) => p.id === leftId);
      mediaConnectedParticipantIds.delete(leftId);
      suppressReconnectJoinForParticipantIds.delete(leftId);
      state.participants = state.participants.filter((p) => p.id !== leftId);
      if (state.participantSubRoomById[leftId]) {
        const leftRoomId = state.participantSubRoomById[leftId];
        const plRoom = state.subRooms.find((r) => r.id === leftRoomId);
        const plRoomLabel = plRoom ? `Room ${plRoom.roomNumber}` : 'a room';
        state.subRooms = state.subRooms.map((room) =>
          room.id === leftRoomId
            ? { ...room, participantIds: room.participantIds.filter((id) => id !== leftId) }
            : room,
        );
        syncDerivedSubRoomState();
        reconcileLocalMicWithRoomMembership(previousJoinedSubRoomId);
        playSubRoomMembershipSounds(previousParticipantSubRoomById, previousJoinedSubRoomId);
        applyEffectiveParticipantVolumes();
        const plName = leftP?.displayName ?? displayNameCache.get(leftId) ?? leftId;
        appendEvent({
          id: makeEventId(),
          timestamp: timestamp(),
          type: 'leave',
          message: `${plName} left ${plRoomLabel}`,
          participantId: leftId,
          shouldToast: shouldToastLeaveFromLocalRoom(
            leftId,
            previousParticipantSubRoomById,
            previousJoinedSubRoomId,
          ),
        });
      }
      speakingTracker.delete(leftId);
      if (leftId in remoteCameraTilesById) {
        const nextTiles = { ...remoteCameraTilesById };
        delete nextTiles[leftId];
        remoteCameraTilesById = nextTiles;
      }
      rebuildVideoTiles();
      void applyCameraQualityForShareState();
      notify();
      break;
    }

    case 'sub_room_state': {
      const previousParticipantSubRoomById = { ...state.participantSubRoomById };
      const previousJoinedSubRoomId = state.joinedSubRoomId;
      const previousPassthrough = state.passthrough;
      const rooms = (msg.rooms || [])
        .map((room) => ({
          id: room.subRoomId,
          roomNumber: room.roomNumber,
          isDefault: Boolean(room.isDefault),
          participantIds: Array.isArray(room.participantIds)
            ? (room.participantIds as string[]).slice()
            : [],
          deleteAtMs: typeof room.deleteAtMs === 'number' ? room.deleteAtMs : null,
        }))
        .sort((a, b) => a.roomNumber - b.roomNumber);
      state.subRooms = rooms;
      const passthrough = msg.passthrough;
      state.passthrough =
        passthrough &&
        typeof passthrough.sourceSubRoomId === 'string' &&
        typeof passthrough.targetSubRoomId === 'string' &&
        typeof passthrough.label === 'string'
          ? {
              sourceSubRoomId: passthrough.sourceSubRoomId,
              targetSubRoomId: passthrough.targetSubRoomId,
              label: passthrough.label,
            }
          : null;
      state.passthroughEnabled =
        typeof msg.passthroughEnabled === 'boolean' ? msg.passthroughEnabled : false;
      if (typeof msg.passthroughVolumePercent === 'number') {
        state.passthroughVolume = Math.max(
          0,
          Math.min(100, Math.round(msg.passthroughVolumePercent)),
        );
      }
      if (typeof msg.passthroughFiltersEnabled === 'boolean') {
        state.passthroughFiltersEnabled = msg.passthroughFiltersEnabled;
      }
      if (typeof msg.passthroughFilterStrength === 'number') {
        state.passthroughFilterStrength = Math.max(
          0,
          Math.min(100, Math.round(msg.passthroughFilterStrength)),
        );
      }
      // Detect passthrough changes and emit a log event + notification
      {
        const newPassthrough = state.passthrough;
        const wasActive = !!previousPassthrough;
        const isActive = !!newPassthrough;
        const pairChanged =
          wasActive &&
          isActive &&
          (previousPassthrough.sourceSubRoomId !== newPassthrough.sourceSubRoomId ||
            previousPassthrough.targetSubRoomId !== newPassthrough.targetSubRoomId);
        if (!wasActive && isActive) {
          appendEvent({
            id: makeEventId(),
            timestamp: timestamp(),
            type: 'passthrough',
            message: `Passthrough turned on for rooms ${newPassthrough.label}`,
          });
        } else if (wasActive && !isActive) {
          appendEvent({
            id: makeEventId(),
            timestamp: timestamp(),
            type: 'passthrough',
            message: `Passthrough turned off for rooms ${previousPassthrough.label}`,
          });
        } else if (pairChanged) {
          appendEvent({
            id: makeEventId(),
            timestamp: timestamp(),
            type: 'passthrough',
            message: `Passthrough turned on for rooms ${newPassthrough.label}`,
          });
        }
      }
      syncDerivedSubRoomState();
      applyPassthroughFilterSettings();
      stopLocalShareAfterLeavingSubRoom(previousJoinedSubRoomId);
      stopLocalCameraAfterLeavingSubRoom(previousJoinedSubRoomId);
      reconcileLocalMicWithRoomMembership(previousJoinedSubRoomId);
      playSubRoomMembershipSounds(previousParticipantSubRoomById, previousJoinedSubRoomId);
      applyEffectiveParticipantVolumes();
      rebuildVideoTiles();
      reconcileDesiredSubRoomMembership();
      // Legacy-client fallback: sub_room_joined fires before sub_room_state (room doesn't exist
      // yet in state when sub_room_joined arrives), so flush here once state catches up.
      flushBufferedMediaTokenIfReady();
      promoteReconnectedSessionIfReady();
      void applyCameraQualityForShareState();
      notify();
      break;
    }

    case 'sub_room_created': {
      const room = msg.room;
      if (!room) break;
      const createdRoom: VoiceSubRoom = {
        id: room.subRoomId,
        roomNumber: room.roomNumber,
        isDefault: Boolean(room.isDefault),
        participantIds: Array.isArray(room.participantIds)
          ? (room.participantIds as string[]).slice()
          : [],
        deleteAtMs: typeof room.deleteAtMs === 'number' ? room.deleteAtMs : null,
      };
      state.subRooms = [
        ...state.subRooms.filter((existing) => existing.id !== createdRoom.id),
        createdRoom,
      ].sort((a, b) => a.roomNumber - b.roomNumber);
      syncDerivedSubRoomState();
      applyEffectiveParticipantVolumes();
      reconcileDesiredSubRoomMembership();
      void applyCameraQualityForShareState();
      notify();
      break;
    }

    case 'sub_room_joined': {
      const previousParticipantSubRoomById = { ...state.participantSubRoomById };
      const previousJoinedSubRoomId = state.joinedSubRoomId;
      const participantId = msg.participantId;
      const subRoomId = msg.subRoomId;
      const source = msg.source === 'legacy_room_one' ? 'legacy_room_one' : 'explicit';
      state.subRooms = state.subRooms.map((room) => {
        if (room.id === subRoomId) {
          return room.participantIds.includes(participantId)
            ? { ...room, deleteAtMs: null }
            : {
                ...room,
                participantIds: [...room.participantIds, participantId],
                deleteAtMs: null,
              };
        }
        return room.participantIds.includes(participantId)
          ? { ...room, participantIds: room.participantIds.filter((id) => id !== participantId) }
          : room;
      });
      syncDerivedSubRoomState();
      stopLocalShareAfterLeavingSubRoom(previousJoinedSubRoomId);
      stopLocalCameraAfterLeavingSubRoom(previousJoinedSubRoomId);
      reconcileLocalMicWithRoomMembership(previousJoinedSubRoomId);
      void source;
      playSubRoomMembershipSounds(previousParticipantSubRoomById, previousJoinedSubRoomId);
      applyEffectiveParticipantVolumes();
      rebuildVideoTiles();
      reconcileDesiredSubRoomMembership();
      {
        const srjName =
          displayNameCache.get(participantId) ??
          state.participants.find((p) => p.id === participantId)?.displayName ??
          participantId;
        const srjRoom = state.subRooms.find((r) => r.id === subRoomId);
        const srjRoomLabel = srjRoom ? `Room ${srjRoom.roomNumber}` : 'a room';
        const srjWasInRoom = !!previousParticipantSubRoomById[participantId];
        const srjIsSelf = participantId === state.selfParticipantId;
        const srjMessage = srjIsSelf
          ? srjWasInRoom
            ? `moved to ${srjRoomLabel}`
            : `joined ${srjRoomLabel}`
          : srjWasInRoom
            ? `${srjName} moved to ${srjRoomLabel}`
            : `${srjName} joined ${srjRoomLabel}`;
        appendEvent({
          id: makeEventId(),
          timestamp: timestamp(),
          type: 'join',
          message: srjMessage,
          participantId,
          shouldToast: shouldToastJoinForLocalRoom(participantId, subRoomId),
        });
      }
      if (participantId === state.selfParticipantId) {
        flushBufferedMediaTokenIfReady();
        promoteReconnectedSessionIfReady();
      }
      void applyCameraQualityForShareState();
      notify();
      break;
    }

    case 'sub_room_left': {
      const previousParticipantSubRoomById = { ...state.participantSubRoomById };
      const previousJoinedSubRoomId = state.joinedSubRoomId;
      const participantId = msg.participantId;
      const subRoomId = msg.subRoomId;
      const srlRoom = state.subRooms.find((r) => r.id === subRoomId);
      const srlRoomLabel = srlRoom ? `Room ${srlRoom.roomNumber}` : 'a room';
      state.subRooms = state.subRooms.map((room) =>
        room.id === subRoomId
          ? { ...room, participantIds: room.participantIds.filter((id) => id !== participantId) }
          : room,
      );
      syncDerivedSubRoomState();
      stopLocalShareAfterLeavingSubRoom(previousJoinedSubRoomId);
      stopLocalCameraAfterLeavingSubRoom(previousJoinedSubRoomId);
      reconcileLocalMicWithRoomMembership(previousJoinedSubRoomId);
      playSubRoomMembershipSounds(previousParticipantSubRoomById, previousJoinedSubRoomId);
      applyEffectiveParticipantVolumes();
      rebuildVideoTiles();
      reconcileDesiredSubRoomMembership();
      {
        const srlName =
          displayNameCache.get(participantId) ??
          state.participants.find((p) => p.id === participantId)?.displayName ??
          participantId;
        const srlIsSelf = participantId === state.selfParticipantId;
        const srlMessage = srlIsSelf ? `left ${srlRoomLabel}` : `${srlName} left ${srlRoomLabel}`;
        appendEvent({
          id: makeEventId(),
          timestamp: timestamp(),
          type: 'leave',
          message: srlMessage,
          participantId,
          shouldToast: shouldToastLeaveFromLocalRoom(
            participantId,
            previousParticipantSubRoomById,
            previousJoinedSubRoomId,
          ),
        });
      }
      void applyCameraQualityForShareState();
      notify();
      break;
    }

    case 'sub_room_deleted': {
      const previousJoinedSubRoomId = state.joinedSubRoomId;
      const subRoomId = msg.subRoomId;
      state.subRooms = state.subRooms.filter((room) => room.id !== subRoomId);
      syncDerivedSubRoomState();
      stopLocalShareAfterLeavingSubRoom(previousJoinedSubRoomId);
      stopLocalCameraAfterLeavingSubRoom(previousJoinedSubRoomId);
      reconcileLocalMicWithRoomMembership(previousJoinedSubRoomId);
      if (desiredSubRoomIntent === subRoomId) {
        desiredSubRoomIntent = undefined;
      }
      applyEffectiveParticipantVolumes();
      rebuildVideoTiles();
      reconcileDesiredSubRoomMembership();
      void applyCameraQualityForShareState();
      notify();
      break;
    }

    case 'room_state': {
      // room_state contains only pre-join participants (excludes the joiner).
      // Merge with existing list to preserve self and any participants already
      // added via participant_joined that arrived before this snapshot.
      const rsParticipants = msg.participants || [];
      const incoming = rsParticipants.slice(0, MAX_PARTICIPANTS).map((p) => {
        displayNameCache.set(p.participantId, p.displayName);
        const isSelf = p.participantId === state.selfParticipantId;
        const rsUserId = p.userId;
        // Annotated so the literal union doesn't widen to string in the
        // non-contextually-typed object literal below.
        const role: ParticipantRole = isSelf && state.selfIsHost ? 'host' : 'guest';
        return {
          id: p.participantId,
          userId: rsUserId,
          displayName: p.displayName,
          color:
            isSelf && sessionProfileColor
              ? sessionProfileColor
              : (p.profileColor ?? colorFor({ userId: rsUserId, id: p.participantId })),
          role,
          isSpeaking: false,
          isMuted: Boolean(p.isMuted),
          isHostMuted: Boolean(p.isHostMuted),
          isDeafened: Boolean(p.isDeafened),
          isSharing: false,
          mediaConnected: !isSelf && mediaConnectedParticipantIds.has(p.participantId),
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

      rebuildVideoTiles();
      notify();
      break;
    }

    case 'participant_kicked': {
      const kickedId = msg.participantId;
      const kickedP = state.participants.find((p) => p.id === kickedId);
      mediaConnectedParticipantIds.delete(kickedId);
      suppressReconnectJoinForParticipantIds.delete(kickedId);
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
          unregisterMuteHotkey(registeredHotkey).catch(() => {});
          registeredHotkey = null;
        }
        if (client) {
          client.disconnect();
        }
        client = null;
        playLocalDisconnectSoundOnce();
        cleanupPublishedMediaForSessionEnd();
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
        unregisterMuteHotkey(registeredHotkey).catch(() => {});
        registeredHotkey = null;
      }
      // Intentional disconnect — suppress reconnect
      if (client) {
        client.disconnect();
      }
      client = null;
      playLocalDisconnectSoundOnce();
      cleanupPublishedMediaForSessionEnd();
      state.machineState = 'idle';
      stopColdStartRetry();
      stopPeriodicMediaRetry();
      notify();
      break;
    }

    case 'participant_muted': {
      const mutedId = msg.participantId;
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
        void lkModule?.setMicEnabled(false);
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
      const unmutedId = msg.participantId;
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

    case 'participant_self_muted': {
      const selfMutedId = msg.participantId;
      console.log(LOG, `received participant_self_muted for ${selfMutedId}`);
      const smp = state.participants.find((pp) => pp.id === selfMutedId);
      if (smp) {
        smp.isMuted = true;
        smp.rmsLevel = 0;
        smp.isSpeaking = false;
      }
      if (selfMutedId === state.selfParticipantId) {
        // Server echo — local state already set by toggleSelfMute
      } else if (smp) {
        appendEvent({
          id: makeEventId(),
          timestamp: timestamp(),
          type: 'muted',
          message: `${smp.displayName} muted microphone`,
          participantId: selfMutedId,
        });
      }
      notify();
      break;
    }

    case 'participant_self_unmuted': {
      const selfUnmutedId = msg.participantId;
      console.log(LOG, `received participant_self_unmuted for ${selfUnmutedId}`);
      const sup = state.participants.find((pp) => pp.id === selfUnmutedId);
      if (sup) {
        sup.isMuted = false;
      }
      if (selfUnmutedId === state.selfParticipantId) {
        // Server echo — local state already set by toggleSelfMute
      } else if (sup) {
        appendEvent({
          id: makeEventId(),
          timestamp: timestamp(),
          type: 'unmuted',
          message: `${sup.displayName} unmuted microphone`,
          participantId: selfUnmutedId,
        });
      }
      notify();
      break;
    }

    case 'participant_deafened': {
      const deafId = msg.participantId;
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
      const undeafId = msg.participantId;
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
      const coloredId = msg.participantId;
      const cp = state.participants.find((pp) => pp.id === coloredId);
      if (cp) {
        cp.color = msg.profileColor;
      }
      rebuildVideoTiles();
      notify();
      break;
    }

    case 'participant_username_updated': {
      const participantId = msg.participantId;
      const username = msg.username;
      const participant = state.participants.find((pp) => pp.id === participantId);
      if (participant) {
        participant.displayName = username;
      }
      displayNameCache.set(participantId, username);
      rebuildVideoTiles();
      notify();
      break;
    }

    case 'share_started': {
      const shareStartId = msg.participantId;
      const shareStartName = msg.displayName;
      const remoteShareType = parseRemoteShareType(msg.shareType);
      const sp = state.participants.find((pp) => pp.id === shareStartId);
      if (sp) {
        sp.isSharing = true;
        // Video-type share and audio-only share are independent slots — a
        // participant may run both concurrently. Only overwrite the video
        // slot when this event is itself for a video-type share (or a
        // legacy untyped restart); an audio_only start must never clobber
        // an already-recorded video type.
        if (remoteShareType !== 'audio_only') {
          sp.shareType = remoteShareType;
        }
      }
      // Keep audioOnlySharers in sync directly — updateRemoteShareType is a no-op
      // when lkModule is not yet initialized, which would leave the UI stuck on ◉.
      // A video share starting must never clear a CONFIRMED audio-only flag
      // (and vice versa) — the two slots are independent, only an explicit
      // share_stopped for that slot does. An UNCONFIRMED flag (LiveKit's
      // TrackSubscribed-based onAudioOnlySharerAdded guess, made before any
      // signaling arrives) is a different case: the first real video-type
      // share_started is authoritative and must correct that guess.
      if (remoteShareType === 'audio_only') {
        if (!state.audioOnlySharers.has(shareStartId)) {
          state.audioOnlySharers = new Set(state.audioOnlySharers);
          state.audioOnlySharers.add(shareStartId);
        }
        if (!state.confirmedAudioOnlySharers.has(shareStartId)) {
          state.confirmedAudioOnlySharers = new Set(state.confirmedAudioOnlySharers);
          state.confirmedAudioOnlySharers.add(shareStartId);
        }
      } else if (
        remoteShareType !== undefined &&
        state.audioOnlySharers.has(shareStartId) &&
        !state.confirmedAudioOnlySharers.has(shareStartId)
      ) {
        state.audioOnlySharers = new Set(state.audioOnlySharers);
        state.audioOnlySharers.delete(shareStartId);
      }
      // Don't pass undefined to lkModule when the track was already inferred as audio-only —
      // setRemoteShareType(undefined) would clear the inference and fire onAudioOnlySharerRemoved.
      if (remoteShareType !== undefined || !state.audioOnlySharers.has(shareStartId)) {
        updateRemoteShareType(shareStartId, remoteShareType);
      }
      const shareStartIsAudioOnly =
        remoteShareType === 'audio_only' ||
        (remoteShareType === undefined && state.audioOnlySharers.has(shareStartId));
      if (!shareStartIsAudioOnly) {
        refreshRemoteScreenShare(shareStartId);
        // Schedule retries for the case where TrackSubscribed is delayed or the SFU
        // treats the republish as a track resume (no TrackPublished/TrackSubscribed).
        if (shareStartId !== state.selfParticipantId) {
          scheduleRefreshRetries(shareStartId);
        }
      }
      // Cache the display name from the server payload
      if (shareStartName) {
        displayNameCache.set(shareStartId, shareStartName);
      }
      const resolvedStartName =
        sp?.displayName ?? shareStartName ?? displayNameCache.get(shareStartId) ?? shareStartId;
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
      void applyCameraQualityForShareState();
      notify();
      break;
    }

    case 'share_stopped': {
      const shareStopId = msg.participantId;
      const shareStopName = msg.displayName;
      // share_type is set when only one slot stopped (the other, if any,
      // keeps running); undefined means every slot for this participant
      // stopped (legacy behavior / host-directed stop-all).
      const stoppedType = parseRemoteShareType(msg.shareType);
      const stopsVideoSlot = stoppedType !== 'audio_only';
      const stopsAudioSlot = stoppedType === undefined || stoppedType === 'audio_only';
      const ssp = state.participants.find((pp) => pp.id === shareStopId);
      if (ssp && stopsVideoSlot) {
        ssp.shareType = undefined;
      }
      if (stopsAudioSlot && state.audioOnlySharers.has(shareStopId)) {
        state.audioOnlySharers = new Set(state.audioOnlySharers);
        state.audioOnlySharers.delete(shareStopId);
        if (state.confirmedAudioOnlySharers.has(shareStopId)) {
          state.confirmedAudioOnlySharers = new Set(state.confirmedAudioOnlySharers);
          state.confirmedAudioOnlySharers.delete(shareStopId);
        }
      }
      if (ssp) {
        ssp.isSharing = ssp.shareType !== undefined || state.audioOnlySharers.has(shareStopId);
      }
      if (stopsVideoSlot) {
        clearRemoteShareType(shareStopId);
        refreshRetryGenerations.delete(shareStopId);
        // Clear the stream reference immediately on signaling. The LiveKit
        // TrackUnsubscribed event may lag by the SFU's disconnect timeout when
        // the sharer closes the app abruptly; without this the viewer's UI shows
        // a stale frozen frame until the SFU eventually fires the event.
        if (state.screenShareStreams.has(shareStopId)) {
          state.screenShareStreams = new Map(state.screenShareStreams);
          state.screenShareStreams.delete(shareStopId);
        }
      }
      // Cache the display name from the server payload
      if (shareStopName) {
        displayNameCache.set(shareStopId, shareStopName);
      }
      const resolvedStopName =
        ssp?.displayName ?? shareStopName ?? displayNameCache.get(shareStopId) ?? shareStopId;
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
      void applyCameraQualityForShareState();
      notify();
      break;
    }

    case 'share_state': {
      // share_state is an authoritative snapshot — reconcile all participants.
      // A participant may have up to two entries in activeShares (one video-type
      // slot, one audio-only slot) — collect them per participant rather than
      // collapsing to a single type, or a late joiner only ever learns about
      // whichever slot happened to be listed last.
      const shareIds = new Set(msg.participantIds || []);
      const typedShares = new Map<string, Set<RemoteShareType>>();
      for (const share of msg.activeShares || []) {
        const t = parseRemoteShareType(share.shareType);
        if (t === undefined) continue;
        const existing = typedShares.get(share.participantId);
        if (existing) {
          existing.add(t);
        } else {
          typedShares.set(share.participantId, new Set([t]));
        }
      }
      for (const p of state.participants) {
        const shouldBeSharing = shareIds.has(p.id);
        if (p.isSharing && !shouldBeSharing) {
          // Server says they're not sharing — clear stale flag + stream
          p.isSharing = false;
          p.shareType = undefined;
          if (state.audioOnlySharers.has(p.id)) {
            state.audioOnlySharers = new Set(state.audioOnlySharers);
            state.audioOnlySharers.delete(p.id);
          }
          if (state.confirmedAudioOnlySharers.has(p.id)) {
            state.confirmedAudioOnlySharers = new Set(state.confirmedAudioOnlySharers);
            state.confirmedAudioOnlySharers.delete(p.id);
          }
          clearRemoteShareType(p.id);
          if (state.screenShareStreams.has(p.id)) {
            state.screenShareStreams = new Map(state.screenShareStreams);
            state.screenShareStreams.delete(p.id);
          }
        } else if (!p.isSharing && shouldBeSharing) {
          p.isSharing = true;
        }
        if (shouldBeSharing) {
          const types = typedShares.get(p.id);
          const videoType = types ? [...types].find((t) => t !== 'audio_only') : undefined;
          const hasAudioOnly = types ? types.has('audio_only') : false;
          p.shareType = videoType;
          if (hasAudioOnly) {
            if (!state.audioOnlySharers.has(p.id)) {
              state.audioOnlySharers = new Set(state.audioOnlySharers);
              state.audioOnlySharers.add(p.id);
            }
            if (!state.confirmedAudioOnlySharers.has(p.id)) {
              state.confirmedAudioOnlySharers = new Set(state.confirmedAudioOnlySharers);
              state.confirmedAudioOnlySharers.add(p.id);
            }
          } else if (types !== undefined && state.audioOnlySharers.has(p.id)) {
            // Typed snapshot explicitly has no audio-only entry for this
            // participant (it was stopped while we were away) — clear it.
            state.audioOnlySharers = new Set(state.audioOnlySharers);
            state.audioOnlySharers.delete(p.id);
            if (state.confirmedAudioOnlySharers.has(p.id)) {
              state.confirmedAudioOnlySharers = new Set(state.confirmedAudioOnlySharers);
              state.confirmedAudioOnlySharers.delete(p.id);
            }
          }
          // else: no typed entry at all (legacy/untyped sender) — keep
          // whatever audioOnlySharers already holds for this participant.
          if (videoType !== undefined || !state.audioOnlySharers.has(p.id)) {
            updateRemoteShareType(p.id, videoType);
          }
          const isPureAudioOnlyShare =
            types !== undefined
              ? videoType === undefined && hasAudioOnly
              : state.audioOnlySharers.has(p.id);
          if (!isPureAudioOnlyShare) {
            refreshRemoteScreenShare(p.id);
            // Schedule retries for late joiners or post-reconnect cases where
            // the stream isn't available yet. share_state doesn't schedule
            // retries (only share_started does), so viewers who join mid-share
            // and whose TrackSubscribed is delayed get stuck without this.
            if (!hasLiveScreenShareStream(p.id) && p.id !== state.selfParticipantId) {
              scheduleRefreshRetries(p.id);
            }
          }
        }
      }
      void applyCameraQualityForShareState();
      notify();
      break;
    }

    case 'share_permission_changed': {
      const newPerm = msg.permission === 'host_only' ? 'host_only' : 'anyone';
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
      const errorMessage = msg.message || 'Unknown error';
      appendEvent({
        id: makeEventId(),
        timestamp: timestamp(),
        type: 'system',
        message: errorMessage,
      });
      state.lastChatError = errorMessage;
      state.lastRateLimitError = isWsRateLimitError(errorMessage)
        ? RATE_LIMIT_RECONNECT_MESSAGE
        : null;
      notify();
      break;
    }

    case 'peer_left': {
      const peerId = msg.participantId || msg.peerId;
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
      // The backend answered our join_voice request (whatever the payload
      // turns out to contain below) — the "did it ever respond" guard no
      // longer applies to this attempt.
      clearMediaTokenRequestTimeout();
      const token = msg.token;
      const sfuUrl = msg.sfuUrl;
      // ice_config lives on the Joined payload, not MediaToken — fall back to what was stored from 'joined'.
      const iceConfig = msg.iceConfig ?? bufferedIceConfig ?? undefined;

      console.log(LOG, 'media_token received:', {
        hasToken: !!token,
        hasSfuUrl: !!sfuUrl,
        hasIceConfig: !!iceConfig,
        iceConfigSource: msg.iceConfig ? 'media_token' : bufferedIceConfig ? 'joined' : 'none',
        iceConfig: iceConfig
          ? {
              stunUrls: iceConfig.stunUrls,
              turnUrls: iceConfig.turnUrls,
              hasTurnCredentials: !!(iceConfig.turnUsername && iceConfig.turnCredential),
            }
          : null,
      });

      if (!token || !sfuUrl) {
        appendEvent({
          id: makeEventId(),
          timestamp: timestamp(),
          type: 'system',
          message: 'media_token: empty token or sfuUrl',
        });
        notify();
        break;
      }

      latestMediaToken = { sfuUrl, token, iceConfig };

      if (state.machineState !== 'active' || state.joinedSubRoomId === null) {
        bufferedMediaToken = latestMediaToken;
        break;
      }

      // Media already connected — normally this is just a proactive refresh from the
      // backend and LiveKit's own reconnection handles it without a visible hiccup.
      // But if a silent WS drop reissued us a new signaling identity while the LiveKit
      // session survived under its old one (see media_token identity mismatch below),
      // the new token's identity will differ from what we're actually connected as —
      // in that case we must force a reconnect or the sharer/viewer identities split
      // permanently and screen-share tracks publish under an identity nobody is
      // looking for.
      if (
        state.mediaState === 'connected' ||
        state.mediaState === 'connecting' ||
        state.mediaState === 'reconnecting'
      ) {
        const newIdentity = decodeMediaTokenIdentity(token);
        if (newIdentity && connectedMediaIdentity && newIdentity !== connectedMediaIdentity) {
          console.log(
            LOG,
            `media_token identity mismatch (connected as ${connectedMediaIdentity}, token is for ${newIdentity}) — forcing media reconnect`,
          );
          appendEvent({
            id: makeEventId(),
            timestamp: timestamp(),
            type: 'system',
            message: 'media identity changed — reconnecting media session',
          });
          connectMedia(sfuUrl, token, iceConfig);
          break;
        }
        console.log(
          LOG,
          `media_token received while media ${state.mediaState} — ignoring (no reconnect needed)`,
        );
        break;
      }

      // When media is in failed state, check if auto-reconnect retries remain
      if (state.mediaState === 'failed') {
        void getReconnectConfig().then((config) => {
          if (state.mediaReconnectFailures < config.maxRetries) {
            connectMedia(sfuUrl, token, iceConfig);
          } else {
            startPeriodicMediaRetry();
            appendEvent({
              id: makeEventId(),
              timestamp: timestamp(),
              type: 'system',
              message: 'media_token ignored — retries exhausted, periodic retry active',
            });
            playLocalDisconnectSoundOnce();
            notify();
          }
        });
        break;
      }

      connectMedia(sfuUrl, token, iceConfig);
      break;
    }

    case 'viewer_token': {
      const windowId = msg.windowId;
      const windowLabel = pendingViewerTokenRequests.get(windowId);
      if (!windowLabel) {
        if (DEBUG_VIEWER_CONNECTION) {
          console.log(LOG, `viewer_token for unknown windowId ${windowId} — ignoring`);
        }
        break;
      }
      pendingViewerTokenRequests.delete(windowId);
      if (DEBUG_VIEWER_CONNECTION) {
        console.log(LOG, `viewer_token relayed to ${windowLabel} (windowId ${windowId})`);
      }
      void emitTo(windowLabel, 'viewer-token:response', {
        windowId,
        token: msg.token,
        sfuUrl: msg.sfuUrl,
        identity: msg.identity,
      });
      break;
    }

    case 'chat_message': {
      const participant = state.participants.find((p) => p.id === msg.participantId);
      const chatMsg: ChatMessage = {
        id: makeEventId(),
        messageId: msg.messageId || undefined,
        timestamp: msg.timestamp,
        participantId: msg.participantId,
        userId: msg.userId || undefined,
        displayName: msg.displayName,
        color: participant?.color ?? '',
        text: msg.text,
      };
      state.chatMessages = [...state.chatMessages, chatMsg];
      if (state.chatMessages.length > MAX_CHAT_MESSAGES) {
        state.chatMessages = state.chatMessages.slice(-MAX_CHAT_MESSAGES);
      }
      if (shouldPlayChatNotification(chatMsg.participantId, state.selfParticipantId)) {
        void playNotificationSound('chat');
      }
      notify();
      break;
    }

    case 'chat_history_response': {
      const messages = msg.messages || [];
      const historyPayload = messages.map((m) => ({
        messageId: m.messageId,
        participantId: m.participantId,
        userId: m.userId || undefined,
        displayName: m.displayName,
        text: m.text,
        timestamp: m.timestamp,
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
  resetCameraRuntimeState();
  mediaConnectedParticipantIds.clear();
  knownLiveKitIdentities.clear();
  suppressReconnectJoinForParticipantIds.clear();
  localSessionJoined = false;
  localDisconnectSoundPlayed = false;
  desiredSubRoomIntent = undefined;
  bufferedMediaToken = null;
  latestMediaToken = null;
  bufferedIceConfig = null;
  pendingViewerTokenRequests.clear();

  // Push initial state to the component
  notify();

  // Set up share picker event listener for Tauri event handshake
  setupSharePickerListener();
  setupLinuxCaptureFallbackListener();
  setupExternalShareHelperListeners();

  // Forward viewer-subscribed events from WatchAllPage/ScreenSharePage to the signaling server
  if (!unlistenViewerJoined) {
    void listen<{ targetId: string }>('viewer-subscribed', ({ payload }) => {
      if (client) {
        client.send({ type: 'viewer_subscribed', targetId: payload.targetId });
      }
    }).then((unlisten) => {
      unlistenViewerJoined = unlisten;
    });
  }

  // Broker viewer-token requests from child viewer windows: forward to the
  // signaling server over this window's WS and relay the response back to the
  // requesting window (children have no WS of their own).
  if (!unlistenViewerTokenRequest) {
    void listen<{ windowId: string; windowLabel: string }>(
      'viewer-token:request',
      ({ payload }) => {
        if (!payload.windowId || !payload.windowLabel) return;
        pendingViewerTokenRequests.set(payload.windowId, payload.windowLabel);
        if (DEBUG_VIEWER_CONNECTION) {
          console.log(
            LOG,
            `viewer-token:request from ${payload.windowLabel} (windowId ${payload.windowId})`,
          );
        }
        if (client) {
          client.send({ type: 'request_viewer_token', windowId: payload.windowId });
        }
      },
    ).then((unlisten) => {
      unlistenViewerTokenRequest = unlisten;
    });
  }

  // Create signaling client and wire handlers
  client = new SignalingClient();
  const thisClient = client; // capture for async guard
  unsubscribe = client.onMessage(dispatchMessage);

  // Detect disconnection during active session for reconnection flow
  unsubStatusChange = client.onStatusChange((status) => {
    if (status === 'connected') {
      // A ws-ticket authenticates the connection at upgrade time (#268), so
      // there is no post-connect auth_success signal to wait for anymore —
      // the socket opening IS the authentication success signal.
      if (state.machineState === 'connecting' || state.machineState === 'reconnecting') {
        authRefreshRetries = 0;
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
      return;
    }
    if (status === 'disconnected') {
      stopColdStartRetry();
      if (state.machineState === 'active' || state.machineState === 'joining') {
        // Connection dropped during active session or mid-join — start reconnecting.
        // Do NOT tear down LiveKit media — it connects directly to the SFU,
        // independent of the signaling WS. Tearing it down causes an
        // unnecessary audio/screenshare interruption during WS reconnects
        // (e.g. CloudFront idle timeout drops the WS every ~10 min).
        state.machineState = 'reconnecting';
        appendEvent({
          id: makeEventId(),
          timestamp: timestamp(),
          type: 'system',
          message: state.lastRateLimitError ?? 'signaling connection lost — reconnecting',
        });
        notify();
      } else if (state.machineState === 'reconnecting') {
        // Already reconnecting and got another disconnect.
        // Only give up once the fast-retry budget is truly spent and a
        // periodic-retry attempt has also failed — NOT merely "no reconnect
        // timer is pending right now" (a timer handle is transiently null
        // between one attempt ending and the next being scheduled, which
        // would declare defeat after a single failed attempt).
        if (client && client.status === 'disconnected' && client.reconnectExhausted) {
          // Unregister hotkey when giving up on reconnection (R22.7)
          if (registeredHotkey) {
            unregisterMuteHotkey(registeredHotkey).catch(() => {});
            registeredHotkey = null;
          }
          appendEvent({
            id: makeEventId(),
            timestamp: timestamp(),
            type: 'system',
            message: 'signaling reconnect failed — session lost',
          });
          state.error = 'Connection lost';
          state.lastRateLimitError = null;
          playLocalDisconnectSoundOnce();
          cleanupPublishedMediaForSessionEnd();
          client.disconnect();
          client = null;
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
  void getServerUrl().then((serverUrl) => {
    if (!serverUrl || client !== thisClient) return;
    const wsUrl = toWsUrl(serverUrl);
    // Load display name, default volume, profile color, persisted channel volumes,
    // notification volumes, and the Windows share-path preference before connecting.
    // Notification volumes are pre-cached here so playNotificationSound() on the
    // first join does not make IPC calls that break the user-activation chain.
    void Promise.all([
      getUsername(),
      getDefaultVolume(),
      getProfileColor(),
      getChannelVolumes(channelId),
      getWindowsSharePath(),
      getNotificationVolume(),
      getSoundVolumes(),
    ]).then(([name, vol, profileColor, savedVols, windowsSharePath, notifVol, soundVols]) => {
      if (client !== thisClient) return;
      sessionUsername = name;
      sessionProfileColor = profileColor;
      channelVolumePrefs = savedVols;
      state.defaultVolume = vol;
      state.masterVolume = savedVols?.master ?? vol;
      state.windowsSharePath = resolveWindowsSharePathPreference(windowsSharePath);
      updateCachedNotificationVolume(notifVol);
      updateCachedSoundVolumes(soundVols);
      client.connectWithAuth(wsUrl).catch((err) => {
        console.error(LOG, 'connect failed:', err);
        if (client !== thisClient) return;
        state.error = 'Connection failed';
        playLocalDisconnectSoundOnce();
        cleanupPublishedMediaForSessionEnd();
        state.machineState = 'idle';
        notify();
      });
    });
  });
}

export function scheduleLeaveRoom(delayMs = BACKGROUND_LEAVE_DISCONNECT_MS): void {
  if (!client || state.machineState === 'idle') {
    leaveRoom();
    return;
  }

  playLocalDisconnectSoundOnce();
  clearBackgroundLeaveTimer();
  // The room screen is about to unmount, so stop pushing state into a stale React callback.
  onChange = null;
  backgroundLeaveTimer = setTimeout(
    () => {
      backgroundLeaveTimer = null;
      leaveRoom();
    },
    Math.max(0, delayMs),
  );
}

export function leaveRoom(): void {
  clearBackgroundLeaveTimer();
  playLocalDisconnectSoundOnce();
  cleanupPublishedMediaForSessionEnd();

  // Notify all child windows (screen share pop-outs) that the session is ending.
  // This must fire before we tear down media/WS so child windows can self-close
  // even if the main window is being destroyed.
  emit('voice-session:ended', {}).catch(() => {});

  // Unregister global mute hotkey (R22.5, R22.7)
  if (registeredHotkey) {
    unregisterMuteHotkey(registeredHotkey).catch(() => {});
    registeredHotkey = null;
  }

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

  // Unregister status-change handler before disconnecting so the intentional
  // disconnect does not trigger the reconnect/error logic in that handler.
  if (unsubStatusChange) {
    unsubStatusChange();
    unsubStatusChange = null;
  }

  // Disconnect the client
  if (client) {
    client.disconnect();
  }

  // Clear client references
  client = null;
  unsubscribe = null;

  // Flush any pending volume save before state is reset — changes made within
  // the 300ms debounce window (e.g. a slider moved right before leaving) would
  // otherwise be silently dropped, causing the wrong volume on the next session.
  if (volumeSaveTimer && channelVolumePrefs && state.channelId) {
    clearTimeout(volumeSaveTimer);
    volumeSaveTimer = null;
    const flushParticipantVols: Record<string, number> = {
      // channelVolumePrefs is deserialized from a Tauri-persisted JSON store
      // with no runtime validation — older/malformed persisted data can
      // genuinely lack `participants` despite the type declaring it required.
      // eslint-disable-next-line @typescript-eslint/no-unnecessary-condition
      ...(channelVolumePrefs.participants ?? {}),
    };
    for (const p of state.participants) {
      if (p.userId && p.id !== state.selfParticipantId) {
        flushParticipantVols[p.userId] = p.volume;
      }
    }
    setChannelVolumes(state.channelId, {
      master: state.masterVolume,
      participants: flushParticipantVols,
      streams: { ...(channelVolumePrefs.streams ?? {}) },
      streamMutes: { ...(channelVolumePrefs.streamMutes ?? {}) },
    }).catch(() => {});
  }

  // Reset state to defaults (fresh arrays to avoid mutating DEFAULT_STATE)
  state = {
    ...DEFAULT_STATE,
    events: [],
    chatMessages: [],
    participants: [],
    screenShareStreams: new Map(),
  };
  mediaConnectedParticipantIds.clear();
  knownLiveKitIdentities.clear();
  suppressReconnectJoinForParticipantIds.clear();
  remoteCameraTilesById = {};
  roomPanelHadAnyVideoActive = false;
  resetCameraQualityController();
  // Reset the toggle chain so stale in-flight toggles from the previous session
  // don't bleed into the next one.
  cameraToggleChain = Promise.resolve();
  desiredSubRoomIntent = undefined;

  // Reset reconnect cooldown timer
  lastReconnectMediaTime = 0;

  // Clear display name cache, speaking tracker, and pending refresh retries
  displayNameCache.clear();
  speakingTracker.clear();
  refreshRetryGenerations.clear();
  preDeafenVolume = null;
  preDeafenSelfMuted = null;

  // Tear down share picker event listener
  teardownSharePickerListener();
  teardownLinuxCaptureFallbackListener();
  teardownExternalShareHelperListeners();
  if (unlistenViewerJoined) {
    unlistenViewerJoined();
    unlistenViewerJoined = null;
  }
  if (unlistenViewerTokenRequest) {
    unlistenViewerTokenRequest();
    unlistenViewerTokenRequest = null;
  }
  pendingViewerTokenRequests.clear();

  // Notify before clearing callback (so component gets the idle state)
  notify();

  // Clear session-scoped references
  onChange = null;
  channelRole = null;
  sessionUsername = null;
  channelVolumePrefs = null;
  if (volumeSaveTimer) {
    clearTimeout(volumeSaveTimer);
    volumeSaveTimer = null;
  }
  if (passthroughVolumeSendTimer) {
    clearTimeout(passthroughVolumeSendTimer);
    passthroughVolumeSendTimer = null;
  }
  if (passthroughFilterSendTimer) {
    clearTimeout(passthroughFilterSendTimer);
    passthroughFilterSendTimer = null;
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
    if (
      state.mediaState === 'connected' ||
      state.mediaState === 'connecting' ||
      state.mediaState === 'reconnecting'
    ) {
      stopPeriodicMediaRetry();
      return;
    }
    console.log(LOG, 'periodic media retry attempt');
    state.mediaReconnectFailures = 0;
    lastReconnectMediaTime = 0;
    void reconnectMedia();
  }, PERIODIC_MEDIA_RETRY_MS);
}

export async function reconnectMedia(): Promise<void> {
  const now = Date.now();
  if (now - lastReconnectMediaTime < RECONNECT_MEDIA_COOLDOWN_MS) {
    appendEvent({
      id: makeEventId(),
      timestamp: timestamp(),
      type: 'system',
      message: 'reconnect cooldown active',
    });
    notify();
    return;
  }
  lastReconnectMediaTime = now;

  // Check retry budget (async — load config then proceed)
  const config = await getReconnectConfig();
  if (state.mediaReconnectFailures >= config.maxRetries) {
    state.mediaState = 'failed';
    appendEvent({
      id: makeEventId(),
      timestamp: timestamp(),
      type: 'system',
      message: `media reconnect retries exhausted (${config.maxRetries}) — periodic retry active`,
    });
    startPeriodicMediaRetry();
    playLocalDisconnectSoundOnce();
    notify();
    return;
  }

  snapshotPendingWasapiResume();

  // Tear down current media
  if (lkModule) {
    suppressMediaDisconnectedReconnect = true;
    try {
      lkModule.disconnect();
    } finally {
      suppressMediaDisconnectedReconnect = false;
    }
    lkModule = null;
  }
  state.mediaState = 'disconnected';
  state.mediaError = null;
  notify();

  // Re-send JoinVoice to request new media_token
  sendJoinVoiceRequest();
  scheduleMediaTokenRequestTimeout();
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
  if (state.joinedSubRoomId === null) {
    clearSelfAudioActivity(self);
    void lkModule?.setMicEnabled(false);
    notify();
    return;
  }
  void lkModule?.setMicEnabled(shouldPublishLocalMic(self));
  console.log(LOG, `toggleSelfMute → sending ${self.isMuted ? 'self_mute' : 'self_unmute'}`);
  client?.send({ type: self.isMuted ? 'self_mute' : 'self_unmute' });
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
    // Undeafen: restore volume and the mute intent that existed before deafen.
    state.isDeafened = false;
    self.isDeafened = false;
    const restored = preDeafenVolume ?? 70;
    preDeafenVolume = null;
    const restoredSelfMuted = preDeafenSelfMuted ?? self.isMuted;
    preDeafenSelfMuted = null;
    state.masterVolume = restored;
    lkModule?.setMasterVolume(restored);
    if (!self.isHostMuted) {
      self.isMuted = restoredSelfMuted;
    }
    clearSelfAudioActivity(self);
    if (state.joinedSubRoomId === null) {
      void lkModule?.setMicEnabled(false);
      notify();
      return;
    }
    void lkModule?.setMicEnabled(shouldPublishLocalMic(self));
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
    preDeafenSelfMuted = self.isMuted;
    state.masterVolume = 0;
    lkModule?.setMasterVolume(0);
    if (!self.isMuted) {
      self.isMuted = true;
    }
    clearSelfAudioActivity(self);
    void lkModule?.setMicEnabled(false);
    if (state.joinedSubRoomId === null) {
      notify();
      return;
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
  ensureInSubRoomForShare();
  emitSharePathSelected('browser', 'external_browser_share');
  await invoke('external_share_start');
}

/**
 * Start screen sharing via the native PipeWire/portal picker.
 * Uses the Rust-side `screen_share_start` IPC which goes through
 * `create_capture_backend()` → PipeWire portal → native source picker.
 * This is the primary path for Wayland compositors (Hyprland, GNOME, KDE).
 */
export async function startPortalShare(): Promise<boolean> {
  ensureInSubRoomForShare();
  await guardLinuxNativeShareStart();
  emitSharePathSelected('linux_native', 'linux_portal_share');
  await syncNativeScreenShareQualityBeforeCapture('linux_portal_share');
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
      const audioStartResult = await invoke<AudioShareStartResult>('audio_share_start', {
        sourceId: audioSourceId,
      });
      emitAudioCaptureSelection(audioStartResult);
      maybeNoticeMacCaptureFallback(audioStartResult);
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
interface StartCustomShareOptions {
  /**
   * When true, the caller already has an active publication and wants to
   * replace only the underlying MediaStreamTrack in-place (source change).
   * Skips prepareNativeCapture() and routes to replaceNativeCaptureSource()
   * instead of startNativeCapture() so the LiveKit publication is never torn
   * down, keeping viewers subscribed without a TrackPublished/TrackSubscribed
   * cycle.
   */
  isSourceChange?: boolean;
}

export async function startCustomShare(
  selection: ShareSelection,
  options: StartCustomShareOptions = {},
): Promise<void> {
  ensureInSubRoomForShare();
  ensureReusePatchReadyForPublish();
  // 1. Check if the requested slot is available
  const check = canStartShare(selection, state.activeVideoShare, state.activeAudioShare);
  if (!check.allowed) {
    throw new Error(check.reason ?? 'share slot occupied');
  }

  if (isLinuxPlatform()) {
    await guardLinuxNativeShareStart();
  }

  emitSharePathSelected(
    isWindowsPlatform() ? 'native' : isMacPlatform() ? 'browser_mac' : 'linux_native',
    isWindowsPlatform()
      ? 'native_custom_picker'
      : isMacPlatform()
        ? 'mac_custom_share'
        : 'linux_custom_share',
  );

  // 2. Set state fields optimistically
  const isVideoShare = selection.mode === 'screen_audio' || selection.mode === 'window';
  if (isVideoShare) {
    state.activeVideoShare = {
      mode: selection.mode as 'screen_audio' | 'window',
      sourceName: selection.sourceName,
      withAudio: selection.withAudio,
      audioSourceId: null,
    };
  } else {
    state.activeAudioShare = {
      sourceId: selection.sourceId,
      sourceName: selection.sourceName,
    };
  }
  notify();

  let videoStarted = false;
  let nativeAudioStarted = false;
  const shareSessionId = isVideoShare ? makeShareSessionId() : null;

  try {
    const needsVideo = isVideoShare;
    const needsAudio = selection.mode === 'audio_only' || (isVideoShare && selection.withAudio);
    if (DEBUG_WASAPI)
      console.log(
        LOG,
        '[wasapi-diag] startCustomShare: mode=%s withAudio=%s needsVideo=%s needsAudio=%s',
        selection.mode,
        selection.withAudio,
        needsVideo,
        needsAudio,
      );

    // 3. Start video first (if needed)
    if (needsVideo) {
      const sourceKind =
        selection.sourceKind ?? (selection.mode === 'window' ? 'window' : 'screen');
      const isPortalVideo = selection.sourceId === 'portal';
      const defaultNativeLeakBackend =
        sourceKind === 'screen' ? 'native-gdi-screen' : 'native-gdi-window';
      const startWindowsNativeAttempt = async (
        captureBackend: 'wgc' | 'gdi_poll',
        retryMetadata?: {
          previousBackend: 'wgc';
          retryReason: 'wgc_sustained_low_js_observed_sequence_fps';
        },
      ): Promise<void> => {
        const liveKitModule = lkModule;
        if (!(liveKitModule instanceof LiveKitModule)) return;
        const attachRustDiagnostics = async (): Promise<void> => {
          try {
            const diagnostics = await invoke<WindowsNativeCaptureDiagnostics | null>(
              'screen_share_get_capture_diagnostics',
            );
            liveKitModule.attachWindowsNativeCaptureDiagnostics(diagnostics);
          } catch (error) {
            console.warn(LOG, 'best-effort native capture diagnostics fetch failed:', error);
          }
        };
        const leakBackend =
          captureBackend === 'wgc'
            ? 'native-wgc'
            : sourceKind === 'screen'
              ? 'native-gdi-screen'
              : 'native-gdi-window';
        if (!options.isSourceChange) {
          liveKitModule.beginNativeCaptureLeakSession({
            shareSessionId,
            mode: selection.mode as 'screen_audio' | 'window',
            sourceId: selection.sourceId,
            sourceName: selection.sourceName,
            sourceKind,
            captureBackend: leakBackend,
          });
          liveKitModule.prepareNativeCapture();
          await liveKitModule.prepareNativeCaptureFailureListener();
        }
        emitTelemetryEvent({
          name: 'capture.path.selected',
          os: 'windows',
          path: leakBackend,
          sourceKind,
          backend: captureBackend,
          ts: Date.now(),
        });
        let rustCaptureStarted = false;
        try {
          await syncNativeScreenShareQualityBeforeCapture(`windows_${captureBackend}`);
          await invoke('screen_share_start_source', {
            request: {
              sourceId: selection.sourceId,
              shareSessionId,
              sourceKind,
              captureBackend,
              compatibilityMode: selection.compatibilityMode ?? false,
              previousBackend: retryMetadata?.previousBackend ?? null,
              retryReason: retryMetadata?.retryReason ?? null,
            },
          });
          rustCaptureStarted = true;
          if (options.isSourceChange) {
            await liveKitModule.replaceNativeCaptureSource();
          } else {
            await liveKitModule.startNativeCapture({
              firstFrameTimeoutMs: captureBackend === 'gdi_poll' ? 4500 : 2000,
              lowJsBridgeFpsRetry:
                captureBackend === 'wgc'
                  ? {
                      thresholdFps: 20,
                      durationMs: 2000,
                      reason: 'wgc_sustained_low_js_observed_sequence_fps',
                    }
                  : undefined,
            });
          }
          await attachRustDiagnostics();
        } catch (error) {
          if (!options.isSourceChange) {
            liveKitModule.markNativeCaptureFailure(
              error instanceof Error ? error.message : String(error),
            );
          }
          await attachRustDiagnostics();
          if (!options.isSourceChange) {
            try {
              await liveKitModule.stopNativeCapture();
            } catch (stopError) {
              console.warn(
                LOG,
                'best-effort stopNativeCapture between native attempts failed:',
                stopError,
              );
            }
          }
          if (rustCaptureStarted) {
            try {
              await invoke('screen_share_stop');
            } catch (stopError) {
              console.warn(
                LOG,
                'best-effort screen_share_stop between native attempts failed:',
                stopError,
              );
            }
          }
          throw error;
        }
      };

      // On Windows (LiveKit JS SDK path), install the frame buffering
      // handler BEFORE starting the Rust capture. prepareNativeCapture()
      // is synchronous — no async, no event listener, no HWND dependency.
      // This branch is only for the custom picker/native-source pipeline.
      // The normal Windows `/share` button opens the native custom picker in
      // ActiveRoom; browser getDisplayMedia is not a Windows retry path.
      if (isWindowsPlatform() && lkModule instanceof LiveKitModule) {
        const firstBackend =
          selection.compatibilityMode === true && sourceKind === 'window' ? 'gdi_poll' : 'wgc';
        try {
          await startWindowsNativeAttempt(firstBackend);
        } catch (error) {
          emitTelemetryEvent({
            name: 'capture.native.failed',
            os: 'windows',
            sourceKind,
            backend: firstBackend,
            reason: error instanceof Error ? error.message : String(error),
            ts: Date.now(),
          });
          const message = error instanceof Error ? error.message : String(error);
          const shouldRetryGdi =
            firstBackend === 'wgc' &&
            !options.isSourceChange &&
            message.includes('retryReason=wgc_sustained_low_js_observed_sequence_fps');
          if (shouldRetryGdi) {
            emitTelemetryEvent({
              name: 'capture.fallback.activated',
              os: 'windows',
              from: 'wgc',
              to: 'gdi_poll',
              reason: 'wgc_sustained_low_js_observed_sequence_fps',
              ts: Date.now(),
            });
            await startWindowsNativeAttempt('gdi_poll', {
              previousBackend: 'wgc',
              retryReason: 'wgc_sustained_low_js_observed_sequence_fps',
            });
          } else {
            throw error;
          }
        }
        videoStarted = true;
      } else if (isPortalVideo) {
        await syncNativeScreenShareQualityBeforeCapture('linux_portal_source');
        await invoke('screen_share_start');
        videoStarted = true;
      } else {
        if (lkModule && lkModule instanceof LiveKitModule && !options.isSourceChange) {
          // Normal start: set up early-frame buffering before Rust capture begins.
          lkModule.beginNativeCaptureLeakSession({
            shareSessionId,
            mode: selection.mode as 'screen_audio' | 'window',
            sourceId: selection.sourceId,
            sourceName: selection.sourceName,
            sourceKind,
            captureBackend: defaultNativeLeakBackend,
          });
          lkModule.prepareNativeCapture();
        }
        await syncNativeScreenShareQualityBeforeCapture(
          isWindowsPlatform() ? 'windows_native_source' : 'linux_native_source',
        );
        await invoke('screen_share_start_source', {
          sourceId: selection.sourceId,
          shareSessionId,
          sourceKind,
          compatibilityMode: selection.compatibilityMode ?? false,
        });
        videoStarted = true;
      }

      // On Windows (LiveKit JS SDK path), the Rust capture writes frames
      // to a shared buffer. startNativeCapture() starts a polling loop
      // via invoke('screen_share_poll_frame') that uses the ipc:// protocol
      // (HTTP-like), completely bypassing PostMessage/HWND. This is immune
      // to the HWND corruption caused by child windows (SharePicker).
      if (!isWindowsPlatform() && !isPortalVideo && lkModule && lkModule instanceof LiveKitModule) {
        if (options.isSourceChange) {
          // Source change: feed new Rust frames into the existing generator so
          // viewers stay subscribed without a TrackUnpublished/TrackPublished cycle.
          await lkModule.replaceNativeCaptureSource();
        } else {
          // Normal start: publish a new track via the LiveKit SDK.
          await lkModule.startNativeCapture();
        }
      }
    }

    // 4. Start audio (if needed)
    if (needsAudio) {
      if (DEBUG_WASAPI) console.log(LOG, '[wasapi] needsAudio=true, resolving audio source...');
      try {
        let audioSourceId: string;
        if (selection.mode === 'audio_only') {
          audioSourceId = selection.sourceId;
        } else {
          const monitorCommand = isLinuxPlatform()
            ? 'get_default_audio_monitor_fast'
            : 'get_default_audio_monitor';
          if (DEBUG_WASAPI)
            console.log(LOG, '[wasapi] resolving default audio monitor via', monitorCommand);
          audioSourceId = await invoke<string>(monitorCommand);
          if (DEBUG_WASAPI)
            console.log(LOG, '[wasapi] default audio monitor resolved:', audioSourceId);
        }
        if (DEBUG_WASAPI)
          console.log(LOG, '[wasapi] invoking audio_share_start, sourceId:', audioSourceId);
        const audioStartResult = await invoke<AudioShareStartResult>('audio_share_start', {
          sourceId: audioSourceId,
        });
        nativeAudioStarted = true;
        emitAudioCaptureSelection(audioStartResult);
        maybeNoticeMacCaptureFallback(audioStartResult);
        if (DEBUG_WASAPI) console.log(LOG, '[wasapi] audio_share_start result:', audioStartResult);
        if (isVideoShare && state.activeVideoShare) {
          state.activeVideoShare.audioSourceId = audioSourceId;
        }

        // On Windows, the Rust WASAPI capture thread streams PCM frames via
        // Tauri events. Start the JS-side AudioWorklet bridge to receive them
        // and publish as a LiveKit ScreenShareAudio track.
        if (DEBUG_WASAPI)
          console.log(
            LOG,
            '[wasapi] lkModule:',
            lkModule?.constructor.name,
            'is LiveKitModule:',
            lkModule instanceof LiveKitModule,
          );
        if (lkModule && lkModule instanceof LiveKitModule) {
          const lk = lkModule; // capture narrowed type for closures
          try {
            if (DEBUG_WASAPI) console.log(LOG, '[wasapi] starting audio bridge');
            // startWasapiAudioBridge now owns the Tauri event listeners internally.
            await lk.startWasapiAudioBridge(audioStartResult.loopback_exclusion_available);
            if (DEBUG_WASAPI) console.log(LOG, '[wasapi] bridge fully active');
          } catch (bridgeErr) {
            console.warn(LOG, '[wasapi] WASAPI audio bridge failed:', bridgeErr);
            throw bridgeErr;
          }
        } else {
          if (DEBUG_WASAPI)
            console.log(LOG, '[wasapi] skipping bridge — not a LiveKitModule or lkModule null');
        }
      } catch (audioErr) {
        if (nativeAudioStarted) {
          if (lkModule && lkModule instanceof LiveKitModule) {
            try {
              await lkModule.stopWasapiAudioBridge();
            } catch (bridgeStopErr) {
              console.warn(
                LOG,
                'best-effort stopWasapiAudioBridge after audio start failure failed:',
                bridgeStopErr,
              );
            }
          }
          try {
            await invoke('audio_share_stop');
          } catch (audioStopErr) {
            console.warn(
              LOG,
              'best-effort audio_share_stop after audio start failure failed:',
              audioStopErr,
            );
          }
          nativeAudioStarted = false;
        }
        if (videoStarted) {
          // Audio failed but video is already running. On Windows (JS SDK path),
          // system audio sharing may not be available yet — downgrade to video-only
          // instead of rolling back the entire share.
          console.warn(LOG, 'audio companion failed, continuing with video-only:', audioErr);
          if (isVideoShare && state.activeVideoShare) {
            state.activeVideoShare.withAudio = false;
            state.activeVideoShare.audioSourceId = null;
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

    // 5. Send signaling on success. Re-sending for a layered share updates the
    // authoritative type used by remote audio gating.
    if (client) {
      client.send({ type: 'start_share', shareType: selection.mode });
    }

    // Start motion-sample source and profile-switch state machine for video shares (W5).
    if (isVideoShare) {
      if (lkModule instanceof LiveKitModule) {
        const track = lkModule.getLocalScreenShareTrack();
        if (track) selectMotionSampleSource(track);
      }
      await initializeProfileSwitchAfterShareStart();
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
    console.error(
      LOG,
      'startCustomShare failed:',
      err instanceof Error ? { name: err.name, message: err.message, stack: err.stack } : err,
    );
    if (nativeAudioStarted) {
      if (lkModule && lkModule instanceof LiveKitModule) {
        try {
          await lkModule.stopWasapiAudioBridge();
        } catch (bridgeStopErr) {
          console.warn(
            LOG,
            'best-effort stopWasapiAudioBridge during startCustomShare rollback failed:',
            bridgeStopErr,
          );
        }
      }
      try {
        await invoke('audio_share_stop');
      } catch (audioStopErr) {
        console.warn(
          LOG,
          'best-effort audio_share_stop during startCustomShare rollback failed:',
          audioStopErr,
        );
      }
    }
    // Clean up native capture on failure.
    // For source-change: replaceNativeCaptureSource stopped the new polling loop
    // but the old publication is still alive — stopNativeCapture unpublishes it.
    // For normal start: stopNativeCapture clears the listener if startNativeCapture
    // never reached publishTrack.
    if (lkModule && lkModule instanceof LiveKitModule && isVideoShare) {
      if (!options.isSourceChange) {
        lkModule.markNativeCaptureFailure(err instanceof Error ? err.message : String(err));
      }
      await lkModule.stopNativeCapture();
    }
    if (isVideoShare && videoStarted) {
      try {
        await invoke('screen_share_stop');
      } catch (screenStopErr) {
        console.error(
          LOG,
          'best-effort screen_share_stop during startCustomShare rollback failed:',
          screenStopErr,
        );
      }
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

interface StopCustomShareOptions {
  suppressSignaling?: boolean;
  /**
   * When true, skip unpublishTrack on the LiveKit publication so it stays
   * alive for an in-place track replacement via replaceNativeCaptureSource().
   * Only meaningful on the Windows/LiveKit native capture path.
   */
  keepPublication?: boolean;
}

/** Stop a specific share slot or all shares. */
export async function stopCustomShare(
  target: 'video' | 'audio' | 'all' = 'all',
  options: StopCustomShareOptions = {},
): Promise<void> {
  const plan = planStopCommands(target, state.activeVideoShare, state.activeAudioShare);

  // 1. Stop video capture (best-effort)
  if (plan.stopVideo) {
    // Stop the native capture bridge on Windows (LiveKit JS SDK path).
    // When keepPublication=true (source-change path), skip unpublishTrack so
    // the LiveKit publication stays alive for replaceNativeCaptureSource().
    if (lkModule && lkModule instanceof LiveKitModule) {
      if (options.keepPublication) {
        // Source change: publication stays alive for replaceNativeCaptureSource().
      } else {
        // Signal before stopNativeCapture: LocalTrackUnpublished fires during the
        // unpublishTrack await inside stopNativeCapture and triggers
        // onLocalScreenShareEnded, which would send a duplicate stop-share.
        localStopShareSent = true;
        try {
          await lkModule.stopNativeCapture();
        } catch (err) {
          console.error(LOG, 'best-effort stopNativeCapture failed:', err);
        }
        // Reset defensively: onLocalScreenShareEnded resets it when it fires;
        // if it didn't fire (no active track), ensure flag is clear for future stops.
        localStopShareSent = false;
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
      try {
        await lkModule.stopWasapiAudioBridge();
      } catch {
        /* best-effort */
      }
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

  // 4. Send stop-share signaling for the slot(s) that stopped. A slot-scoped
  // shareType tells the backend to clear only that slot and leave the other
  // slot (if still active) running — the participant stays "sharing" there.
  if (client && !options.suppressSignaling) {
    if (target === 'all') {
      client.send({ type: 'stop-share' });
    } else if (target === 'video' && state.activeVideoShare) {
      client.send({ type: 'stop-share', shareType: state.activeVideoShare.mode });
    } else if (target === 'audio') {
      client.send({ type: 'stop-share', shareType: 'audio_only' });
    }
  }

  // 5. Clear affected slot(s)
  if (target === 'video' || target === 'all') {
    state.activeVideoShare = null;
    state.shareQualityInfo = null;
    state.shareStats = null;
    stopMotionDetection();
  }
  if (target === 'audio' || target === 'all') {
    state.activeAudioShare = null;
  }

  await updateShareIndicator();

  notify();

  appendEvent({
    id: makeEventId(),
    timestamp: timestamp(),
    type: 'share-stop',
    message: `${selfName()} stopped sharing (${target})`,
    participantId: state.selfParticipantId ?? undefined,
  });
}

/** No-op: share indicator window removed. Stop is accessible from the main room UI. */

async function updateShareIndicator(): Promise<void> {}

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
    await invoke('external_share_stop').catch(() => {});
  }
  // Stop native PipeWire/portal capture if running (best-effort).
  await invoke('screen_share_stop').catch(() => {});
  // Stop system audio capture if running (portal audio share).
  await invoke('audio_share_stop').catch(() => {});
  await lkModule?.stopScreenShare();
  if (client) {
    client.send({ type: 'stop-share' });
  }
}

export type ShareQuality = 'low' | 'high' | 'max';

export type { ShareQualityInfo } from './livekit-media';

export async function setShareQuality(quality: ShareQuality): Promise<void> {
  selectedShareQuality = quality;
  if (lkModule && 'setScreenShareQuality' in lkModule) {
    await (
      lkModule as { setScreenShareQuality(q: ShareQuality): Promise<void> }
    ).setScreenShareQuality(quality);
  }
}

export async function toggleShareAudio(withAudio: boolean): Promise<boolean> {
  if (DEBUG_SHARE_AUDIO) {
    console.log(LOG, '[share-audio] toggleShareAudio called', {
      withAudio,
      hasLkModule: !!lkModule,
      lkModuleType: lkModule?.constructor.name,
      userActivationIsActive: (navigator as { userActivation?: { isActive: boolean } })
        .userActivation?.isActive,
    });
  }
  if (lkModule && 'restartScreenShareWithAudio' in lkModule) {
    localSourceChanging = true;
    try {
      const result = await lkModule.restartScreenShareWithAudio(withAudio);
      if (result && state.activeVideoShare) {
        state.activeVideoShare.withAudio = withAudio;
        if (!withAudio) {
          state.activeVideoShare.audioSourceId = null;
        }
        notify();
      }
      if (DEBUG_SHARE_AUDIO) console.log(LOG, '[share-audio] toggleShareAudio result:', result);
      return result;
    } finally {
      localSourceChanging = false;
    }
  }
  return false;
}

export async function changeShareSource(): Promise<boolean> {
  if (lkModule && 'changeScreenShareSource' in lkModule) {
    localSourceChanging = true;
    const result = await lkModule.changeScreenShareSource();
    localSourceChanging = false;

    if (!result && client && !localStopShareSent) {
      // If the source change failed AND there is no active screen share
      // publication, reconcile backend state by sending stop_share. This
      // prevents the backend from staying stuck in a "sharing" state when
      // local media is already dead (e.g. replaceTrack threw after teardown).
      const stillActive = lkModule.hasActiveScreenShareTrack();
      if (!stillActive) {
        client.send({ type: 'stop-share' });
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

export function updateSessionUsername(username: string): void {
  sessionUsername = username;
  if (state.selfParticipantId) {
    displayNameCache.set(state.selfParticipantId, username);
    state.participants = state.participants.map((p) =>
      p.id === state.selfParticipantId ? { ...p, displayName: username } : p,
    );
    rebuildVideoTiles();
    notify();
  }
  client?.send({ type: 'update_username', username });
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
      liveKitIdentityFor(participantId),
      computeEffectiveParticipantVolume(
        clamped,
        participantId,
        state.selfParticipantId,
        state.joinedSubRoomId,
        state.participantSubRoomById,
        state.passthrough,
        state.passthroughVolume / 100,
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
    preDeafenSelfMuted = null;
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
    (lkModule as LiveKitModule).setScreenShareAudioVolume(
      liveKitIdentityFor(participantId),
      clamped,
    );
  }
}

export function persistStreamVolume(participantId: string, volume: number): void {
  const clamped = Math.max(0, Math.min(100, Math.round(volume)));
  const p = state.participants.find((pp) => pp.id === participantId);
  if (p?.userId) {
    if (!channelVolumePrefs) {
      channelVolumePrefs = { master: state.masterVolume, participants: {} };
    }
    channelVolumePrefs = {
      ...channelVolumePrefs,
      streams: { ...(channelVolumePrefs.streams ?? {}), [p.userId]: clamped },
    };
  }
  saveVolumesDebounced();
}

export function getPersistedStreamVolume(participantId: string): number | null {
  if (!channelVolumePrefs?.streams) return null;
  const p = state.participants.find((pp) => pp.id === participantId);
  if (!p?.userId) return null;
  return channelVolumePrefs.streams[p.userId] ?? null;
}

export function persistStreamMuted(participantId: string, muted: boolean): void {
  const p = state.participants.find((pp) => pp.id === participantId);
  if (p?.userId) {
    if (!channelVolumePrefs) {
      channelVolumePrefs = { master: state.masterVolume, participants: {} };
    }
    channelVolumePrefs = {
      ...channelVolumePrefs,
      streamMutes: { ...(channelVolumePrefs.streamMutes ?? {}), [p.userId]: muted },
    };
  }
  saveVolumesDebounced();
}

export function getPersistedStreamMuted(participantId: string): boolean | null {
  const p = state.participants.find((pp) => pp.id === participantId);
  if (!p?.userId) return null;
  const savedMute = channelVolumePrefs?.streamMutes?.[p.userId];
  if (savedMute !== undefined) return savedMute;
  // Backward compatibility: older preferences represented mute as volume 0.
  return channelVolumePrefs?.streams?.[p.userId] === 0 ? true : null;
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
  client.send({ type: 'stop-share', targetParticipantId: participantId });
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

export function setPassthroughEnabled(enabled: boolean): void {
  if (!state.selfIsHost) return;
  if (!client || client.status !== 'connected') return;
  client.send({ type: 'set_passthrough_enabled', enabled });
}

export function setPassthroughVolume(volume: number): void {
  const clamped = Math.max(0, Math.min(100, Math.round(volume)));
  // Debounce WS sends to avoid spamming the server (and its DB role-check) on every
  // slider pixel. The slider's local useState updates immediately for responsive UI;
  // the actual signal (and the server-echo that updates state.passthroughVolume) follows
  // up to 150ms later — an intentional optimistic divergence for a host-only control.
  if (passthroughVolumeSendTimer) clearTimeout(passthroughVolumeSendTimer);
  passthroughVolumeSendTimer = setTimeout(() => {
    passthroughVolumeSendTimer = null;
    if (!client || client.status !== 'connected') return;
    client.send({ type: 'set_passthrough_volume', volume: clamped });
  }, 150);
}

export function setPassthroughFilter(enabled: boolean, strength: number): void {
  const clamped = Math.max(0, Math.min(100, Math.round(strength)));
  if (passthroughFilterSendTimer) clearTimeout(passthroughFilterSendTimer);
  passthroughFilterSendTimer = setTimeout(() => {
    passthroughFilterSendTimer = null;
    if (!client || client.status !== 'connected') return;
    client.send({ type: 'set_passthrough_filter', enabled, strength: clamped });
  }, 150);
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
  // The local mic monitor polls every 50ms for the lifetime of the call, but
  // rmsLevel itself isn't rendered — only the isSpeaking transition is. Skip
  // the broadcast (and the full ActiveRoom re-render it triggers) on ticks
  // that don't actually flip speaking state.
  const wasSpeaking = p.isSpeaking;
  p.isSpeaking = updateSpeakingTracker(p.id, level, p.isSpeaking, p.isMuted);
  if (p.isSpeaking !== wasSpeaking) notify();
}

/* ─── Screen Share Audio ─────────────────────────────────────────── */

/** Attach deferred screen share audio when user opens the viewer. */
export function attachScreenShareAudio(participantId: string): void {
  if (state.joinedSubRoomId === null) return;
  if (lkModule && 'attachScreenShareAudio' in lkModule) {
    (lkModule as LiveKitModule).attachScreenShareAudio(liveKitIdentityFor(participantId));
  }
}

/** Detach screen share audio when user closes the viewer. */
export function detachScreenShareAudio(participantId: string): void {
  if (lkModule && 'detachScreenShareAudio' in lkModule) {
    (lkModule as LiveKitModule).detachScreenShareAudio(liveKitIdentityFor(participantId));
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
    videoTilesById: { ...state.videoTilesById },
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
