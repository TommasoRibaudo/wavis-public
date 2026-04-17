/**
 * Property-based tests for the Watch All grid layout algorithm.
 *
 * Uses fast-check to verify correctness properties across generated inputs.
 */

import { describe, it, expect } from 'vitest';
import fc from 'fast-check';
import { computeGridLayout } from '../watch-all-grid';

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
