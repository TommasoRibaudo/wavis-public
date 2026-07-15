import { useState, useEffect, useRef, useCallback, memo, useMemo } from 'react';
import type { ViewerRoomConnection } from './viewer-connection';
import { shouldShowShareLoadingOverlay, useShareTransitionOverlay } from './share-transition';
import { useVideoStallDetector } from './useVideoStallDetector';
import { useAutoHide } from '@shared/hooks/useAutoHide';
import { VolumeSlider } from '@shared/VolumeSlider';
import ShareSwitchingOverlay from './ShareSwitchingOverlay';
import ShareLoadingOverlay from './ShareLoadingOverlay';
import {
  DEBUG_SHARE_VIEW,
  LOG,
  STREAM_MUTED_ICON,
  STREAM_UNMUTED_ICON,
} from './watch-all-constants';
import {
  getScreenShareStreamUrl,
  pollScreenShareFrame,
  onScreenShareFrame,
  type PolledScreenShareFrame,
} from './screen-share-frame-bridge';
import { emitScreenShareViewerReady, emitViewerSubscribed } from './share-window-bridge';

const LABEL_FADE_DELAY_MS = 3000;

export interface ShareTileProps {
  participantId: string;
  liveKitIdentity?: string;
  displayName: string;
  color: string;
  kind: 'live' | 'test';
  canvasFallback: boolean;
  muted: boolean;
  volume: number;
  nativeWidth: number | null;
  nativeHeight: number | null;
  aspectRatio: number;
  /** Window-wide direct LiveKit viewer connection (null in diagnostics test mode). */
  viewerConn: ViewerRoomConnection | null;
  onToggleMute: (participantId: string) => void;
  onVolumeChange: (participantId: string, volume: number) => void;
  onPopOut: (participantId: string, volume: number, muted: boolean) => void;
  onAspectRatioDetected: (participantId: string, ratio: number) => void;
}

