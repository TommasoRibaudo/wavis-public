import { useState, useEffect, useRef, useCallback } from 'react';
import { getCurrentWindow } from '@tauri-apps/api/window';
import { emit, emitTo, listen } from '@tauri-apps/api/event';
import { startReceiving, stopReceiving } from './screen-share-viewer';
import type { ShareQuality } from '@features/voice/voice-room';
import { useShareTransitionOverlay } from './share-transition';
import { useVideoStallDetector } from './useVideoStallDetector';
import { useShareReconnect } from './useShareReconnect';
import { useAutoHide } from '@shared/hooks/useAutoHide';
import StreamHoverBar from '@shared/StreamHoverBar';
import type { MixerParticipant } from '@shared/ParticipantMixer';
import ShareSwitchingOverlay from './ShareSwitchingOverlay';

/* ─── Constants ─────────────────────────────────────────────────── */

const MIN_ZOOM = 1;
const MAX_ZOOM = 10;
const ZOOM_STEP = 0.15;

/* ─── Types ─────────────────────────────────────────────────────── */

interface ShareWindowParams {
  participantId: string;
  username: string;
  userColor: string;
  isOwner: boolean;
  canvasFallback?: boolean;
  initialVolume?: number;
}

interface ShareUserState {
  isMuted: boolean;
  isDeafened: boolean;
}

/* ─── Helpers ───────────────────────────────────────────────────── */

function parseHashParams(): ShareWindowParams | null {
  try {
    const hash = window.location.hash.slice(1);
    if (!hash) return null;
    return JSON.parse(decodeURIComponent(hash));
  } catch {
    return null;
  }
}

/* ═══ Component ═════════════════════════════════════════════════════ */

