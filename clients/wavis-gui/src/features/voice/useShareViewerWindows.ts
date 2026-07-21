/**
 * Remote screen-share viewer windows.
 *
 * Owns everything about WATCHING other participants' shares: per-sharer
 * pop-out windows and the Rust-native viewer, the Watch All grid window
 * (open/close/toggle, ready handshake, dynamic share/audio-share tracking),
 * per-stream volume/mute state with persistence, audio attach/detach
 * routing, and every child→main event channel for those windows.
 *
 * Self-share concerns (starting/stopping a share, picker, quality, share
 * audio) stay in ActiveRoom — this hook only reaches them through the
 * onToggleSelfShare callback for the share:toggle-share hotkey channel.
 */

import { useCallback, useEffect, useRef, useState } from 'react';
import {
  emitShareRestoreVolume,
  emitShareUserState,
  emitShareVoiceParticipants,
  emitWatchAllAudioShareAdded,
  emitWatchAllAudioShareRemoved,
  emitWatchAllRestoreVolume,
  emitWatchAllShareAdded,
  emitWatchAllShareRemoved,
  emitWatchAllShareUpdated,
  emitWatchAllVoiceParticipants,
  listenWatchAllReady,
  onScreenShareClosed,
  onScreenShareMuteChange,
  onScreenSharePopBackIn,
  onScreenShareViewerReady,
  onScreenShareVolumeChange,
  onShareToggleDeafen,
  onShareToggleMute,
  onShareToggleShare,
  onShareVoiceVolumeChange,
  onWatchAllClosed,
  onWatchAllMuteChange,
  onWatchAllPopOut,
  onWatchAllVolumeChange,
  openNativeScreenShareViewer,
  closeNativeScreenShareViewer,
  openScreenShareViewerWindow,
  openWatchAllGridWindow,
  requestScreenShareWindowClose,
  type ChildWindowHandle,
} from '@features/screen-share/share-window-bridge';
import {
  attachScreenShareAudio,
  detachScreenShareAudio,
  setScreenShareAudioVolume,
  setParticipantVolume,
  toggleSelfMute,
  toggleSelfDeafen,
  persistStreamVolume,
  getPersistedStreamVolume,
  persistStreamMuted,
  getPersistedStreamMuted,
  liveKitIdentityForParticipant,
  type RoomParticipant,
  type VoiceRoomState,
} from './voice-room';
import {
  planOpenShareWindow,
  planOpenWatchAllWindow,
  decideWatchAllOpenWhenClosed,
  decideWatchAllToggleWhenOpen,
  shouldCloseOnSubRoomChange,
  planAudioOnlySharerBaselineSeed,
} from './share-viewer-windows-model';

export type ShareViewerScope = 'direct' | 'watch-all';

interface ShareViewerWindow {
  scope: ShareViewerScope;
  window: ChildWindowHandle;
}

export interface ShareUserState {
  isMuted: boolean;
  isDeafened: boolean;
  isSharing: boolean;
  shareEnabled: boolean;
}

export interface WatchAllScope {
  participantIds: Set<string>;
  participants: RoomParticipant[];
  remoteSharers: RoomParticipant[];
  streams: Map<string, MediaStream | null>;
}

interface BridgedVoiceParticipant {
  id: string;
  name: string;
  color: string;
  volume: number;
  muted: boolean;
}

interface UseShareViewerWindowsOptions {
  roomState: VoiceRoomState | null;
  /** Fresh room state for callbacks that outlive a render. */
  getRoomSnapshot: () => VoiceRoomState | null;
  /** Self mute/deafen/share flags mirrored to child windows. */
  shareUserState: ShareUserState;
  /** share:toggle-share hotkey — start or stop the local share. */
  onToggleSelfShare: () => void;
  /** Surface a transient error toast in the room UI. */
  onError: (message: string) => void;
  /** Tear down the camera pop-out (part of closeAllShareWindows). */
  closeVideoPopoutWindow: () => void;
}

