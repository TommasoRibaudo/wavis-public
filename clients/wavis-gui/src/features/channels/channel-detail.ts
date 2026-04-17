/**
 * Wavis Channel Detail Service (Tauri)
 *
 * Single-channel REST operations: detail fetch, voice status,
 * invite management, ban management, role changes, delete, leave.
 * All calls go through api.ts authenticated fetch wrapper.
 */

import type { ChannelRole } from './channels';
import { apiFetch } from '@shared/api';

// ─── Types ─────────────────────────────────────────────────────────

export interface ChannelMember {
  userId: string;
  role: ChannelRole;
  joinedAt: string;
  displayName: string;
}

export interface ChannelDetailData {
  channelId: string;
  name: string;
  ownerUserId: string;
  createdAt: string;
  role: ChannelRole;
  members: ChannelMember[];
}

export interface VoiceStatus {
  active: boolean;
  participantCount: number | null;
  participants: VoiceParticipant[] | null;
}

export interface VoiceParticipant {
  displayName: string;
}

export interface ChannelInvite {
  code: string;
  channelId: string;
  expiresAt: string | null;
  maxUses: number | null;
  uses: number;
}

export interface BannedMember {
  userId: string;
  bannedAt: string;
}


// ─── Backend Response Types (private) ──────────────────────────────

interface BackendChannelDetailResponse {
  channel_id: string;
  name: string;
  owner_user_id: string;
  created_at: string;
  role: string;
  members: Array<{
    user_id: string;
    role: string;
    joined_at: string;
    display_name: string;
  }>;
}

interface BackendVoiceStatusResponse {
  active: boolean;
  participant_count?: number;
  participants?: Array<{ display_name: string }>;
}

interface BackendCreateInviteResponse {
  code: string;
  channel_id: string;
  expires_at: string | null;
  max_uses: number | null;
  uses: number;
}

interface BackendInviteListItem {
  code: string;
  channel_id: string;
  expires_at: string | null;
  max_uses: number | null;
  uses: number;
}

interface BackendBanListResponse {
  banned: Array<{ user_id: string; banned_at: string }>;
}

interface BackendBanResponse {
  channel_id: string;
  user_id: string;
  banned_at: string;
}

interface BackendRoleChangeResponse {
  channel_id: string;
  user_id: string;
  role: string;
}

// ─── Helpers (private) ─────────────────────────────────────────────

function mapChannelDetail(res: BackendChannelDetailResponse): ChannelDetailData {
  return {
    channelId: res.channel_id,
    name: res.name,
    ownerUserId: res.owner_user_id,
    createdAt: res.created_at,
    role: res.role as ChannelRole,
    members: res.members.map((m) => ({
      userId: m.user_id,
      role: m.role as ChannelRole,
      joinedAt: m.joined_at,
      displayName: m.display_name,
    })),
  };
}

function mapVoiceStatus(res: BackendVoiceStatusResponse): VoiceStatus {
  return {
    active: res.active,
    participantCount: res.participant_count ?? null,
    participants: res.participants?.map((p) => ({
      displayName: p.display_name,
    })) ?? null,
  };
}

function mapInvite(res: BackendCreateInviteResponse | BackendInviteListItem): ChannelInvite {
  return {
    code: res.code,
    channelId: res.channel_id,
    expiresAt: res.expires_at,
    maxUses: res.max_uses,
    uses: res.uses,
  };
}

// ─── Constants ─────────────────────────────────────────────────────

/** Timeout for auto-refresh voice status fetches (NFR-2: 5 seconds) */
export const AUTO_REFRESH_TIMEOUT_MS = 5000;

// ─── API Functions (exported) ──────────────────────────────────────

export async function fetchChannelDetail(
  channelId: string,
  signal?: AbortSignal,
): Promise<ChannelDetailData> {
  const res = await apiFetch<BackendChannelDetailResponse>(
    `/channels/${channelId}`,
    signal ? { signal } : {},
  );
  return mapChannelDetail(res);
}

export async function fetchVoiceStatus(channelId: string): Promise<VoiceStatus> {
  const res = await apiFetch<BackendVoiceStatusResponse>(
    `/channels/${channelId}/voice`,
  );
  return mapVoiceStatus(res);
}

export async function fetchInvites(channelId: string): Promise<ChannelInvite[]> {
  const items = await apiFetch<BackendInviteListItem[]>(
    `/channels/${channelId}/invites`,
  );
  return items.map(mapInvite);
}

