/**
 * Tests for the three Phase-4 share-viewer hooks:
 *   useShareReconnect, useAutoHide, useVideoStallDetector
 *
 * vitest env is 'node' (no jsdom) — tests exercise the pure decision logic
 * that lives inside each hook by replicating its state transitions as plain
 * functions, following the pattern established in WatchAllPage.test.ts.
 */

import { describe, it, expect, vi, beforeEach, afterEach } from 'vitest';
import fc from 'fast-check';
import { isPlaybackHealthyWithoutFreshFrames } from '../useVideoStallDetector';

const HAVE_METADATA = 1;
const HAVE_CURRENT_DATA = 2;

/* ═══ useShareReconnect ════════════════════════════════════════════
 *
 * Core contract:
 *   - triggerReconnect increments retryCount
 *   - triggerReconnect calls onTrigger before incrementing
 *   - multiple triggers accumulate correctly
 * ================================================================= */

/**
 * Mirrors the state machine inside useShareReconnect.
 * retryCount starts at 0; each call to trigger increments it and fires onTrigger.
 */
function makeReconnectState(onTrigger?: () => void) {
  let retryCount = 0;
  const trigger = () => {
    onTrigger?.();
    retryCount += 1;
  };
  return { getCount: () => retryCount, trigger };
}

describe('useShareReconnect logic', () => {
  it('retryCount starts at 0', () => {
    const { getCount } = makeReconnectState();
    expect(getCount()).toBe(0);
  });

  it('each trigger increments retryCount by 1', () => {
    const { getCount, trigger } = makeReconnectState();
    trigger();
    expect(getCount()).toBe(1);
    trigger();
    expect(getCount()).toBe(2);
  });

  it('calls onTrigger before incrementing', () => {
    const calls: number[] = [];
    let count = 0;
    const onTrigger = () => calls.push(count); // captures value before increment
    const { getCount, trigger } = makeReconnectState(onTrigger);
    // reassign count reference via closure — track the order
    trigger();
    count = getCount(); // now 1
    expect(calls).toEqual([0]); // onTrigger fired when count was still 0
  });

  it('works without onTrigger', () => {
    const { getCount, trigger } = makeReconnectState();
    expect(() => trigger()).not.toThrow();
    expect(getCount()).toBe(1);
  });

  it('property: n triggers → retryCount === n', () => {
    fc.assert(
      fc.property(fc.integer({ min: 0, max: 20 }), (n) => {
        const { getCount, trigger } = makeReconnectState();
        for (let i = 0; i < n; i++) trigger();
        expect(getCount()).toBe(n);
      }),
    );
  });

  it('property: onTrigger is called exactly once per trigger', () => {
    fc.assert(
      fc.property(fc.integer({ min: 1, max: 10 }), (n) => {
        let callCount = 0;
        const { trigger } = makeReconnectState(() => { callCount += 1; });
        for (let i = 0; i < n; i++) trigger();
        expect(callCount).toBe(n);
      }),
    );
  });
});

/* ═══ isPlaybackHealthyWithoutFreshFrames ══════════════════════════ */

function makePlaybackHealthVideo(
  overrides: Partial<Pick<HTMLVideoElement, 'paused' | 'ended' | 'readyState'>> = {},
): HTMLVideoElement {
  return {
    paused: false,
    ended: false,
    readyState: HAVE_CURRENT_DATA,
    ...overrides,
  } as HTMLVideoElement;
}

