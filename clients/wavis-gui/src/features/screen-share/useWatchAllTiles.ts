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
import {
  addShareTile,
  removeShareTile,
  updateShareTile,
  applyRestoreVolumeToTiles,
  applyRestoreVolumeToAudioTiles,
  addAudioTile,
  removeAudioTile,
  type PendingRestoreVolumes,
} from './watch-all-tiles-model';

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
  const pendingRestoreVolumesRef = useRef<PendingRestoreVolumes>(new Map());

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
        setTiles((prev) => {
          const result = addShareTile(prev, pendingRestoreVolumesRef.current, payload);
          pendingRestoreVolumesRef.current = result.pendingRestoreVolumes;
          return result.tiles;
        });
      });

      const unlistenRemoved = await onWatchAllShareRemoved((participantId) => {
        setTiles((prev) => removeShareTile(prev, participantId));
      });

      const unlistenUpdated = await onWatchAllShareUpdated((payload) => {
        setTiles((prev) => updateShareTile(prev, payload));
      });

      const unlistenRestoreVolume = await onWatchAllRestoreVolume((payload) => {
        setTiles((prev) => {
          const result = applyRestoreVolumeToTiles(prev, pendingRestoreVolumesRef.current, payload);
          pendingRestoreVolumesRef.current = result.pendingRestoreVolumes;
          return result.tiles;
        });
        setAudioTiles((prev) => applyRestoreVolumeToAudioTiles(prev, payload));
      });

      const unlistenAudioAdded = await onWatchAllAudioShareAdded((payload) => {
        setAudioTiles((prev) => addAudioTile(prev, payload));
      });

      const unlistenAudioRemoved = await onWatchAllAudioShareRemoved((participantId) => {
        setAudioTiles((prev) => removeAudioTile(prev, participantId));
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
