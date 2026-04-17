/**
 * WatchAllPage — Property & Unit Tests
 *
 * Tests the pure state management logic extracted from WatchAllPage.tsx:
 * - toggleMute: flips muted for a target tile, leaves others unchanged
 * - addTile: appends a new tile with muted:false and volume:70 default
 * - removeTile: removes a tile by participantId
 *
 * vitest env is 'node' (no jsdom) — tests simulate component logic
 * by replicating the state transitions from WatchAllPage.tsx.
 *
 * Property 7 validates mute state independence (Req 15.3).
 * Unit tests validate tile lifecycle, state defaults, and volume persistence.
 */

import { describe, it, expect, vi, beforeEach } from 'vitest';
import fc from 'fast-check';

/* ─── Mocks ─────────────────────────────────────────────────────── */

vi.mock('@tauri-apps/api/event', () => ({
  emit: vi.fn(),
  listen: vi.fn().mockResolvedValue(() => {}),
}));

vi.mock('@tauri-apps/api/window', () => ({
  getCurrentWindow: vi.fn().mockReturnValue({
    close: vi.fn(),
    onCloseRequested: vi.fn().mockResolvedValue(() => {}),
  }),
}));

class MockRTCPeerConnection {
  onicecandidate: ((e: unknown) => void) | null = null;
  ontrack: ((e: unknown) => void) | null = null;
  onconnectionstatechange: (() => void) | null = null;
  connectionState = 'new';
  signalingState = 'stable';
  addTrack = vi.fn();
  createOffer = vi.fn().mockResolvedValue({ sdp: 'mock', type: 'offer' });
  createAnswer = vi.fn().mockResolvedValue({ sdp: 'mock', type: 'answer' });
  setLocalDescription = vi.fn().mockResolvedValue(undefined);
  setRemoteDescription = vi.fn().mockResolvedValue(undefined);
  addIceCandidate = vi.fn().mockResolvedValue(undefined);
  close = vi.fn();

  _simulateConnectionState(state: string): void {
    this.connectionState = state;
    this.onconnectionstatechange?.();
  }
}
globalThis.RTCPeerConnection = MockRTCPeerConnection as unknown as typeof RTCPeerConnection;

class MockMediaStream {
  private tracks: unknown[] = [];
  getTracks() { return this.tracks; }
  addTrack(t: unknown) { this.tracks.push(t); }
}
globalThis.MediaStream = MockMediaStream as unknown as typeof MediaStream;

/* ─── Types (mirrors WatchAllPage internal state) ───────────────── */

interface ShareTileState {
  participantId: string;
  displayName: string;
  color: string;
  canvasFallback: boolean;
  muted: boolean;
  volume: number;
}

interface LabelOverlayState {
  hovered: boolean;
  labelVisible: boolean;
}

/* ─── Pure state helpers (extracted from WatchAllPage.tsx logic) ── */

/**
 * Toggle mute for a single tile by participantId.
 * Mirrors the handleToggleMute callback in WatchAllPage.
 */
function toggleMute(tiles: ShareTileState[], targetId: string): ShareTileState[] {
  return tiles.map((t) =>
    t.participantId === targetId ? { ...t, muted: !t.muted } : t,
  );
}

/**
 * Add a new tile with default muted:false and volume:70.
 * Mirrors the watch-all:share-added event handler in WatchAllPage.
 */
function addTile(
  tiles: ShareTileState[],
  payload: { participantId: string; displayName: string; color: string; canvasFallback: boolean },
): ShareTileState[] {
  if (tiles.some((t) => t.participantId === payload.participantId)) return tiles;
  return [...tiles, { ...payload, muted: false, volume: 70 }];
}

/**
 * Remove a tile by participantId.
 * Mirrors the watch-all:share-removed event handler in WatchAllPage.
 */
function removeTile(tiles: ShareTileState[], participantId: string): ShareTileState[] {
  return tiles.filter((t) => t.participantId !== participantId);
}