describe('isPlaybackHealthyWithoutFreshFrames', () => {
  it('returns true when the track is live and the video element has current data', () => {
    const video = makePlaybackHealthVideo();
    const stream = {
      getVideoTracks: () => [{ readyState: 'live' }] as MediaStreamTrack[],
    } as unknown as MediaStream;

    expect(isPlaybackHealthyWithoutFreshFrames(video, stream)).toBe(true);
  });

  it('returns false when the video element is paused', () => {
    const video = makePlaybackHealthVideo({ paused: true });
    const stream = {
      getVideoTracks: () => [{ readyState: 'live' }] as MediaStreamTrack[],
    } as unknown as MediaStream;

    expect(isPlaybackHealthyWithoutFreshFrames(video, stream)).toBe(false);
  });

  it('returns false when the element lacks current data', () => {
    const video = makePlaybackHealthVideo({ readyState: HAVE_METADATA });
    const stream = {
      getVideoTracks: () => [{ readyState: 'live' }] as MediaStreamTrack[],
    } as unknown as MediaStream;

    expect(isPlaybackHealthyWithoutFreshFrames(video, stream)).toBe(false);
  });

  it('returns false when the video track has ended', () => {
    const video = makePlaybackHealthVideo();
    const stream = {
      getVideoTracks: () => [{ readyState: 'ended' }] as MediaStreamTrack[],
    } as unknown as MediaStream;

    expect(isPlaybackHealthyWithoutFreshFrames(video, stream)).toBe(false);
  });
});

/* ═══ useAutoHide ══════════════════════════════════════════════════
 *
 * Core contract:
 *   - isVisible starts true
 *   - after delayMs of inactivity, isVisible becomes false
 *   - resetTimer shows and restarts the countdown
 *   - cleanup cancels any pending timer
 * ================================================================= */

describe('useAutoHide timer logic', () => {
  beforeEach(() => { vi.useFakeTimers(); });
  afterEach(() => { vi.useRealTimers(); });

  /**
   * Simulates the useAutoHide state machine using fake timers.
   * Returns the same interface the hook returns.
   */
  function makeAutoHideState(delayMs: number) {
    let isVisible = true;
    let timerId: ReturnType<typeof setTimeout> | null = null;

    const resetTimer = () => {
      isVisible = true;
      if (timerId !== null) clearTimeout(timerId);
      timerId = setTimeout(() => { isVisible = false; }, delayMs);
    };

    const cleanup = () => {
      if (timerId !== null) clearTimeout(timerId);
    };

    // Start initial timer (mirrors mount behavior for both listenToMouseMove paths)
    resetTimer();

    return {
      getVisible: () => isVisible,
      resetTimer,
      cleanup,
    };
  }

  it('starts visible', () => {
    const { getVisible } = makeAutoHideState(2000);
    expect(getVisible()).toBe(true);
  });

  it('hides after the delay elapses', () => {
    const { getVisible } = makeAutoHideState(2000);
    vi.advanceTimersByTime(2000);
    expect(getVisible()).toBe(false);
  });

  it('remains visible before the delay elapses', () => {
    const { getVisible } = makeAutoHideState(2000);
    vi.advanceTimersByTime(1999);
    expect(getVisible()).toBe(true);
  });

  it('resetTimer makes visible again and restarts the countdown', () => {
    const { getVisible, resetTimer } = makeAutoHideState(2000);
    vi.advanceTimersByTime(1500);
    resetTimer();
    vi.advanceTimersByTime(1999); // only 1999ms after reset — still visible
    expect(getVisible()).toBe(true);
    vi.advanceTimersByTime(1);   // now 2000ms after reset — hidden
    expect(getVisible()).toBe(false);
  });

  it('resetTimer cancels the previous timer so it does not fire early', () => {
    const { getVisible, resetTimer } = makeAutoHideState(2000);
    vi.advanceTimersByTime(1000);
    resetTimer();
    // Without the cancel, the original timer would fire at t=2000 (1000ms from now).
    // With the cancel it should not fire until 2000ms after the reset.
    vi.advanceTimersByTime(1000); // t=2000 total — original timer would have fired
    expect(getVisible()).toBe(true);
  });

  it('cleanup prevents the timer from firing after unmount', () => {
    const { getVisible, cleanup } = makeAutoHideState(2000);
    cleanup();
    vi.advanceTimersByTime(5000);
    // Timer was cancelled — isVisible still reflects last set value (true on mount)
    expect(getVisible()).toBe(true);
  });

  it('property: hides exactly at delayMs for any positive delay', () => {
    fc.assert(
      fc.property(fc.integer({ min: 1, max: 10_000 }), (delayMs) => {
        vi.useFakeTimers();
        const { getVisible } = makeAutoHideState(delayMs);
        vi.advanceTimersByTime(delayMs - 1);
        const visibleBefore = getVisible();
        vi.advanceTimersByTime(1);
        const visibleAfter = getVisible();
        vi.useRealTimers();
        return visibleBefore === true && visibleAfter === false;
      }),
    );
  });

  it('property: any number of resetTimer calls keeps visible until delayMs after last call', () => {
    fc.assert(
      fc.property(
        fc.integer({ min: 1, max: 5 }),      // n resets
        fc.integer({ min: 100, max: 1000 }), // delayMs
        (n, delayMs) => {
          vi.useFakeTimers();
          const { getVisible, resetTimer } = makeAutoHideState(delayMs);
          for (let i = 0; i < n; i++) {
            vi.advanceTimersByTime(delayMs - 1);
            resetTimer();
          }
          // Just before the final timer fires
          vi.advanceTimersByTime(delayMs - 1);
          const stillVisible = getVisible();
          vi.advanceTimersByTime(1);
          const nowHidden = getVisible();
          vi.useRealTimers();
          return stillVisible === true && nowHidden === false;
        },
      ),
    );
  });
});

