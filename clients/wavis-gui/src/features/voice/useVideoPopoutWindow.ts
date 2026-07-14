/**
 * Camera video pop-out window.
 *
 * Owns the camera-popout child window: open/close lifecycle, the ready
 * handshake (listener registered before the window is constructed), and
 * diffing the camera tile grid into camera-popout:tile-* events plus
 * per-tile track streaming via screen-share-viewer senders.
 */

import { useCallback, useEffect, useRef, useState } from 'react';
import {
  listenCameraPopoutReady,
  onCameraPopoutClosed,
  openCameraPopoutWindow,
  emitCameraPopoutTileAdded,
  emitCameraPopoutTileRemoved,
  emitCameraPopoutTileUpdated,
  type ChildWindowHandle,
} from '@features/screen-share/share-window-bridge';
import {
  startSending,
  stopSending,
  stopSendingForWindow,
  resendStream,
} from '@features/screen-share/screen-share-viewer';
import { liveKitIdentityForParticipant, selectRoomPanelTab } from './voice-room';
import type { VideoTileSnapshot, VideoTileViewModel } from './camera-types';

const POPOUT_WINDOW_LABEL = 'camera-popout';

export function buildVideoTileSnapshot(tile: VideoTileViewModel): VideoTileSnapshot {
  return {
    participantId: tile.participantId,
    liveKitIdentity: liveKitIdentityForParticipant(tile.participantId),
    displayName: tile.displayName,
    color: tile.color,
    hasTrack: tile.track !== null,
    isSelf: tile.isSelf,
    isMuted: tile.isMuted,
    hasError: tile.hasError,
  };
}

export function areVideoTileSnapshotsEqual(a: VideoTileSnapshot, b: VideoTileSnapshot): boolean {
  return (
    a.participantId === b.participantId &&
    a.liveKitIdentity === b.liveKitIdentity &&
    a.displayName === b.displayName &&
    a.color === b.color &&
    a.hasTrack === b.hasTrack &&
    a.isSelf === b.isSelf &&
    a.isMuted === b.isMuted &&
    a.hasError === b.hasError
  );
}

interface UseVideoPopoutWindowOptions {
  /** Current tile grid — drives the change-sync effect. */
  videoTilesById: Record<string, VideoTileViewModel> | undefined;
  /** Fresh room snapshot for callbacks that outlive a render. */
  getRoomSnapshot: () => {
    channelName: string;
    videoTilesById: Record<string, VideoTileViewModel>;
  } | null;
}

