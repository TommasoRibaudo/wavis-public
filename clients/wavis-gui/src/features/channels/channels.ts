/**
 * Wavis Channels Service (Tauri)
 *
 * Channel CRUD via authenticated API calls.
 * Maps backend snake_case responses to client camelCase interfaces.
 */

import { apiFetch } from '@shared/api';

// ─── Types ─────────────────────────────────────────────────────────

export type ChannelRole = 'owner' | 'admin' | 'member';

export interface Channel {
  id: string;
  name: string;
  role: ChannelRole;
  ownerUserId: string;
  createdAt: string;
}

// Backend response types (private, for mapping)
interface BackendChannelListItem {
  channel_id: string;
  name: string;
  owner_user_id: string;
  created_at: string;
  role: string;
}

interface BackendCreateChannelResponse {
  channel_id: string;
  name: string;
  owner_user_id: string;
  created_at: string;
}

interface BackendJoinChannelResponse {
  channel_id: string;
  name: string;
  role: string;
}

// ─── Helpers (private) ─────────────────────────────────────────────

function mapChannelListItem(item: BackendChannelListItem): Channel {
  return {
    id: item.channel_id,
    name: item.name,
    ownerUserId: item.owner_user_id,
    createdAt: item.created_at,
    role: item.role as ChannelRole,
  };
}

// ─── API Functions (exported) ──────────────────────────────────────

export async function fetchChannels(): Promise<Channel[]> {
  const items = await apiFetch<BackendChannelListItem[]>('/channels');
  return items.map(mapChannelListItem);
}

export async function createChannel(name: string): Promise<Channel> {
  const res = await apiFetch<BackendCreateChannelResponse>('/channels', {
    method: 'POST',
    body: JSON.stringify({ name }),
  });
  return {
    id: res.channel_id,
    name: res.name,
    ownerUserId: res.owner_user_id,
    createdAt: res.created_at,
    role: 'owner',
  };
}

/**
 * Join a channel by invite code.
 * Re-fetches the full channel list after join to get complete data
 * (join response omits owner_user_id and created_at).
 */
export async function joinChannelByInvite(code: string): Promise<Channel> {
  const joinRes = await apiFetch<BackendJoinChannelResponse>('/channels/join', {
    method: 'POST',
    body: JSON.stringify({ code }),
  });
  const channels = await fetchChannels();
  const found = channels.find((ch) => ch.id === joinRes.channel_id);
  if (found) return found;
  // Fallback if channel not found in list (shouldn't happen)
  return {
    id: joinRes.channel_id,
    name: joinRes.name,
    ownerUserId: '',
    createdAt: '',
    role: joinRes.role as ChannelRole,
  };
}

/**
 * Normalize a channel name: lowercase, replace non-alphanumeric with hyphens,
 * collapse consecutive hyphens, trim leading/trailing hyphens.
 */
export function normalizeChannelName(name: string): string {
  return name
    .toLowerCase()
    .replace(/[^a-z0-9]/g, '-')
    .replace(/-+/g, '-')
    .replace(/^-|-$/g, '');
}
