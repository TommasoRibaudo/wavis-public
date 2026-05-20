/**
 * Property-based tests for the Watch All grid layout algorithm.
 *
 * Uses fast-check to verify correctness properties across generated inputs.
 */

import { describe, it, expect } from 'vitest';
import fc from 'fast-check';
import { computeGridLayout, computeWatchAllLayout } from '../watch-all-grid';

const shareCountArb = fc.integer({ min: 1, max: 6 });
const containerWidthArb = fc.integer({ min: 480, max: 3840 });
const containerHeightArb = fc.integer({ min: 320, max: 2160 });

function computeContainedVideoArea(tileWidth: number, tileHeight: number): number {
  const aspectRatio = 16 / 9;
  const videoWidth = Math.min(tileWidth, tileHeight * aspectRatio);
  const videoHeight = Math.min(tileHeight, tileWidth / aspectRatio);
  return videoWidth * videoHeight;
}

describe('Property 1: Grid layout maximizes visible video area', () => {
  // Feature: watch-all-streams, Property 1: Grid layout maximizes visible video area
  // Validates: Requirements 2.1, 2.3, 11.1

  it('returned layout visible video area >= every other candidate column count', () => {
    fc.assert(
      fc.property(shareCountArb, containerWidthArb, containerHeightArb, (shareCount, width, height) => {
        const result = computeGridLayout(shareCount, width, height);
        const resultArea = computeContainedVideoArea(result.tileWidth, result.tileHeight);

        for (let c = 1; c <= shareCount; c++) {
          const rows = Math.ceil(shareCount / c);
          const tileWidth = Math.floor(width / c);
          const tileHeight = Math.floor(height / rows);
          const candidateArea = computeContainedVideoArea(tileWidth, tileHeight);

          expect(resultArea).toBeGreaterThanOrEqual(candidateArea);
        }
      }),
      { numRuns: 200 },
    );
  });
});

describe('Property 2: Grid layout produces uniform tiles', () => {
  // Feature: watch-all-streams, Property 2: Grid layout produces uniform tiles
  // Validates: Requirements 11.2

  it('tileWidth > 0, tileHeight > 0, columns * rows >= shareCount, at most one partial row', () => {
    fc.assert(
      fc.property(shareCountArb, containerWidthArb, containerHeightArb, (shareCount, width, height) => {
        const result = computeGridLayout(shareCount, width, height);

        expect(result.tileWidth).toBeGreaterThan(0);
        expect(result.tileHeight).toBeGreaterThan(0);
        expect(result.columns * result.rows).toBeGreaterThanOrEqual(shareCount);

        const emptyCells = result.columns * result.rows - shareCount;
        expect(emptyCells).toBeLessThan(result.columns);
      }),
      { numRuns: 200 },
    );
  });
});

/* ═══ computeWatchAllLayout properties ══════════════════════════════ */

const aspectRatioArb = fc.float({ min: 0.5, max: 5.0, noNaN: true });
const streamArb = fc.record({
  id: fc.uuid(),
  aspectRatio: aspectRatioArb,
});
const streamsArb = fc.array(streamArb, { minLength: 1, maxLength: 5 }).map((streams) => {
  // Ensure unique ids
  const seen = new Set<string>();
  return streams.filter((s) => { if (seen.has(s.id)) return false; seen.add(s.id); return true; });
}).filter((s) => s.length >= 1);

describe('Property 3: computeWatchAllLayout — all streams appear exactly once', () => {
  it('every input stream id appears in exactly one tile', () => {
    fc.assert(
      fc.property(streamsArb, containerWidthArb, containerHeightArb, (streams, width, height) => {
        const result = computeWatchAllLayout(streams, width, height);
        const allIds = result.rows.flatMap((r) => r.tiles.map((t) => t.id));

        expect(allIds).toHaveLength(streams.length);
        for (const s of streams) {
          expect(allIds.filter((id) => id === s.id)).toHaveLength(1);
        }
      }),
      { numRuns: 200 },
    );
  });
});

describe('Property 4: computeWatchAllLayout — positive flex values', () => {
  it('all row and tile flexGrow values are > 0', () => {
    fc.assert(
      fc.property(streamsArb, containerWidthArb, containerHeightArb, (streams, width, height) => {
        const result = computeWatchAllLayout(streams, width, height);

        for (const row of result.rows) {
          expect(row.flexGrow).toBeGreaterThan(0);
          for (const tile of row.tiles) {
            expect(tile.flexGrow).toBeGreaterThan(0);
          }
        }
      }),
      { numRuns: 200 },
    );
  });
});

describe('Property 5: computeWatchAllLayout — tile flexGrow matches aspect ratio', () => {
  it('tile flexGrow equals its stream aspect ratio', () => {
    fc.assert(
      fc.property(streamsArb, containerWidthArb, containerHeightArb, (streams, width, height) => {
        const result = computeWatchAllLayout(streams, width, height);

        for (const row of result.rows) {
          for (const tile of row.tiles) {
            const stream = streams.find((s) => s.id === tile.id)!;
            expect(tile.flexGrow).toBeCloseTo(stream.aspectRatio, 10);
          }
        }
      }),
      { numRuns: 200 },
    );
  });
});
