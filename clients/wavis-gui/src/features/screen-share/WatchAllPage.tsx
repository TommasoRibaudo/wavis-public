import { useState, useEffect, useRef, useCallback, memo, useMemo } from 'react';
import { Volume2 } from 'lucide-react';
import { invoke } from '@tauri-apps/api/core';
import { getCurrentWindow } from '@tauri-apps/api/window';
import { emit, emitTo, listen } from '@tauri-apps/api/event';
import { ViewerRoomConnection } from './viewer-connection';
import { computeWatchAllLayout } from './watch-all-grid';
import { shouldShowShareLoadingOverlay, useShareTransitionOverlay } from './share-transition';
import { useVideoStallDetector } from './useVideoStallDetector';
import {
  WATCH_ALL_TEST_READY_EVENT,
  WATCH_ALL_TEST_STATE_EVENT,
  type WatchAllParams,
  type WatchAllTestState,
} from './watch-all-test-mode';
import { useAutoHide } from '@shared/hooks/useAutoHide';
import { useFullscreen } from '@shared/hooks/useFullscreen';
import { VolumeSlider } from '@shared/VolumeSlider';
import ParticipantMixer, { type MixerParticipant } from '@shared/ParticipantMixer';
import QuickActionButtons from '@shared/QuickActionButtons';
import FocusMainButton from '@shared/FocusMainButton';
import FullscreenButton from '@shared/FullscreenButton';
import { FixedBugReportButton } from '@features/diagnostics/BugReportButton';
import ShareSwitchingOverlay from './ShareSwitchingOverlay';
import ShareLoadingOverlay from './ShareLoadingOverlay';

/* ─── Constants ─────────────────────────────────────────────────── */

const DEBUG_SHARE_VIEW = import.meta.env.VITE_DEBUG_SCREEN_SHARE_VIEW === 'true';
const LOG = '[wavis:watch-all]';

interface PolledScreenShareFrame {
  identity: string;
  frame: string;
  width: number;
  height: number;
  seq: number;
}

const TITLE_BAR_HEIGHT = 32;
const LABEL_FADE_DELAY_MS = 3000;
const GLOBAL_BAR_FADE_DELAY_MS = 5000;

/* ─── Types ─────────────────────────────────────────────────────── */

interface ShareTileState {
  participantId: string;
  /** LiveKit identity the Rust side keys native share frames by. Newer
   *  backends use the durable userId, which differs from participantId. */
  liveKitIdentity?: string;
  displayName: string;
  color: string;
  kind: 'live' | 'test';
  canvasFallback: boolean;
  muted: boolean;
  volume: number;
  nativeWidth: number | null;
  nativeHeight: number | null;
  /** Detected width/height ratio; defaults to 16/9 until the stream connects. */
  aspectRatio: number;
}

interface ShareUserState {
  isMuted: boolean;
  isDeafened: boolean;
}

interface AudioTileState {
  participantId: string;
  displayName: string;
  color: string;
  muted: boolean;
  volume: number;
}

type MixerPanel = 'voice' | 'share';

const STREAM_MUTED_ICON = '\u25cb';
const STREAM_UNMUTED_ICON = '\u25cf';
const MIXER_ICON = '\u229e';

/* ─── Helpers ───────────────────────────────────────────────────── */

function parseHashParams(): WatchAllParams | null {
  try {
    const hash = window.location.hash.slice(1);
    if (!hash) return null;
    return JSON.parse(decodeURIComponent(hash)) as WatchAllParams;
  } catch {
    return null;
  }
}

/* ─── Sub-components ────────────────────────────────────────────── */

interface ShareTileProps {
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
      setError(null);
      setStream(s);
      void emitTo('main', 'screen-share-viewer:ready', {
        participantId,
        windowLabel: 'watch-all',
      });
      void emitTo('main', 'viewer-subscribed', { targetId: participantId });
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
        const frame = await invoke<PolledScreenShareFrame | null>('media_poll_screen_share_frame', {
          identity: nativeIdentity,
          lastSeq,
        });
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

    invoke<string>('media_get_screen_share_stream_url', { identity: nativeIdentity })
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

