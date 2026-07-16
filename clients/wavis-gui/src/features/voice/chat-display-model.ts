/**
 * Chat & Room-Event Display Model
 *
 * Pure, deterministic helpers for the voice-room chat panel and event log:
 * message/event types, date-divider grouping, history merge/dedup/cap,
 * cursor computation, and stable participant coloring.
 *
 * Zero IO — no state, no LiveKit, no Tauri. Extracted from voice-room.ts;
 * exported for direct consumption and property testing.
 */

import { PROFILE_COLORS } from '@shared/colors';

/* ─── Types ─────────────────────────────────────────────────────── */

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
  | 'passthrough'
  | 'system';

export interface ChatMessage {
  id: string;
  messageId?: string;
  timestamp: string;
  participantId: string;
  userId?: string;
  displayName: string;
  color: string;
  text: string;
  isHistory?: boolean;
  isDivider?: boolean;
}

export type ChatDisplayItem =
  { type: 'date-divider'; id: string; label: string } | { type: 'message'; message: ChatMessage };

export interface RoomEvent {
  id: string;
  timestamp: string;
  type: RoomEventType;
  message: string;
  participantId?: string;
  shouldToast?: boolean;
}

export type RoomEventDisplayItem =
  { type: 'date-divider'; id: string; label: string } | { type: 'event'; event: RoomEvent };

/* ─── Constants ─────────────────────────────────────────────────── */

export const MAX_CHAT_MESSAGES = 200;

/* ─── Helpers ───────────────────────────────────────────────────── */

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
  return PROFILE_COLORS[Math.abs(h) % PROFILE_COLORS.length];
}

export function resolveChatMessageDisplayColor(
  message: Pick<ChatMessage, 'participantId' | 'userId' | 'color'>,
  participants: Array<{ id: string; userId?: string; color: string }>,
): string {
  if (message.userId) {
    const userMatch = participants.find((p) => p.userId === message.userId);
    if (userMatch?.color) return userMatch.color;
  }

  const participantMatch = participants.find((p) => p.id === message.participantId);
  if (participantMatch?.color) return participantMatch.color;

  return message.color || colorFor({ userId: message.userId, id: message.participantId });
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

export function getLocalChatDateKey(timestamp: string): string {
  const date = new Date(timestamp);
  if (Number.isNaN(date.getTime())) return timestamp;
  const year = date.getFullYear();
  const month = String(date.getMonth() + 1).padStart(2, '0');
  const day = String(date.getDate()).padStart(2, '0');
  return `${year}-${month}-${day}`;
}

export function formatChatDateLabel(timestamp: string): string {
  const date = new Date(timestamp);
  if (Number.isNaN(date.getTime())) return timestamp;
  return date.toLocaleDateString('en-US', {
    month: 'long',
    day: 'numeric',
    year: 'numeric',
  });
}

export function buildChatDisplayItems(messages: ChatMessage[]): ChatDisplayItem[] {
  const items: ChatDisplayItem[] = [];
  let previousDateKey: string | null = null;

  for (const message of messages) {
    if (message.isDivider) continue;

    const dateKey = getLocalChatDateKey(message.timestamp);
    if (dateKey !== previousDateKey) {
      items.push({
        type: 'date-divider',
        id: `date-${dateKey}-${message.id}`,
        label: formatChatDateLabel(message.timestamp),
      });
      previousDateKey = dateKey;
    }

    items.push({ type: 'message', message });
  }

  return items;
}

export function buildRoomEventDisplayItems(events: RoomEvent[]): RoomEventDisplayItem[] {
  const items: RoomEventDisplayItem[] = [];
  let previousDateKey: string | null = null;

  for (const event of events) {
    const dateKey = getLocalChatDateKey(event.timestamp);
    if (dateKey !== previousDateKey) {
      items.push({
        type: 'date-divider',
        id: `date-${dateKey}-${event.id}`,
        label: formatChatDateLabel(event.timestamp),
      });
      previousDateKey = dateKey;
    }

    items.push({ type: 'event', event });
  }

  return items;
}

/** Format an ISO timestamp for chat/event-log display: `HH:MM:SS` (24h, local time). */
export function formatTime(isoString: string): string {
  try {
    const d = new Date(isoString);
    return d.toLocaleTimeString('en-US', { hour12: false });
  } catch {
    return '??:??:??';
  }
}

export function shouldPlayChatNotification(
  participantId: string,
  selfParticipantId: string | null,
): boolean {
  return !!selfParticipantId && participantId !== selfParticipantId;
}

/**
 * Pure function: merge history messages with existing real-time messages.
 * Deduplicates by messageId, prepends history, inserts divider, enforces cap.
 * Exported for property testing.
 */
export function mergeHistoryMessages(
  historyPayload: Array<{
    messageId: string;
    participantId: string;
    userId?: string;
    displayName: string;
    text: string;
    timestamp: string;
  }>,
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
      userId: h.userId,
      displayName: h.displayName,
      color: colorFor({ userId: h.userId, id: h.participantId }),
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