/* ═══ useVideoStallDetector ════════════════════════════════════════
 *
 * Core contract:
 *   - No action when elapsed <= stallThresholdMs
 *   - Live-track stall: re-attach srcObject, reset lastFrameTime
 *   - Dead-track + onDeadTrack provided: call it, set disposed=true
 *   - Dead-track + no onDeadTrack: log and reset lastFrameTime (keep checking)
 * ================================================================= */

/**
 * Extracts the stall-check branch logic from the setInterval callback
 * in useVideoStallDetector. Returns a descriptor of what action was taken.
 */
type StallAction = 'none' | 'reattach' | 'dead-track-reconnect' | 'dead-track-wait';

function checkStall({
  elapsed,
  stallThresholdMs,
  trackAlive,
  onDeadTrack,
  onReattach,
}: {
  elapsed: number;
  stallThresholdMs: number;
  trackAlive: boolean;
  onDeadTrack?: () => void;
  onReattach?: () => void;
}): { action: StallAction; onDeadTrackCalled: boolean } {
  if (elapsed <= stallThresholdMs) {
    return { action: 'none', onDeadTrackCalled: false };
  }
  if (!trackAlive) {
    if (onDeadTrack) {
      onDeadTrack();
      return { action: 'dead-track-reconnect', onDeadTrackCalled: true };
    }
    return { action: 'dead-track-wait', onDeadTrackCalled: false };
  }
  onReattach?.();
  return { action: 'reattach', onDeadTrackCalled: false };
}