export function useVideoPopoutWindow({
  videoTilesById,
  getRoomSnapshot,
}: UseVideoPopoutWindowOptions): {
  videoPopoutOpen: boolean;
  openVideoPopoutWindow: () => Promise<void>;
  closeVideoPopoutWindow: (alreadyDestroyed?: boolean) => void;
} {
  const windowRef = useRef<ChildWindowHandle | null>(null);
  const readyUnlistenRef = useRef<(() => void) | null>(null);
  const [videoPopoutOpen, setVideoPopoutOpen] = useState(false);
  const readyRef = useRef(false);
  const prevTracksRef = useRef<Map<string, MediaStreamTrack | null>>(new Map());
  const prevTilesRef = useRef<Map<string, VideoTileSnapshot>>(new Map());

  const closeVideoPopoutWindow = (alreadyDestroyed = false) => {
    if (!windowRef.current) return;
    if (readyUnlistenRef.current) {
      readyUnlistenRef.current();
      readyUnlistenRef.current = null;
    }
    readyRef.current = false;
    stopSendingForWindow(POPOUT_WINDOW_LABEL);
    if (!alreadyDestroyed) {
      windowRef.current.close().catch(() => {});
    }
    windowRef.current = null;
    setVideoPopoutOpen(false);
    prevTracksRef.current = new Map();
    prevTilesRef.current = new Map();
  };
  const closeRef = useRef(closeVideoPopoutWindow);
  closeRef.current = closeVideoPopoutWindow;

  const syncVideoPopoutSnapshot = useCallback(
    (nextTilesById: Record<string, VideoTileViewModel>) => {
      const prevTracks = prevTracksRef.current;
      const prevTiles = prevTilesRef.current;
      const nextTracks = new Map<string, MediaStreamTrack | null>();
      const nextTiles = new Map<string, VideoTileSnapshot>();

      for (const tile of Object.values(nextTilesById)) {
        const snapshot = buildVideoTileSnapshot(tile);
        const sendableTrack =
          snapshot.hasTrack && !snapshot.isMuted && !snapshot.hasError ? tile.track : null;
        nextTracks.set(tile.participantId, sendableTrack);
        nextTiles.set(tile.participantId, snapshot);

        const prevSnapshot = prevTiles.get(tile.participantId);
        if (!prevSnapshot) {
          emitCameraPopoutTileAdded(snapshot);
        } else if (!areVideoTileSnapshotsEqual(prevSnapshot, snapshot)) {
          emitCameraPopoutTileUpdated(snapshot);
        }

        const prevTrack = prevTracks.get(tile.participantId) ?? null;
        if (sendableTrack && !prevTrack) {
          void startSending(
            tile.participantId,
            POPOUT_WINDOW_LABEL,
            new MediaStream([sendableTrack]),
          );
        } else if (sendableTrack && prevTrack !== sendableTrack) {
          void resendStream(
            tile.participantId,
            POPOUT_WINDOW_LABEL,
            new MediaStream([sendableTrack]),
          );
        } else if (!sendableTrack && prevTrack) {
          stopSending(tile.participantId, POPOUT_WINDOW_LABEL);
        }
      }

      for (const participantId of prevTiles.keys()) {
        if (!nextTiles.has(participantId)) {
          stopSending(participantId, POPOUT_WINDOW_LABEL);
          emitCameraPopoutTileRemoved(participantId);
        }
      }

      prevTracksRef.current = nextTracks;
      prevTilesRef.current = nextTiles;
    },
    [],
  );

  const openVideoPopoutWindow = useCallback(async () => {
    if (windowRef.current) {
      windowRef.current.setFocus().catch(() => {});
      return;
    }

    const snapshot = getRoomSnapshot();
    if (!snapshot) return;

    try {
      readyRef.current = false;
      const unlistenReady = await listenCameraPopoutReady(() => {
        if (readyRef.current) return;
        readyRef.current = true;
        syncVideoPopoutSnapshot(getRoomSnapshot()?.videoTilesById ?? {});
      });
      readyUnlistenRef.current = unlistenReady;

      const win = openCameraPopoutWindow(snapshot.channelName);

      win.onceError((event) => {
        console.error('[wavis:active-room] video popout window error:', event);
        closeRef.current(true);
      });

      win.onceDestroyed(() => {
        closeRef.current(true);
      });

      windowRef.current = win;
      setVideoPopoutOpen(true);
      // Switch away from the video tab — it's now in the pop-out window
      selectRoomPanelTab('logs');
    } catch (err) {
      console.error('[wavis:active-room] failed to open video popout window:', err);
    }
  }, [getRoomSnapshot, syncVideoPopoutSnapshot]);

  // Child page closed itself (or was destroyed) — tear down our side.
  useEffect(() => {
    return onCameraPopoutClosed(() => closeRef.current(true));
  }, []);

  // Push tile-grid changes into the open pop-out.
  useEffect(() => {
    if (!videoPopoutOpen) {
      prevTracksRef.current = new Map();
      prevTilesRef.current = new Map();
      return;
    }
    if (!readyRef.current) return;
    syncVideoPopoutSnapshot(videoTilesById ?? {});
  }, [videoTilesById, syncVideoPopoutSnapshot, videoPopoutOpen]);

  return { videoPopoutOpen, openVideoPopoutWindow, closeVideoPopoutWindow };
}
