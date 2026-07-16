/**
 * Tests for watch-all-tiles-model.ts (P2-8) — the tile-list reducers
 * extracted from useWatchAllTiles.ts, which had no direct tests at all.
 * Covers the dedup-by-participantId behavior and the pending-restore-volume
 * queue (a volume/mute restore arriving before its target tile exists).
 */
import { describe, it, expect } from 'vitest';
import fc from 'fast-check';
import {
  addShareTile,
  removeShareTile,
  updateShareTile,
  applyRestoreVolumeToTiles,
  applyRestoreVolumeToAudioTiles,
  addAudioTile,
  removeAudioTile,
} from '../watch-all-tiles-model';
import type { ShareTileState, AudioTileState } from '../useWatchAllTiles';

function makeTile(overrides: Partial<ShareTileState> = {}): ShareTileState {
  return {
    participantId: 'p1',
    displayName: 'Alice',
    color: '#fff',
    kind: 'live',
    canvasFallback: false,
    muted: false,
    volume: 70,
    nativeWidth: null,
    nativeHeight: null,
    aspectRatio: 16 / 9,
    ...overrides,
  };
}

/* ═══ addShareTile ══════════════════════════════════════════════ */

describe('addShareTile', () => {
  it('adds a new tile with default volume/mute when nothing was pending', () => {
    const result = addShareTile([], new Map(), {
      participantId: 'p1',
      displayName: 'Alice',
      color: '#fff',
      canvasFallback: false,
    });
    expect(result.tiles).toHaveLength(1);
    expect(result.tiles[0]).toMatchObject({ participantId: 'p1', volume: 70, muted: false });
    expect(result.pendingRestoreVolumes.size).toBe(0);
  });

  it('is a no-op if the participant already has a tile (dedup)', () => {
    const existing = [makeTile({ participantId: 'p1', volume: 40 })];
    const result = addShareTile(existing, new Map(), {
      participantId: 'p1',
      displayName: 'Alice',
      color: '#fff',
      canvasFallback: false,
    });
    expect(result.tiles).toHaveLength(1);
    expect(result.tiles[0].volume).toBe(40); // untouched, not reset to default
  });

  it('applies a pending restore-volume queued before this tile existed, then consumes it', () => {
    const pending = new Map([['p1', { volume: 12, muted: true }]]);
    const result = addShareTile([], pending, {
      participantId: 'p1',
      displayName: 'Alice',
      color: '#fff',
      canvasFallback: false,
    });
    expect(result.tiles[0]).toMatchObject({ volume: 12, muted: true });
    expect(result.pendingRestoreVolumes.has('p1')).toBe(false); // consumed
  });

  it('a pending restore for a different participant is left untouched', () => {
    const pending = new Map([['other', { volume: 12, muted: true }]]);
    const result = addShareTile([], pending, {
      participantId: 'p1',
      displayName: 'Alice',
      color: '#fff',
      canvasFallback: false,
    });
    expect(result.tiles[0]).toMatchObject({ volume: 70, muted: false });
    expect(result.pendingRestoreVolumes.has('other')).toBe(true);
  });

  it('does not mutate the input tiles array or pending map', () => {
    const inputTiles = [makeTile({ participantId: 'existing' })];
    const inputPending = new Map([['x', { volume: 1, muted: false }]]);
    addShareTile(inputTiles, inputPending, {
      participantId: 'p1',
      displayName: 'A',
      color: '#fff',
      canvasFallback: false,
    });
    expect(inputTiles).toHaveLength(1);
    expect(inputPending.size).toBe(1);
  });
});

/* ═══ removeShareTile / updateShareTile ═════════════════════════ */

describe('removeShareTile', () => {
  it('removes only the matching participant', () => {
    const tiles = [makeTile({ participantId: 'p1' }), makeTile({ participantId: 'p2' })];
    const result = removeShareTile(tiles, 'p1');
    expect(result.map((t) => t.participantId)).toEqual(['p2']);
  });

  it('removing an untracked participant is a no-op', () => {
    const tiles = [makeTile({ participantId: 'p1' })];
    expect(removeShareTile(tiles, 'ghost')).toEqual(tiles);
  });
});

describe('updateShareTile', () => {
  it('updates displayName/color for the matching participant only', () => {
    const tiles = [
      makeTile({ participantId: 'p1', displayName: 'Old', color: '#000' }),
      makeTile({ participantId: 'p2', displayName: 'Keep', color: '#111' }),
    ];
    const result = updateShareTile(tiles, {
      participantId: 'p1',
      displayName: 'New',
      color: '#fff',
    });
    expect(result.find((t) => t.participantId === 'p1')).toMatchObject({
      displayName: 'New',
      color: '#fff',
    });
    expect(result.find((t) => t.participantId === 'p2')).toMatchObject({
      displayName: 'Keep',
      color: '#111',
    });
  });
});

/* ═══ applyRestoreVolumeToTiles / applyRestoreVolumeToAudioTiles ═ */

