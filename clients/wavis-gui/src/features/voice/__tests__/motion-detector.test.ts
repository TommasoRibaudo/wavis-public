import { describe, expect, it } from 'vitest';
import { MotionDetector } from '../motion-detector';

/**
 * Push N identical samples starting at t=startMs, spaced 250ms apart (4 Hz).
 * Returns the next available timestamp (startMs + N*250).
 */
function pushSamples(
  det: MotionDetector,
  count: number,
  ratio: number,
  startMs: number,
): number {
  let t = startMs;
  for (let i = 0; i < count; i++) {
    det.push({ timestampMs: t, changedAreaRatio: ratio });
    t += 250;
  }
  return t;
}

/**
 * Timing reference (sampleRateHz=4, sampleWindowMs=1000):
 *
 * Switch-IN (detail→motion), switchInDwellMs=1000:
 *   - dwell timer starts at first sample where rolling avg >= 0.40
 *   - with all-high samples: timer starts at t=0, triggers at t=1000ms (5th sample, index 4)
 *   - 5 samples: t=0,250,500,750,1000ms → dwell = 1000ms >= 1000ms ✓
 *   - 4 samples: t=0,...,750ms → dwell = 750ms < 1000ms ✗
 *
 * Switch-OUT (motion→detail), switchOutDwellMs=5000, starting at t=1250ms after 5 high samples:
 *   - rolling window needs 4 low samples (1s) to clear out high-ratio history
 *   - avg drops below 0.10 at the 4th low sample (t=2000ms): only low samples in window
 *   - dwell timer starts at t=2000ms; triggers at t=7000ms (20 more samples × 250ms = 5000ms)
 *   - total low samples needed: 4 (window clear) + 20 (dwell) = 24 → last at t=7000ms
 *   - 23 low samples: dwell = 6750 - 2000 = 4750ms < 5000ms ✗
 */

describe('MotionDetector', () => {
  it('starts in detail with no switch reason', () => {
    const det = new MotionDetector();
    expect(det.currentRecommendation()).toBe('detail');
    expect(det.lastSwitchReason()).toBeNull();
  });

  it('switches detail→motion after ≥1 s sustained ≥40% motion', () => {
    const det = new MotionDetector();
    // 5th sample is at t=1000ms → dwell from t=0 = 1000ms >= 1000ms
    pushSamples(det, 5, 0.5, 0);
    expect(det.currentRecommendation()).toBe('motion');
    expect(det.lastSwitchReason()).toBe('auto_switch_in');
  });

  it('does NOT switch before the 1 s dwell elapses', () => {
    const det = new MotionDetector();
    // 4 samples: last at t=750ms → dwell = 750ms < 1000ms
    pushSamples(det, 4, 0.5, 0);
    expect(det.currentRecommendation()).toBe('detail');
  });

  it('switches motion→detail after ≥5 s sustained <10% motion (accounting for rolling window)', () => {
    const det = new MotionDetector();
    let t = pushSamples(det, 5, 0.5, 0);
    expect(det.currentRecommendation()).toBe('motion');

    // 44 low samples: well past the 5 s dwell (triggers at t=7000ms)
    pushSamples(det, 44, 0.05, t);
    expect(det.currentRecommendation()).toBe('detail');
    expect(det.lastSwitchReason()).toBe('auto_switch_out');
  });

  it('does NOT switch-out before the 5 s dwell elapses', () => {
    const det = new MotionDetector();
    let t = pushSamples(det, 5, 0.5, 0);
    expect(det.currentRecommendation()).toBe('motion');

    // 23 low samples: dwell = 6750 - 2000 = 4750ms < 5000ms → no switch
    pushSamples(det, 23, 0.05, t);
    expect(det.currentRecommendation()).toBe('motion');
  });

  it('hysteresis: ratio between 0.10 and 0.40 does NOT trigger switch-out from motion', () => {
    const det = new MotionDetector();
    let t = pushSamples(det, 5, 0.5, 0);
    expect(det.currentRecommendation()).toBe('motion');

    // 0.15 is above switchOutAreaThreshold (0.10) → belowThresholdSinceMs never set
    pushSamples(det, 60, 0.15, t);
    expect(det.currentRecommendation()).toBe('motion');
  });

  it('hysteresis: ratio between 0.10 and 0.40 does NOT trigger switch-in from detail', () => {
    const det = new MotionDetector();
    // 0.30 is below switchInAreaThreshold (0.40) → aboveThresholdSinceMs never set
    pushSamples(det, 20, 0.30, 0);
    expect(det.currentRecommendation()).toBe('detail');
  });

  it('resets dwell timer when condition drops below threshold mid-dwell', () => {
    const det = new MotionDetector();
    // 4 high samples → dwell = 750ms at last sample (not enough for 1000ms)
    let t = pushSamples(det, 4, 0.5, 0);
    expect(det.currentRecommendation()).toBe('detail');

    // 4 low samples reset aboveThresholdSinceMs and flush high samples from rolling window
    t = pushSamples(det, 4, 0.05, t);

    // 7 high samples: window refills with high at ~t=2750ms (4th new-high sample)
    // Dwell restarts at t=2750ms; last sample t=3500ms → dwell = 750ms < 1000ms → no switch
    pushSamples(det, 7, 0.5, t);
    expect(det.currentRecommendation()).toBe('detail');
  });

  it('ring buffer wraparound: handles more than capacity samples correctly', () => {
    const det = new MotionDetector();
    // 200 samples far exceeds ring buffer capacity (8) and dwell requirement
    pushSamples(det, 200, 0.5, 0);
    expect(det.currentRecommendation()).toBe('motion');
  });

  it('switches when ratio is exactly at switchInAreaThreshold (>= boundary)', () => {
    const det = new MotionDetector();
    // Exactly 0.40 satisfies >=, should switch after dwell
    pushSamples(det, 5, 0.40, 0);
    expect(det.currentRecommendation()).toBe('motion');
  });

  it('does NOT switch out when ratio is exactly at switchOutAreaThreshold (< required, not <=)', () => {
    const det = new MotionDetector();
    let t = pushSamples(det, 5, 0.5, 0);
    expect(det.currentRecommendation()).toBe('motion');

    // 0.10 is NOT < 0.10 → condition never satisfied → no switch-out
    pushSamples(det, 60, 0.10, t);
    expect(det.currentRecommendation()).toBe('motion');
  });

  it('lastSwitchReason tracks the most recent switch', () => {
    const det = new MotionDetector();
    expect(det.lastSwitchReason()).toBeNull();

    let t = pushSamples(det, 5, 0.5, 0);
    expect(det.lastSwitchReason()).toBe('auto_switch_in');

    // 44 low samples to clear window and expire dwell
    pushSamples(det, 44, 0.05, t);
    expect(det.lastSwitchReason()).toBe('auto_switch_out');
  });

  it('custom config overrides defaults', () => {
    // Custom threshold 0.60 — ratio=0.50 should NOT trigger switch
    const det = new MotionDetector({
      switchInAreaThreshold: 0.60,
      switchInDwellMs: 1_000,
    });
    pushSamples(det, 10, 0.50, 0);
    expect(det.currentRecommendation()).toBe('detail');

    // Custom threshold 0.60, dwell=1000ms: 5th sample at t=1000ms → dwell = 1000ms >= 1000ms → switch
    const det2 = new MotionDetector({
      switchInAreaThreshold: 0.60,
      switchInDwellMs: 1_000,
    });
    pushSamples(det2, 5, 0.70, 0);
    expect(det2.currentRecommendation()).toBe('motion');
  });
});
