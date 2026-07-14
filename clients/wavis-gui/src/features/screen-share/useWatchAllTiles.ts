import { useEffect, useRef, useState } from 'react';
import {
  onWatchAllAudioShareAdded,
  onWatchAllAudioShareRemoved,
  onWatchAllRestoreVolume,
  onWatchAllShareAdded,
  onWatchAllShareRemoved,
  onWatchAllShareUpdated,
  emitWatchAllReady,
} from './share-window-bridge';
import { onWatchAllTestState, emitWatchAllTestReady } from './watch-all-test-mode';
import { DEBUG_SHARE_VIEW, LOG } from './watch-all-constants';

export interface ShareTileState {
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

export interface AudioTileState {
  participantId: string;
  displayName: string;
  color: string;
  muted: boolean;
  volume: number;
}

interface UseWatchAllTilesOptions {
  diagnosticsTestSessionId: string | null;
  isDiagnosticsTestMode: boolean;
}

/**
 * Owns the Watch All grid's tile state (video + audio-only) and the
 * event wiring that feeds it: either the diagnostics test-mode state pushes
 * from DiagnosticsPage, or the live watch-all:* channel from the main window.
 */
export function useWatchAllTiles({
  diagnosticsTestSessionId,
  isDiagnosticsTestMode,
}: UseWatchAllTilesOptions) {
  const [tiles, setTiles] = useState<ShareTileState[]>([]);
  const [audioTiles, setAudioTiles] = useState<AudioTileState[]>([]);
  const pendingRestoreVolumesRef = useRef<Map<string, { volume: number; muted: boolean }>>(
    new Map(),
  );

  useEffect(() => {
    if (isDiagnosticsTestMode) {
      let cleanup: (() => void) | null = null;
      let mounted = true;

      void onWatchAllTestState((state) => {
        if (state.sessionId !== diagnosticsTestSessionId) return;
        setTiles(
          state.tiles.map((tile) => ({
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
        if (diagnosticsTestSessionId) {
          emitWatchAllTestReady(diagnosticsTestSessionId);
        }
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
      const unlistenAdded = await onWatchAllShareAdded((payload) => {
        if (DEBUG_SHARE_VIEW)
          console.log(LOG, 'share-added received:', payload.participantId, payload.displayName);
        const { participantId, liveKitIdentity, displayName, color, canvasFallback } = payload;
        setTiles((prev) => {
          if (prev.some((t) => t.participantId === participantId)) return prev;
          const restored = pendingRestoreVolumesRef.current.get(participantId);
          if (restored !== undefined) {
            pendingRestoreVolumesRef.current.delete(participantId);
          }
          const baseTile: ShareTileState = {
            participantId,
            liveKitIdentity,
            displayName,
            color,
            kind: 'live',
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

      const unlistenRemoved = await onWatchAllShareRemoved((participantId) => {
        setTiles((prev) => prev.filter((t) => t.participantId !== participantId));
      });

      const unlistenUpdated = await onWatchAllShareUpdated(
        ({ participantId, displayName, color }) => {
          setTiles((prev) =>
            prev.map((t) => (t.participantId === participantId ? { ...t, displayName, color } : t)),
          );
        },
      );

      const unlistenRestoreVolume = await onWatchAllRestoreVolume(
        ({ participantId, volume, muted }) => {
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
        },
      );

      const unlistenAudioAdded = await onWatchAllAudioShareAdded(
        ({ participantId, displayName, color, volume, muted }) => {
          setAudioTiles((prev) => {
            if (prev.some((t) => t.participantId === participantId)) return prev;
            return [...prev, { participantId, displayName, color, muted, volume }];
          });
        },
      );

      const unlistenAudioRemoved = await onWatchAllAudioShareRemoved((participantId) => {
        setAudioTiles((prev) => prev.filter((t) => t.participantId !== participantId));
      });

      // All listeners registered — signal readiness to ActiveRoom
      if (DEBUG_SHARE_VIEW) console.log(LOG, 'emitting watch-all:ready');
      emitWatchAllReady();

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

  return { tiles, setTiles, audioTiles, setAudioTiles };
}
