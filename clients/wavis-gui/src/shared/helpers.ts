/**
 * Pure helper functions extracted from components for testability.
 * No Tauri/React/DOM dependencies — safe to run in Node.
 */

import type { ChannelRole } from '@features/channels/channels';
import type { ApiErrorKind } from './api';
import type { ChannelMember } from '@features/channels/channel-detail';
import type { RoomEventType } from '@features/voice/voice-room';
import type { NotificationToggles } from '@features/settings/settings-store';

// ─── Auth helpers ──────────────────────────────────────────────────

/** Derive WS URL from server URL: https→wss, http→ws, append /ws */
export function toWsUrl(serverUrl: string): string {
  return serverUrl
    .replace(/^https:\/\//, 'wss://')
    .replace(/^http:\/\//, 'ws://')
    .replace(/\/+$/, '') + '/ws';
}

/** Parse hostname from a URL string, fallback to 'wavis' */
export function parseHostname(url: string): string {
  try {
    return new URL(url).hostname;
  } catch {
    return 'wavis';
  }
}

// ─── Channel Detail helpers ────────────────────────────────────────

export function shouldBlockRoomNavigation(
  currentPathname: string,
  nextPathname: string,
  allowNavigation: boolean,
): boolean {
  return !allowNavigation && currentPathname === '/room' && nextPathname !== '/room';
}

export function shouldPreventRoomNavigationGesture(
  input: { button?: number; key?: string; altKey?: boolean },
): boolean {
  if (input.button === 3) return true;
  if (input.key === 'BrowserBack' || input.key === 'GoBack') return true;
  return Boolean(input.altKey && input.key === 'ArrowLeft');
}

export function roleBadgeInfo(role: ChannelRole): { label: string; color: string } {
  switch (role) {
    case 'owner':
      return { label: 'OWNER', color: 'var(--wavis-purple)' };
    case 'admin':
      return { label: 'ADMIN', color: 'var(--wavis-warn)' };
    case 'member':
      return { label: 'MEMBER', color: 'var(--wavis-text-secondary)' };
  }
}

export function channelDetailErrorMessage(kind: ApiErrorKind): string {
  switch (kind) {
    case 'Forbidden':
      return "you don't have permission";
    case 'NotFound':
      return 'not found';
    case 'RateLimited':
      return 'too many requests — try again later';
    case 'Network':
      return 'connection error — check your network';
    case 'AlreadyBanned':
      return 'user is already banned';
    default:
      return 'something went wrong — try again';
  }
}

export function sortMembers(members: ChannelMember[]): ChannelMember[] {
  const priority: Record<ChannelRole, number> = { owner: 0, admin: 1, member: 2 };
  return [...members].sort((a, b) => {
    const rp = priority[a.role] - priority[b.role];
    if (rp !== 0) return rp;
    return a.joinedAt.localeCompare(b.joinedAt);
  });
}

export function getCommands(role: ChannelRole): string[] {
  switch (role) {
    case 'owner':
      return ['/voice', '/invite', '/revoke', '/ban', '/unban', '/role', '/delete', '/back'];
    case 'admin':
      return ['/voice', '/invite', '/revoke', '/ban', '/unban', '/leave', '/back'];
    case 'member':
      return ['/voice', '/leave', '/back'];
  }
}

// ─── Channels List helpers ─────────────────────────────────────────

export function roleBadgeColor(role: ChannelRole): string {
  switch (role) {
    case 'owner':
      return 'var(--wavis-accent)';
    case 'admin':
      return 'var(--wavis-purple)';
    case 'member':
      return 'var(--wavis-text-secondary)';
  }
}

export function channelsListErrorMessage(kind: ApiErrorKind): string {
  switch (kind) {
    case 'InvalidInvite':
      return 'invalid invite code';
    case 'AlreadyMember':
      return 'already a member of this channel';
    case 'RateLimited':
      return 'too many requests — try again later';
    case 'Network':
      return 'connection error — check your network';
    case 'Unauthorized':
      return 'session expired';
    case 'Unknown':
    default:
      return 'something went wrong — try again';
  }
}

// ─── Debug / redaction helpers ─────────────────────────────────────

/** Log level options for the DEBUG settings section */
export const LOG_LEVELS = ['off', 'error', 'warn', 'info', 'debug', 'trace'] as const;
export type LogLevel = (typeof LOG_LEVELS)[number];

/**
 * Redact a token based on the showSecrets flag.
 * When showSecrets is true, returns the full token unchanged.
 * When false, returns the first 16 characters + "...".
 * For tokens shorter than 16 characters, the entire token + "..." is returned.
 */
export function redactToken(token: string, showSecrets: boolean): string {
  if (showSecrets) return token;
  return token.slice(0, 16) + '...';
}


// ─── Connection mode badge helpers ─────────────────────────────────

/**
 * Compute the connection mode badge text, or null if the badge should be hidden.
 * Badge is visible only when showSecrets is true AND connectionMode is defined.
 */
export function connectionModeBadgeText(
  showSecrets: boolean,
  connectionMode: 'livekit' | 'native' | undefined,
): string | null {
  if (!showSecrets || connectionMode === undefined) return null;
  return connectionMode === 'livekit' ? 'LiveKit' : 'Proxy';
}


// ─── Toast helpers ─────────────────────────────────────────────────

/**
 * Map a room event type to a notification toggle key, or null if the event
 * type has no corresponding toggle (e.g. system events, muted/unmuted).
 */
export function eventToToggleKey(
  type: RoomEventType,
): keyof NotificationToggles | null {
  switch (type) {
    case 'join': return 'participantJoined';
    case 'leave': return 'participantLeft';
    case 'kicked': return 'participantKicked';
    case 'host-mute': return 'participantMutedByHost';
    case 'host-unmute': return 'participantMutedByHost';
    case 'deafen': return null;
    case 'undeafen': return null;
    default: return null;
  }
}

/**
 * Generate a toast message for a room event.
 * Returns null for event types that should not produce toasts (system, muted/unmuted).
 */
export function toastMessageForEvent(
  type: RoomEventType,
  displayName: string,
): string | null {
  switch (type) {
    case 'join': return `${displayName} joined`;
    case 'leave': return `${displayName} left`;
    case 'kicked': return `${displayName} was kicked`;
    case 'host-mute': return `${displayName} was muted by host`;
    case 'host-unmute': return `${displayName} was unmuted by host`;
    case 'share-start': return `${displayName} started sharing`;
    case 'share-stop': return `${displayName} stopped sharing`;
    case 'share-permission': return `share permission changed`;
    case 'deafen': return `${displayName} deafened`;
    case 'undeafen': return `${displayName} undeafened`;
    default: return null;
  }
}

/**
 * Get the CSS color variable for a toast border based on event type.
 */
export function toastColorForEvent(type: RoomEventType): string {
  switch (type) {
    case 'join': return 'var(--wavis-accent)';
    case 'leave':
    case 'kicked': return 'var(--wavis-danger)';
    case 'host-mute': return 'var(--wavis-warn)';
    case 'host-unmute': return 'var(--wavis-accent)';
    case 'share-start':
    case 'share-stop': return 'var(--wavis-purple)';
    case 'share-permission': return 'var(--wavis-warn)';
    case 'deafen': return 'var(--wavis-warn)';
    case 'undeafen': return 'var(--wavis-accent)';
    default: return 'var(--wavis-text)';
  }
}

// ─── Share indicator helpers ───────────────────────────────────────

/** Pure: map share type to contextual indicator. Exported for testing. */
export function shareIndicatorForType(shareType?: string): { char: string; label: string } {
  switch (shareType) {
    case 'audio_only':
      return { char: '🎵', label: 'sharing audio' };
    case 'screen_audio':
      return { char: '▲', label: 'sharing screen' };
    case 'window':
      return { char: '▲', label: 'sharing window' };
    default:
      // Backward compatibility: absent or unrecognized → screen share
      return { char: '▲', label: 'sharing screen' };
  }
}