describe('useVideoStallDetector stall-check logic', () => {
  it('takes no action when elapsed is below the threshold', () => {
    const { action } = checkStall({ elapsed: 2999, stallThresholdMs: 3000, trackAlive: true });
    expect(action).toBe('none');
  });

  it('takes no action at exactly the threshold boundary', () => {
    const { action } = checkStall({ elapsed: 3000, stallThresholdMs: 3000, trackAlive: true });
    expect(action).toBe('none');
  });

  it('re-attaches srcObject when elapsed exceeds threshold and track is alive', () => {
    const { action } = checkStall({ elapsed: 3001, stallThresholdMs: 3000, trackAlive: true });
    expect(action).toBe('reattach');
  });

  it('calls onDeadTrack and returns dead-track-reconnect when track is dead and onDeadTrack provided', () => {
    const onDeadTrack = vi.fn();
    const { action, onDeadTrackCalled } = checkStall({
      elapsed: 4000,
      stallThresholdMs: 3000,
      trackAlive: false,
      onDeadTrack,
    });
    expect(action).toBe('dead-track-reconnect');
    expect(onDeadTrackCalled).toBe(true);
    expect(onDeadTrack).toHaveBeenCalledOnce();
  });

  it('returns dead-track-wait (log only) when track is dead and no onDeadTrack', () => {
    const { action, onDeadTrackCalled } = checkStall({
      elapsed: 4000,
      stallThresholdMs: 3000,
      trackAlive: false,
    });
    expect(action).toBe('dead-track-wait');
    expect(onDeadTrackCalled).toBe(false);
  });

  it('property: action is always none when elapsed <= threshold', () => {
    fc.assert(
      fc.property(
        fc.integer({ min: 0, max: 3000 }),
        fc.boolean(),
        (elapsed, trackAlive) => {
          const { action } = checkStall({ elapsed, stallThresholdMs: 3000, trackAlive });
          expect(action).toBe('none');
        },
      ),
    );
  });

  it('property: action is never none when elapsed > threshold', () => {
    fc.assert(
      fc.property(
        fc.integer({ min: 3001, max: 60_000 }),
        fc.boolean(),
        (elapsed, trackAlive) => {
          const { action } = checkStall({ elapsed, stallThresholdMs: 3000, trackAlive });
          expect(action).not.toBe('none');
        },
      ),
    );
  });

  it('property: dead track always routes to dead-track-* action when elapsed > threshold', () => {
    fc.assert(
      fc.property(
        fc.integer({ min: 3001, max: 60_000 }),
        fc.boolean(), // whether onDeadTrack is provided
        (elapsed, hasCallback) => {
          const onDeadTrack = hasCallback ? vi.fn() : undefined;
          const { action } = checkStall({ elapsed, stallThresholdMs: 3000, trackAlive: false, onDeadTrack });
          if (hasCallback) {
            expect(action).toBe('dead-track-reconnect');
          } else {
            expect(action).toBe('dead-track-wait');
          }
        },
      ),
    );
  });

  it('property: live track always routes to reattach when elapsed > threshold', () => {
    fc.assert(
      fc.property(
        fc.integer({ min: 3001, max: 60_000 }),
        (elapsed) => {
          const { action } = checkStall({ elapsed, stallThresholdMs: 3000, trackAlive: true });
          expect(action).toBe('reattach');
        },
      ),
    );
  });

  it('onDeadTrack is called exactly once on dead-track-reconnect path', () => {
    const onDeadTrack = vi.fn();
    checkStall({ elapsed: 5000, stallThresholdMs: 3000, trackAlive: false, onDeadTrack });
    expect(onDeadTrack).toHaveBeenCalledTimes(1);
  });

  it('calls onReattach when live-track stall triggers re-attach', () => {
    const onReattach = vi.fn();
    const { action } = checkStall({ elapsed: 3001, stallThresholdMs: 3000, trackAlive: true, onReattach });
    expect(action).toBe('reattach');
    expect(onReattach).toHaveBeenCalledOnce();
  });

  it('does not call onReattach on dead-track path', () => {
    const onReattach = vi.fn();
    const { action } = checkStall({
      elapsed: 3001,
      stallThresholdMs: 3000,
      trackAlive: false,
      onDeadTrack: vi.fn(),
      onReattach,
    });
    expect(action).toBe('dead-track-reconnect');
    expect(onReattach).not.toHaveBeenCalled();
  });

  it('does not call onReattach when elapsed <= threshold (action is none)', () => {
    const onReattach = vi.fn();
    const { action } = checkStall({ elapsed: 3000, stallThresholdMs: 3000, trackAlive: true, onReattach });
    expect(action).toBe('none');
    expect(onReattach).not.toHaveBeenCalled();
  });

  it('onReattach is optional — does not throw when omitted', () => {
    expect(() =>
      checkStall({ elapsed: 3001, stallThresholdMs: 3000, trackAlive: true }),
    ).not.toThrow();
  });

  it('property: onReattach is called exactly once per reattach action (single tick simulation)', () => {
    // checkStall models a single interval tick, so one reattach path → one onReattach call.
    fc.assert(
      fc.property(fc.integer({ min: 3001, max: 60_000 }), (elapsed) => {
        let calls = 0;
        checkStall({ elapsed, stallThresholdMs: 3000, trackAlive: true, onReattach: () => { calls += 1; } });
        expect(calls).toBe(1);
      }),
    );
  });
});
