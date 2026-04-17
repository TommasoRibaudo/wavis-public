import { useState, useEffect, useRef, useCallback, memo } from 'react';
import { Volume2 } from 'lucide-react';
import { getCurrentWindow } from '@tauri-apps/api/window';
import { emit, emitTo, listen } from '@tauri-apps/api/event';
import { StreamReceiver } from './screen-share-viewer';
import { computeGridLayout } from './watch-all-grid';
import { useShareTransitionOverlay } from './share-transition';
import { isPlaybackHealthyWithoutFreshFrames } from './useVideoStallDetector';
import { useAutoHide } from '@shared/hooks/useAutoHide';
import { VolumeSlider } from '@shared/VolumeSlider';
import ParticipantMixer, { type MixerParticipant } from '@shared/ParticipantMixer';
import QuickActionButtons from '@shared/QuickActionButtons';
import ShareSwitchingOverlay from './ShareSwitchingOverlay';

/* ─── Constants ─────────────────────────────────────────────────── */

const DEBUG_SHARE_VIEW = import.meta.env.VITE_DEBUG_SCREEN_SHARE_VIEW === 'true';
const LOG = '[wavis:watch-all]';

const TITLE_BAR_HEIGHT = 32;
const LABEL_FADE_DELAY_MS = 3000;
const GLOBAL_BAR_FADE_DELAY_MS = 5000;
// Delay before auto-retrying after a bridge failure. Gives the main window
// time to call resendStream() after LiveKit reconnects the screen share track.
const AUTO_RETRY_DELAY_MS = 1500;

/* ─── Types ─────────────────────────────────────────────────────── */

interface WatchAllParams {
  channelName: string;
}

interface ShareTileState {
  participantId: string;
  displayName: string;
  color: string;
  canvasFallback: boolean;
  muted: boolean;
  volume: number;
}

interface ShareUserState {
  isMuted: boolean;
  isDeafened: boolean;
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
    return JSON.parse(decodeURIComponent(hash));
  } catch {
    return null;
  }
}

/* ─── Sub-components ────────────────────────────────────────────── */

interface ShareTileProps {
  participantId: string;
  displayName: string;
  color: string;
  canvasFallback: boolean;
  muted: boolean;
  volume: number;
  onToggleMute: (participantId: string) => void;
  onVolumeChange: (participantId: string, volume: number) => void;
  onPopOut: (participantId: string, volume: number) => void;
}