export default function ScreenSharePage() {
  const params = useRef(parseHashParams());
  const shareParams = params.current;
  const videoRef = useRef<HTMLVideoElement>(null);
  const videoAreaRef = useRef<HTMLDivElement>(null);
  const initialVolume = shareParams?.initialVolume ?? 70;

  const [stream, setStream] = useState<MediaStream | null>(null);
  const [error, setError] = useState<string | null>(null);
  const [volume, setVolume] = useState(initialVolume);
  const [muted, setMuted] = useState(initialVolume === 0);
  const [quality, setQuality] = useState<ShareQuality>('high');
  const [sharingAudio, setSharingAudio] = useState(false);
  const [userState, setUserState] = useState<ShareUserState>({
    isMuted: false,
    isDeafened: false,
  });
  const [voiceParticipants, setVoiceParticipants] = useState<MixerParticipant[]>([]);
  const canvasRef = useRef<HTMLCanvasElement>(null);
  const [debugInfo, setDebugInfo] = useState('init');
  const { isSwitching, markFrameRendered } = useShareTransitionOverlay({
    hasSurface: Boolean(stream) || Boolean(shareParams?.canvasFallback),
    hasError: Boolean(error),
  });

  // Zoom state
  const [zoom, setZoom] = useState(1);
  const [pan, setPan] = useState({ x: 0, y: 0 });
  const panRef = useRef<{
    startX: number; startY: number;
    origPanX: number; origPanY: number;
  } | null>(null);

  // Auto-hide controls on mouse idle
  const { isVisible: controlsVisible } = useAutoHide({ delayMs: 2000, listenToMouseMove: true });

  const p = params.current;

  const { retryCount, triggerReconnect } = useShareReconnect({
    onTrigger: () => {
      setError(null);
      setStream(null);
    },
  });

  /* ── Receive stream from main window via WebRTC bridge (primary) or
       listen for screen_share_frame events directly (canvas fallback) ── */

  useEffect(() => {
    if (!p) return;
    let cancelled = false;

    if (p.canvasFallback) {
      // Canvas fallback: listen for screen-share-frame Tauri events directly
      // and paint onto a visible canvas element.
      //
      // Two event names exist:
      //   - Linux emits 'screen_share_frame' (underscores) with an identity field
      //   - Windows emits 'screen-share-frame' (hyphens) without identity
      // Subscribe to both so the fallback works cross-platform.
      let frameCount = 0;
      setDebugInfo('canvas-fallback: listening');
      const handleFrame = (payload: { identity?: string; frame: string; width?: number; height?: number }) => {
        if (cancelled) return;
        // If identity is present (Linux path), filter by participant
        if (payload.identity && payload.identity !== p.participantId) return;

        frameCount++;
        if (frameCount <= 3 || frameCount % 30 === 0) {
          setDebugInfo(`canvas: frame #${frameCount} (${payload.width ?? '?'}x${payload.height ?? '?'})`);
        }

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

      // Subscribe to both event name variants
      const unlistenLinux = listen<{ identity: string; frame: string }>(
        'screen_share_frame',
        (event) => handleFrame(event.payload),
      );
      const unlistenWindows = listen<{ frame: string; width: number; height: number }>(
        'screen-share-frame',
        (event) => handleFrame(event.payload),
      );

      // Mark as "connected" immediately — frames will arrive as they come
      setStream(null);
      void emitTo('main', 'screen-share-viewer:ready', {
        participantId: p.participantId,
        windowLabel: `screen-share-${p.participantId}`,
      });

      return () => {
        cancelled = true;
        unlistenLinux.then((fn) => fn());
        unlistenWindows.then((fn) => fn());
      };
    }

    // Primary path: receive stream via WebRTC loopback bridge
    setDebugInfo('bridge: connecting...');
    startReceiving(p.participantId, `screen-share-${p.participantId}`, () => {
      if (!cancelled) {
        console.warn('[wavis:screen-share] bridge connection failed, triggering reconnect');
        triggerReconnect();
      }
    })
      .then((s) => {
        if (!cancelled) {
          void emitTo('main', 'screen-share-viewer:ready', {
            participantId: p.participantId,
            windowLabel: `screen-share-${p.participantId}`,
          });
          void emit('viewer-subscribed', { targetId: p.participantId });
          const tracks = s.getVideoTracks();
          const vt = tracks[0];
          const updateDebug = () => {
            if (cancelled || !vt) return;
            setDebugInfo(
              `bridge: ${vt.readyState}, muted=${vt.muted}, enabled=${vt.enabled}`,
            );
          };
          updateDebug();
          // Poll track state every second to detect muted→unmuted transitions
          const pollId = setInterval(updateDebug, 1000);
          // Also listen for unmute event
          if (vt) {
            vt.addEventListener('unmute', updateDebug);
            vt.addEventListener('mute', updateDebug);
          }
          // Store cleanup in a ref-accessible way via the cancelled flag
          const origCancel = () => {
            clearInterval(pollId);
            if (vt) {
              vt.removeEventListener('unmute', updateDebug);
              vt.removeEventListener('mute', updateDebug);
            }
          };
          // Attach to the stream so the cleanup effect can find it
          (s as unknown as Record<string, unknown>).__debugCleanup = origCancel;
          setStream(s);
        }
      })
      .catch((err) => {
        if (!cancelled) {
          const msg = err instanceof Error ? err.message : 'Failed to receive stream';
          setDebugInfo(`bridge: error — ${msg}`);
          setError(msg);
        }
      });

    return () => {
      cancelled = true;
      stopReceiving();
    };
  }, [p, retryCount, triggerReconnect]); // retryCount increments trigger a fresh bridge connection

  // Attach stream to video element
  useEffect(() => {
    const video = videoRef.current;
    if (!video || !stream) return;
    video.srcObject = stream;
    video.play().catch(() => {});
  }, [stream]);

  useVideoStallDetector({
    videoRef,
    stream,
    onFrameDetected: markFrameRendered,
    onDeadTrack: triggerReconnect,
    onReattach: markFrameRendered,
  });

  /* ── Listen for close event from main window ── */

  useEffect(() => {
    const unlisten = listen('screen-share:close', () => {
      getCurrentWindow().close();
    });
    return () => { unlisten.then((fn) => fn()); };
  }, []);

  // Defense-in-depth: self-close when the voice session ends (e.g. main
  // window closed, user kicked, or leaveRoom() called for any reason).
  useEffect(() => {
    const unlisten = listen('voice-session:ended', () => {
      getCurrentWindow().close();
    });
    return () => { unlisten.then((fn) => fn()); };
  }, []);

  // Notify main window when this window closes
  useEffect(() => {
    const win = getCurrentWindow();
    const unlisten = win.onCloseRequested(async () => {
      await emit('screen-share:closed', { participantId: p?.participantId });
    });
    return () => { unlisten.then((fn) => fn()); };
  }, [p]);

  useEffect(() => {
    if (!p) return;
    const unlisten = listen<{ participantId: string; volume: number }>('screen-share:restore-volume', (event) => {
      if (event.payload.participantId !== p.participantId) return;
      setVolume(event.payload.volume);
      setMuted(event.payload.volume === 0);
    });
    return () => { unlisten.then((fn) => fn()); };
  }, [p]);

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
    const unlisten = listen<{ participants: MixerParticipant[] }>('share:voice-participants', (event) => {
      setVoiceParticipants(event.payload.participants);
    });
    return () => { unlisten.then((fn) => fn()); };
  }, []);

  /* ── Zoom (scroll wheel) + Pan (drag when zoomed) ── */

  const clampPan = useCallback(
    (px: number, py: number, z: number, w: number, h: number) => {
      if (z <= 1) return { x: 0, y: 0 };
      const maxPanX = (w * (z - 1)) / 2;
      const maxPanY = (h * (z - 1)) / 2;
      return {
        x: Math.max(-maxPanX, Math.min(maxPanX, px)),
        y: Math.max(-maxPanY, Math.min(maxPanY, py)),
      };
    },
    [],
  );

  const resetZoom = useCallback(() => {
    setZoom(1);
    setPan({ x: 0, y: 0 });
  }, []);

  // Scroll-to-zoom toward cursor position
  useEffect(() => {
    const area = videoAreaRef.current;
    if (!area) return;

    const onWheel = (e: WheelEvent) => {
      e.preventDefault();
      const rect = area.getBoundingClientRect();
      const cx = e.clientX - rect.left - rect.width / 2;
      const cy = e.clientY - rect.top - rect.height / 2;

      setZoom((prev) => {
        const direction = e.deltaY < 0 ? 1 : -1;
        const next = Math.max(MIN_ZOOM, Math.min(MAX_ZOOM, prev + direction * ZOOM_STEP * prev));
        const scaleFactor = next / prev;
        setPan((prevPan) => {
          const newPx = cx - scaleFactor * (cx - prevPan.x);
          const newPy = cy - scaleFactor * (cy - prevPan.y);
          return clampPan(newPx, newPy, next, rect.width, rect.height);
        });
        return next;
      });
    };

    area.addEventListener('wheel', onWheel, { passive: false });
    return () => area.removeEventListener('wheel', onWheel);
  }, [clampPan]);

  // Pan via mouse drag when zoomed in
  useEffect(() => {
    const area = videoAreaRef.current;
    if (!area) return;

    const onDown = (e: MouseEvent) => {
      if (zoom <= 1) return;
      if ((e.target as HTMLElement).closest('[data-no-drag]')) return;
      panRef.current = {
        startX: e.clientX, startY: e.clientY,
        origPanX: pan.x, origPanY: pan.y,
      };
      e.preventDefault();
    };
    const onMove = (e: MouseEvent) => {
      if (!panRef.current) return;
      const dx = e.clientX - panRef.current.startX;
      const dy = e.clientY - panRef.current.startY;
      const rect = area.getBoundingClientRect();
      setPan(
        clampPan(
          panRef.current.origPanX + dx,
          panRef.current.origPanY + dy,
          zoom,
          rect.width,
          rect.height,
        ),
      );
    };
    const onUp = () => { panRef.current = null; };

    area.addEventListener('mousedown', onDown);
    window.addEventListener('mousemove', onMove);
    window.addEventListener('mouseup', onUp);
    return () => {
      area.removeEventListener('mousedown', onDown);
      window.removeEventListener('mousemove', onMove);
      window.removeEventListener('mouseup', onUp);
    };
  }, [zoom, pan, clampPan]);

  /* ── Owner actions → emit to main window ── */

  const handleQualityChange = (q: ShareQuality) => {
    setQuality(q);
    emit('screen-share:quality', { quality: q });
  };

  const handleToggleAudio = () => {
    const next = !sharingAudio;
    setSharingAudio(next);
    emit('screen-share:toggle-audio', { withAudio: next });
  };

  const handleChangeSource = () => {
    emit('screen-share:change-source', {});
  };

  const handleVolumeChange = useCallback((nextVolume: number) => {
    if (!p) return;
    setVolume(nextVolume);
    setMuted(nextVolume === 0);
    emit('screen-share:volume-change', { participantId: p.participantId, volume: nextVolume });
  }, [p]);

  const handleToggleMute = useCallback(() => {
    const nextVolume = muted ? (volume > 0 ? volume : 50) : 0;
    handleVolumeChange(nextVolume);
  }, [handleVolumeChange, muted, volume]);

  const handleVoiceVolumeChange = useCallback((participantId: string, nextVolume: number) => {
    setVoiceParticipants((prev) =>
      prev.map((participant) =>
        participant.id === participantId
          ? { ...participant, volume: nextVolume, muted: nextVolume === 0 }
          : participant,
      ),
    );
    void emit('share:voice-volume-change', { participantId, volume: nextVolume });
  }, []);

  const handleVoiceMuteToggle = useCallback((participantId: string) => {
    setVoiceParticipants((prev) =>
      prev.map((participant) => {
        if (participant.id !== participantId) return participant;
        const nextVolume = participant.muted ? (participant.volume > 0 ? participant.volume : 50) : 0;
        void emit('share:voice-volume-change', { participantId, volume: nextVolume });
        return { ...participant, volume: nextVolume, muted: nextVolume === 0 };
      }),
    );
  }, []);

  const emitShareAction = useCallback((eventName: 'share:toggle-mute' | 'share:toggle-deafen') => {
    void emit(eventName);
  }, []);

  const handleClose = () => {
    getCurrentWindow().close();
  };

  /** Double-click on video area: request pop-back-in to Watch All grid.
   *  ActiveRoom handles this — it only acts if Watch All is open.
   *  Ignore clicks that land on control overlays. */
  const handleDoubleClick = useCallback((e: React.MouseEvent) => {
    if ((e.target as HTMLElement).closest('[data-no-drag]')) return;
    if (!p) return;
    emit('screen-share:pop-back-in', { participantId: p.participantId });
  }, [p]);

  /* ── Render ── */

  if (!p) {
    return (
      <div className="h-screen flex items-center justify-center bg-wavis-bg font-mono text-wavis-danger">
        missing screen share parameters
      </div>
    );
  }

  const qualityLabel: Record<ShareQuality, string> = {
    low: 'Smooth',
    high: 'Sharp',
    max: 'Max',
  };

  const ownerControls = p.isOwner ? (
    <div className="flex items-center gap-2 flex-wrap min-w-0">
      <button
        onClick={(e) => {
          e.stopPropagation();
          handleChangeSource();
        }}
        className="text-wavis-text-secondary hover:opacity-70 transition-opacity"
      >
        /window
      </button>
      <span className="text-wavis-text-secondary opacity-30 select-none">|</span>
      {(['low', 'high', 'max'] as ShareQuality[]).map((q) => (
        <button
          key={q}
          onClick={(e) => {
            e.stopPropagation();
            handleQualityChange(q);
          }}
          className="hover:opacity-70 transition-opacity"
          style={{
            color: quality === q
              ? 'var(--wavis-accent)'
              : 'var(--wavis-text-secondary)',
          }}
        >
          {qualityLabel[q]}
        </button>
      ))}
      <span className="text-wavis-text-secondary opacity-30 select-none">|</span>
      <button
        onClick={(e) => {
          e.stopPropagation();
          handleToggleAudio();
        }}
        className="hover:opacity-70 transition-opacity"
        style={{
          color: sharingAudio
            ? 'var(--wavis-accent)'
            : 'var(--wavis-text-secondary)',
        }}
      >
        {sharingAudio ? 'audio on' : 'audio off'}
      </button>
      {zoom > 1 && (
        <>
          <span className="text-wavis-text-secondary opacity-30 select-none">|</span>
          <span className="text-wavis-text-secondary">
            {Math.round(zoom * 100)}%
          </span>
          <button
            onClick={(e) => {
              e.stopPropagation();
              resetZoom();
            }}
            className="text-wavis-text-secondary hover:opacity-70 transition-opacity"
          >
            /reset
          </button>
        </>
      )}
    </div>
  ) : undefined;

  return (
    <div className="h-screen flex flex-col bg-wavis-overlay-base font-mono text-wavis-text overflow-hidden select-none">
      {/* Header — draggable title bar */}
      <div
        data-tauri-drag-region
        className="flex items-center justify-between px-2 border-b border-wavis-text-secondary bg-wavis-panel text-xs shrink-0"
        style={{ height: 32 }}
      >
        <div className="flex items-center gap-2 min-w-0">
          <span style={{ color: 'var(--wavis-purple)' }}>▲</span>
          <span className="truncate" style={{ color: p.userColor }}>{p.username}</span>
          <span className="text-wavis-text-secondary">
            {p.isOwner ? '(you)' : 'screen share'}
          </span>
        </div>
        <button
          onClick={handleClose}
          className="inline-flex items-center justify-center w-8 h-8 hover:bg-wavis-danger hover:text-wavis-text-contrast text-wavis-danger shrink-0 transition-colors"
          aria-label="Close screen share window"
        >
          [x]
        </button>
      </div>

      {/* Video area — double-click to pop back into Watch All grid */}
      <div
        ref={videoAreaRef}
        className="flex-1 relative overflow-hidden"
        style={{ cursor: zoom > 1 ? 'grab' : 'default' }}
        onDoubleClick={handleDoubleClick}
      >
        {stream ? (
          <video
            ref={videoRef}
            autoPlay
            playsInline
            style={{
              width: '100%',
              height: '100%',
              objectFit: 'contain',
              transform: `scale(${zoom}) translate(${pan.x / zoom}px, ${pan.y / zoom}px)`,
              transformOrigin: 'center center',
              willChange: zoom > 1 ? 'transform' : undefined,
            }}
          />
        ) : p.canvasFallback ? (
          <canvas
            ref={canvasRef}
            style={{
              width: '100%',
              height: '100%',
              objectFit: 'contain',
              transform: `scale(${zoom}) translate(${pan.x / zoom}px, ${pan.y / zoom}px)`,
              transformOrigin: 'center center',
              willChange: zoom > 1 ? 'transform' : undefined,
            }}
          />
        ) : error ? (
          <div className="h-full flex items-center justify-center text-wavis-danger text-xs">
            {error}
          </div>
        ) : (
          <div className="h-full flex items-center justify-center text-wavis-text-secondary text-xs">
            connecting...
          </div>
        )}
        {isSwitching && <ShareSwitchingOverlay displayName={p.username} />}

        {/* Zoom overlay */}
        {zoom > 1 && (
          <div
            data-no-drag
            className="absolute top-2 right-2 text-[0.625rem] transition-opacity duration-300"
            style={{
              opacity: controlsVisible ? 1 : 0,
              pointerEvents: controlsVisible ? 'auto' : 'none',
            }}
          >
            <div className="flex items-center gap-2 px-2 py-1 bg-wavis-panel/90">
              <span className="text-wavis-text-secondary">
                {Math.round(zoom * 100)}%
              </span>
              <button
                onClick={resetZoom}
                className="text-wavis-text-secondary hover:opacity-70 transition-opacity"
              >
                /reset
              </button>
            </div>
          </div>
        )}

        <StreamHoverBar
          visible={controlsVisible}
          isMuted={userState.isMuted}
          isDeafened={userState.isDeafened}
          onToggleMute={() => emitShareAction('share:toggle-mute')}
          onToggleDeafen={() => emitShareAction('share:toggle-deafen')}
          streamVolume={volume}
          streamMuted={muted}
          streamVolumeColor={p.userColor}
          onStreamVolumeChange={handleVolumeChange}
          onStreamMuteToggle={handleToggleMute}
          voiceParticipants={voiceParticipants}
          onVoiceVolumeChange={handleVoiceVolumeChange}
          onVoiceMuteToggle={handleVoiceMuteToggle}
          ownerControls={ownerControls}
        />

        {import.meta.env.VITE_DEBUG_SHOW_STREAM_OVERLAY === 'true' && (
          <div
            className="absolute top-1 left-1 text-[0.5rem] text-wavis-warn font-mono pointer-events-none"
            style={{ backgroundColor: 'rgba(0,0,0,0.7)', padding: '2px 4px' }}
          >
            {debugInfo} | fallback={String(!!p.canvasFallback)} | stream={String(!!stream)} | owner={String(p.isOwner)}
          </div>
        )}
      </div>
    </div>
  );
}