export async function createInvite(
  channelId: string,
  expiresInSecs?: number,
  maxUses?: number,
): Promise<ChannelInvite> {
  const body: Record<string, unknown> = {};
  if (expiresInSecs !== undefined) body.expires_in_secs = expiresInSecs;
  if (maxUses !== undefined) body.max_uses = maxUses;
  const res = await apiFetch<BackendCreateInviteResponse>(
    `/channels/${channelId}/invites`,
    { method: 'POST', body: JSON.stringify(body) },
  );
  return mapInvite(res);
}

export async function revokeInvite(
  channelId: string,
  code: string,
): Promise<void> {
  await apiFetch(`/channels/${channelId}/invites/${code}`, {
    method: 'DELETE',
  });
}

export async function fetchBannedMembers(
  channelId: string,
): Promise<BannedMember[]> {
  const res = await apiFetch<BackendBanListResponse>(
    `/channels/${channelId}/bans`,
  );
  return res.banned.map((b) => ({
    userId: b.user_id,
    bannedAt: b.banned_at,
  }));
}

export async function banMember(
  channelId: string,
  userId: string,
): Promise<void> {
  await apiFetch<BackendBanResponse>(
    `/channels/${channelId}/bans/${userId}`,
    { method: 'POST' },
  );
}

export async function unbanMember(
  channelId: string,
  userId: string,
): Promise<void> {
  await apiFetch(`/channels/${channelId}/bans/${userId}`, {
    method: 'DELETE',
  });
}

export async function changeMemberRole(
  channelId: string,
  userId: string,
  role: 'admin' | 'member',
): Promise<void> {
  await apiFetch<BackendRoleChangeResponse>(
    `/channels/${channelId}/members/${userId}/role`,
    { method: 'PUT', body: JSON.stringify({ role }) },
  );
}

export async function deleteChannel(channelId: string): Promise<void> {
  await apiFetch(`/channels/${channelId}`, { method: 'DELETE' });
}

export async function leaveChannel(channelId: string): Promise<void> {
  await apiFetch(`/channels/${channelId}/leave`, { method: 'POST' });
}


// ─── Voice Status Batch Fetching ───────────────────────────────────

interface BatchVoiceStatusResponse {
  [channelId: string]: { active: boolean; participant_count: number };
}

/**
 * Fetch voice status for multiple channels with three-tier fallback:
 * 1. Inline fields from channel objects (voiceActive/voiceParticipantCount)
 * 2. Batch GET /channels/voice-status?ids=a,b,c
 * 3. Per-channel fetchVoiceStatus() loop via Promise.allSettled()
 *
 * Aborts on timeout (AUTO_REFRESH_TIMEOUT_MS), retains stale data.
 * Hides all indicators on fetch failure (silent).
 */
export async function fetchVoiceStatusWithFallback(
  channelIds: string[],
  inlineData?: Array<{ id: string; voiceActive?: boolean; voiceParticipantCount?: number }>,
): Promise<Map<string, { active: boolean; participantCount: number }>> {
  const result = new Map<string, { active: boolean; participantCount: number }>();
  if (channelIds.length === 0) return result;

  // Tier 1: inline fields from channel list response
  if (inlineData) {
    let allPresent = true;
    for (const ch of inlineData) {
      if (ch.voiceActive !== undefined) {
        result.set(ch.id, {
          active: ch.voiceActive,
          participantCount: ch.voiceParticipantCount ?? 0,
        });
      } else {
        allPresent = false;
      }
    }
    if (allPresent && result.size === channelIds.length) return result;
    result.clear();
  }

  const controller = new AbortController();
  const timeout = setTimeout(() => controller.abort(), AUTO_REFRESH_TIMEOUT_MS);

  try {
    // Tier 2: batch endpoint
    const ids = channelIds.join(',');
    const batch = await apiFetch<BatchVoiceStatusResponse>(
      `/channels/voice-status?ids=${encodeURIComponent(ids)}`,
      { signal: controller.signal },
    );
    for (const [id, status] of Object.entries(batch)) {
      result.set(id, {
        active: status.active,
        participantCount: status.participant_count,
      });
    }
    return result;
  } catch {
    // Tier 2 failed — fall back to tier 3
  } finally {
    clearTimeout(timeout);
  }

  // Tier 3: per-channel fetchVoiceStatus loop
  const controller2 = new AbortController();
  const timeout2 = setTimeout(() => controller2.abort(), AUTO_REFRESH_TIMEOUT_MS);
  try {
    const results = await Promise.allSettled(
      channelIds.map((id) => fetchVoiceStatus(id)),
    );
    for (let i = 0; i < channelIds.length; i++) {
      const r = results[i];
      if (r.status === 'fulfilled') {
        result.set(channelIds[i], {
          active: r.value.active,
          participantCount: r.value.participantCount ?? 0,
        });
      }
    }
  } catch {
    // Silent failure — return empty map
  } finally {
    clearTimeout(timeout2);
  }

  return result;
}