const ShareTile = memo(function ShareTile({
  participantId,
  displayName,
  color,
  canvasFallback,
  muted,
  volume,
  onToggleMute,
  onVolumeChange,
  onPopOut,
}: ShareTileProps) {
  const videoRef = useRef<HTMLVideoElement>(null);
  const canvasRef = useRef<HTMLCanvasElement>(null);
  const receiverRef = useRef<StreamReceiver | null>(null);
  const [stream, setStream] = useState<MediaStream | null>(null);
  const [error, setError] = useState<string | null>(null);
  const [hovered, setHovered] = useState(false);
  const { isVisible: labelVisible, resetTimer: revealLabel } = useAutoHide({ delayMs: LABEL_FADE_DELAY_MS });
  const { isSwitching, markFrameRendered } = useShareTransitionOverlay({
    hasSurface: canvasFallback || Boolean(stream),
    hasError: Boolean(error),
  });

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

  /* ── Stream lifecycle ── */

  const [retryCount, setRetryCount] = useState(0);
  const retryTimerRef = useRef<ReturnType<typeof setTimeout> | null>(null);
  const requestSenderResync = useCallback(() => {
    void emitTo('main', 'watch-all:request-resend', { participantId });
  }, [participantId]);

  // Called by StreamReceiver when the RTCPeerConnection transitions to 'failed'.
  // Sets error state and schedules an automatic bridge reconnect after a short
  // delay to give the main window time to call resendStream() once LiveKit
  // finishes reconnecting the screen share track.
  const scheduleRetry = useCallback(() => {
    if (DEBUG_SHARE_VIEW) console.warn(LOG, `scheduleRetry — participantId: ${participantId}, retryCount: ${retryCount}, timestamp: ${Date.now()}`);
    requestSenderResync();
    setError('connection failed');
    if (retryTimerRef.current !== null) return; // already scheduled
    retryTimerRef.current = setTimeout(() => {
      retryTimerRef.current = null;
      setRetryCount((c) => c + 1);
    }, AUTO_RETRY_DELAY_MS);
  }, [participantId, requestSenderResync, retryCount]);

  useEffect(() => {
    if (canvasFallback) return;
    let cancelled = false;

    // Stop the previous receiver before creating a fresh one (handles
    // both the initial mount and every auto-retry / manual-retry cycle).
    if (receiverRef.current) {
      receiverRef.current.stop();
      receiverRef.current = null;
    }

    setError(null);
    const receiver = new StreamReceiver(participantId, 'watch-all');
    receiverRef.current = receiver;

    // Pass scheduleRetry as onConnectionFailed so it is wired to
    // pc.onconnectionstatechange synchronously — before any await — which
    // closes the race window where a failure could arrive before the
    // manual pc.addEventListener() call after start() resolved (old code).
    requestSenderResync();
    receiver.start(scheduleRetry)
      .then((s) => {
        if (cancelled) return;
        if (DEBUG_SHARE_VIEW) console.log(LOG, `receiver.start() resolved — participantId: ${participantId}, retryCount: ${retryCount}`);
        setStream(s);
        void emitTo('main', 'screen-share-viewer:ready', {
          participantId,
          windowLabel: 'watch-all',
        });
        void emitTo('main', 'viewer-subscribed', { targetId: participantId });
      })
      .catch((err) => {
        if (cancelled) return;
        if (DEBUG_SHARE_VIEW) console.error(LOG, `receiver.start() rejected — participantId: ${participantId}, error:`, err);
        const message = err instanceof Error ? err.message : 'connection failed';
        if (message === 'Timed out waiting for screen share stream') {
          scheduleRetry();
          return;
        }
        setError(message);
      });

    return () => {
      cancelled = true;
      // Cancel any pending auto-retry timer on cleanup/unmount.
      if (retryTimerRef.current !== null) {
        clearTimeout(retryTimerRef.current);
        retryTimerRef.current = null;
      }
      if (receiverRef.current) {
        receiverRef.current.stop();
        receiverRef.current = null;
      }
    };
  }, [canvasFallback, participantId, requestSenderResync, retryCount, scheduleRetry]);

  // Attach stream to video element
  useEffect(() => {
    const video = videoRef.current;
    if (!video || !stream) return;
    video.srcObject = stream;
    video.play().catch(() => {});
  }, [stream]);

  /* ── Canvas fallback (Linux) ── */

  useEffect(() => {
    if (!canvasFallback) return;
    let cancelled = false;

    const handleFrame = (payload: { identity?: string; frame: string; width?: number; height?: number }) => {
      if (cancelled) return;
      if (payload.identity && payload.identity !== participantId) return;

      const canvas = canvasRef.current;
      if (!canvas) return;
      const ctx = canvas.getContext('2d');
      if (!ctx) return;

      const img = new Image();
      img.onload = () => {
        if (canvas.width !== img.width || canvas.height !== img.height) {
          canvas.width = img.width;
          canvas.height = img.height;
        }
        ctx.drawImage(img, 0, 0);
        markFrameRendered();
      };
      img.src = `data:image/jpeg;base64,${payload.frame}`;
    };

    const unlistenLinux = listen<{ identity: string; frame: string }>(
      `ss-frame:${participantId}`,
      (event) => handleFrame(event.payload),
    );
    // Also listen for the generic event names (cross-platform compat)
    const unlistenGenericLinux = listen<{ identity: string; frame: string }>(
      'screen_share_frame',
      (event) => handleFrame(event.payload),
    );
    const unlistenGenericWin = listen<{ frame: string; width: number; height: number }>(
      'screen-share-frame',
      (event) => handleFrame(event.payload),
    );
    void emitTo('main', 'screen-share-viewer:ready', {
      participantId,
      windowLabel: 'watch-all',
    });

    return () => {
      cancelled = true;
      unlistenLinux.then((fn) => fn());
      unlistenGenericLinux.then((fn) => fn());
      unlistenGenericWin.then((fn) => fn());
    };
  }, [canvasFallback, participantId]);

  useEffect(() => {
    const video = videoRef.current;
    if (!video || !stream || canvasFallback) return;

    let disposed = false;
    const hasRvfc = 'requestVideoFrameCallback' in HTMLVideoElement.prototype;
    let timeupdateHandler: (() => void) | null = null;
    const healthInterval = setInterval(() => {
      if (disposed) return;
      if (isPlaybackHealthyWithoutFreshFrames(video, stream)) {
        markFrameRendered();
      }
    }, 1000);

    if (hasRvfc) {
      const scheduleRvfc = () => {
        if (disposed) return;
        // eslint-disable-next-line @typescript-eslint/no-explicit-any
        (video as any).requestVideoFrameCallback(() => {
          markFrameRendered();
          scheduleRvfc();
        });
      };
      scheduleRvfc();
    } else {
      timeupdateHandler = () => {
        markFrameRendered();
      };
      video.addEventListener('timeupdate', timeupdateHandler);
    }

    return () => {
      disposed = true;
      clearInterval(healthInterval);
      if (timeupdateHandler) {
        video.removeEventListener('timeupdate', timeupdateHandler);
      }
    };
  }, [canvasFallback, markFrameRendered, stream]);

  /* ── Retry handler ── */

  const handleRetry = useCallback(() => {
    // Cancel any scheduled auto-retry so we don't double-reconnect.
    if (retryTimerRef.current !== null) {
      clearTimeout(retryTimerRef.current);
      retryTimerRef.current = null;
    }
    setStream(null);
    setError(null);
    requestSenderResync();
    // Increment retryCount — the bridge useEffect re-runs, stops the old
    // receiver, creates a new StreamReceiver, and emits receiver-ready.
    // The main window's sender (re-established by resendStream() in ActiveRoom)
    // will respond with the current offer. No resendStream() call here: that
    // must only be called by the MAIN window which owns the sender entry.
    setRetryCount((c) => c + 1);
  }, [requestSenderResync]);

  /* ── Double-click → pop out ── */

  const handleDoubleClick = useCallback(() => {
    onPopOut(participantId, volume);
  }, [onPopOut, participantId, volume]);

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
      ) : canvasFallback ? (
        <canvas
          ref={canvasRef}
          style={{ width: '100%', height: '100%', objectFit: 'contain' }}
        />
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

      {/* Pop-out icon (top-right on hover) */}
      {hovered && (
        <button
          className="absolute top-1 right-1 text-wavis-text hover:text-wavis-accent text-xs bg-wavis-overlay-base/60 px-1 py-0.5 rounded"
          onClick={(e) => { e.stopPropagation(); onPopOut(participantId, volume); }}
          aria-label={`Pop out ${displayName}`}
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
        <span className="truncate" style={{ color }}>{displayName}</span>

        {/* Mute toggle (hidden on canvas fallback — no audio available) */}
        {!canvasFallback && (
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
              onClick={(e) => { e.stopPropagation(); onToggleMute(participantId); }}
              aria-label={muted ? `Unmute ${displayName}` : `Mute ${displayName}`}
            >
              {muted ? STREAM_MUTED_ICON : STREAM_UNMUTED_ICON}
            </button>
          </div>
        )}
      </div>
    </div>
  );
});

/* ═══ Component ═════════════════════════════════════════════════════ */

export default function WatchAllPage() {
  const params = useRef(parseHashParams());
  const [tiles, setTiles] = useState<ShareTileState[]>([]);
  const pendingRestoreVolumesRef = useRef<Map<string, number>>(new Map());
  const gridRef = useRef<HTMLDivElement>(null);
  const previousLayoutRef = useRef<{ shareCount: number; columns: number } | null>(null);
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

  const p = params.current;

  /* ── Tile event listeners ── */

  useEffect(() => {
    // Register ALL listeners first, then signal readiness to ActiveRoom.
    // ActiveRoom waits for watch-all:ready before emitting share-added events,
    // avoiding the race where events fire before listeners are registered.
    const setup = async () => {
      const unlistenAdded = await listen<{ participantId: string; displayName: string; color: string; canvasFallback: boolean }>(
        'watch-all:share-added',
        (event) => {
          console.log('[wavis:watch-all] share-added received:', event.payload.participantId, event.payload.displayName);
          const { participantId, displayName, color, canvasFallback } = event.payload;
          setTiles((prev) => {
            if (prev.some((t) => t.participantId === participantId)) return prev;
            const restoredVolume = pendingRestoreVolumesRef.current.get(participantId);
            if (restoredVolume !== undefined) {
              pendingRestoreVolumesRef.current.delete(participantId);
            }
            const baseTile = { participantId, displayName, color, canvasFallback, muted: false, volume: 70 };
            return [
              ...prev,
              restoredVolume === undefined
                ? baseTile
                : { ...baseTile, muted: restoredVolume === 0, volume: restoredVolume },
            ];
          });
        },
      );

      const unlistenRemoved = await listen<{ participantId: string }>(
        'watch-all:share-removed',
        (event) => {
          setTiles((prev) => prev.filter((t) => t.participantId !== event.payload.participantId));
        },
      );

      const unlistenUpdated = await listen<{ participantId: string; displayName: string; color: string }>(
        'watch-all:share-updated',
        (event) => {
          const { participantId, displayName, color } = event.payload;
          setTiles((prev) =>
            prev.map((t) =>
              t.participantId === participantId
                ? { ...t, displayName, color }
                : t,
            ),
          );
        },
      );

      const unlistenRestoreVolume = await listen<{ participantId: string; volume: number }>(
        'watch-all:restore-volume',
        (event) => {
          const { participantId, volume } = event.payload;
          setTiles((prev) => {
            let found = false;
            const next = prev.map((tile) => {
              if (tile.participantId !== participantId) return tile;
              found = true;
              return { ...tile, volume, muted: volume === 0 };
            });
            if (!found) {
              pendingRestoreVolumesRef.current.set(participantId, volume);
            }
            return next;
          });
        },
      );

      // All listeners registered — signal readiness to ActiveRoom
      console.log('[wavis:watch-all] emitting watch-all:ready');
      emit('watch-all:ready', {});

      return [unlistenAdded, unlistenRemoved, unlistenUpdated, unlistenRestoreVolume];
    };

    let cleanups: Array<() => void> = [];
    setup().then((fns) => { cleanups = fns; });

    return () => {
      for (const fn of cleanups) fn();
    };
  }, []);

  useEffect(() => {
    const unlisten = listen<ShareUserState>('share:user-state', (event) => {
      setUserState({
        isMuted: Boolean(event.payload.isMuted),
        isDeafened: Boolean(event.payload.isDeafened),
      });
    });
    return () => { unlisten.then((fn) => fn()); };
  }, []);

  useEffect(() => {
    const unlisten = listen<{ participants: MixerParticipant[] }>('watch-all:voice-participants', (event) => {
      setVoiceParticipants(event.payload.participants);
    });
    return () => { unlisten.then((fn) => fn()); };
  }, []);

  /* ── Window close / self-close listeners ── */

  // Listen for close command from ActiveRoom (room leave)
  useEffect(() => {
    const unlisten = listen('watch-all:close', () => {
      getCurrentWindow().close();
    });
    return () => { unlisten.then((fn) => fn()); };
  }, []);

  // Defense-in-depth: self-close when voice session ends
  useEffect(() => {
    const unlisten = listen('voice-session:ended', () => {
      getCurrentWindow().close();
    });
    return () => { unlisten.then((fn) => fn()); };
  }, []);

  // Notify ActiveRoom when this window closes
  useEffect(() => {
    const win = getCurrentWindow();
    const unlisten = win.onCloseRequested(async () => {
      await emit('watch-all:closed', {});
    });
    return () => { unlisten.then((fn) => fn()); };
  }, []);

  /* ── Grid resize tracking ── */

  useEffect(() => {
    const el = gridRef.current;
    if (!el) return;

    const observer = new ResizeObserver((entries) => {
      const entry = entries[0];
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
    setTiles((prev) =>
      prev.map((t) => {
        if (t.participantId !== participantId) return t;
        const nextMuted = !t.muted;
        const nextVolume = nextMuted ? 0 : (t.volume > 0 ? t.volume : 50);
        emit('watch-all:volume-change', { participantId, volume: nextVolume });
        return { ...t, muted: nextMuted, volume: nextVolume };
      }),
    );
  }, []);

  const handleVolumeChange = useCallback((participantId: string, volume: number) => {
    setTiles((prev) =>
      prev.map((t) =>
        t.participantId === participantId
          ? { ...t, volume, muted: volume === 0 }
          : t,
      ),
    );
    emit('watch-all:volume-change', { participantId, volume });
  }, []);

  const handleVoiceVolumeChange = useCallback((participantId: string, volume: number) => {
    setVoiceParticipants((prev) =>
      prev.map((participant) =>
        participant.id === participantId
          ? { ...participant, volume, muted: volume === 0 }
          : participant,
      ),
    );
    emit('share:voice-volume-change', { participantId, volume });
  }, []);

  const handleVoiceMuteToggle = useCallback((participantId: string) => {
    setVoiceParticipants((prev) =>
      prev.map((participant) => {
        if (participant.id !== participantId) return participant;
        const nextVolume = participant.muted ? (participant.volume > 0 ? participant.volume : 50) : 0;
        emit('share:voice-volume-change', { participantId, volume: nextVolume });
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

  const toggleMixerPanel = useCallback((panel: MixerPanel) => {
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
  }, [openMixerPanel]);

  /* ── Pop-out ── */

  const handlePopOut = useCallback((participantId: string, volume: number) => {
    emit('watch-all:pop-out', { participantId, volume });
  }, []);

  /* ── Close button ── */

  const handleClose = useCallback(() => {
    getCurrentWindow().close();
  }, []);

  /* ── Compute grid layout ── */

  const currentColumns = previousLayoutRef.current?.shareCount === tiles.length
    ? previousLayoutRef.current.columns
    : undefined;
  const layout = tiles.length > 0 && gridSize.width > 0 && gridSize.height > 0
    ? computeGridLayout(tiles.length, gridSize.width, gridSize.height, currentColumns)
    : null;
  const layoutColumns = layout?.columns ?? null;
  const bottomBarActive = bottomBarVisible || mixerOpen || voiceMixerOpen;
  const activeMixerPanelOrder = mixerPanelOrder.filter((panel) =>
    panel === 'voice' ? voiceMixerOpen : mixerOpen,
  );
  const mixerPositionClass = (panel: MixerPanel) => (
    activeMixerPanelOrder.indexOf(panel) <= 0
      ? 'absolute bottom-full right-0 mb-1'
      : 'absolute bottom-full right-[228px] mb-1'
  );

  useEffect(() => {
    previousLayoutRef.current = layoutColumns === null
      ? null
      : { shareCount: tiles.length, columns: layoutColumns };
  }, [layoutColumns, tiles.length]);

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
      {/* Title bar */}
      <div
        data-tauri-drag-region
        className="flex items-center justify-between px-2 border-b border-wavis-text-secondary bg-wavis-panel text-xs shrink-0"
        style={{ height: TITLE_BAR_HEIGHT }}
      >
        <div className="flex items-center gap-2 min-w-0">
          <span style={{ color: 'var(--wavis-purple)' }}>▲</span>
          <span className="truncate text-wavis-text">
            Watch All — {p.channelName}
          </span>
        </div>
        <button
          onClick={handleClose}
          className="inline-flex items-center justify-center w-8 h-8 hover:bg-wavis-danger hover:text-wavis-text-contrast text-wavis-danger shrink-0 transition-colors"
          aria-label="Close watch all window"
        >
          [x]
        </button>
      </div>

      {/* Grid container */}
      <div ref={gridRef} className="flex-1 overflow-hidden relative">
        {tiles.length === 0 ? (
          /* Empty state */
          <div className="h-full flex items-center justify-center text-wavis-text-secondary text-sm">
            no active shares
          </div>
        ) : layout ? (
          /* Grid of tiles */
          <div
            className="w-full h-full"
            style={{
              display: 'grid',
              gridTemplateColumns: `repeat(${layout.columns}, 1fr)`,
              gridTemplateRows: `repeat(${layout.rows}, 1fr)`,
            }}
          >
            {tiles.map((tile) => (
              <ShareTile
                key={tile.participantId}
                participantId={tile.participantId}
                displayName={tile.displayName}
                color={tile.color}
                canvasFallback={tile.canvasFallback}
                muted={tile.muted}
                volume={tile.volume}
                onToggleMute={handleToggleMute}
                onVolumeChange={handleVolumeChange}
                onPopOut={handlePopOut}
              />
            ))}
          </div>
        ) : null}
      </div>
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
          onToggleMute={() => { void emit('share:toggle-mute'); }}
          onToggleDeafen={() => { void emit('share:toggle-deafen'); }}
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
              <span className="absolute inset-0 flex items-center justify-center">{MIXER_ICON}</span>
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
    </div>
  );
}
