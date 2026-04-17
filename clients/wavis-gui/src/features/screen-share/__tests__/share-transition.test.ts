import { describe, expect, it } from 'vitest';
import fc from 'fast-check';
import {
  SHARE_TRANSITION_THRESHOLD_MS,
  SHARE_STREAM_STABILIZATION_MS,
  shouldShowShareTransitionOverlay,
} from '../share-transition';

describe('shouldShowShareTransitionOverlay', () => {
  it('returns false when no render surface is active', () => {
    expect(
      shouldShowShareTransitionOverlay({
        hasSurface: false,
        hasRenderedFrame: true,
        lastFrameAt: 1000,
        hasError: false,
        now: 4000,
      }),
    ).toBe(false);
  });

  it('returns false before the first frame has been rendered', () => {
    expect(
      shouldShowShareTransitionOverlay({
        hasSurface: true,
        hasRenderedFrame: false,
        lastFrameAt: null,
        hasError: false,
        now: 4000,
      }),
    ).toBe(false);
  });

  it('returns false while frames are still fresh', () => {
    expect(
      shouldShowShareTransitionOverlay({
        hasSurface: true,
        hasRenderedFrame: true,
        lastFrameAt: 4000,
        hasError: false,
        now: 4000 + SHARE_TRANSITION_THRESHOLD_MS - 1,
      }),
    ).toBe(false);
  });

  it('returns true once the stream has stalled past the threshold', () => {
    expect(
      shouldShowShareTransitionOverlay({
        hasSurface: true,
        hasRenderedFrame: true,
        lastFrameAt: 4000,
        hasError: false,
        now: 4000 + SHARE_TRANSITION_THRESHOLD_MS,
      }),
    ).toBe(true);
  });

  it('returns false when the viewer is already in an explicit error state', () => {
    expect(
      shouldShowShareTransitionOverlay({
        hasSurface: true,
        hasRenderedFrame: true,
        lastFrameAt: 1000,
        hasError: true,
        now: 1000 + SHARE_TRANSITION_THRESHOLD_MS + 5000,
      }),
    ).toBe(false);
  });

  describe('stabilization window suppresses overlay after first frame', () => {
    it('returns false within stabilizationMs of firstFrameAt even if lastFrameAt is stale', () => {
      // firstFrameAt=1000, window expires at 3000; now=2999 is still inside
      expect(
        shouldShowShareTransitionOverlay({
          hasSurface: true,
          hasRenderedFrame: true,
          firstFrameAt: 1000,
          lastFrameAt: 1000,
          hasError: false,
          now: 2999,
          stabilizationMs: SHARE_STREAM_STABILIZATION_MS,
        }),
      ).toBe(false);
    });

    it('returns true once now >= firstFrameAt + stabilizationMs and threshold exceeded', () => {
      // firstFrameAt=1000, window expires at 3000; now=3001, lastFrameAt=1000 → stale by 2001ms > 1200ms
      expect(
        shouldShowShareTransitionOverlay({
          hasSurface: true,
          hasRenderedFrame: true,
          firstFrameAt: 1000,
          lastFrameAt: 1000,
          hasError: false,
          now: 3001,
          stabilizationMs: SHARE_STREAM_STABILIZATION_MS,
        }),
      ).toBe(true);
    });

    it('does not suppress when firstFrameAt is null (backward-compatible with no-stabilization callers)', () => {
      expect(
        shouldShowShareTransitionOverlay({
          hasSurface: true,
          hasRenderedFrame: true,
          firstFrameAt: null,
          lastFrameAt: 1000,
          hasError: false,
          now: 1000 + SHARE_TRANSITION_THRESHOLD_MS,
        }),
      ).toBe(true);
    });

    it('does not suppress when stabilizationMs is 0 (treated as disabled)', () => {
      expect(
        shouldShowShareTransitionOverlay({
          hasSurface: true,
          hasRenderedFrame: true,
          firstFrameAt: 1000,
          lastFrameAt: 1000,
          hasError: false,
          now: 1000 + SHARE_TRANSITION_THRESHOLD_MS,
          stabilizationMs: 0,
        }),
      ).toBe(true);
    });

    it('property: overlay never shows within stabilizationMs of firstFrameAt regardless of lastFrameAt', () => {
      // Uses a fixed epoch (firstFrameAt=5000) so the test is time-independent.
      fc.assert(
        fc.property(
          fc.integer({ min: 0, max: SHARE_STREAM_STABILIZATION_MS - 1 }),
          (offset) => {
            const result = shouldShowShareTransitionOverlay({
              hasSurface: true,
              hasRenderedFrame: true,
              firstFrameAt: 5000,
              lastFrameAt: 0, // maximally stale — would trigger without stabilization
              hasError: false,
              now: 5000 + offset,
              stabilizationMs: SHARE_STREAM_STABILIZATION_MS,
            });
            expect(result).toBe(false);
          },
        ),
      );
    });

    it('property: overlay shows normally after stabilization window when threshold is also exceeded', () => {
      // Uses a fixed epoch. After the stabilization window, the overlay is gated
      // only by the normal stall threshold against lastFrameAt.
      fc.assert(
        fc.property(
          // offset past stabilization window; also exceeds stall threshold vs lastFrameAt=0
          fc.integer({ min: SHARE_STREAM_STABILIZATION_MS, max: SHARE_STREAM_STABILIZATION_MS + 10_000 }),
          (offset) => {
            const result = shouldShowShareTransitionOverlay({
              hasSurface: true,
              hasRenderedFrame: true,
              firstFrameAt: 5000,
              lastFrameAt: 0,
              hasError: false,
              now: 5000 + offset,
              stabilizationMs: SHARE_STREAM_STABILIZATION_MS,
            });
            // stabilization window passed; lastFrameAt=0 is stale by offset > thresholdMs
            expect(result).toBe(true);
          },
        ),
      );
    });
  });

  describe('brief stalls below threshold do not trigger overlay (false-overlay regression)', () => {
    it('property: stalls shorter than thresholdMs never trigger the overlay', () => {
      // Any frame gap < thresholdMs must never show the overlay, regardless of
      // how long ago the stream started. This is the key invariant: a LiveKit
      // track-ended+replaced cycle that completes in < 1200ms must produce no
      // visible overlay even if frames pause briefly during the transition.
      fc.assert(
        fc.property(
          fc.integer({ min: 1, max: SHARE_TRANSITION_THRESHOLD_MS - 1 }),
          fc.integer({ min: 0, max: 10_000 }),
          (stalledFor, age) => {
            const lastFrameAt = 10_000 + age;
            const now = lastFrameAt + stalledFor;
            const result = shouldShowShareTransitionOverlay({
              hasSurface: true,
              hasRenderedFrame: true,
              lastFrameAt,
              hasError: false,
              now,
            });
            expect(result).toBe(false);
          },
        ),
      );
    });

    it('stall of exactly thresholdMs - 1 does not trigger the overlay', () => {
      const lastFrameAt = 5000;
      expect(
        shouldShowShareTransitionOverlay({
          hasSurface: true,
          hasRenderedFrame: true,
          lastFrameAt,
          hasError: false,
          now: lastFrameAt + SHARE_TRANSITION_THRESHOLD_MS - 1,
        }),
      ).toBe(false);
    });

    it('stall of exactly thresholdMs triggers the overlay (boundary)', () => {
      const lastFrameAt = 5000;
      expect(
        shouldShowShareTransitionOverlay({
          hasSurface: true,
          hasRenderedFrame: true,
          lastFrameAt,
          hasError: false,
          now: lastFrameAt + SHARE_TRANSITION_THRESHOLD_MS,
        }),
      ).toBe(true);
    });
  });
});