describe('applyRestoreVolumeToTiles', () => {
  it('applies volume/mute to the matching tile and does not queue a pending restore', () => {
    const tiles = [makeTile({ participantId: 'p1', volume: 70, muted: false })];
    const result = applyRestoreVolumeToTiles(tiles, new Map(), {
      participantId: 'p1',
      volume: 33,
      muted: true,
    });
    expect(result.tiles[0]).toMatchObject({ volume: 33, muted: true });
    expect(result.pendingRestoreVolumes.size).toBe(0);
  });

  it('queues the restore when no tile matches yet, without touching existing tiles', () => {
    const tiles = [makeTile({ participantId: 'other', volume: 50 })];
    const result = applyRestoreVolumeToTiles(tiles, new Map(), {
      participantId: 'p1',
      volume: 33,
      muted: true,
    });
    expect(result.tiles).toEqual(tiles);
    expect(result.pendingRestoreVolumes.get('p1')).toEqual({ volume: 33, muted: true });
  });

  it('a restore for one participant never changes another tile', () => {
    const tiles = [
      makeTile({ participantId: 'p1', volume: 10 }),
      makeTile({ participantId: 'p2', volume: 20 }),
    ];
    const result = applyRestoreVolumeToTiles(tiles, new Map(), {
      participantId: 'p1',
      volume: 99,
      muted: false,
    });
    expect(result.tiles.find((t) => t.participantId === 'p2')).toMatchObject({ volume: 20 });
  });
});

describe('applyRestoreVolumeToAudioTiles', () => {
  it('applies volume/mute only to the matching audio tile', () => {
    const audioTiles: AudioTileState[] = [
      { participantId: 'p1', displayName: 'A', color: '#fff', volume: 70, muted: false },
      { participantId: 'p2', displayName: 'B', color: '#000', volume: 50, muted: false },
    ];
    const result = applyRestoreVolumeToAudioTiles(audioTiles, {
      participantId: 'p1',
      volume: 5,
      muted: true,
    });
    expect(result.find((t) => t.participantId === 'p1')).toMatchObject({ volume: 5, muted: true });
    expect(result.find((t) => t.participantId === 'p2')).toMatchObject({
      volume: 50,
      muted: false,
    });
  });
});

/* ═══ addAudioTile / removeAudioTile ═════════════════════════════ */

describe('addAudioTile / removeAudioTile', () => {
  it('adds a new audio tile', () => {
    const result = addAudioTile([], {
      participantId: 'p1',
      displayName: 'A',
      color: '#fff',
      volume: 70,
      muted: true,
    });
    expect(result).toHaveLength(1);
  });

  it('is a no-op if the participant already has an audio tile (dedup)', () => {
    const existing: AudioTileState[] = [
      { participantId: 'p1', displayName: 'A', color: '#fff', volume: 1, muted: true },
    ];
    const result = addAudioTile(existing, {
      participantId: 'p1',
      displayName: 'A',
      color: '#fff',
      volume: 99,
      muted: false,
    });
    expect(result).toHaveLength(1);
    expect(result[0].volume).toBe(1); // untouched
  });

  it('removes only the matching participant', () => {
    const existing: AudioTileState[] = [
      { participantId: 'p1', displayName: 'A', color: '#fff', volume: 1, muted: true },
      { participantId: 'p2', displayName: 'B', color: '#000', volume: 2, muted: false },
    ];
    expect(removeAudioTile(existing, 'p1').map((t) => t.participantId)).toEqual(['p2']);
  });
});

/* ═══ Properties ══════════════════════════════════════════════════ */

describe('properties', () => {
  it('addShareTile followed by removeShareTile for the same id returns to the original list content', () => {
    fc.assert(
      fc.property(fc.array(fc.string({ minLength: 1, maxLength: 6 }), { maxLength: 5 }), (ids) => {
        const uniqueIds = [...new Set(ids)];
        const tiles = uniqueIds.map((id) => makeTile({ participantId: id }));
        const added = addShareTile(tiles, new Map(), {
          participantId: 'new-participant',
          displayName: 'New',
          color: '#abc',
          canvasFallback: false,
        });
        const removed = removeShareTile(added.tiles, 'new-participant');
        expect(removed.map((t) => t.participantId).sort()).toEqual(uniqueIds.sort());
      }),
      { numRuns: 20 },
    );
  });

  it('addShareTile is idempotent under repeated delivery of the same share-added event', () => {
    fc.assert(
      fc.property(fc.string({ minLength: 1, maxLength: 8 }), (id) => {
        const payload = {
          participantId: id,
          displayName: 'X',
          color: '#fff',
          canvasFallback: false,
        };
        const once = addShareTile([], new Map(), payload);
        const twice = addShareTile(once.tiles, once.pendingRestoreVolumes, payload);
        expect(twice.tiles).toHaveLength(1);
      }),
      { numRuns: 20 },
    );
  });
});
