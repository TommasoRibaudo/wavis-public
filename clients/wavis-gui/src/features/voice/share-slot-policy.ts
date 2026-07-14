/**
 * Share-Slot Policy Model
 *
 * Pure, deterministic policy for the two-slot share model (one video share +
 * one standalone audio share): slot occupancy, start/stop routing, button
 * state, capture command planning, and signaling message construction.
 *
 * Zero IO — no state, no LiveKit, no Tauri. Extracted from voice-room.ts;
 * exported for direct consumption and property testing
 * (voice-room-share.property.test.ts).
 */

import type { ShareMode, ShareSelection, FallbackReason } from '@features/screen-share/share-types';

/* ─── Slot types ────────────────────────────────────────────────── */

/** Active video share slot (screen or window). Null when no video share. */
export interface ActiveVideoShare {
  mode: 'screen_audio' | 'window';
  sourceName: string;
  withAudio: boolean;
  audioSourceId: string | null;
}

/** Active standalone audio share slot. Null when no audio-only share. */
export interface ActiveAudioShare {
  sourceId: string;
  sourceName: string;
}

/* ─── Policy ────────────────────────────────────────────────────── */

/** Derive the legacy activeShareType from the two-slot model. */
export function activeShareType(
  videoShare: ActiveVideoShare | null,
  audioShare: ActiveAudioShare | null,
): ShareMode | null {
  // Video share takes precedence for display purposes
  if (videoShare) return videoShare.mode;
  if (audioShare) return 'audio_only';
  return null;
}

/** Whether any share is active (either slot occupied). */
export function isAnyShareActive(
  videoShare: ActiveVideoShare | null,
  audioShare: ActiveAudioShare | null,
): boolean {
  return videoShare !== null || audioShare !== null;
}

/** Check if a given share selection conflicts with current state. */
export function canStartShare(
  selection: ShareSelection,
  videoShare: ActiveVideoShare | null,
  audioShare: ActiveAudioShare | null,
): { allowed: boolean; reason?: string } {
  if (selection.mode === 'audio_only') {
    if (audioShare) return { allowed: false, reason: 'audio-only share already active' };
    return { allowed: true };
  }
  // screen_audio or window
  if (videoShare) return { allowed: false, reason: 'video share already active' };
  // Cannot add companion audio while an audio-only share is already using the audio device.
  if (audioShare && selection.withAudio) {
    return {
      allowed: false,
      reason: 'audio-only share active — start video without audio, or stop audio first',
    };
  }
  return { allowed: true };
}

export function preserveVideoShareSelectionForSourceChange(
  selection: ShareSelection,
  videoShare: ActiveVideoShare | null,
): ShareSelection {
  if (!videoShare) return selection;
  if (selection.mode === 'audio_only') {
    throw new Error('changing a video share source cannot switch to audio-only');
  }
  return {
    ...selection,
    withAudio: videoShare.withAudio,
  };
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
  'open_picker' | 'fallback_share' | 'error_toast' | 'no_sources_toast';

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
 * Disabled when video share is active (can't stack two video shares), or when
 * the fallback (getDisplayMedia) share is running. Audio-only share does NOT
 * disable the button — the user can layer a video share on top.
 */
export function isShareButtonDisabled(
  activeVideoShare: ActiveVideoShare | null,
  selfSharing: boolean,
): boolean {
  return activeVideoShare !== null || selfSharing;
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

export function screenShareActiveInRoom(currentState: {
  joinedSubRoomId: string | null;
  participants: Array<{ id: string; isSharing: boolean }>;
  participantSubRoomById: Record<string, string>;
}): boolean {
  const joinedSubRoomId = currentState.joinedSubRoomId;
  if (!joinedSubRoomId) {
    return false;
  }

  return currentState.participants.some(
    (participant) =>
      participant.isSharing === true &&
      currentState.participantSubRoomById[participant.id] === joinedSubRoomId,
  );
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
  videoShare: ActiveVideoShare | null,
  audioShare: ActiveAudioShare | null,
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
