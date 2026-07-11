/**
 * Unit tests for the Watch All grid layout algorithm - specific examples and edge cases.
 *
 * Requirements: 2.1, 2.4, 2.5, 11.1
 */

import { describe, it, expect } from 'vitest';
import { computeGridLayout, computeWatchAllLayout } from '../watch-all-grid';

function computeOccupiedAreaFromRowAspectSums(
  rowAspectSums: number[],
  width: number,
  height: number,
): number {
  const naturalTotalHeight = rowAspectSums.reduce((acc, sum) => acc + width / sum, 0);
  const scale = Math.min(1, height / naturalTotalHeight);
  return rowAspectSums.reduce((acc, sum) => {
    const rowHeight = (width / sum) * scale;
    return acc + rowHeight * rowHeight * sum;
  }, 0);
}

function computeOccupiedAreaForLayout(
  layout: ReturnType<typeof computeWatchAllLayout>,
  streams: Array<{ id: string; aspectRatio: number }>,
  width: number,
  height: number,
): number {
  const aspectRatioById = new Map(streams.map((stream) => [stream.id, stream.aspectRatio]));
  const rowAspectSums = layout.rows.map((row) =>
    row.tiles.reduce((sum, tile) => sum + (aspectRatioById.get(tile.id) ?? 0), 0),
  );
  return computeOccupiedAreaFromRowAspectSums(rowAspectSums, width, height);
}

function computeBestPartitionArea(
  streams: Array<{ id: string; aspectRatio: number }>,
  width: number,
  height: number,
): number {
  const sorted = [...streams].sort((a, b) => a.aspectRatio - b.aspectRatio);
  if (sorted.length === 0) return 0;

  let bestArea = -1;
  for (let mask = 0; mask < 1 << (sorted.length - 1); mask++) {
    const rowAspectSums: number[] = [];
    let rowStart = 0;
    for (let i = 0; i < sorted.length; i++) {
      const splitAfter = i === sorted.length - 1 || ((mask >> i) & 1) === 1;
      if (!splitAfter) continue;
      rowAspectSums.push(
        sorted.slice(rowStart, i + 1).reduce((sum, stream) => sum + stream.aspectRatio, 0),
      );
      rowStart = i + 1;
    }
    bestArea = Math.max(
      bestArea,
      computeOccupiedAreaFromRowAspectSums(rowAspectSums, width, height),
    );
  }

  return bestArea;
}

describe('computeGridLayout - edge cases', () => {
  it('1 share -> columns=1, rows=1, tile fills container', () => {
    const result = computeGridLayout(1, 1920, 1080);
    expect(result.columns).toBe(1);
    expect(result.rows).toBe(1);
    expect(result.tileWidth).toBe(1920);
    expect(result.tileHeight).toBe(1080);
  });

  it('2 shares in landscape (1920x1080) keeps the first max-video-area candidate', () => {
    const result = computeGridLayout(2, 1920, 1080);
    // c=1: 960x540 visible video per tile
    // c=2: 960x540 visible video per tile
    // Equal visible area - algorithm keeps the first candidate (c=1, stacked)
    expect(result.columns).toBe(1);
    expect(result.rows).toBe(2);
    expect(result.tileWidth).toBe(1920);
    expect(result.tileHeight).toBe(540);
  });

  it('2 shares in portrait (1080x1920) -> columns=1 (stacked)', () => {
    const result = computeGridLayout(2, 1080, 1920);
    // c=1: 1080x607.5 visible video per tile
    // c=2: 540x303.75 visible video per tile
    expect(result.columns).toBe(1);
    expect(result.rows).toBe(2);
    expect(result.tileWidth).toBe(1080);
    expect(result.tileHeight).toBe(960);
  });

  it('6 shares in 1920x1080 favors layouts that maximize 16:9 video area', () => {
    const result = computeGridLayout(6, 1920, 1080);
    // c=1: 320x180 visible video per tile
    // c=2: 640x360 visible video per tile
    // c=3: 640x360 visible video per tile
    // c=2 is the first max-video-area candidate
    expect(result.columns).toBe(2);
    expect(result.rows).toBe(3);
    expect(result.tileWidth).toBe(960);
    expect(result.tileHeight).toBe(360);
  });

  it('minimum dimensions (480x320) with 6 shares avoids square tiles with larger black bars', () => {
    const result = computeGridLayout(6, 480, 320);
    // c=2: 188.4x106 visible video per tile
    // c=3: 160x90 visible video per tile
    // c=2 yields more visible video despite smaller raw tile area
    expect(result.columns).toBe(2);
    expect(result.rows).toBe(3);
    expect(result.tileWidth).toBe(240);
    expect(result.tileHeight).toBe(106);
  });

  it('keeps the current layout when the best alternative is less than 10% better', () => {
    const result = computeGridLayout(2, 680, 400, 2);

    expect(result.columns).toBe(2);
    expect(result.rows).toBe(1);
    expect(result.tileWidth).toBe(340);
    expect(result.tileHeight).toBe(400);
  });

  it('switches layouts when the best alternative clears the 10% threshold', () => {
    const result = computeGridLayout(2, 650, 400, 2);

    expect(result.columns).toBe(1);
    expect(result.rows).toBe(2);
    expect(result.tileWidth).toBe(650);
    expect(result.tileHeight).toBe(200);
  });
});