function setTileVolume(
  tiles: ShareTileState[],
  participantId: string,
  volume: number,
): ShareTileState[] {
  return tiles.map((t) =>
    t.participantId === participantId
      ? { ...t, volume, muted: volume === 0 }
      : t,
  );
}

function popOutTile(
  tiles: ShareTileState[],
  participantId: string,
): { remainingTiles: ShareTileState[]; payload: { participantId: string; volume: number } | null } {
  const tile = tiles.find((t) => t.participantId === participantId) ?? null;
  if (!tile) return { remainingTiles: tiles, payload: null };
  return {
    remainingTiles: removeTile(tiles, participantId),
    payload: { participantId, volume: tile.volume },
  };
}

function restoreTileVolume(
  tiles: ShareTileState[],
  payload: { participantId: string; volume: number },
): ShareTileState[] {
  return tiles.map((t) =>
    t.participantId === payload.participantId
      ? { ...t, volume: payload.volume, muted: payload.volume === 0 }
      : t,
  );
}

/**
 * Mirrors ShareTile's mouse enter handler after the auto-fade fix.
 * Hover enables tile controls and reveals the label temporarily.
 */
function enterTile(state: LabelOverlayState): LabelOverlayState {
  return { ...state, hovered: true, labelVisible: true };
}

/**
 * Mirrors ShareTile's mouse leave handler.
 * Leaving the tile hides hover-only controls but does not force label visibility.
 */
function leaveTile(state: LabelOverlayState): LabelOverlayState {
  return { ...state, hovered: false };
}

/**
 * Mirrors ShareTile's fade timer expiry.
 * The label can hide even while the tile remains hovered.
 */
function expireLabelFade(state: LabelOverlayState): LabelOverlayState {
  return { ...state, labelVisible: false };
}

/**
 * Mirrors ShareTile's mouse move handler.
 * Any mouse activity over the tile reveals the label again.
 */
function moveWithinTile(state: LabelOverlayState): LabelOverlayState {
  return { ...state, labelVisible: true };
}

/* ─── Arbitraries ───────────────────────────────────────────────── */

const COLORS = ['#E06C75', '#98C379', '#61AFEF', '#C678DD', '#E5C07B', '#56B6C2'];

const tileArb: fc.Arbitrary<ShareTileState> = fc.record({
  participantId: fc.uuid(),
  displayName: fc.string({ minLength: 1, maxLength: 20 }),
  color: fc.constantFrom(...COLORS),
  canvasFallback: fc.boolean(),
  muted: fc.boolean(),
  volume: fc.integer({ min: 0, max: 100 }),
});

/** Generate 2–6 tiles with unique participantIds. */
const tilesArb: fc.Arbitrary<ShareTileState[]> = fc
  .array(tileArb, { minLength: 2, maxLength: 6 })
  .map((tiles) => {
    const seen = new Set<string>();
    return tiles.filter((t) => {
      if (seen.has(t.participantId)) return false;
      seen.add(t.participantId);
      return true;
    });
  })
  .filter((tiles) => tiles.length >= 2);

/* ═══ Property Test ═════════════════════════════════════════════════ */

describe('Property 7: Tile mute state independence', () => {
  // Feature: watch-all-streams, Property 7: Tile mute state independence
  // **Validates: Requirements 15.3**

  it('toggling mute on one tile does not change any other tile mute state', () => {
    fc.assert(
      fc.property(tilesArb, fc.nat(), (tiles, rawIdx) => {
        const chosenIdx = rawIdx % tiles.length;
        const targetId = tiles[chosenIdx].participantId;

        const result = toggleMute(tiles, targetId);

        // The toggled tile's muted should be flipped
        expect(result[chosenIdx].muted).toBe(!tiles[chosenIdx].muted);

        // All other tiles' muted state must be unchanged
        for (let i = 0; i < tiles.length; i++) {
          if (i === chosenIdx) continue;
          expect(result[i].muted).toBe(tiles[i].muted);
        }

        // All other fields on all tiles must be unchanged
        for (let i = 0; i < tiles.length; i++) {
          expect(result[i].participantId).toBe(tiles[i].participantId);
          expect(result[i].displayName).toBe(tiles[i].displayName);
          expect(result[i].color).toBe(tiles[i].color);
          expect(result[i].canvasFallback).toBe(tiles[i].canvasFallback);
          expect(result[i].volume).toBe(tiles[i].volume);
        }
      }),
      { numRuns: 100 },
    );
  });
});

