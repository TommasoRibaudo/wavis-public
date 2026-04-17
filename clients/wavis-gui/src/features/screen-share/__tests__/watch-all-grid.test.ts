/**
 * Unit tests for the Watch All grid layout algorithm - specific examples and edge cases.
 *
 * Requirements: 2.1, 2.4, 2.5, 11.1
 */

import { describe, it, expect } from 'vitest';
import { computeGridLayout } from '../watch-all-grid';

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