    const unlistenLinux = listen<{ identity: string; frame: string }>(
      `ss-frame:${nativeIdentity}`,
      (event) => {
        void handleFrame(event.payload);
      },
    );
    // Also listen for the generic event names (cross-platform compat)
    const unlistenGenericLinux = listen<{ identity: string; frame: string }>(
      'screen_share_frame',
      (event) => {
        void handleFrame(event.payload);
      },
    );
    const unlistenGenericWin = listen<{ frame: string; width: number; height: number }>(
      'screen-share-frame',
      (event) => {
        void handleFrame(event.payload);
      },
    );
    void emitTo('main', 'screen-share-viewer:ready', {
      participantId,
      windowLabel: 'watch-all',
    });

    return () => {
      cancelled = true;
      if (pollFrameId !== null) cancelAnimationFrame(pollFrameId);
      void unlistenLinux.then((fn) => fn());
      void unlistenGenericLinux.then((fn) => fn());
      void unlistenGenericWin.then((fn) => fn());
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

  // Frame-health + dead-track recovery. Canvas-fallback/diagnostic-test tiles
  // don't use a real <video> element, so pass stream: null to no-op the hook
  // for them (mirrors the previous effect's early-return guard).
  useVideoStallDetector({
    videoRef,
    stream: canvasFallback || isDiagnosticTest ? null : stream,
    onFrameDetected: markFrameRendered,
    onDeadTrack: handleRetry,
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

/* ─── AudioOnlyTile ─────────────────────────────────────────────── */

interface AudioOnlyTileProps {
  participantId: string;
  displayName: string;
  color: string;
  muted: boolean;
  volume: number;
  onToggleMute: (participantId: string) => void;
  onVolumeChange: (participantId: string, volume: number) => void;
}

const AudioOnlyTile = memo(function AudioOnlyTile({
  participantId,
  displayName,
  color,
  muted,
  volume,
  onToggleMute,
  onVolumeChange,
}: AudioOnlyTileProps) {
  return (
    <div className="flex items-center gap-3 px-3 py-2 text-xs font-mono select-none">
      <button
        className="shrink-0 hover:opacity-70 transition-opacity"
        style={{
          color: muted ? 'var(--wavis-danger)' : 'transparent',
          WebkitTextStroke: muted ? undefined : '1px var(--wavis-danger)',
          animation: muted ? 'watchPulse 2s ease-in-out infinite' : undefined,
        }}
        onClick={() => onToggleMute(participantId)}
        aria-label={muted ? `Unmute ${displayName} audio` : `Mute ${displayName} audio`}
        title={muted ? 'click to unmute' : 'click to mute'}
      >
        {'♪'}
      </button>
      <span className="truncate min-w-0" style={{ color }}>
        {displayName}
      </span>
      <div className="flex items-center gap-2 ml-auto shrink-0">
        <span className="text-wavis-text-secondary whitespace-nowrap hidden sm:block">
          audio vol
        </span>
        <div
          className="w-20"
          onPointerDown={(e) => e.stopPropagation()}
          onClick={(e) => e.stopPropagation()}
        >
          <VolumeSlider
            value={volume}
            onChange={(v) => onVolumeChange(participantId, v)}
            color={color}
          />
        </div>
        <span className="text-wavis-text-secondary w-5 text-right tabular-nums">
          {muted ? 0 : volume}
        </span>
        <button
          className="shrink-0 hover:opacity-70 transition-opacity"
          style={{ color: muted ? 'var(--wavis-text-secondary)' : color }}
          onClick={() => onToggleMute(participantId)}
          aria-label={muted ? `Unmute ${displayName} audio` : `Mute ${displayName} audio`}
          title="Mute stream"
        >
          {muted ? STREAM_MUTED_ICON : STREAM_UNMUTED_ICON}
        </button>
      </div>
    </div>
  );
});

/* ═══ Component ═════════════════════════════════════════════════════ */

export default function WatchAllPage() {
  const params = useRef(parseHashParams());
  const [tiles, setTiles] = useState<ShareTileState[]>([]);
  const [audioTiles, setAudioTiles] = useState<AudioTileState[]>([]);
  const pendingRestoreVolumesRef = useRef<Map<string, { volume: number; muted: boolean }>>(
    new Map(),
  );
  const gridRef = useRef<HTMLDivElement>(null);
  const [gridSize, setGridSize] = useState({ width: 0, height: 0 });
  const [userState, setUserState] = useState<ShareUserState>({
    isMuted: false,
    isDeafened: false,
  });
  const [mixerOpen, setMixerOpen] = useState(false);
  const [voiceMixerOpen, setVoiceMixerOpen] = useState(false);
  const [mixerPanelOrder, setMixerPanelOrder] = useState<MixerPanel[]>([]);
  const [voiceParticipants, setVoiceParticipants] = useState<MixerParticipant[]>([]);
  const { isVisible: bottomBarVisible } = useAutoHide({
    delayMs: GLOBAL_BAR_FADE_DELAY_MS,
    listenToMouseMove: true,
  });
  const { isFullscreen, toggleFullscreen } = useFullscreen();

  const p = params.current;
  const diagnosticsTestSessionId = p?.testSessionId ?? null;
  const isDiagnosticsTestMode = diagnosticsTestSessionId !== null;

  // One direct LiveKit viewer connection for the whole window — every live
  // tile subscribes over this single Room. Constructor is side-effect free;
  // the Room only connects once the first tile calls watch().
  const viewerConn = useMemo(
    () => (isDiagnosticsTestMode ? null : new ViewerRoomConnection({ windowLabel: 'watch-all' })),
    [isDiagnosticsTestMode],
  );
  useEffect(() => {
    if (!viewerConn) return;
    return () => viewerConn.dispose();
  }, [viewerConn]);

  /* ── Tile event listeners ── */

  useEffect(() => {
    if (isDiagnosticsTestMode) {
      let cleanup: (() => void) | null = null;
      let mounted = true;

      void listen<WatchAllTestState>(WATCH_ALL_TEST_STATE_EVENT, (event) => {
        if (event.payload.sessionId !== diagnosticsTestSessionId) return;
        setTiles(
          event.payload.tiles.map((tile) => ({
            participantId: tile.participantId,
            displayName: tile.displayName,
            color: tile.color,
            kind: 'test',
            canvasFallback: false,
            muted: false,
            volume: 70,
            nativeWidth: tile.width,
            nativeHeight: tile.height,
            aspectRatio: tile.width / tile.height,
          })),
        );
      }).then((unlisten) => {
        if (!mounted) {
          unlisten();
          return;
        }
        cleanup = unlisten;
        void emit(WATCH_ALL_TEST_READY_EVENT, { sessionId: diagnosticsTestSessionId });
      });

      return () => {
        mounted = false;
        cleanup?.();
      };
    }

    // Register ALL listeners first, then signal readiness to ActiveRoom.
    // ActiveRoom waits for watch-all:ready before emitting share-added events,
    // avoiding the race where events fire before listeners are registered.
    const setup = async () => {
      const unlistenAdded = await listen<{
        participantId: string;
        liveKitIdentity?: string;
        displayName: string;
        color: string;
        canvasFallback: boolean;
      }>('watch-all:share-added', (event) => {
        console.log(
          '[wavis:watch-all] share-added received:',
          event.payload.participantId,
          event.payload.displayName,
        );
        const { participantId, liveKitIdentity, displayName, color, canvasFallback } =
          event.payload;
        setTiles((prev) => {
          if (prev.some((t) => t.participantId === participantId)) return prev;
          const restored = pendingRestoreVolumesRef.current.get(participantId);
          if (restored !== undefined) {
            pendingRestoreVolumesRef.current.delete(participantId);
          }
          const baseTile = {
            participantId,
            liveKitIdentity,
            displayName,
            color,
            kind: 'live' as const,
            canvasFallback,
            muted: false,
            volume: 70,
            nativeWidth: null,
            nativeHeight: null,
            aspectRatio: 16 / 9,
          };
          return [
            ...prev,
            restored === undefined
              ? baseTile
              : { ...baseTile, muted: restored.muted, volume: restored.volume },
          ];
        });
      });

      const unlistenRemoved = await listen<{ participantId: string }>(
        'watch-all:share-removed',
        (event) => {
          setTiles((prev) => prev.filter((t) => t.participantId !== event.payload.participantId));
        },
      );

      const unlistenUpdated = await listen<{
        participantId: string;
        displayName: string;
        color: string;
      }>('watch-all:share-updated', (event) => {
        const { participantId, displayName, color } = event.payload;
        setTiles((prev) =>
          prev.map((t) => (t.participantId === participantId ? { ...t, displayName, color } : t)),
        );
      });

      const unlistenRestoreVolume = await listen<{
        participantId: string;
        volume: number;
        muted: boolean;
      }>('watch-all:restore-volume', (event) => {
        const { participantId, volume, muted } = event.payload;
        setTiles((prev) => {
          let found = false;
          const next = prev.map((tile) => {
            if (tile.participantId !== participantId) return tile;
            found = true;
            return { ...tile, volume, muted };
          });
          // ESLint's narrowing doesn't see `found` mutated inside the prev.map()
          // callback above — if no tile matches, found genuinely stays false and
          // this branch is how a not-yet-created tile's volume gets queued.
          // eslint-disable-next-line @typescript-eslint/no-unnecessary-condition
          if (!found) {
            pendingRestoreVolumesRef.current.set(participantId, { volume, muted });
          }
          return next;
        });
        setAudioTiles((prev) =>
          prev.map((tile) =>
            tile.participantId === participantId ? { ...tile, volume, muted } : tile,
          ),
        );
      });

      const unlistenAudioAdded = await listen<{
        participantId: string;
        displayName: string;
        color: string;
        volume: number;
        muted: boolean;
      }>('watch-all:audio-share-added', (event) => {
        const { participantId, displayName, color, volume, muted } = event.payload;
        setAudioTiles((prev) => {
          if (prev.some((t) => t.participantId === participantId)) return prev;
          return [...prev, { participantId, displayName, color, muted, volume }];
        });
      });

      const unlistenAudioRemoved = await listen<{ participantId: string }>(
        'watch-all:audio-share-removed',
        (event) => {
          setAudioTiles((prev) =>
            prev.filter((t) => t.participantId !== event.payload.participantId),
          );
        },
      );

      // All listeners registered — signal readiness to ActiveRoom
      console.log('[wavis:watch-all] emitting watch-all:ready');
      void emit('watch-all:ready', {});

      return [
        unlistenAdded,
        unlistenRemoved,
        unlistenUpdated,
        unlistenRestoreVolume,
        unlistenAudioAdded,
        unlistenAudioRemoved,
      ];
    };

    let cleanups: Array<() => void> = [];
    void setup().then((fns) => {
      cleanups = fns;
    });

    return () => {
      for (const fn of cleanups) fn();
    };
  }, [diagnosticsTestSessionId, isDiagnosticsTestMode]);

  useEffect(() => {
    if (isDiagnosticsTestMode) return;
    const unlisten = listen<ShareUserState>('share:user-state', (event) => {
      setUserState({
        isMuted: Boolean(event.payload.isMuted),
        isDeafened: Boolean(event.payload.isDeafened),
      });
    });
    return () => {
      void unlisten.then((fn) => fn());
    };
  }, [isDiagnosticsTestMode]);

  useEffect(() => {
    if (isDiagnosticsTestMode) return;
    const unlisten = listen<{ participants: MixerParticipant[] }>(
      'watch-all:voice-participants',
      (event) => {
        setVoiceParticipants(event.payload.participants);
      },
    );
    return () => {
      void unlisten.then((fn) => fn());
    };
  }, [isDiagnosticsTestMode]);

  /* ── Window close / self-close listeners ── */

  // Listen for close command from ActiveRoom (room leave)
  useEffect(() => {
    if (isDiagnosticsTestMode) return;
    const unlisten = listen('watch-all:close', () => {
      void getCurrentWindow().close();
    });
    return () => {
      void unlisten.then((fn) => fn());
    };
  }, [isDiagnosticsTestMode]);

  // Defense-in-depth: self-close when voice session ends
  useEffect(() => {
    if (isDiagnosticsTestMode) return;
    const unlisten = listen('voice-session:ended', () => {
      void getCurrentWindow().close();
    });
    return () => {
      void unlisten.then((fn) => fn());
    };
  }, [isDiagnosticsTestMode]);

  // Notify ActiveRoom when this window closes
  useEffect(() => {
    if (isDiagnosticsTestMode) return;
    const win = getCurrentWindow();
    const unlisten = win.onCloseRequested(async () => {
      await emit('watch-all:closed', {});
    });
    return () => {
      void unlisten.then((fn) => fn());
    };
  }, [isDiagnosticsTestMode]);

  /* ── Grid resize tracking ── */

  useEffect(() => {
    const el = gridRef.current;
    if (!el) return;

    const observer = new ResizeObserver((entries) => {
      const entry = entries[0];
      // Without noUncheckedIndexedAccess, TS types entries[0] as always-defined even
      // though the ResizeObserverEntry array could theoretically be empty.
      // eslint-disable-next-line @typescript-eslint/no-unnecessary-condition
      if (!entry) return;

      setGridSize((current) => {
        const next = {
          width: entry.contentRect.width,
          height: entry.contentRect.height,
        };

        if (current.width === next.width && current.height === next.height) {
          return current;
        }

        return next;
      });
    });
    observer.observe(el);
    return () => {
      observer.disconnect();
    };
  }, []);

  /* ── Mute toggle ── */

  const handleToggleMute = useCallback((participantId: string) => {
    // Try video tiles first, then audio-only tiles
    let handled = false;
    setTiles((prev) => {
      const next = prev.map((t) => {
        if (t.participantId !== participantId) return t;
        handled = true;
        const nextMuted = !t.muted;
        void emit('watch-all:mute-change', { participantId, muted: nextMuted });
        return { ...t, muted: nextMuted };
      });
      return handled ? next : prev;
    });
    // Same ESLint narrowing gap as above: `handled` is mutated inside the
    // setTiles updater's prev.map() callback.
    // eslint-disable-next-line @typescript-eslint/no-unnecessary-condition
    if (!handled) {
      setAudioTiles((prev) =>
        prev.map((t) => {
          if (t.participantId !== participantId) return t;
          const nextMuted = !t.muted;
          void emit('watch-all:mute-change', { participantId, muted: nextMuted });
          return { ...t, muted: nextMuted };
        }),
      );
    }
  }, []);

  const handleVolumeChange = useCallback((participantId: string, volume: number) => {
    setTiles((prev) =>
      prev.map((t) =>
        t.participantId === participantId ? { ...t, volume, muted: volume === 0 } : t,
      ),
    );
    setAudioTiles((prev) =>
      prev.map((t) =>
        t.participantId === participantId ? { ...t, volume, muted: volume === 0 } : t,
      ),
    );
    void emit('watch-all:volume-change', { participantId, volume });
  }, []);

  const handleAspectRatioDetected = useCallback((participantId: string, ratio: number) => {
    setTiles((prev) =>
      prev.map((t) =>
        t.participantId === participantId && Math.abs(t.aspectRatio - ratio) > 0.01
          ? { ...t, aspectRatio: ratio }
          : t,
      ),
    );
  }, []);

  const handleVoiceVolumeChange = useCallback((participantId: string, volume: number) => {
    setVoiceParticipants((prev) =>
      prev.map((participant) =>
        participant.id === participantId
          ? { ...participant, volume, muted: volume === 0 }
          : participant,
      ),
    );
    void emit('share:voice-volume-change', { participantId, volume });
  }, []);

  const handleVoiceMuteToggle = useCallback((participantId: string) => {
    setVoiceParticipants((prev) =>
      prev.map((participant) => {
        if (participant.id !== participantId) return participant;
        const nextVolume = participant.muted
          ? participant.volume > 0
            ? participant.volume
            : 50
          : 0;
        void emit('share:voice-volume-change', { participantId, volume: nextVolume });
        return { ...participant, volume: nextVolume, muted: nextVolume === 0 };
      }),
    );
  }, []);

  const openMixerPanel = useCallback((panel: MixerPanel) => {
    setMixerPanelOrder((prev) => [...prev.filter((item) => item !== panel), panel]);
  }, []);

  const closeMixerPanel = useCallback((panel: MixerPanel) => {
    setMixerPanelOrder((prev) => prev.filter((item) => item !== panel));
    if (panel === 'voice') {
      setVoiceMixerOpen(false);
    } else {
      setMixerOpen(false);
    }
  }, []);

  const toggleMixerPanel = useCallback(
    (panel: MixerPanel) => {
      if (panel === 'voice') {
        setVoiceMixerOpen((open) => {
          if (open) {
            setMixerPanelOrder((prev) => prev.filter((item) => item !== panel));
            return false;
          }
          openMixerPanel(panel);
          return true;
        });
        return;
      }

      setMixerOpen((open) => {
        if (open) {
          setMixerPanelOrder((prev) => prev.filter((item) => item !== panel));
          return false;
        }
        openMixerPanel(panel);
        return true;
      });
    },
    [openMixerPanel],
  );

  /* ── Pop-out ── */

  const handlePopOut = useCallback((participantId: string, volume: number, muted: boolean) => {
    void emit('watch-all:pop-out', { participantId, volume, muted });
  }, []);

  /* ── Close button ── */

  const handleClose = useCallback(() => {
    void getCurrentWindow().close();
  }, []);

  /* ── Compute layout ── */

  // Layout is computed in two phases to keep the partition stable during resize.
  //
  // Phase 1 — partition: which streams share a row. Computed from a fixed 16:9
  //   reference so the row grouping never changes when the panel is resized.
  //   Stable partition = stable row keys = no ShareTile remounts = no WebRTC drops.
  //
  // Phase 2 — flex values: proportional row heights derived from the actual
  //   container size. These are just CSS numbers; changing them never remounts tiles.
  const layout = useMemo(() => {
    if (tiles.length === 0 || gridSize.width <= 0 || gridSize.height <= 0) return null;
    const streams = tiles.map((t) => ({ id: t.participantId, aspectRatio: t.aspectRatio }));
    if (isDiagnosticsTestMode) {
      return computeWatchAllLayout(streams, gridSize.width, gridSize.height);
    }
    const { rows: baseRows } = computeWatchAllLayout(streams, 1920, 1080);
    const arById = new Map(streams.map((s) => [s.id, s.aspectRatio]));
    return {
      rows: baseRows.map((row) => {
        const S = row.tiles.reduce((sum, t) => sum + (arById.get(t.id) ?? 16 / 9), 0);
        return { ...row, flexGrow: gridSize.width / S };
      }),
    };
  }, [gridSize, isDiagnosticsTestMode, tiles]);
  const tilePositions = useMemo(() => {
    if (!layout)
      return new Map<string, { left: string; top: string; width: string; height: string }>();

    const positions = new Map<
      string,
      { left: string; top: string; width: string; height: string }
    >();
    const totalRowFlex = layout.rows.reduce((sum, row) => sum + row.flexGrow, 0);
    let top = 0;

    for (const row of layout.rows) {
      const rowHeight = totalRowFlex > 0 ? (row.flexGrow / totalRowFlex) * 100 : 0;
      const totalTileFlex = row.tiles.reduce((sum, tile) => sum + tile.flexGrow, 0);
      let left = 0;

      for (const tile of row.tiles) {
        const tileWidth = totalTileFlex > 0 ? (tile.flexGrow / totalTileFlex) * 100 : 0;
        positions.set(tile.id, {
          left: `${left}%`,
          top: `${top}%`,
          width: `${tileWidth}%`,
          height: `${rowHeight}%`,
        });
        left += tileWidth;
      }

      top += rowHeight;
    }

    return positions;
  }, [layout]);
  const bottomBarActive = bottomBarVisible || mixerOpen || voiceMixerOpen;
  const activeMixerPanelOrder = mixerPanelOrder.filter((panel) =>
    panel === 'voice' ? voiceMixerOpen : mixerOpen,
  );
  const mixerPositionClass = (panel: MixerPanel) =>
    activeMixerPanelOrder.indexOf(panel) <= 0
      ? 'absolute bottom-full right-0 mb-1'
      : 'absolute bottom-full right-[228px] mb-1';

  /* ── Render ── */

  if (!p) {
    return (
      <div className="h-screen flex items-center justify-center bg-wavis-bg font-mono text-wavis-danger">
        missing watch-all parameters
      </div>
    );
  }

  return (
    <div className="h-screen flex flex-col bg-wavis-overlay-base font-mono text-wavis-text overflow-hidden select-none">
      {/* Title bar; overlays grid and auto-hides in fullscreen */}
      <div
        data-tauri-drag-region
        className="flex items-center justify-between px-2 border-b border-wavis-text-secondary bg-wavis-panel text-xs shrink-0 transition-opacity duration-300"
        style={
          isFullscreen
            ? {
                position: 'absolute',
                top: 0,
                left: 0,
                right: 0,
                zIndex: 20,
                height: TITLE_BAR_HEIGHT,
                opacity: bottomBarVisible ? 1 : 0,
                pointerEvents: bottomBarVisible ? 'auto' : 'none',
              }
            : { height: TITLE_BAR_HEIGHT }
        }
      >
        <div className="flex items-center gap-2 min-w-0">
          <span style={{ color: 'var(--wavis-purple)' }}>▲</span>
          <span className="truncate text-wavis-text">Watch All — {p.channelName}</span>
        </div>
        <div data-no-drag className="flex items-center shrink-0">
          <FixedBugReportButton captureScreenshot={false} />
          <FullscreenButton isFullscreen={isFullscreen} onToggle={toggleFullscreen} />
          <button
            onClick={handleClose}
            className="inline-flex items-center justify-center w-8 h-8 hover:bg-wavis-danger hover:text-wavis-text-contrast text-wavis-danger shrink-0 transition-colors"
            aria-label="Close watch all window"
          >
            [x]
          </button>
        </div>
      </div>

      {/* Grid container */}
      <div ref={gridRef} className="flex-1 overflow-hidden relative">
        {tiles.length === 0 && audioTiles.length === 0 ? (
          /* Empty state */
          <div className="h-full flex items-center justify-center text-wavis-text-secondary text-sm">
            no active shares
          </div>
        ) : layout ? (
          /* Keep ShareTile nodes flat and keyed only by participantId so
             layout churn never remounts live WebRTC receivers. */
          <div className="relative w-full h-full">
            {tiles.map((tile) => {
              const position = tilePositions.get(tile.participantId);
              if (!position) return null;

              return (
                <div
                  key={tile.participantId}
                  className="absolute min-w-0 overflow-hidden"
                  style={position}
                >
                  <ShareTile
                    participantId={tile.participantId}
                    liveKitIdentity={tile.liveKitIdentity}
                    displayName={tile.displayName}
                    color={tile.color}
                    kind={tile.kind}
                    canvasFallback={tile.canvasFallback}
                    muted={tile.muted}
                    volume={tile.volume}
                    nativeWidth={tile.nativeWidth}
                    nativeHeight={tile.nativeHeight}
                    aspectRatio={tile.aspectRatio}
                    viewerConn={viewerConn}
                    onToggleMute={handleToggleMute}
                    onVolumeChange={handleVolumeChange}
                    onPopOut={handlePopOut}
                    onAspectRatioDetected={handleAspectRatioDetected}
                  />
                </div>
              );
            })}
          </div>
        ) : null}
      </div>

      {/* Audio-only shares strip — below video grid, above bottom bar. Auto-hides with the bottom bar. */}
      {audioTiles.length > 0 && (
        <div
          className="shrink-0 border-t border-wavis-text-secondary/20 bg-wavis-panel divide-y divide-wavis-text-secondary/10 transition-opacity duration-700"
          style={{
            opacity: bottomBarActive ? 1 : 0,
            pointerEvents: bottomBarActive ? 'auto' : 'none',
          }}
        >
          {audioTiles.map((tile) => (
            <AudioOnlyTile
              key={tile.participantId}
              participantId={tile.participantId}
              displayName={tile.displayName}
              color={tile.color}
              muted={tile.muted}
              volume={tile.volume}
              onToggleMute={handleToggleMute}
              onVolumeChange={handleVolumeChange}
            />
          ))}
        </div>
      )}

      {isDiagnosticsTestMode ? (
        <div className="bg-wavis-panel border-t border-wavis-text-secondary/20 px-3 py-1.5 flex items-center gap-2 text-xs">
          <span className="text-wavis-text-secondary">diagnostics mode</span>
          <span className="text-wavis-text-secondary opacity-30 select-none leading-none">|</span>
          <span className="text-wavis-text">
            {tiles.length} test stream{tiles.length === 1 ? '' : 's'}
          </span>
          <div className="flex-1" />
          <FocusMainButton
            onClick={() => {
              console.log('[wavis:focus-main] button clicked in watch-all');
              void emitTo('main', 'focus-main-window', {})
                .then(() => console.log('[wavis:focus-main] emitTo resolved'))
                .catch((e) => console.error('[wavis:focus-main] emitTo failed', e));
            }}
          />
        </div>
      ) : (
        <div
          className="bg-wavis-panel border-t border-wavis-text-secondary/20 px-3 py-1.5 flex items-center gap-1 relative text-xs leading-none transition-opacity duration-700"
          style={{
            opacity: bottomBarActive ? 1 : 0,
            pointerEvents: bottomBarActive ? 'auto' : 'none',
          }}
        >
          <QuickActionButtons
            isMuted={userState.isMuted}
            isDeafened={userState.isDeafened}
            onToggleMute={() => {
              void emit('share:toggle-mute');
            }}
            onToggleDeafen={() => {
              void emit('share:toggle-deafen');
            }}
          />
          <span className="text-wavis-text-secondary opacity-30 select-none leading-none px-0.5">
            │
          </span>
          <FocusMainButton
            onClick={() => {
              console.log('[wavis:focus-main] button clicked in watch-all');
              void emitTo('main', 'focus-main-window', {})
                .then(() => console.log('[wavis:focus-main] emitTo resolved'))
                .catch((e) => console.error('[wavis:focus-main] emitTo failed', e));
            }}
          />
          <div className="flex-1" />
          <div className="relative flex items-center gap-1 shrink-0">
            <button
              onMouseDown={(e) => e.stopPropagation()}
              onClick={(e) => {
                e.stopPropagation();
                toggleMixerPanel('voice');
              }}
              className="h-5 w-5 flex items-center justify-center text-wavis-text-secondary hover:opacity-70 transition-opacity"
              aria-label="Open voice volume"
              title="voice volume"
            >
              <Volume2 size={14} strokeWidth={1.8} aria-hidden="true" />
            </button>
            {voiceMixerOpen && (
              <ParticipantMixer
                participants={voiceParticipants}
                onVolumeChange={handleVoiceVolumeChange}
                onToggleMute={handleVoiceMuteToggle}
                onClose={() => closeMixerPanel('voice')}
                title="voice volume"
                emptyMessage="no voice participants"
                positionClassName={mixerPositionClass('voice')}
              />
            )}
            <button
              onMouseDown={(e) => e.stopPropagation()}
              onClick={(e) => {
                e.stopPropagation();
                toggleMixerPanel('share');
              }}
              className="h-5 w-5 flex items-center justify-center text-wavis-text-secondary hover:opacity-70 transition-opacity"
              aria-label="Open stream volume"
              title="stream volume"
            >
              <span className="relative block h-4 w-4 leading-none">
                <span className="absolute inset-0 flex items-center justify-center">
                  {MIXER_ICON}
                </span>
                <Volume2
                  className="absolute -bottom-1 -right-1.5 text-wavis-text"
                  size={11}
                  strokeWidth={2.5}
                  aria-hidden="true"
                />
              </span>
            </button>
            {mixerOpen && (
              <ParticipantMixer
                participants={tiles.map((tile) => ({
                  id: tile.participantId,
                  name: tile.displayName,
                  color: tile.color,
                  volume: tile.volume,
                  muted: tile.muted,
                }))}
                onVolumeChange={handleVolumeChange}
                onToggleMute={handleToggleMute}
                onClose={() => closeMixerPanel('share')}
                title="stream volume"
                positionClassName={mixerPositionClass('share')}
              />
            )}
          </div>
        </div>
      )}
    </div>
  );
}
