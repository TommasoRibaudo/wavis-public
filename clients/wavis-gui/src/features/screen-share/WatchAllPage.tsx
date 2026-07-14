import { useState, useEffect, useRef, useCallback, useMemo } from 'react';
import { Volume2 } from 'lucide-react';
import { ViewerRoomConnection } from './viewer-connection';
import { computeWatchAllLayout } from './watch-all-grid';
import { type WatchAllParams } from './watch-all-test-mode';
import { useAutoHide } from '@shared/hooks/useAutoHide';
import { useFullscreen } from '@shared/hooks/useFullscreen';
import ParticipantMixer, { type MixerParticipant } from '@shared/ParticipantMixer';
import QuickActionButtons from '@shared/QuickActionButtons';
import FocusMainButton from '@shared/FocusMainButton';
import FullscreenButton from '@shared/FullscreenButton';
import { FixedBugReportButton } from '@features/diagnostics/BugReportButton';
import ShareTile from './ShareTile';
import AudioOnlyTile from './AudioOnlyTile';
import { useWatchAllTiles } from './useWatchAllTiles';
import { DEBUG_SHARE_VIEW, LOG, MIXER_ICON } from './watch-all-constants';
import {
  closeCurrentChildWindow,
  onCurrentChildWindowCloseRequested,
  emitWatchAllClosed,
  onShareUserState,
  onWatchAllVoiceParticipantsUpdate,
  onWatchAllCloseCommand,
  onVoiceSessionEnded,
  emitWatchAllMuteChange,
  emitWatchAllVolumeChange,
  emitWatchAllPopOut,
  emitShareToggleMute,
  emitShareToggleDeafen,
  emitShareVoiceVolumeChange,
} from './share-window-bridge';
import { emitFocusMainWindow } from '@shared/window-bridge';

/* ─── Constants ─────────────────────────────────────────────────── */

const TITLE_BAR_HEIGHT = 32;
const GLOBAL_BAR_FADE_DELAY_MS = 5000;

/* ─── Types ─────────────────────────────────────────────────────── */

interface ShareUserState {
  isMuted: boolean;
  isDeafened: boolean;
}

type MixerPanel = 'voice' | 'share';

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

/* ═══ Component ═════════════════════════════════════════════════════ */

export default function WatchAllPage() {
  const params = useRef(parseHashParams());
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

  const { tiles, setTiles, audioTiles, setAudioTiles } = useWatchAllTiles({
    diagnosticsTestSessionId,
    isDiagnosticsTestMode,
  });

  useEffect(() => {
    if (isDiagnosticsTestMode) return;
    return onShareUserState((state) => {
      setUserState({
        isMuted: Boolean(state.isMuted),
        isDeafened: Boolean(state.isDeafened),
      });
    });
  }, [isDiagnosticsTestMode]);

  useEffect(() => {
    if (isDiagnosticsTestMode) return;
    return onWatchAllVoiceParticipantsUpdate((payload) => {
      setVoiceParticipants(payload.participants);
    });
  }, [isDiagnosticsTestMode]);

  /* ── Window close / self-close listeners ── */

  // Listen for close command from ActiveRoom (room leave)
  useEffect(() => {
    if (isDiagnosticsTestMode) return;
    return onWatchAllCloseCommand(() => {
      void closeCurrentChildWindow();
    });
  }, [isDiagnosticsTestMode]);

  // Defense-in-depth: self-close when voice session ends
  useEffect(() => {
    if (isDiagnosticsTestMode) return;
    return onVoiceSessionEnded(() => {
      void closeCurrentChildWindow();
    });
  }, [isDiagnosticsTestMode]);

  // Notify ActiveRoom when this window closes
  useEffect(() => {
    if (isDiagnosticsTestMode) return;
    return onCurrentChildWindowCloseRequested(async () => {
      await emitWatchAllClosed();
    });
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

  const handleToggleMute = useCallback(
    (participantId: string) => {
      // Try video tiles first, then audio-only tiles
      let handled = false;
      setTiles((prev) => {
        const next = prev.map((t) => {
          if (t.participantId !== participantId) return t;
          handled = true;
          const nextMuted = !t.muted;
          emitWatchAllMuteChange(participantId, nextMuted);
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
            emitWatchAllMuteChange(participantId, nextMuted);
            return { ...t, muted: nextMuted };
          }),
        );
      }
    },
    [setTiles, setAudioTiles],
  );

  const handleVolumeChange = useCallback(
    (participantId: string, volume: number) => {
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
      emitWatchAllVolumeChange(participantId, volume);
    },
    [setTiles, setAudioTiles],
  );

  const handleAspectRatioDetected = useCallback(
    (participantId: string, ratio: number) => {
      setTiles((prev) =>
        prev.map((t) =>
          t.participantId === participantId && Math.abs(t.aspectRatio - ratio) > 0.01
            ? { ...t, aspectRatio: ratio }
            : t,
        ),
      );
    },
    [setTiles],
  );

  const handleVoiceVolumeChange = useCallback((participantId: string, volume: number) => {
    setVoiceParticipants((prev) =>
      prev.map((participant) =>
        participant.id === participantId
          ? { ...participant, volume, muted: volume === 0 }
          : participant,
      ),
    );
    emitShareVoiceVolumeChange(participantId, volume);
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
        emitShareVoiceVolumeChange(participantId, nextVolume);
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
    emitWatchAllPopOut({ participantId, volume, muted });
  }, []);

  /* ── Close button ── */

  const handleClose = useCallback(() => {
    void closeCurrentChildWindow();
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

  /* ── Focus main window (debug-logged) ── */

  const handleFocusMain = useCallback(() => {
    if (DEBUG_SHARE_VIEW) console.log(LOG, 'focus-main button clicked in watch-all');
    emitFocusMainWindow()
      .then(() => {
        if (DEBUG_SHARE_VIEW) console.log(LOG, 'focus-main-window emit resolved');
      })
      .catch((e: unknown) => console.error(LOG, 'focus-main-window emit failed', e));
  }, []);

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
          <FocusMainButton onClick={handleFocusMain} />
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
              emitShareToggleMute();
            }}
            onToggleDeafen={() => {
              emitShareToggleDeafen();
            }}
          />
          <span className="text-wavis-text-secondary opacity-30 select-none leading-none px-0.5">
            │
          </span>
          <FocusMainButton onClick={handleFocusMain} />
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
