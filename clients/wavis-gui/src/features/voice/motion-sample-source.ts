/**
 * Motion-sample source: OffscreenCanvas-based 4 Hz luma-diff sampler.
 * Feeds MotionSamples to a MotionDetector without DOM access.
 * Design.md §4.5 fallback source (primary source requires Leak_Diagnostics
 * thumbnailing which is not yet implemented — always uses fallback).
 */
import type { MotionDetector } from './motion-detector';

const DEBUG_MOTION = import.meta.env.VITE_DEBUG_MOTION_DETECTOR === 'true';

const SAMPLE_WIDTH = 320;
const SAMPLE_HEIGHT = 180;
const SAMPLE_INTERVAL_MS = 250; // 4 Hz
/** Absolute luma change threshold per pixel (0–255). Design §2.3. */
const LUMA_THRESHOLD = 16;

function computeChangedAreaRatio(current: Uint8ClampedArray, previous: Uint8ClampedArray): number {
  let changed = 0;
  const totalPixels = current.length / 4; // RGBA
  for (let i = 0; i < current.length; i += 4) {
    const yr = 0.299 * current[i] + 0.587 * current[i + 1] + 0.114 * current[i + 2];
    const yp = 0.299 * previous[i] + 0.587 * previous[i + 1] + 0.114 * previous[i + 2];
    if (Math.abs(yr - yp) >= LUMA_THRESHOLD) changed++;
  }
  return changed / totalPixels;
}

/**
 * Starts a 4 Hz OffscreenCanvas sampling loop against the given
 * MediaStreamTrack. Returns a stop function. Does nothing if
 * ImageCapture or OffscreenCanvas are unavailable in this runtime.
 */
export function startOffscreenCanvasSampling(
  track: MediaStreamTrack,
  detector: MotionDetector,
): () => void {
  let imageCapture: ImageCapture;
  try {
    imageCapture = new ImageCapture(track);
  } catch (err) {
    if (DEBUG_MOTION) console.warn('[motion-sample-source] ImageCapture unavailable:', err);
    return () => {};
  }

  const offscreen = new OffscreenCanvas(SAMPLE_WIDTH, SAMPLE_HEIGHT);
  const ctx = offscreen.getContext('2d', {
    willReadFrequently: true,
  }) as OffscreenCanvasRenderingContext2D | null;
  if (!ctx) return () => {};

  let previousData: Uint8ClampedArray | null = null;
  let stopped = false;

  const intervalId = setInterval(async () => {
    if (stopped || track.readyState !== 'live') return;
    try {
      const bitmap = await (
        imageCapture as ImageCapture & { grabFrame(): Promise<ImageBitmap> }
      ).grabFrame();
      ctx.drawImage(bitmap, 0, 0, SAMPLE_WIDTH, SAMPLE_HEIGHT);
      bitmap.close();
      const { data } = ctx.getImageData(0, 0, SAMPLE_WIDTH, SAMPLE_HEIGHT);
      const changedAreaRatio =
        previousData !== null ? computeChangedAreaRatio(data, previousData) : 0;
      previousData = data;
      detector.push({ timestampMs: performance.now(), changedAreaRatio });
      if (DEBUG_MOTION) {
        console.log('[motion-sample-source]', {
          changedAreaRatio: changedAreaRatio.toFixed(3),
          recommendation: detector.currentRecommendation(),
        });
      }
    } catch {
      // grabFrame throws when track ends; share-stop path calls the stop fn.
    }
  }, SAMPLE_INTERVAL_MS);

  return () => {
    stopped = true;
    clearInterval(intervalId);
  };
}

/**
 * Returns true when Leak_Diagnostics thumbnailing is active for the current session.
 * Placeholder: thumbnailing infrastructure is not yet implemented.
 * When implemented, this gate controls primary vs. fallback source selection.
 */
export function isLeakDiagnosticsThumbnailingActive(): boolean {
  return false;
}
