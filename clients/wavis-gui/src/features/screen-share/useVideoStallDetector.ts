import { useEffect } from 'react';
import type { RefObject } from 'react';
import { isVideoTrackAlive } from './screen-share-viewer';

const HAVE_CURRENT_DATA = 2;

/**
 * How often to ping markFrameRendered when rvfc stops for static content.
 * Must be small enough that lastFrameAt never goes stale past
 * SHARE_TRANSITION_THRESHOLD_MS (1200ms) due to timer jitter.
 * 400ms gives 800ms of margin.
 */
export const STATIC_CONTENT_HEALTH_PING_MS = 400;

interface UseVideoStallDetectorOptions {
  videoRef: RefObject<HTMLVideoElement | null>;
  stream: MediaStream | null;
  onFrameDetected?: () => void;
  onDeadTrack?: () => void;
  onReattach?: () => void;
  stallThresholdMs?: number;
  checkIntervalMs?: number;
}

/**
 * Some platforms legitimately stop presenting fresh decoded frames for static
 * screen-share content, even though the last frame remains visible and the
 * underlying track is still healthy. Treat that as healthy playback so the
 * viewer does not show a false interruption/reconnect cycle every few seconds.
 */
export function isPlaybackHealthyWithoutFreshFrames(
  video: HTMLVideoElement,
  stream: MediaStream,
): boolean {
  return (
    isVideoTrackAlive(stream) &&
    !video.paused &&
    !video.ended &&
    video.readyState >= HAVE_CURRENT_DATA
  );
}

export function useVideoStallDetector({
  videoRef,
  stream,
  onFrameDetected,
  onDeadTrack,
  onReattach,
  stallThresholdMs = 3000,
  checkIntervalMs = 1000,
}: UseVideoStallDetectorOptions): void {
  useEffect(() => {
    const video = videoRef.current;
    if (!video || !stream) return;

    let lastFrameTime = Date.now();
    let disposed = false;
    const hasRvfc = 'requestVideoFrameCallback' in HTMLVideoElement.prototype;
    let timeupdateHandler: (() => void) | null = null;

    if (hasRvfc) {
      const scheduleRvfc = () => {
        if (disposed) return;
        // eslint-disable-next-line @typescript-eslint/no-explicit-any
        (video as any).requestVideoFrameCallback(() => {
          lastFrameTime = Date.now();
          onFrameDetected?.();
          scheduleRvfc();
        });
      };
      scheduleRvfc();
    } else {
      timeupdateHandler = () => {
        lastFrameTime = Date.now();
        onFrameDetected?.();
      };
      video.addEventListener('timeupdate', timeupdateHandler);
    }

    // Health ping: keeps onFrameDetected alive when rvfc stops for static content.
    // Runs at STATIC_CONTENT_HEALTH_PING_MS (400ms) to stay well below the
    // 1200ms overlay threshold even with timer jitter.
    const healthPing = setInterval(() => {
      if (disposed) return;
      const elapsed = Date.now() - lastFrameTime;
      if (
        elapsed >= STATIC_CONTENT_HEALTH_PING_MS &&
        isPlaybackHealthyWithoutFreshFrames(video, stream)
      ) {
        lastFrameTime = Date.now();
        onFrameDetected?.();
      }
    }, STATIC_CONTENT_HEALTH_PING_MS);

    // Stall detection: fires when no frame (real or healthy-ping) has arrived
    // for stallThresholdMs. Separate from the health ping so detection timing
    // is not coupled to the ping frequency.
    const interval = setInterval(() => {
      if (disposed) return;
      const elapsed = Date.now() - lastFrameTime;
      if (elapsed <= stallThresholdMs) return;

      if (!isVideoTrackAlive(stream)) {
        if (onDeadTrack) {
          disposed = true;
          onDeadTrack();
        } else {
          console.warn(
            '[wavis:screen-share] stall: video track dead, waiting for LiveKit reconnect',
          );
          lastFrameTime = Date.now();
        }
        return;
      }

      console.warn('[wavis:screen-share] stall detected, re-attaching stream');
      video.srcObject = null;
      video.srcObject = stream;
      video.play().catch(() => {});
      onReattach?.();
      lastFrameTime = Date.now();
    }, checkIntervalMs);

    return () => {
      disposed = true;
      clearInterval(healthPing);
      clearInterval(interval);
      if (timeupdateHandler) {
        video.removeEventListener('timeupdate', timeupdateHandler);
      }
    };
  }, [
    checkIntervalMs,
    onDeadTrack,
    onFrameDetected,
    onReattach,
    stallThresholdMs,
    stream,
    videoRef,
  ]);
}