const ShareTile = memo(function ShareTile({
  participantId,
  liveKitIdentity,
  displayName,
  color,
  kind,
  canvasFallback,
  muted,
  volume,
  nativeWidth,
  nativeHeight,
  aspectRatio,
  viewerConn,
  onToggleMute,
  onVolumeChange,
  onPopOut,
  onAspectRatioDetected,
}: ShareTileProps) {
  const videoRef = useRef<HTMLVideoElement>(null);
  const canvasRef = useRef<HTMLCanvasElement>(null);
  const diagnosticViewportRef = useRef<HTMLDivElement>(null);
  const onAspectRatioDetectedRef = useRef(onAspectRatioDetected);
  onAspectRatioDetectedRef.current = onAspectRatioDetected;
  const isDiagnosticTest = kind === 'test';
  const [stream, setStream] = useState<MediaStream | null>(null);
  const [mjpegUrl, setMjpegUrl] = useState<string | null>(null);
  const [error, setError] = useState<string | null>(null);
  const [hovered, setHovered] = useState(false);
  const [diagnosticViewport, setDiagnosticViewport] = useState({ width: 0, height: 0 });
  const { isVisible: labelVisible, resetTimer: revealLabel } = useAutoHide({
    delayMs: LABEL_FADE_DELAY_MS,
  });
  const { isSwitching, hasRenderedFrame, markFrameRendered } = useShareTransitionOverlay({
    hasSurface: isDiagnosticTest || canvasFallback || Boolean(stream),
    hasError: Boolean(error),
  });
  const diagnosticSurfaceSize = useMemo(() => {
    if (!isDiagnosticTest || diagnosticViewport.width <= 0 || diagnosticViewport.height <= 0) {
      return null;
    }

    let width = diagnosticViewport.width;
    let height = width / aspectRatio;

    if (height > diagnosticViewport.height) {
      height = diagnosticViewport.height;
      width = height * aspectRatio;
    }

    return { width, height };
  }, [aspectRatio, diagnosticViewport, isDiagnosticTest]);

  /* ── Label auto-fade ── */

  const handleMouseEnter = useCallback(() => {
    setHovered(true);
    revealLabel();
  }, [revealLabel]);

  const handleMouseLeave = useCallback(() => {
    setHovered(false);
  }, []);

  const handleMouseMove = useCallback(() => {
    revealLabel();
  }, [revealLabel]);

  useEffect(() => {
    if (!isDiagnosticTest) return;

    const element = diagnosticViewportRef.current;
    if (!element) return;

    const updateViewport = (width: number, height: number) => {
      setDiagnosticViewport((current) => {
        if (Math.abs(current.width - width) < 0.5 && Math.abs(current.height - height) < 0.5) {
          return current;
        }
        return { width, height };
      });
    };

    const measure = () => {
      const rect = element.getBoundingClientRect();
      updateViewport(rect.width, rect.height);
    };

    measure();

    const observer = new ResizeObserver((entries) => {
      const entry = entries[0];
      // Without noUncheckedIndexedAccess, TS types entries[0] as always-defined even
      // though the ResizeObserverEntry array could theoretically be empty.
      // eslint-disable-next-line @typescript-eslint/no-unnecessary-condition
      if (!entry) return;
      updateViewport(entry.contentRect.width, entry.contentRect.height);
    });

    observer.observe(element);
    return () => observer.disconnect();
  }, [isDiagnosticTest]);

  /* ── Stream lifecycle ── */

  const [retryCount, setRetryCount] = useState(0);
  // Consecutive automatic dead-track retries since the last successfully
  // delivered stream — escalates from per-identity refresh to a full Room
  // reconnect so one poisoned publication can't loop the whole Watch All room.
  const consecutiveDeadTrackRetriesRef = useRef(0);

  useEffect(() => {
    if (canvasFallback || isDiagnosticTest || !viewerConn) return;
    let cancelled = false;
    setError(null);

    // Direct LiveKit subscription over the window-wide viewer connection.
    // Reconnection/backoff live inside ViewerRoomConnection — a tile only
    // registers interest in its identity and renders whatever arrives.
    const identity = liveKitIdentity ?? participantId;
    const unwatch = viewerConn.watch(identity, (s) => {
      if (cancelled) return;
      if (DEBUG_SHARE_VIEW)
        console.log(
          LOG,
          `viewer stream ${s ? 'delivered' : 'ended'} — participantId: ${participantId}, retryCount: ${retryCount}`,
        );
      if (!s) {
        setStream(null);
        return;
      }
      consecutiveDeadTrackRetriesRef.current = 0;
      setError(null);
      setStream(s);
      emitScreenShareViewerReady(participantId, 'watch-all');
      emitViewerSubscribed(participantId);
    });

    return () => {
      cancelled = true;
      unwatch();
    };
  }, [canvasFallback, isDiagnosticTest, liveKitIdentity, participantId, retryCount, viewerConn]);

  // Attach stream to video element and detect aspect ratio from metadata
  useEffect(() => {
    const video = videoRef.current;
    if (!video || !stream || isDiagnosticTest) return;
    video.srcObject = stream;

    const handleMetadata = () => {
      if (video.videoWidth > 0 && video.videoHeight > 0) {
        onAspectRatioDetectedRef.current(participantId, video.videoWidth / video.videoHeight);
      }
    };

    video.addEventListener('loadedmetadata', handleMetadata);
    video.play().catch(() => {});

    return () => video.removeEventListener('loadedmetadata', handleMetadata);
  }, [isDiagnosticTest, stream, participantId]);

  /* ── Canvas fallback (Linux) ── */

  useEffect(() => {
    if (!canvasFallback || isDiagnosticTest) return;
    let cancelled = false;
    setMjpegUrl(null);
    // Native frame events and polling commands are keyed by LiveKit identity,
    // which on newer backends differs from the signaling participantId.
    const nativeIdentity = liveKitIdentity ?? participantId;

    const handleFrame = (payload: {
      identity?: string;
      frame: string;
      width?: number;
      height?: number;
    }): Promise<boolean> => {
      if (cancelled) return Promise.resolve(false);
      if (payload.identity && payload.identity !== nativeIdentity) return Promise.resolve(false);

      const canvas = canvasRef.current;
      if (!canvas) return Promise.resolve(false);
      const ctx = canvas.getContext('2d');
      if (!ctx) return Promise.resolve(false);

      return new Promise((resolve) => {
        const img = new Image();
        img.onload = () => {
          if (cancelled) {
            resolve(false);
            return;
          }
          if (canvas.width !== img.width || canvas.height !== img.height) {
            canvas.width = img.width;
            canvas.height = img.height;
            if (img.width > 0 && img.height > 0) {
              onAspectRatioDetectedRef.current(participantId, img.width / img.height);
            }
          }
          ctx.drawImage(img, 0, 0);
          markFrameRendered();
          resolve(true);
        };
        img.onerror = () => resolve(false);
        img.src = `data:image/jpeg;base64,${payload.frame}`;
      });
    };

    let pollFrameId: number | null = null;
    let lastSeq: number | null = null;
    let mjpegActive = false;
    const pollLatestFrame = async () => {
      if (cancelled || mjpegActive) return;
      try {
        const frame: PolledScreenShareFrame | null = await pollScreenShareFrame(
          nativeIdentity,
          lastSeq,
        );
        // TS narrows cancelled/mjpegActive to false from the early-return guard above and
        // doesn't see that the effect cleanup (a separate closure over the same variables)
        // can flip them to true while this invoke() call was in flight. Real re-check, not
        // dead code — do not remove.
        // eslint-disable-next-line @typescript-eslint/no-unnecessary-condition
        if (frame && !cancelled) {
          lastSeq = frame.seq;
          await handleFrame(frame);
        }
      } catch {
        // Non-Linux builds and older builds may not expose the polling command.
      } finally {
        // Same post-await re-check as above.
        // eslint-disable-next-line @typescript-eslint/no-unnecessary-condition
        if (!cancelled && !mjpegActive) {
          pollFrameId = requestAnimationFrame(() => {
            void pollLatestFrame();
          });
        }
      }
    };
    pollFrameId = requestAnimationFrame(() => {
      void pollLatestFrame();
    });

    getScreenShareStreamUrl(nativeIdentity)
      .then((url) => {
        if (cancelled) return;
        mjpegActive = true;
        if (pollFrameId !== null) {
          cancelAnimationFrame(pollFrameId);
          pollFrameId = null;
        }
        setMjpegUrl(`${url}&t=${Date.now()}`);
      })
      .catch(() => {
        // Older/non-Linux builds use the polling fallback above.
      });

    const unlistenFrame = onScreenShareFrame(nativeIdentity, (payload) => {
      void handleFrame(payload);
    });
    emitScreenShareViewerReady(participantId, 'watch-all');

    return () => {
      cancelled = true;
      if (pollFrameId !== null) cancelAnimationFrame(pollFrameId);
      unlistenFrame();
    };
  }, [canvasFallback, isDiagnosticTest, participantId, liveKitIdentity]);

  /* ── Retry handler ── */

  const handleRetry = useCallback(() => {
    setStream(null);
    setError(null);
    // Rebuild the viewer Room (dead-transport escape hatch), then bump
    // retryCount so the watch effect re-registers cleanly.
    viewerConn?.forceReconnect();
    setRetryCount((c) => c + 1);
  }, [viewerConn]);

  // Automatic dead-track recovery, escalating: the first two attempts
  // resubscribe only this identity's publication (refreshIdentity) so a
  // single poisoned track can't tear down the Room other tiles share; a
  // third consecutive failure falls back to a full reconnect.
  const handleDeadTrack = useCallback(() => {
    setStream(null);
    setError(null);
    const identity = liveKitIdentity ?? participantId;
    consecutiveDeadTrackRetriesRef.current += 1;
    if (consecutiveDeadTrackRetriesRef.current >= 3) {
      consecutiveDeadTrackRetriesRef.current = 0;
      viewerConn?.forceReconnect();
    } else {
      viewerConn?.refreshIdentity(identity);
    }
    setRetryCount((c) => c + 1);
  }, [liveKitIdentity, participantId, viewerConn]);

  // Frame-health + dead-track recovery. Canvas-fallback/diagnostic-test tiles
  // don't use a real <video> element, so pass stream: null to no-op the hook
  // for them (mirrors the previous effect's early-return guard).
  useVideoStallDetector({
    videoRef,
    stream: canvasFallback || isDiagnosticTest ? null : stream,
    onFrameDetected: markFrameRendered,
    onDeadTrack: handleDeadTrack,
    onReattach: markFrameRendered,
  });

  /* ── Double-click → pop out ── */

  const handleDoubleClick = useCallback(() => {
    if (isDiagnosticTest) return;
    onPopOut(participantId, volume, muted);
  }, [isDiagnosticTest, muted, onPopOut, participantId, volume]);

  /* ── Render ── */

  return (
    <div
      className="relative overflow-hidden bg-wavis-overlay-base"
      style={{ width: '100%', height: '100%' }}
      onMouseEnter={handleMouseEnter}
      onMouseLeave={handleMouseLeave}
      onMouseMove={handleMouseMove}
      onDoubleClick={handleDoubleClick}
    >
      {error ? (
        /* Error state */
        <div className="h-full flex flex-col items-center justify-center gap-2">
          <span className="text-wavis-danger text-xs">connection failed</span>
          <button
            onClick={handleRetry}
            className="text-xs border border-wavis-accent text-wavis-accent hover:bg-wavis-accent hover:text-wavis-bg transition-colors px-2 py-0.5"
          >
            /retry
          </button>
        </div>
      ) : isDiagnosticTest ? (
        <div
          className="h-full w-full relative overflow-hidden"
          style={{
            backgroundColor: '#050816',
            backgroundImage: [
              `radial-gradient(circle at top left, ${color}22, transparent 42%)`,
              'linear-gradient(180deg, rgba(2, 6, 23, 0.98), rgba(2, 6, 23, 0.88))',
            ].join(', '),
          }}
        >
          <div
            ref={diagnosticViewportRef}
            className="absolute inset-0 flex items-center justify-center p-4"
          >
            <div
              className="relative max-w-full max-h-full overflow-hidden rounded-md border shadow-[0_18px_40px_rgba(0,0,0,0.35)]"
              style={{
                aspectRatio: `${nativeWidth ?? 16}/${nativeHeight ?? 9}`,
                width: diagnosticSurfaceSize ? `${diagnosticSurfaceSize.width}px` : undefined,
                height: diagnosticSurfaceSize ? `${diagnosticSurfaceSize.height}px` : undefined,
                maxWidth: '100%',
                maxHeight: '100%',
                borderColor: `${color}66`,
                backgroundColor: '#0b1120',
                backgroundImage: [
                  `linear-gradient(135deg, ${color}22, rgba(15, 23, 42, 0.96) 55%)`,
                  `repeating-linear-gradient(135deg, transparent 0 22px, ${color}18 22px 24px)`,
                ].join(', '),
              }}
            >
              <div
                className="absolute inset-x-0 top-0 h-8 border-b flex items-center justify-between px-3 text-[0.65rem]"
                style={{ borderColor: `${color}40`, backgroundColor: 'rgba(2, 6, 23, 0.62)' }}
              >
                <span className="uppercase tracking-[0.25em] text-wavis-text-secondary">test</span>
                <span className="font-mono tabular-nums text-wavis-text-secondary">
                  {nativeWidth}x{nativeHeight}
                </span>
              </div>
              <div className="absolute inset-0 flex flex-col items-center justify-center px-4 pt-8 text-center">
                <span className="text-xl font-semibold" style={{ color }}>
                  {displayName}
                </span>
                <span className="mt-2 text-xs text-wavis-text-secondary">
                  fixed height {nativeHeight ?? 1080}
                </span>
                <span className="mt-1 text-xs text-wavis-text-secondary">
                  aspect {aspectRatio.toFixed(2)}:1
                </span>
              </div>
            </div>
          </div>
        </div>
      ) : canvasFallback && mjpegUrl ? (
        <img
          src={mjpegUrl}
          alt=""
          draggable={false}
          onLoad={(event) => {
            const img = event.currentTarget;
            if (img.naturalWidth > 0 && img.naturalHeight > 0) {
              onAspectRatioDetectedRef.current(participantId, img.naturalWidth / img.naturalHeight);
            }
            markFrameRendered();
          }}
          style={{ width: '100%', height: '100%', objectFit: 'contain' }}
        />
      ) : canvasFallback ? (
        <canvas ref={canvasRef} style={{ width: '100%', height: '100%', objectFit: 'contain' }} />
      ) : stream ? (
        <video
          ref={videoRef}
          autoPlay
          playsInline
          muted
          style={{ width: '100%', height: '100%', objectFit: 'contain' }}
        />
      ) : (
        <div className="h-full flex items-center justify-center text-wavis-text-secondary text-xs">
          connecting...
        </div>
      )}
      {isSwitching && <ShareSwitchingOverlay compact displayName={displayName} />}
      {!isDiagnosticTest && shouldShowShareLoadingOverlay(hasRenderedFrame, Boolean(error)) && (
        <ShareLoadingOverlay compact />
      )}

      {/* Pop-out icon (top-right on hover) */}
      {hovered && !isDiagnosticTest && (
        <button
          className="absolute top-1 right-1 text-wavis-text hover:text-wavis-accent text-xs bg-wavis-overlay-base/60 px-1 py-0.5 rounded"
          onClick={(e) => {
            e.stopPropagation();
            onPopOut(participantId, volume, muted);
          }}
          aria-label={`Pop out ${displayName}`}
          title="Pop out window"
        >
          ⧉
        </button>
      )}

      {/* Participant label overlay (bottom) — fades out after 3s, reappears on hover */}
      <div
        className="absolute bottom-0 left-0 right-0 flex items-center justify-between px-2 py-1 text-xs transition-opacity duration-500 bg-wavis-panel/80"
        style={{
          opacity: labelVisible ? 1 : 0,
          pointerEvents: labelVisible ? 'auto' : 'none',
        }}
      >
        <span className="truncate" style={{ color }}>
          {displayName}
        </span>

        {/* Mute toggle (hidden on canvas fallback — no audio available) */}
        {!canvasFallback && !isDiagnosticTest && (
          <div className="flex items-center gap-2 shrink-0 ml-2">
            {hovered && (
              <>
                <span className="text-wavis-text-secondary whitespace-nowrap">stream volume</span>
                <div
                  className="w-20"
                  onPointerDown={(e) => e.stopPropagation()}
                  onClick={(e) => e.stopPropagation()}
                  aria-label={`${displayName} stream volume`}
                >
                  <VolumeSlider
                    value={volume}
                    onChange={(nextVolume) => onVolumeChange(participantId, nextVolume)}
                    color={color}
                  />
                </div>
              </>
            )}
            <button
              className="shrink-0 hover:opacity-70 transition-opacity"
              style={{ color: muted ? 'var(--wavis-text-secondary)' : color }}
              onClick={(e) => {
                e.stopPropagation();
                onToggleMute(participantId);
              }}
              aria-label={muted ? `Unmute ${displayName}` : `Mute ${displayName}`}
              title="Mute stream"
            >
              {muted ? STREAM_MUTED_ICON : STREAM_UNMUTED_ICON}
            </button>
          </div>
        )}
      </div>
    </div>
  );
});

export default ShareTile;