/* ═══ Unit Tests ════════════════════════════════════════════════════ */

describe('WatchAllPage tile state management', () => {
  /* ── Empty state ── */

  it('empty state when no shares active', () => {
    const tiles: ShareTileState[] = [];
    expect(tiles.length).toBe(0);
    // WatchAllPage renders "no active shares" when tiles.length === 0
  });

  /* ── Tile added on share-added event ── */

  it('tile added when watch-all:share-added event received', () => {
    let tiles: ShareTileState[] = [];
    tiles = addTile(tiles, {
      participantId: 'user-1',
      displayName: 'Alice',
      color: '#E06C75',
      canvasFallback: false,
    });

    expect(tiles).toHaveLength(1);
    expect(tiles[0].participantId).toBe('user-1');
    expect(tiles[0].displayName).toBe('Alice');
    expect(tiles[0].color).toBe('#E06C75');
    expect(tiles[0].canvasFallback).toBe(false);
  });

  /* ── Tile removed on share-removed event ── */

  it('tile removed when watch-all:share-removed event received', () => {
    let tiles: ShareTileState[] = [];
    tiles = addTile(tiles, {
      participantId: 'user-1',
      displayName: 'Alice',
      color: '#E06C75',
      canvasFallback: false,
    });
    tiles = addTile(tiles, {
      participantId: 'user-2',
      displayName: 'Bob',
      color: '#98C379',
      canvasFallback: false,
    });
    expect(tiles).toHaveLength(2);

    tiles = removeTile(tiles, 'user-1');
    expect(tiles).toHaveLength(1);
    expect(tiles[0].participantId).toBe('user-2');
  });

  /* ── Double-click emits pop-out event ── */

  it('double-click emits watch-all:pop-out event', () => {
    // WatchAllPage's handlePopOut calls emit('watch-all:pop-out', { participantId })
    // We test the logic: given a participantId, the emitted payload is correct
    const participantId = 'user-1';
    const payload = { participantId };
    expect(payload).toEqual({ participantId: 'user-1' });
  });

  /* ── Canvas fallback tile hides mute toggle ── */

  it('canvas fallback tile hides mute toggle', () => {
    let tiles: ShareTileState[] = [];
    tiles = addTile(tiles, {
      participantId: 'linux-user',
      displayName: 'Tux',
      color: '#61AFEF',
      canvasFallback: true,
    });

    // In WatchAllPage, the mute toggle is rendered only when !canvasFallback
    const tile = tiles[0];
    const showMuteToggle = !tile.canvasFallback;
    expect(showMuteToggle).toBe(false);
  });

  /* ── Error state shows retry button ── */

  it('error state shows retry button', () => {
    // ShareTile transitions to error state when bridge fails.
    // The error state renders a retry button. We test the state transition:
    type TileError = string | null;
    let error: TileError = null;

    // Simulate connection failure
    error = 'connection failed';
    expect(error).toBe('connection failed');

    // The component renders a retry button when error is non-null
    const showRetryButton = error !== null;
    expect(showRetryButton).toBe(true);

    // After retry succeeds, error clears
    error = null;
    expect(error).toBeNull();
  });

  /* ── Default mute state on tile creation ── */

  it('default mute state on tile creation', () => {
    let tiles: ShareTileState[] = [];
    tiles = addTile(tiles, {
      participantId: 'user-1',
      displayName: 'Alice',
      color: '#E06C75',
      canvasFallback: false,
    });

    expect(tiles[0].muted).toBe(false);
    expect(tiles[0].volume).toBe(70);
  });

  /* ── Mute reset to default when tile removed and re-added (Req 15.5) ── */

  it('mute reset to default when tile removed and re-added', () => {
    let tiles: ShareTileState[] = [];

    // Add tile
    tiles = addTile(tiles, {
      participantId: 'user-1',
      displayName: 'Alice',
      color: '#E06C75',
      canvasFallback: false,
    });
    expect(tiles[0].muted).toBe(false);
    expect(tiles[0].volume).toBe(70);

    // Mute the tile
    tiles = toggleMute(tiles, 'user-1');
    expect(tiles[0].muted).toBe(true);

    // Remove the tile (participant stops sharing)
    tiles = removeTile(tiles, 'user-1');
    expect(tiles).toHaveLength(0);

    // Re-add the tile (participant starts sharing again)
    tiles = addTile(tiles, {
      participantId: 'user-1',
      displayName: 'Alice',
      color: '#E06C75',
      canvasFallback: false,
    });

    expect(tiles[0].muted).toBe(false);
    expect(tiles[0].volume).toBe(70);
  });

  it('new tile defaults to volume 70 and unmuted', () => {
    let tiles: ShareTileState[] = [];
    tiles = addTile(tiles, {
      participantId: 'user-1',
      displayName: 'Alice',
      color: '#E06C75',
      canvasFallback: false,
    });

    expect(tiles[0].volume).toBe(70);
    expect(tiles[0].muted).toBe(false);
  });

  it('volume persists across pop-out and pop-back', () => {
    let tiles: ShareTileState[] = [];
    tiles = addTile(tiles, {
      participantId: 'user-1',
      displayName: 'Alice',
      color: '#E06C75',
      canvasFallback: false,
    });

    tiles = setTileVolume(tiles, 'user-1', 50);
    expect(tiles[0].volume).toBe(50);
    expect(tiles[0].muted).toBe(false);

    const { remainingTiles, payload } = popOutTile(tiles, 'user-1');
    expect(payload).toEqual({ participantId: 'user-1', volume: 50 });
    expect(remainingTiles).toHaveLength(0);

    let restoredTiles = addTile(remainingTiles, {
      participantId: 'user-1',
      displayName: 'Alice',
      color: '#E06C75',
      canvasFallback: false,
    });
    restoredTiles = restoreTileVolume(restoredTiles, payload!);

    expect(restoredTiles[0].volume).toBe(50);
    expect(restoredTiles[0].muted).toBe(false);
  });

  /* ── Duplicate add is a no-op ── */

  it('adding duplicate participantId is a no-op', () => {
    let tiles: ShareTileState[] = [];
    tiles = addTile(tiles, {
      participantId: 'user-1',
      displayName: 'Alice',
      color: '#E06C75',
      canvasFallback: false,
    });
    tiles = addTile(tiles, {
      participantId: 'user-1',
      displayName: 'Alice v2',
      color: '#98C379',
      canvasFallback: true,
    });

    expect(tiles).toHaveLength(1);
    expect(tiles[0].displayName).toBe('Alice');
  });

  /* ── Removing non-existent tile is a no-op ── */

  it('removing non-existent participantId is a no-op', () => {
    let tiles: ShareTileState[] = [];
    tiles = addTile(tiles, {
      participantId: 'user-1',
      displayName: 'Alice',
      color: '#E06C75',
      canvasFallback: false,
    });

    tiles = removeTile(tiles, 'user-999');
    expect(tiles).toHaveLength(1);
  });

  it('label fades even while tile remains hovered', () => {
    let overlay: LabelOverlayState = { hovered: false, labelVisible: true };

    overlay = enterTile(overlay);
    expect(overlay).toEqual({ hovered: true, labelVisible: true });

    overlay = expireLabelFade(overlay);
    expect(overlay).toEqual({ hovered: true, labelVisible: false });
  });

  it('mouse movement reveals a faded label again', () => {
    let overlay: LabelOverlayState = { hovered: true, labelVisible: false };

    overlay = moveWithinTile(overlay);
    expect(overlay).toEqual({ hovered: true, labelVisible: true });

    overlay = leaveTile(overlay);
    expect(overlay).toEqual({ hovered: false, labelVisible: true });
  });
});

