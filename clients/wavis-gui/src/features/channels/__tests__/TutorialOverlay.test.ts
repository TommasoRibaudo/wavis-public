import { describe, it, expect } from 'vitest';
import { computeTutorialPlacement } from '../TutorialOverlay';

const VIEWPORT = { width: 960, height: 680 };

describe('computeTutorialPlacement', () => {
  it('returns nulls when there is no anchor', () => {
    expect(computeTutorialPlacement(null, VIEWPORT)).toEqual({
      placeBelow: false,
      dialogLeft: null,
      arrowLeft: null,
    });
  });

  it('places the dialog above a target in the bottom half, arrow pointing down', () => {
    const anchor = { top: 640, bottom: 664, centerX: 60 }; // bottom command bar
    const { placeBelow, dialogLeft, arrowLeft } = computeTutorialPlacement(anchor, VIEWPORT);
    expect(placeBelow).toBe(false);
    expect(dialogLeft).not.toBeNull();
    expect(arrowLeft).not.toBeNull();
  });

  it('places the dialog below a target in the top half, arrow pointing up', () => {
    const anchor = { top: 120, bottom: 200, centerX: 480 }; // channel list near the top
    const { placeBelow } = computeTutorialPlacement(anchor, VIEWPORT);
    expect(placeBelow).toBe(true);
  });

  it('clamps the dialog to the left viewport margin for a target near the left edge', () => {
    const anchor = { top: 640, bottom: 664, centerX: 20 }; // e.g. /create, near x=20
    const { dialogLeft } = computeTutorialPlacement(anchor, VIEWPORT);
    expect(dialogLeft).toBe(16); // VIEWPORT_MARGIN
  });

  it('clamps the dialog to the right viewport margin for a target near the right edge', () => {
    const anchor = { top: 640, bottom: 664, centerX: 940 };
    const { dialogLeft } = computeTutorialPlacement(anchor, VIEWPORT);
    expect(dialogLeft).toBe(VIEWPORT.width - 480 - 16); // width - DIALOG_WIDTH - VIEWPORT_MARGIN
  });

  it('keeps the arrow pointing at the true target center even when the dialog is clamped', () => {
    // centerX=100 clamps the dialog left (would otherwise center at -140)
    // but leaves the arrow's own offset (84px) within its own margin, so
    // this isolates the dialog-clamp behavior from the arrow-clamp behavior
    // covered by the next test.
    const anchor = { top: 640, bottom: 664, centerX: 100 };
    const { dialogLeft, arrowLeft } = computeTutorialPlacement(anchor, VIEWPORT);
    // The arrow's offset from the dialog's clamped left edge should still
    // land on the target's real center-x, not the dialog's own center.
    expect((dialogLeft ?? 0) + (arrowLeft ?? 0)).toBe(anchor.centerX);
  });

  it('clamps the arrow to stay within the dialog bounds when the target center-arrow math would push it out', () => {
    const anchor = { top: 640, bottom: 664, centerX: 20 };
    const { arrowLeft } = computeTutorialPlacement(anchor, VIEWPORT);
    expect(arrowLeft).toBeGreaterThanOrEqual(24); // ARROW_MARGIN
    expect(arrowLeft).toBeLessThanOrEqual(480 - 24); // DIALOG_WIDTH - ARROW_MARGIN
  });
});