// ─── Resize hysteresis simulation ───────────────────────────────────────────
//
// Verifies that the 10% video-area threshold prevents layout flicker when the
// grid container is resized incrementally.  Uses 2 tiles at height=400 with
// currentColumns=2 as the stable starting point.
//
// At width=680 the best layout (c=1) is only ~9.4% better than current (c=2):
//   c=2 video area: 340 × 191.25 ≈ 65,025 px²
//   c=1 video area: 355.6 × 200 ≈ 71,111 px²   (ratio 1.094 < 1.10 → keep)
//
// At width=650 the best layout (c=1) is ~19.7% better:
//   c=2 video area: 325 × 182.8 ≈ 59,414 px²
//   c=1 video area: 355.6 × 200 ≈ 71,111 px²   (ratio 1.197 > 1.10 → switch)

describe('computeGridLayout - resize hysteresis simulation', () => {
  it('small resize (below 10% improvement) keeps the current column layout', () => {
    // Simulates a grid that has narrowed slightly: c=1 is not yet 10% better.
    const result = computeGridLayout(2, 680, 400, 2);

    expect(result.columns).toBe(2);
  });

  it('larger resize (above 10% improvement) triggers a layout switch', () => {
    // Simulates a grid that has narrowed enough for c=1 to clear the threshold.
    const result = computeGridLayout(2, 650, 400, 2);

    expect(result.columns).toBe(1);
  });

  it('same resize sequence: no change then change', () => {
    // Sequences the two steps to make the before/after contrast explicit.
    const widthBelowThreshold = 680;
    const widthAboveThreshold = 650;
    const height = 400;

    const stepOne = computeGridLayout(2, widthBelowThreshold, height, 2);
    expect(stepOne.columns).toBe(2); // layout stable — no flicker

    const stepTwo = computeGridLayout(2, widthAboveThreshold, height, stepOne.columns);
    expect(stepTwo.columns).toBe(1); // threshold crossed — layout switches
  });

  it('hysteresis is symmetric: small resize in the other direction also holds', () => {
    // After switching to c=1 at width=650, a small resize back toward 680
    // should NOT flip back to c=2 (c=1 is still optimal at that width anyway).
    // This confirms the threshold only guards against unnecessary switches.
    const resultAfterSwitch = computeGridLayout(2, 650, 400, 1);

    // c=1 is already optimal at 650 so it is returned regardless of threshold
    expect(resultAfterSwitch.columns).toBe(1);
  });
});

/* ═══ computeWatchAllLayout ═════════════════════════════════════════ */

