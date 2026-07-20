/**
 * Direct tests for the pure functions in useVideoPopoutWindow.ts
 * (P2-8): buildVideoTileSnapshot and areVideoTileSnapshotsEqual drive the
 * pop-out's add/update/remove diffing (syncVideoPopoutSnapshot) and had no
 * test coverage at all before this file.
 */
import { describe, it, expect } from 'vitest';
import fc from 'fast-check';
import { buildVideoTileSnapshot, areVideoTileSnapshotsEqual } from '../useVideoPopoutWindow';
import type { VideoTileViewModel, VideoTileSnapshot } from '../camera-types';

function makeTile(overrides: Partial<VideoTileViewModel> = {}): VideoTileViewModel {
  return {
    participantId: 'p1',
    displayName: 'Alice',
    color: '#fff',
    track: null,
    isSelf: false,
    isMuted: false,
    hasError: false,
    ...overrides,
  };
}

describe('buildVideoTileSnapshot', () => {
  it('hasTrack is true iff the tile has a live MediaStreamTrack', () => {
    const withTrack = buildVideoTileSnapshot(makeTile({ track: {} as MediaStreamTrack }));
    const withoutTrack = buildVideoTileSnapshot(makeTile({ track: null }));
    expect(withTrack.hasTrack).toBe(true);
    expect(withoutTrack.hasTrack).toBe(false);
  });

  it('carries participant identity/display fields through unchanged', () => {
    const snapshot = buildVideoTileSnapshot(
      makeTile({ participantId: 'abc', displayName: 'Bob', color: '#123', isSelf: true }),
    );
    expect(snapshot.participantId).toBe('abc');
    expect(snapshot.displayName).toBe('Bob');
    expect(snapshot.color).toBe('#123');
    expect(snapshot.isSelf).toBe(true);
  });

  it('propagates isMuted and hasError independently of hasTrack', () => {
    const muted = buildVideoTileSnapshot(
      makeTile({ track: {} as MediaStreamTrack, isMuted: true }),
    );
    const errored = buildVideoTileSnapshot(
      makeTile({ track: {} as MediaStreamTrack, hasError: true }),
    );
    expect(muted.hasTrack).toBe(true);
    expect(muted.isMuted).toBe(true);
    expect(errored.hasTrack).toBe(true);
    expect(errored.hasError).toBe(true);
  });

  it('derives a stable liveKitIdentity from participantId (pure, no IO)', () => {
    const a = buildVideoTileSnapshot(makeTile({ participantId: 'same-id' }));
    const b = buildVideoTileSnapshot(makeTile({ participantId: 'same-id' }));
    expect(a.liveKitIdentity).toBe(b.liveKitIdentity);
  });
});

describe('areVideoTileSnapshotsEqual', () => {
  const base: VideoTileSnapshot = {
    participantId: 'p1',
    liveKitIdentity: 'lk-p1',
    displayName: 'Alice',
    color: '#fff',
    hasTrack: true,
    isSelf: false,
    isMuted: false,
    hasError: false,
  };

  it('identical snapshots are equal', () => {
    expect(areVideoTileSnapshotsEqual(base, { ...base })).toBe(true);
  });

  it.each([
    ['participantId', { participantId: 'p2' }],
    ['liveKitIdentity', { liveKitIdentity: 'lk-p2' }],
    ['displayName', { displayName: 'Bob' }],
    ['color', { color: '#000' }],
    ['hasTrack', { hasTrack: false }],
    ['isSelf', { isSelf: true }],
    ['isMuted', { isMuted: true }],
    ['hasError', { hasError: true }],
  ] as const)(
    'a change in %s makes snapshots unequal (drives an update emit, not a no-op)',
    (_field, diff) => {
      expect(areVideoTileSnapshotsEqual(base, { ...base, ...diff })).toBe(false);
    },
  );

  it('property: reflexive — any snapshot equals a structural copy of itself', () => {
    fc.assert(
      fc.property(
        fc.record({
          participantId: fc.string({ minLength: 1, maxLength: 8 }),
          displayName: fc.string({ minLength: 1, maxLength: 12 }),
          color: fc.string({ minLength: 1, maxLength: 8 }),
          hasTrack: fc.boolean(),
          isSelf: fc.boolean(),
          isMuted: fc.boolean(),
          hasError: fc.boolean(),
        }),
        (fields) => {
          const snapshot: VideoTileSnapshot = fields;
          expect(areVideoTileSnapshotsEqual(snapshot, { ...snapshot })).toBe(true);
        },
      ),
      { numRuns: 30 },
    );
  });
});