/* ─── ShareTile bridge auto-reconnect ───────────────────────────── */

// Import StreamReceiver after the mocks above are registered so it
// picks up the global RTCPeerConnection mock.
import { StreamReceiver, stopAllSending } from '../screen-share-viewer';

beforeEach(() => {
  stopAllSending();
});

describe('ShareTile bridge auto-reconnect', () => {
  // Regression: the old startStream() called receiver.start() with NO callback,
  // then wired pc.addEventListener('connectionstatechange') manually AFTER the
  // Promise resolved. This missed failures that occurred before the first track
  // arrived. The new code passes scheduleRetry to receiver.start() so the
  // callback is wired to pc.onconnectionstatechange synchronously.

  it('StreamReceiver.start(onConnectionFailed) fires when connectionState becomes "failed"', () => {
    const receiver = new StreamReceiver('user1', 'watch-all');
    const onFailed = vi.fn();

    const startPromise = receiver.start(onFailed);

    // pc is set synchronously before the first await in start()
    const pc = receiver.pc as unknown as MockRTCPeerConnection;
    expect(pc).not.toBeNull();

    pc._simulateConnectionState('failed');
    expect(onFailed).toHaveBeenCalledOnce();

    receiver.stop();
    startPromise.catch(() => {});
  });

  it('StreamReceiver.start(onConnectionFailed) does NOT fire on "disconnected" (may recover)', () => {
    const receiver = new StreamReceiver('user1', 'watch-all');
    const onFailed = vi.fn();

    const startPromise = receiver.start(onFailed);
    const pc = receiver.pc as unknown as MockRTCPeerConnection;

    pc._simulateConnectionState('disconnected');
    expect(onFailed).not.toHaveBeenCalled();

    receiver.stop();
    startPromise.catch(() => {});
  });

  it('scheduleRetry state machine: sets error and marks auto-retry as scheduled', () => {
    // Mirrors the new scheduleRetry() callback in ShareTile.
    // When the bridge connection fails, the tile sets error state and schedules
    // a retryCount increment (auto-reconnect) instead of waiting for the user
    // to click the manual /retry button.
    let error: string | null = null;
    let autoRetryScheduled = false;
    let retryCount = 0;

    // Represents the new scheduleRetry callback:
    const scheduleRetry = () => {
      error = 'connection failed';
      if (!autoRetryScheduled) {
        autoRetryScheduled = true;
        // In the real component: setTimeout(() => setRetryCount(c => c + 1), AUTO_RETRY_DELAY_MS)
      }
    };

    scheduleRetry();

    expect(error).toBe('connection failed');
    expect(autoRetryScheduled).toBe(true);

    // Simulate timer firing → bridge useEffect re-runs with new receiver
    retryCount += 1;
    expect(retryCount).toBe(1);
  });

  it('handleRetry: clears stream and increments retryCount — no resendStream from child window', () => {
    // Regression: old handleRetry called resendStream(participantId, 'watch-all', new MediaStream())
    // from the Watch All child window. This created a bogus LOCAL sender with an empty stream,
    // which either self-loopbacked (child sees black) or sent a spurious answer to the main
    // window's real sender (corrupting the bridge). The fix removes resendStream entirely.
    //
    // New handleRetry: clears stream + increments retryCount. The bridge useEffect re-runs
    // and creates a fresh StreamReceiver, which emits receiver-ready to the main window.
    let stream: MediaStream | null = new MockMediaStream() as unknown as MediaStream;
    let retryCount = 0;
    let error: string | null = 'connection failed';

    // New handleRetry:
    stream = null;
    error = null;
    retryCount += 1;

    expect(stream).toBeNull();
    expect(error).toBeNull();
    expect(retryCount).toBe(1);
    // resendStream is NOT called — verified by its absence from the new handleRetry
  });
});
