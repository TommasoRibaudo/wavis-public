import { useCallback, useEffect, useRef, useState } from 'react';

export const SHARE_TRANSITION_THRESHOLD_MS = 1200;
export const SHARE_STREAM_STABILIZATION_MS = 2000;
const SHARE_TRANSITION_POLL_MS = 250;

interface ShareTransitionVisibilityInput {
  hasSurface: boolean;
  hasRenderedFrame: boolean;
  firstFrameAt?: number | null;
  lastFrameAt: number | null;
  hasError: boolean;
  now: number;
  thresholdMs?: number;
  stabilizationMs?: number;
}

export function shouldShowShareTransitionOverlay({
  hasSurface,
  hasRenderedFrame,
  firstFrameAt,
  lastFrameAt,
  hasError,
  now,
  thresholdMs = SHARE_TRANSITION_THRESHOLD_MS,
  stabilizationMs,
}: ShareTransitionVisibilityInput): boolean {
  if (hasError || !hasSurface || !hasRenderedFrame || lastFrameAt === null) {
    return false;
  }

  if (
    firstFrameAt != null &&
    stabilizationMs != null &&
    stabilizationMs > 0 &&
    now - firstFrameAt < stabilizationMs
  ) {
    return false;
  }

  return now - lastFrameAt >= thresholdMs;
}

interface UseShareTransitionOverlayOptions {
  hasSurface: boolean;
  hasError: boolean;
  thresholdMs?: number;
}

export function useShareTransitionOverlay({
  hasSurface,
  hasError,
  thresholdMs = SHARE_TRANSITION_THRESHOLD_MS,
}: UseShareTransitionOverlayOptions) {
  const [isSwitching, setIsSwitching] = useState(false);
  const hasRenderedFrameRef = useRef(false);
  const firstFrameAtRef = useRef<number | null>(null);
  const lastFrameAtRef = useRef<number | null>(null);

  const markFrameRendered = useCallback(() => {
    hasRenderedFrameRef.current = true;
    if (firstFrameAtRef.current === null) {
      firstFrameAtRef.current = Date.now();
    }
    lastFrameAtRef.current = Date.now();
    setIsSwitching(false);
  }, []);

  const reset = useCallback(() => {
    hasRenderedFrameRef.current = false;
    firstFrameAtRef.current = null;
    lastFrameAtRef.current = null;
    setIsSwitching(false);
  }, []);

  useEffect(() => {
    if (!hasSurface || hasError) {
      reset();
    }
  }, [hasError, hasSurface, reset]);

  useEffect(() => {
    if (!hasSurface || hasError) return;

    const interval = setInterval(() => {
      const now = Date.now();
      const next = shouldShowShareTransitionOverlay({
        hasSurface,
        hasRenderedFrame: hasRenderedFrameRef.current,
        firstFrameAt: firstFrameAtRef.current,
        lastFrameAt: lastFrameAtRef.current,
        hasError,
        now,
        thresholdMs,
        stabilizationMs: SHARE_STREAM_STABILIZATION_MS,
      });
      setIsSwitching((current) => {
        if (current !== next) {
          if (next) {
            const lastFrame = lastFrameAtRef.current;
            console.log(
              '[wavis:share-transition] overlay activated — stalledFor:',
              lastFrame != null ? now - lastFrame : 'n/a',
              'ms, threshold:', thresholdMs, 'ts:', now,
            );
          }
        }
        return current === next ? current : next;
      });
    }, SHARE_TRANSITION_POLL_MS);

    return () => clearInterval(interval);
  }, [hasError, hasSurface, thresholdMs]);

  return { isSwitching, markFrameRendered, reset };
}