export function useShareViewerWindows({
  roomState,
  getRoomSnapshot,
  shareUserState,
  onToggleSelfShare,
  onError,
  closeVideoPopoutWindow,
}: UseShareViewerWindowsOptions) {
  const [watchingShareIds, setWatchingShareIds] = useState<Set<string>>(new Set());
  const [shareVolumes, setShareVolumes] = useState<Map<string, number>>(new Map());
  const shareVolumesRef = useRef(shareVolumes);
  const watchAllVolumesRef = useRef<Map<string, number>>(new Map());
  const [shareMuted, setShareMuted] = useState<Map<string, boolean>>(new Map());
  const shareMutedRef = useRef(shareMuted);
  const watchAllAttachedAudioRef = useRef<Set<string>>(new Set());
  // Refs to the screen share OS windows (keyed by participantId)
  const shareWindowsRef = useRef<Map<string, ShareViewerWindow>>(new Map());
  /** participantId → LiveKit identity used when the native viewer was opened,
   *  so close targets the same Rust-side frame key even if the mapping
   *  changes while the viewer is open. */
  const nativeShareViewersRef = useRef<Map<string, string>>(new Map());
  const watchAllWindowRef = useRef<ChildWindowHandle | null>(null);
  const watchAllReadyUnlistenRef = useRef<(() => void) | null>(null);
  const [watchAllOpen, setWatchAllOpen] = useState(false);
  const watchAllReadyRef = useRef(false);
  const prevWatchAllStreamsRef = useRef<Map<string, MediaStream | null>>(new Map());
  const prevAudioOnlySharersRef = useRef<Set<string>>(new Set());
  // See planAudioOnlySharerBaselineSeed's doc comment for why this baseline
  // can't just be seeded at mount.
  const hasSeededAudioOnlySharersRef = useRef(false);

  // Mirrors pushed to child windows; assigned during render so the values
  // are always fresh when a viewer-ready callback fires.
  const shareUserStateRef = useRef<ShareUserState>(shareUserState);
  const voiceParticipantsRef = useRef<{ participants: BridgedVoiceParticipant[] }>({
    participants: [],
  });
  const watchAllVoiceParticipantsRef = useRef<{ participants: BridgedVoiceParticipant[] }>({
    participants: [],
  });
  const onToggleSelfShareRef = useRef(onToggleSelfShare);
  onToggleSelfShareRef.current = onToggleSelfShare;
  const onErrorRef = useRef(onError);
  onErrorRef.current = onError;
  const closeVideoPopoutRef = useRef(closeVideoPopoutWindow);
  closeVideoPopoutRef.current = closeVideoPopoutWindow;
  // getRoomSnapshot is passed as a fresh inline arrow every render (see
  // ActiveRoom.tsx) — mirror it so window-lifecycle callbacks can read fresh
  // state without needing an unstable function in their dependency arrays.
  const getRoomSnapshotRef = useRef(getRoomSnapshot);
  getRoomSnapshotRef.current = getRoomSnapshot;

  const getWatchAllScope = useCallback((currentState: VoiceRoomState | null): WatchAllScope => {
    if (!currentState || !currentState.joinedSubRoomId) {
      return {
        participantIds: new Set<string>(),
        participants: [] as RoomParticipant[],
        remoteSharers: [] as RoomParticipant[],
        streams: new Map<string, MediaStream | null>(),
      };
    }

    const scopedSubRoomIds = new Set<string>([currentState.joinedSubRoomId]);
    const passthrough = currentState.passthrough;
    if (passthrough?.sourceSubRoomId === currentState.joinedSubRoomId) {
      scopedSubRoomIds.add(passthrough.targetSubRoomId);
    } else if (passthrough?.targetSubRoomId === currentState.joinedSubRoomId) {
      scopedSubRoomIds.add(passthrough.sourceSubRoomId);
    }

    const participantIds = new Set<string>();
    for (const subRoom of currentState.subRooms) {
      if (!scopedSubRoomIds.has(subRoom.id)) continue;
      for (const participantId of subRoom.participantIds) {
        participantIds.add(participantId);
      }
    }
    const participants = currentState.participants.filter((participant) =>
      participantIds.has(participant.id),
    );
    const remoteSharers = participants.filter(
      (participant) => participant.isSharing && participant.id !== currentState.selfParticipantId,
    );
    const streams = new Map(
      [...currentState.screenShareStreams].filter(([participantId]) =>
        participantIds.has(participantId),
      ),
    );

    return { participantIds, participants, remoteSharers, streams };
  }, []);

  // Render-time refresh of the child-window mirrors.
  shareUserStateRef.current = shareUserState;
  voiceParticipantsRef.current = {
    participants:
      roomState?.participants
        .filter((participant) => participant.id !== roomState.selfParticipantId)
        .map((participant) => ({
          id: participant.id,
          name: participant.displayName,
          color: participant.color,
          volume: participant.volume,
          muted: participant.volume === 0,
        })) ?? [],
  };
  watchAllVoiceParticipantsRef.current = {
    participants: getWatchAllScope(roomState)
      .participants.filter((participant) => participant.id !== roomState?.selfParticipantId)
      .map((participant) => ({
        id: participant.id,
        name: participant.displayName,
        color: participant.color,
        volume: participant.volume,
        muted: participant.volume === 0,
      })),
  };

  const getSavedShareVolume = useCallback((participantId: string) => {
    // watchAllVolumesRef is updated synchronously by syncScreenShareVolume;
    // shareVolumesRef lags by one render cycle (useEffect). Prefer the sync ref.
    return (
      watchAllVolumesRef.current.get(participantId) ??
      shareVolumesRef.current.get(participantId) ??
      getPersistedStreamVolume(participantId) ??
      70
    );
  }, []);

  const getSavedShareMuted = useCallback(
    (participantId: string) => {
      return (
        shareMutedRef.current.get(participantId) ??
        getPersistedStreamMuted(participantId) ??
        getSavedShareVolume(participantId) === 0
      );
    },
    [getSavedShareVolume],
  );

  const applySavedScreenShareAudio = useCallback(
    (participantId: string) => {
      const volume = getSavedShareVolume(participantId);
      setScreenShareAudioVolume(participantId, getSavedShareMuted(participantId) ? 0 : volume);
    },
    [getSavedShareMuted, getSavedShareVolume],
  );

  const syncScreenShareMuted = useCallback(
    (participantId: string, muted: boolean) => {
      const savedVolume = getSavedShareVolume(participantId);
      const restoredVolume = !muted && savedVolume === 0 ? 70 : savedVolume;
      if (restoredVolume !== savedVolume) {
        watchAllVolumesRef.current.set(participantId, restoredVolume);
        setShareVolumes((prev) => {
          const next = new Map(prev);
          next.set(participantId, restoredVolume);
          return next;
        });
        persistStreamVolume(participantId, restoredVolume);
      }
      shareMutedRef.current.set(participantId, muted);
      setShareMuted((prev) => {
        if (prev.get(participantId) === muted) return prev;
        const next = new Map(prev);
        next.set(participantId, muted);
        return next;
      });
      persistStreamMuted(participantId, muted);
      if (muted) {
        detachScreenShareAudio(participantId);
      } else {
        attachScreenShareAudio(participantId);
      }
      setScreenShareAudioVolume(participantId, muted ? 0 : restoredVolume);
      emitShareRestoreVolume({ participantId, volume: restoredVolume, muted });
    },
    [getSavedShareVolume],
  );

  const syncScreenShareVolume = useCallback((participantId: string, volume: number) => {
    setShareVolumes((prev) => {
      if (prev.get(participantId) === volume) return prev;
      const next = new Map(prev);
      next.set(participantId, volume);
      return next;
    });
    watchAllVolumesRef.current.set(participantId, volume);
    const muted = volume === 0;
    shareMutedRef.current.set(participantId, muted);
    setShareMuted((prev) => {
      if (prev.get(participantId) === muted) return prev;
      const next = new Map(prev);
      next.set(participantId, muted);
      return next;
    });
    if (muted) {
      detachScreenShareAudio(participantId);
    } else {
      attachScreenShareAudio(participantId);
    }
    setScreenShareAudioVolume(participantId, muted ? 0 : volume);
    persistStreamVolume(participantId, volume);
    persistStreamMuted(participantId, muted);
    emitShareRestoreVolume({ participantId, volume, muted });
  }, []);

  const restoreWatchAllVolumeForParticipant = useCallback(
    (participantId: string) => {
      emitWatchAllRestoreVolume({
        participantId,
        volume: getSavedShareVolume(participantId),
        muted: getSavedShareMuted(participantId),
      });
    },
    [getSavedShareMuted, getSavedShareVolume],
  );

  const handleViewerReady = useCallback(
    (participantId: string, windowLabel: string) => {
      const rs = getRoomSnapshotRef.current();
      if (!rs || !rs.screenShareStreams.has(participantId)) return;

      if (windowLabel === 'watch-all') {
        if (!watchAllWindowRef.current || !watchAllReadyRef.current) {
          console.log(
            '[wavis:active-room] handleViewerReady: watch-all skipped — window:',
            !!watchAllWindowRef.current,
            'ready:',
            watchAllReadyRef.current,
          );
          return;
        }
        if (shareWindowsRef.current.has(participantId)) {
          console.log(
            '[wavis:active-room] handleViewerReady: watch-all skipped — pop-out window exists for',
            participantId,
          );
          return;
        }
        console.log(
          '[wavis:active-room] handleViewerReady: attaching watch-all audio for',
          participantId,
        );
        attachScreenShareAudio(participantId);
        applySavedScreenShareAudio(participantId);
        watchAllAttachedAudioRef.current.add(participantId);
        emitShareUserState(shareUserStateRef.current);
        emitWatchAllVoiceParticipants(watchAllVoiceParticipantsRef.current);
        return;
      }

      const shareWindow = shareWindowsRef.current.get(participantId);
      if (!shareWindow || shareWindow.window.label !== windowLabel) return;
      attachScreenShareAudio(participantId);
      applySavedScreenShareAudio(participantId);
      emitShareUserState(shareUserStateRef.current);
      emitShareVoiceParticipants(voiceParticipantsRef.current);
    },
    [applySavedScreenShareAudio],
  );

  /** Re-add a participant's stream to the Watch All grid after their pop-out closes. */
  const reAddStreamToWatchAll = useCallback(
    (participantId: string) => {
      if (!watchAllWindowRef.current || !watchAllReadyRef.current) return;
      const rs = getRoomSnapshotRef.current();
      const scope = getWatchAllScope(rs);
      if (!scope.streams.has(participantId)) return;
      const stream = scope.streams.get(participantId) ?? null;
      const participant = scope.participants.find((p) => p.id === participantId);
      if (!participant) return;
      emitWatchAllShareAdded({
        participantId,
        liveKitIdentity: liveKitIdentityForParticipant(participantId),
        displayName: participant.displayName,
        color: participant.color,
        canvasFallback: stream === null,
      });
      restoreWatchAllVolumeForParticipant(participantId);
      prevWatchAllStreamsRef.current.set(participantId, stream);
      // Attach audio directly. If the tile already exists in Watch All,
      // share-added is a no-op and viewer-ready never fires, so audio would be
      // left unattached. This covers both the new-tile path (idempotent with
      // the viewer-ready attach) and the existing-tile path.
      if (!shareWindowsRef.current.has(participantId)) {
        attachScreenShareAudio(participantId);
        applySavedScreenShareAudio(participantId);
        watchAllAttachedAudioRef.current.add(participantId);
      }
    },
    [getWatchAllScope, restoreWatchAllVolumeForParticipant, applySavedScreenShareAudio],
  );

  const handleShareWindowClosed = useCallback(
    (participantId: string) => {
      // The map entry marks which pop-out currently owns this participant.
      // If another close path already removed it, skip duplicate cleanup.
      if (!shareWindowsRef.current.delete(participantId)) return;
      // Skip detach when Watch All will take the stream back: detaching triggers
      // setSubscribed(false/true) which can race TrackUnsubscribed/TrackSubscribed
      // unpredictably. Keeping the gain node alive lets reAddStreamToWatchAll
      // update volume immediately without any subscription churn.
      if (!watchAllWindowRef.current || !watchAllReadyRef.current) {
        detachScreenShareAudio(participantId);
      }
      setWatchingShareIds((prev) => {
        const next = new Set(prev);
        next.delete(participantId);
        return next;
      });
      reAddStreamToWatchAll(participantId);
    },
    [reAddStreamToWatchAll],
  );

  /** Close the Watch All window and clean up audio routing. */
  const closeWatchAllWindow = useCallback(() => {
    if (!watchAllWindowRef.current) return; // idempotent
    // Clean up the ready listener to avoid leaks
    if (watchAllReadyUnlistenRef.current) {
      watchAllReadyUnlistenRef.current();
      watchAllReadyUnlistenRef.current = null;
    }
    watchAllReadyRef.current = false;
    for (const participantId of watchAllAttachedAudioRef.current) {
      detachScreenShareAudio(participantId);
    }
    watchAllAttachedAudioRef.current.clear();
    watchAllWindowRef.current.close().catch(() => {});
    watchAllWindowRef.current = null;
    setWatchAllOpen(false);
  }, []);

  /** Close a specific screen share OS window and clean up audio routing. */
  const closeShareWindow = useCallback(
    (participantId: string) => {
      if (!watchAllWindowRef.current || !watchAllReadyRef.current) {
        detachScreenShareAudio(participantId);
      }
      const openedNativeIdentity = nativeShareViewersRef.current.get(participantId);
      if (openedNativeIdentity !== undefined) {
        nativeShareViewersRef.current.delete(participantId);
        closeNativeScreenShareViewer(openedNativeIdentity);
      }
      const shareWindow = shareWindowsRef.current.get(participantId);
      if (shareWindow) {
        // Delete BEFORE win.close() so the screen-share:closed handler
        // sees delete() return false and skips its re-add (no double-fire).
        shareWindowsRef.current.delete(participantId);
        shareWindow.window.close().catch(() => {});
      }
      setWatchingShareIds((prev) => {
        const next = new Set(prev);
        next.delete(participantId);
        return next;
      });
      reAddStreamToWatchAll(participantId);
    },
    [reAddStreamToWatchAll],
  );

  /** Close all share windows. */
  const closeAllShareWindows = useCallback(() => {
    closeVideoPopoutRef.current(); // tears down the camera-popout bridge senders
    closeWatchAllWindow(); // close Watch All window first
    for (const [pid, nativeIdentity] of nativeShareViewersRef.current) {
      detachScreenShareAudio(pid);
      closeNativeScreenShareViewer(nativeIdentity);
    }
    nativeShareViewersRef.current.clear();
    for (const [pid, shareWindow] of shareWindowsRef.current) {
      detachScreenShareAudio(pid);
      shareWindow.window.close().catch(() => {});
    }
    shareWindowsRef.current.clear();
    setWatchingShareIds(new Set());
  }, [closeWatchAllWindow]);

  /** Open a real OS window for a screen share viewer. Supports multiple simultaneous windows. */
  const openShareWindow = useCallback(
    async (
      participantId: string,
      participant: RoomParticipant,
      stream: MediaStream | null,
      scope: ShareViewerScope = 'direct',
    ) => {
      const openPlan = planOpenShareWindow(
        { natives: nativeShareViewersRef.current, popouts: shareWindowsRef.current },
        participantId,
      );

      if (openPlan.closeExistingNative) {
        closeShareWindow(participantId);
      }

      // If already watching this participant, close it first and wait for Tauri to
      // destroy the webview before creating a new one with the same label.
      if (openPlan.closeExistingPopout) {
        const oldWin = shareWindowsRef.current.get(participantId)!;
        closeShareWindow(participantId);
        await oldWin.window.waitForDestroyed(1000);
      }

      const rs = getRoomSnapshotRef.current();
      const isSelf = participantId === rs?.selfParticipantId;
      const params = {
        participantId,
        liveKitIdentity: liveKitIdentityForParticipant(participantId),
        username: participant.displayName,
        userColor: participant.color,
        isOwner: isSelf,
        canvasFallback: stream === null,
        initialVolume: getSavedShareVolume(participantId),
        initialMuted: getSavedShareMuted(participantId),
      };

      try {
        if (stream === null) {
          // The Rust side keys share frames by LiveKit identity (the durable
          // userId on new backends), not by the signaling participantId.
          const nativeIdentity = liveKitIdentityForParticipant(participantId);
          await openNativeScreenShareViewer(
            nativeIdentity,
            `${participant.displayName} — screen share`,
          );

          nativeShareViewersRef.current.set(participantId, nativeIdentity);
          attachScreenShareAudio(participantId);
          applySavedScreenShareAudio(participantId);
          setWatchingShareIds((prev) => new Set(prev).add(participantId));

          if (watchAllWindowRef.current && watchAllReadyRef.current) {
            watchAllAttachedAudioRef.current.delete(participantId);
            prevWatchAllStreamsRef.current.delete(participantId);
            emitWatchAllShareRemoved(participantId);
          }
          return;
        }

        const win = openScreenShareViewerWindow(
          params,
          `${participant.displayName} — screen share`,
        );

        // The pop-out subscribes to the share directly via its own LiveKit
        // viewer connection (see viewer-connection.ts) — no loopback bridge.
        win.onceError((e) => {
          console.error('[wavis:active-room] screen share window error:', e);
          setWatchingShareIds((prev) => {
            const next = new Set(prev);
            next.delete(participantId);
            return next;
          });
        });

        // Defense-in-depth: restore the tile even if the page-level close event
        // is missed and only the native window destruction fires.
        win.onceDestroyed(() => {
          handleShareWindowClosed(participantId);
        });

        shareWindowsRef.current.set(participantId, { window: win, scope });
        setWatchingShareIds((prev) => new Set(prev).add(participantId));

        // If Watch All is open, remove this tile from the grid — the pop-out owns it now.
        // Do NOT detach audio here: the gain node and subscription remain alive so the
        // pop-out's handleViewerReady finds them intact and only needs to set the volume.
        // closeShareWindow handles detach when the pop-out is actually closed.
        if (watchAllWindowRef.current && watchAllReadyRef.current) {
          watchAllAttachedAudioRef.current.delete(participantId);
          prevWatchAllStreamsRef.current.delete(participantId);
          emitWatchAllShareRemoved(participantId);
        }
      } catch (err) {
        console.error('[wavis:active-room] failed to open screen share window:', err);
        onErrorRef.current(err instanceof Error ? err.message : String(err));
      }
    },
    [
      closeShareWindow,
      handleShareWindowClosed,
      getSavedShareVolume,
      getSavedShareMuted,
      applySavedScreenShareAudio,
    ],
  );

  /** Open the Watch All window showing all active screen shares in a grid. */
  const openWatchAllWindow = useCallback(async () => {
    // If already open, bring to foreground (reuse rather than recreate)
    if (planOpenWatchAllWindow(!!watchAllWindowRef.current) === 'focus-existing') {
      void watchAllWindowRef.current!.setFocus();
      return;
    }

    const initialSnapshot = getRoomSnapshotRef.current();
    if (!initialSnapshot) return;

    // Close any existing individual pop-out windows — WatchAll subsumes them.
    // We close the windows but don't detach audio (WatchAll doesn't handle
    // per-stream audio — the main window's audio attachment is independent).
    for (const [pid, shareWindow] of [...shareWindowsRef.current.entries()]) {
      detachScreenShareAudio(pid);
      shareWindow.window.close().catch(() => {});
    }
    shareWindowsRef.current.clear();
    setWatchingShareIds(new Set());

    try {
      // Await the ready listener registration so it's guaranteed to be
      // active before the child window can emit watch-all:ready.
      // Previous bug: listen() returns a Promise — calling it without
      // await meant the listener wasn't registered yet when the child
      // window mounted and emitted the ready event.
      watchAllReadyRef.current = false;
      const unlistenReady = await listenWatchAllReady(() => {
        console.log(
          '[wavis:active-room] watch-all:ready received, readyRef was:',
          watchAllReadyRef.current,
        );
        if (watchAllReadyRef.current) return; // idempotent
        watchAllReadyRef.current = true;
        void watchAllWindowRef.current?.setFocus();
        // Read fresh roomState via ref — the closure captured at
        // openWatchAllWindow time may be stale by now.
        const rs = getRoomSnapshotRef.current();
        if (!rs) {
          console.warn('[wavis:active-room] watch-all:ready fired but roomStateRef is null');
          return;
        }
        const scope = getWatchAllScope(rs);
        console.log(
          '[wavis:active-room] watch-all:ready: screenShareStreams size =',
          scope.streams.size,
        );
        for (const [pid, stream] of scope.streams) {
          const participant = scope.participants.find((p) => p.id === pid);
          if (participant) {
            emitWatchAllShareAdded({
              participantId: pid,
              liveKitIdentity: liveKitIdentityForParticipant(pid),
              displayName: participant.displayName,
              color: participant.color,
              canvasFallback: stream === null,
            });
            restoreWatchAllVolumeForParticipant(pid);
          }
        }
        // Seed the dynamic tracking ref so the useEffect doesn't
        // re-emit these same shares as "new".
        prevWatchAllStreamsRef.current = new Map(scope.streams);
        // Seed audio-only sharers into Watch All — always start muted
        for (const identity of rs.audioOnlySharers) {
          const participant = scope.participants.find((p) => p.id === identity);
          if (!participant) continue;
          const vol = getSavedShareVolume(identity);
          shareMutedRef.current.set(identity, true);
          setShareMuted((prev) => {
            if (prev.get(identity) === true) return prev;
            const next = new Map(prev);
            next.set(identity, true);
            return next;
          });
          setScreenShareAudioVolume(identity, 0);
          emitWatchAllAudioShareAdded({
            participantId: identity,
            displayName: participant.displayName,
            color: participant.color,
            volume: vol,
            muted: true,
          });
        }
        prevAudioOnlySharersRef.current = new Set(rs.audioOnlySharers);
      });
      watchAllReadyUnlistenRef.current = unlistenReady;

      const win = openWatchAllGridWindow(initialSnapshot.channelName);

      win.onceError((e) => {
        console.error('[wavis:active-room] watch-all window error:', e);
      });

      // Defense-in-depth: tauri://destroyed fires even if watch-all:closed doesn't
      win.onceDestroyed(() => {
        closeWatchAllWindow();
      });

      watchAllWindowRef.current = win;
      setWatchAllOpen(true);
    } catch (err) {
      console.error('[wavis:active-room] failed to open watch-all window:', err);
    }
  }, [
    closeWatchAllWindow,
    getWatchAllScope,
    restoreWatchAllVolumeForParticipant,
    getSavedShareVolume,
  ]);

  /** Toggle the Watch All window:
   *  - closed          → open + focus
   *  - open + visible  → close
   *  - open + minimized → restore + focus (don't close)
   */
  const toggleWatchAllWindow = useCallback(async () => {
    if (!watchAllWindowRef.current) {
      const rs = getRoomSnapshotRef.current();
      const hasShares = rs ? getWatchAllScope(rs).remoteSharers.length > 0 : false;
      if (decideWatchAllOpenWhenClosed(hasShares) === 'open') {
        void openWatchAllWindow();
      }
      return;
    }
    const minimized = await watchAllWindowRef.current.isMinimized();
    if (decideWatchAllToggleWhenOpen(minimized) === 'unminimize') {
      await watchAllWindowRef.current.unminimize();
      await watchAllWindowRef.current.setFocus();
    } else {
      closeWatchAllWindow();
    }
  }, [getWatchAllScope, openWatchAllWindow, closeWatchAllWindow]);

  /* ─── Effects ───────────────────────────────────────────────────── */

  // Close share windows when watched participants stop sharing
  useEffect(() => {
    const participants = roomState?.participants;
    if (!participants) return;
    for (const id of watchingShareIds) {
      const stillSharing = participants.some((p) => p.id === id && p.isSharing);
      if (!stillSharing) {
        closeShareWindow(id);
      }
    }
  }, [watchingShareIds, roomState?.participants, closeShareWindow]);

  useEffect(() => {
    shareVolumesRef.current = shareVolumes;
  }, [shareVolumes]);

  useEffect(() => {
    shareMutedRef.current = shareMuted;
  }, [shareMuted]);

  // Re-attach share audio when the underlying MediaStream changes for an
  // already-open viewer window (e.g. after LiveKit adaptive stream
  // pause→resume re-emits onScreenShareSubscribed with a fresh stream).
  // Video no longer flows through this window — pop-out windows hold their
  // own direct LiveKit subscription and recover on their own.
  const prevStreamsRef = useRef<Map<string, MediaStream | null>>(new Map());
  useEffect(() => {
    const screenShareStreams = roomState?.screenShareStreams;
    if (!screenShareStreams) return;
    for (const id of watchingShareIds) {
      const current = screenShareStreams.get(id) ?? null;
      const prev = prevStreamsRef.current.get(id) ?? null;
      if (current && current !== prev) {
        attachScreenShareAudio(id);
        const volume =
          watchAllVolumesRef.current.get(id) ??
          shareVolumesRef.current.get(id) ??
          getPersistedStreamVolume(id) ??
          70;
        const muted = shareMutedRef.current.get(id) ?? getPersistedStreamMuted(id) ?? volume === 0;
        setScreenShareAudioVolume(id, muted ? 0 : volume);
      }
    }
    prevStreamsRef.current = new Map(screenShareStreams);
  }, [watchingShareIds, roomState?.screenShareStreams]);

  // Listen for child windows closing themselves
  useEffect(() => {
    return onScreenShareClosed(handleShareWindowClosed);
  }, [handleShareWindowClosed]);

  // Viewer-window owner actions: volume/mute sync, voice volume, and the
  // mute/deafen/share toggles proxied from child windows.
  useEffect(() => {
    const cleanups: Array<() => void> = [];

    cleanups.push(
      onWatchAllVolumeChange((participantId, volume) => {
        syncScreenShareVolume(participantId, volume);
      }),
    );
    cleanups.push(
      onScreenShareVolumeChange((participantId, volume) => {
        syncScreenShareVolume(participantId, volume);
      }),
    );
    cleanups.push(
      onWatchAllMuteChange((participantId, muted) => {
        syncScreenShareMuted(participantId, muted);
      }),
    );
    cleanups.push(
      onScreenShareMuteChange((participantId, muted) => {
        syncScreenShareMuted(participantId, muted);
      }),
    );
    cleanups.push(
      onShareVoiceVolumeChange((participantId, volume) => {
        setParticipantVolume(participantId, volume);
      }),
    );
    cleanups.push(
      onShareToggleMute(() => {
        toggleSelfMute();
      }),
    );
    cleanups.push(
      onShareToggleDeafen(() => {
        toggleSelfDeafen();
      }),
    );
    cleanups.push(
      onShareToggleShare(() => {
        onToggleSelfShareRef.current();
      }),
    );

    return () => {
      for (const cleanup of cleanups) cleanup();
    };
  }, [syncScreenShareMuted, syncScreenShareVolume]);

  useEffect(() => {
    return onScreenShareViewerReady(handleViewerReady);
  }, [handleViewerReady]);

  // Watch All: listen for close event from WatchAllPage
  useEffect(() => {
    return onWatchAllClosed(() => closeWatchAllWindow());
  }, [closeWatchAllWindow]);

  // Screen share: listen for pop-back-in request from ScreenSharePage
  // Only acts when Watch All is open — otherwise double-click is a no-op.
  useEffect(() => {
    return onScreenSharePopBackIn((pid) => {
      if (!watchAllWindowRef.current || !watchAllReadyRef.current) return;
      handleShareWindowClosed(pid); // deletes from shareWindowsRef, re-adds to watch-all
      // Tell the child window to close itself — emitTo is reliable; win.close() from parent is not.
      // screen-share:closed will fire but is a no-op since the map entry was already deleted above.
      requestScreenShareWindowClose(pid);
    });
  }, [handleShareWindowClosed]);

  // Watch All: listen for pop-out request from WatchAllPage
  useEffect(() => {
    return onWatchAllPopOut((payload) => {
      const pid = payload.participantId;
      if (typeof payload.volume === 'number') {
        syncScreenShareVolume(pid, payload.volume);
      }
      if (typeof payload.muted === 'boolean') {
        syncScreenShareMuted(pid, payload.muted);
      }
      const rs = getRoomSnapshotRef.current();
      const participant = rs?.participants.find((p) => p.id === pid);
      if (!participant) return;
      // If already open, bring to foreground
      const existingWin = shareWindowsRef.current.get(pid);
      if (existingWin) {
        void existingWin.window.setFocus();
        return;
      }
      // openShareWindow handles removing the tile from Watch All grid
      void openShareWindow(pid, participant, rs?.screenShareStreams.get(pid) ?? null, 'watch-all');
    });
  }, [syncScreenShareMuted, syncScreenShareVolume, openShareWindow]);

  // Dynamic share tracking for Watch All window
  useEffect(() => {
    if (!roomState || !watchAllOpen) {
      prevWatchAllStreamsRef.current = new Map();
      return;
    }

    // Don't emit events until the child window has signaled readiness.
    // The ready callback in openWatchAllWindow handles the initial
    // share emission and seeds prevWatchAllStreamsRef. This effect
    // only handles changes that happen AFTER the window is ready.
    if (!watchAllReadyRef.current) return;

    const scope = getWatchAllScope(roomState);
    const currentStreams = scope.streams;
    const prevStreams = prevWatchAllStreamsRef.current;

    // New shares: in current but not in prev
    for (const [pid, stream] of currentStreams) {
      if (!prevStreams.has(pid)) {
        // Skip participants that have an individual pop-out window open —
        // their stream is already shown in the pop-out window.
        if (shareWindowsRef.current.has(pid)) continue;
        // New participant started sharing
        const participant = scope.participants.find((p) => p.id === pid);
        if (participant) {
          emitWatchAllShareAdded({
            participantId: pid,
            liveKitIdentity: liveKitIdentityForParticipant(pid),
            displayName: participant.displayName,
            color: participant.color,
            canvasFallback: stream === null,
          });
          restoreWatchAllVolumeForParticipant(pid);
        }
      } else {
        // Existing participant — check if stream reference changed
        // Skip if this participant has an individual pop-out window
        if (shareWindowsRef.current.has(pid)) continue;
        const prevStream = prevStreams.get(pid) ?? null;
        if (stream && stream !== prevStream) {
          // Only re-attach audio if the viewer already owns this stream's audio.
          // A stream reference change can fire before viewer-ready resolves (e.g.
          // the SFU delivers the track muted then unmutes it within the same
          // subscription window). Attaching audio here in that case would bypass
          // the viewer-ready gate and leak audio before the tile visually connects.
          if (watchAllAttachedAudioRef.current.has(pid)) {
            attachScreenShareAudio(pid);
            applySavedScreenShareAudio(pid);
          }
        }
      }
    }

    // Removed shares: in prev but not in current
    for (const pid of prevStreams.keys()) {
      if (!currentStreams.has(pid)) {
        detachScreenShareAudio(pid);
        watchAllAttachedAudioRef.current.delete(pid);
        emitWatchAllShareRemoved(pid);
      }
    }

    prevWatchAllStreamsRef.current = new Map(currentStreams);
    // roomState itself must stay out of this dep array — it's a large object
    // that gets a new reference on nearly every unrelated state change,
    // whereas the narrow fields below are the ones this effect actually reacts
    // to. The bare reference in the guard above is unavoidable:
    // getWatchAllScope(roomState) needs the whole object.
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [
    getWatchAllScope,
    watchAllOpen,
    roomState?.screenShareStreams,
    roomState?.participants,
    roomState?.joinedSubRoomId,
    roomState?.subRooms,
    roomState?.passthrough,
    applySavedScreenShareAudio,
    restoreWatchAllVolumeForParticipant,
  ]);

  // Watch All: sync audio-only sharer additions/removals
  useEffect(() => {
    const audioOnlySharers = roomState?.audioOnlySharers;
    const participants = roomState?.participants;
    if (!audioOnlySharers || !participants) return;
    const curr = audioOnlySharers;
    const baselineSeed = planAudioOnlySharerBaselineSeed(
      hasSeededAudioOnlySharersRef.current,
      roomState?.joinedSubRoomId ?? null,
      curr,
    );
    if (baselineSeed) {
      prevAudioOnlySharersRef.current = baselineSeed;
      hasSeededAudioOnlySharersRef.current = true;
    } else if (!hasSeededAudioOnlySharersRef.current) {
      return;
    }
    const prev = prevAudioOnlySharersRef.current;
    for (const identity of curr) {
      if (prev.has(identity)) continue;
      const participant = participants.find((p) => p.id === identity);
      if (!participant) continue;
      const vol = getSavedShareVolume(identity);
      // Always start audio shares muted; preserve the last-set volume for restore.
      shareMutedRef.current.set(identity, true);
      setShareMuted((prev) => {
        if (prev.get(identity) === true) return prev;
        const next = new Map(prev);
        next.set(identity, true);
        return next;
      });
      detachScreenShareAudio(identity);
      setScreenShareAudioVolume(identity, 0);
      if (watchAllOpen && watchAllReadyRef.current) {
        emitWatchAllAudioShareAdded({
          participantId: identity,
          displayName: participant.displayName,
          color: participant.color,
          volume: vol,
          muted: true,
        });
      }
    }
    for (const identity of prev) {
      if (curr.has(identity)) continue;
      if (watchAllOpen && watchAllReadyRef.current) {
        emitWatchAllAudioShareRemoved(identity);
      }
    }
    prevAudioOnlySharersRef.current = new Set(curr);
  }, [
    getSavedShareVolume,
    watchAllOpen,
    roomState?.audioOnlySharers,
    roomState?.participants,
    roomState?.joinedSubRoomId,
  ]);

  // Watch All: emit share-updated when participant info changes
  const prevParticipantsRef = useRef<Map<string, { displayName: string; color: string }>>(
    new Map(),
  );
  useEffect(() => {
    if (!roomState || !watchAllOpen) return;

    const sharers = getWatchAllScope(roomState).remoteSharers;
    for (const p of sharers) {
      const prev = prevParticipantsRef.current.get(p.id);
      if (prev && (prev.displayName !== p.displayName || prev.color !== p.color)) {
        emitWatchAllShareUpdated({
          participantId: p.id,
          displayName: p.displayName,
          color: p.color,
        });
      }
    }

    const newMap = new Map<string, { displayName: string; color: string }>();
    for (const p of sharers) {
      newMap.set(p.id, { displayName: p.displayName, color: p.color });
    }
    prevParticipantsRef.current = newMap;
    // Same unavoidable-whole-object reasoning as the dynamic share-tracking
    // effect above: getWatchAllScope(roomState) needs the whole object, so
    // roomState itself stays out of the dep array below.
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [
    getWatchAllScope,
    watchAllOpen,
    roomState?.participants,
    roomState?.joinedSubRoomId,
    roomState?.subRooms,
    roomState?.passthrough,
  ]);

  // Cleanup all share windows on unmount / leave
  useEffect(() => {
    return () => {
      closeAllShareWindows();
    };
  }, [closeAllShareWindows]);

  // Close Watch All and any share pop-outs when the joined sub-room changes.
  const previousJoinedSubRoomIdRef = useRef<string | null>(null);
  useEffect(() => {
    const previousJoinedSubRoomId = previousJoinedSubRoomIdRef.current;
    const nextJoinedSubRoomId = roomState?.joinedSubRoomId ?? null;
    previousJoinedSubRoomIdRef.current = nextJoinedSubRoomId;

    if (previousJoinedSubRoomId === nextJoinedSubRoomId) return;

    const scopeParticipantIds = getWatchAllScope(roomState).participantIds;
    closeWatchAllWindow();

    for (const [participantId, shareWindow] of [...shareWindowsRef.current.entries()]) {
      if (shouldCloseOnSubRoomChange(shareWindow.scope, participantId, scopeParticipantIds)) {
        closeShareWindow(participantId);
      }
    }
  }, [
    getWatchAllScope,
    roomState,
    roomState?.joinedSubRoomId,
    closeWatchAllWindow,
    closeShareWindow,
  ]);

  // Close Watch All and any share pop-outs when the room session fully ends.
  // This covers disconnect/error paths that transition to idle without going
  // through the explicit leave flow, such as the session being displaced.
  useEffect(() => {
    if (roomState?.machineState !== 'idle') return;
    closeAllShareWindows();
  }, [roomState?.machineState, closeAllShareWindows]);

  // Mirror self-state / participant lists into the open child windows.
  useEffect(() => {
    if (!roomState) return;
    emitShareUserState(shareUserStateRef.current);
  }, [
    roomState,
    shareUserState.isMuted,
    shareUserState.isDeafened,
    shareUserState.isSharing,
    shareUserState.shareEnabled,
  ]);

  useEffect(() => {
    if (!roomState) return;
    emitShareVoiceParticipants(voiceParticipantsRef.current);
  }, [roomState, roomState?.participants, roomState?.selfParticipantId]);

  useEffect(() => {
    if (!roomState) return;
    emitWatchAllVoiceParticipants(watchAllVoiceParticipantsRef.current);
  }, [
    roomState,
    roomState?.participants,
    roomState?.selfParticipantId,
    roomState?.joinedSubRoomId,
    roomState?.subRooms,
    roomState?.passthrough,
  ]);

  return {
    watchingShareIds,
    shareVolumes,
    shareMuted,
    watchAllOpen,
    getWatchAllScope,
    getSavedShareVolume,
    getSavedShareMuted,
    syncScreenShareVolume,
    syncScreenShareMuted,
    openShareWindow,
    closeShareWindow,
    closeAllShareWindows,
    openWatchAllWindow,
    toggleWatchAllWindow,
  };
}
