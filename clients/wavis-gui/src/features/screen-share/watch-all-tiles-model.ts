/**
 * Pure tile-list reducers extracted from useWatchAllTiles.ts.
 *
 * The hook owns the Tauri event wiring (onWatchAllShareAdded etc.); these
 * functions own the actual state transitions, including the
 * pending-restore-volume queue (a volume/mute restore that arrives before
 * the tile it targets exists yet, and must be applied once that tile shows
 * up instead of being dropped).
 */
import type { ShareTileState, AudioTileState } from './useWatchAllTiles';

export type PendingRestoreVolumes = ReadonlyMap<string, { volume: number; muted: boolean }>;

export interface ShareAddedPayload {
  participantId: string;
  liveKitIdentity?: string;
  displayName: string;
  color: string;
  canvasFallback: boolean;
}

export interface AddShareTileResult {
  tiles: ShareTileState[];
  pendingRestoreVolumes: PendingRestoreVolumes;
}

/** Adds a tile for a newly-seen sharer; a no-op if already present (dedup by participantId). */
export function addShareTile(
  tiles: readonly ShareTileState[],
  pendingRestoreVolumes: PendingRestoreVolumes,
  payload: ShareAddedPayload,
): AddShareTileResult {
  if (tiles.some((t) => t.participantId === payload.participantId)) {
    return { tiles: [...tiles], pendingRestoreVolumes };
  }

  const restored = pendingRestoreVolumes.get(payload.participantId);
  const nextPending = new Map(pendingRestoreVolumes);
  if (restored !== undefined) nextPending.delete(payload.participantId);

  const baseTile: ShareTileState = {
    participantId: payload.participantId,
    liveKitIdentity: payload.liveKitIdentity,
    displayName: payload.displayName,
    color: payload.color,
    kind: 'live',
    canvasFallback: payload.canvasFallback,
    muted: false,
    volume: 70,
    nativeWidth: null,
    nativeHeight: null,
    aspectRatio: 16 / 9,
  };

  const tile = restored === undefined ? baseTile : { ...baseTile, ...restored };
  return { tiles: [...tiles, tile], pendingRestoreVolumes: nextPending };
}

export function removeShareTile(
  tiles: readonly ShareTileState[],
  participantId: string,
): ShareTileState[] {
  return tiles.filter((t) => t.participantId !== participantId);
}

export function updateShareTile(
  tiles: readonly ShareTileState[],
  payload: { participantId: string; displayName: string; color: string },
): ShareTileState[] {
  return tiles.map((t) =>
    t.participantId === payload.participantId
      ? { ...t, displayName: payload.displayName, color: payload.color }
      : t,
  );
}

export interface RestoreVolumeTilesResult {
  tiles: ShareTileState[];
  pendingRestoreVolumes: PendingRestoreVolumes;
}

/**
 * Applies a volume/mute restore to whichever share tile currently has this
 * participantId. If no tile has it yet, the restore is queued (consumed
 * later by addShareTile) rather than dropped.
 */
export function applyRestoreVolumeToTiles(
  tiles: readonly ShareTileState[],
  pendingRestoreVolumes: PendingRestoreVolumes,
  payload: { participantId: string; volume: number; muted: boolean },
): RestoreVolumeTilesResult {
  let found = false;
  const nextTiles = tiles.map((tile) => {
    if (tile.participantId !== payload.participantId) return tile;
    found = true;
    return { ...tile, volume: payload.volume, muted: payload.muted };
  });

  const nextPending = new Map(pendingRestoreVolumes);
  // ESLint's narrowing doesn't see `found` mutated inside the tiles.map()
  // callback above — if no tile matches, found genuinely stays false and
  // this branch is how a not-yet-created tile's volume gets queued.
  // eslint-disable-next-line @typescript-eslint/no-unnecessary-condition
  if (!found) {
    nextPending.set(payload.participantId, { volume: payload.volume, muted: payload.muted });
  }

  return { tiles: nextTiles, pendingRestoreVolumes: nextPending };
}

/** Same restore applied to the audio-only tile list (no pending-queue — audio tiles are seeded separately). */
export function applyRestoreVolumeToAudioTiles(
  audioTiles: readonly AudioTileState[],
  payload: { participantId: string; volume: number; muted: boolean },
): AudioTileState[] {
  return audioTiles.map((tile) =>
    tile.participantId === payload.participantId
      ? { ...tile, volume: payload.volume, muted: payload.muted }
      : tile,
  );
}

export function addAudioTile(
  audioTiles: readonly AudioTileState[],
  payload: AudioTileState,
): AudioTileState[] {
  if (audioTiles.some((t) => t.participantId === payload.participantId)) {
    return [...audioTiles];
  }
  return [...audioTiles, payload];
}

export function removeAudioTile(
  audioTiles: readonly AudioTileState[],
  participantId: string,
): AudioTileState[] {
  return audioTiles.filter((t) => t.participantId !== participantId);
}