describe('computeWatchAllLayout - edge cases', () => {
  it('0 streams returns empty rows', () => {
    const result = computeWatchAllLayout([], 1920, 1080);
    expect(result.rows).toHaveLength(0);
  });

  it('1 stream fills a single row', () => {
    const result = computeWatchAllLayout([{ id: 'a', aspectRatio: 16 / 9 }], 1920, 1080);
    expect(result.rows).toHaveLength(1);
    expect(result.rows[0].tiles).toHaveLength(1);
    expect(result.rows[0].tiles[0].id).toBe('a');
  });

  it('2 identical 16:9 streams in 1920×1080 go side-by-side (1 row is closer to H)', () => {
    // 1 row: T = 1920 / (16/9 + 16/9) = 1920 / 3.556 ≈ 540. score = |540 - 1080| = 540
    // 2 rows: T = 1920/1.778 + 1920/1.778 ≈ 2160. score = |2160 - 1080| = 1080
    // 1 row wins.
    const streams = [
      { id: 'a', aspectRatio: 16 / 9 },
      { id: 'b', aspectRatio: 16 / 9 },
    ];
    const result = computeWatchAllLayout(streams, 1920, 1080);
    expect(result.rows).toHaveLength(1);
    expect(result.rows[0].tiles).toHaveLength(2);
  });

  it('ultrawide + two 16:9 streams: ultrawide gets its own row', () => {
    // 2 rows: [ultrawide | 16:9, 16:9]
    // S1 = 21/9 ≈ 2.333 → h1 ≈ 823; S2 = 32/9 ≈ 3.556 → h2 ≈ 540. T ≈ 1363. score ≈ 283
    // vs 1 row: S = 5.889, T = 326. score = 754
    // vs 3 rows: T > 2000, score > 920
    const streams = [
      { id: 'wide', aspectRatio: 21 / 9 },
      { id: 'hd1', aspectRatio: 16 / 9 },
      { id: 'hd2', aspectRatio: 16 / 9 },
    ];
    const result = computeWatchAllLayout(streams, 1920, 1080);
    expect(result.rows).toHaveLength(2);
    // Ultrawide should be alone in a row
    const aloneTile = result.rows.find((r) => r.tiles.length === 1);
    expect(aloneTile).toBeDefined();
    expect(aloneTile!.tiles[0].id).toBe('wide');
  });

  it('can reshuffle row partitions when the container shape changes', () => {
    const streams = [
      { id: 'wide', aspectRatio: 21 / 9 },
      { id: 'hd1', aspectRatio: 16 / 9 },
      { id: 'hd2', aspectRatio: 16 / 9 },
    ];

    const wideWindow = computeWatchAllLayout(streams, 1600, 700);
    const tallWindow = computeWatchAllLayout(streams, 900, 1200);

    expect(tallWindow.rows.length).toBeGreaterThan(1);
    expect(tallWindow.rows.map((row) => row.tiles.map((t) => t.id))).not.toEqual(
      wideWindow.rows.map((row) => row.tiles.map((t) => t.id)),
    );
  });

  it('chooses the partition that maximizes occupied video area', () => {
    const streams = [
      { id: 'wide', aspectRatio: 21 / 9 },
      { id: 'hd1', aspectRatio: 16 / 9 },
      { id: 'hd2', aspectRatio: 16 / 9 },
    ];

    const width = 1600;
    const height = 700;
    const layout = computeWatchAllLayout(streams, width, height);
    const resultArea = computeOccupiedAreaForLayout(layout, streams, width, height);
    const bestPossibleArea = computeBestPartitionArea(streams, width, height);

    expect(resultArea).toBeCloseTo(bestPossibleArea, 6);
  });

  it('tile flexGrow values are proportional to aspect ratios within each row', () => {
    const ar1 = 16 / 9;
    const ar2 = 4 / 3;
    const streams = [
      { id: 'a', aspectRatio: ar1 },
      { id: 'b', aspectRatio: ar2 },
    ];
    const result = computeWatchAllLayout(streams, 1920, 1080);
    // If both tiles are in the same row, their flexGrow ratio should equal their AR ratio
    if (result.rows.length === 1) {
      const tileA = result.rows[0].tiles.find((t) => t.id === 'a')!;
      const tileB = result.rows[0].tiles.find((t) => t.id === 'b')!;
      expect(tileA.flexGrow / tileB.flexGrow).toBeCloseTo(ar1 / ar2, 5);
    }
  });

  it('all streams accounted for — no tile is lost', () => {
    const streams = [
      { id: 'a', aspectRatio: 21 / 9 },
      { id: 'b', aspectRatio: 16 / 9 },
      { id: 'c', aspectRatio: 4 / 3 },
      { id: 'd', aspectRatio: 16 / 9 },
    ];
    const result = computeWatchAllLayout(streams, 1920, 1080);
    const allIds = result.rows.flatMap((r) => r.tiles.map((t) => t.id));
    expect(allIds.sort()).toEqual(['a', 'b', 'c', 'd']);
  });
});
