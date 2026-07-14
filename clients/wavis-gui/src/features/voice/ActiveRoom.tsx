import { type ReactNode, useState, useEffect, useRef, useCallback } from 'react';
import { LoadingBars } from '@shared/LoadingBars';
import { useLocation, useNavigate } from 'react-router';
import type { ChannelRole } from '@features/channels/channels';
import type { Channel } from '@features/channels/channels';
import { ChannelSwitcherPanel } from '@features/channels/ChannelSwitcherPanel';
import type { VoiceRoomState, ShareQuality } from './voice-room';
import {
  initSession,
  leaveRoom,
  toggleSelfMute,
  toggleSelfDeafen,
  toggleCameraIntent,
  stopShare,
  startCustomShare,
  stopCustomShare,
  setParticipantVolume,
  setMasterVolume,
  kickParticipant,
  muteParticipant,
  stopParticipantShare,
  stopAllShares,
  setSharePermission,
  reconnectMedia,
  setShareQuality,
  toggleShareAudio,
  changeShareSource,
  startFallbackShare,
  startPortalShare,
  setPendingSharePickerData,
} from './voice-room';
import {
  activeShareType,
  computeStopRoute,
  preserveVideoShareSelectionForSourceChange,
} from './share-slot-policy';
import type { EnumerationResult } from '@features/screen-share/share-types';
import SharePicker from '@features/screen-share/SharePicker';
import type { OccupiedSlots } from '@features/screen-share/SharePicker';
import { setCurrentWindowSize } from '@shared/window-bridge';
import {
  ensureScreenRecordingAccess,
  listShareSources,
  onNativeShareError,
  onScreenShareChangeSource,
  onScreenShareQuality,
  onScreenShareToggleAudio,
  onShareIndicatorStop,
  onSharePickerCancelled,
  onSharePickerSelected,
  onSharePickerUsePortal,
  openSharePickerWindow,
} from '@features/screen-share/share-window-bridge';
import { setLastChannel, clearLastChannel } from '@features/settings/settings-store';

const DEBUG_SHARE_AUDIO = import.meta.env.VITE_DEBUG_SHARE_AUDIO === 'true';
import { useHotkeys } from '@shared/useHotkeys';
import { useTrayIntegration } from './useTrayIntegration';
import { useRoomNavigationGuard } from './useRoomNavigationGuard';
import { useRoomNotifications } from './useRoomNotifications';
import { useTransientChatError } from './useTransientChatError';
import { useCliFocusShortcut } from './useCliFocusShortcut';
import { useMainWindowLifecycle } from './useMainWindowLifecycle';
import { useRoomHotkeys } from './useRoomHotkeys';
import { useVideoPopoutWindow } from './useVideoPopoutWindow';
import { useShareViewerWindows } from './useShareViewerWindows';
import { useDebug } from '@shared/debug-context';
import {
  isShareEnabled,
  shareButtonLabel,
  appendSystemEvent,
  selectRoomPanelTab,
} from './voice-room';
import { navigateCliHistory, pushCliHistory, resetCliHistoryNavigation } from './cli-history';
import { Toaster, toast } from 'sonner';
import Settings from '@features/settings/Settings';
import { useAudioDriverInstall } from '@features/screen-share/useAudioDriverInstall';
import { AudioDriverInstallPrompt } from '@features/screen-share/AudioDriverInstallPrompt';
import {
  cameraButtonLabel,
  hasBrowserCameraMediaSupport,
  shouldDisableCameraButton,
  shouldMountCameraButton,
} from './active-room-camera';
import {
  SHARE_PICKER_LOADING_LABEL,
  SHARE_STARTING_LOADING_LABEL,
  isVideoShareSelectionMode,
} from './active-room-share-loading';
import { VideoTab } from './VideoTab';
import {
  RoomHeaderBar,
  MediaStatusBanners,
  ChannelSwitcherToggle,
  StatusDot,
  signalingIndicator,
  mediaIndicator,
  rttColor,
} from './RoomHeaderBar';
import { ChatPanel } from './ChatPanel';
import { LogsPanel } from './LogsPanel';
import { ParticipantsPanel, ROOM_REMOVAL_COUNTDOWN_INTERVAL_MS } from './ParticipantsPanel';
import type { ParticipantRowViewModel } from './ParticipantRow';

/* ─── Helpers ───────────────────────────────────────────────────── */

function shareLoadingNotice(label: string, className: string): ReactNode {
  return <LoadingBars label={label} className={className} />;
}

/**
 * Temporarily expands the Tauri window before the macOS getDisplayMedia system
 * picker opens so its grid is never clipped on narrow windows, then restores the
 * original size when the dialog closes (or if it throws).
 * No-op on macOS (system dialog ignores WebView width) and unused on Windows
 * (Windows uses the inline SharePicker, not getDisplayMedia).
 */
async function withPickerResize<T>(isMacPlatform: boolean, fn: () => Promise<T>): Promise<T> {
  const MIN_NATIVE_PICKER_WIDTH = 700;
  const originalWidth = window.innerWidth;
  const originalHeight = window.innerHeight;
  const targetWidth = Math.min(MIN_NATIVE_PICKER_WIDTH, window.screen.availWidth - 20);
  const needsResize = !isMacPlatform && originalWidth < targetWidth;
  if (needsResize) {
    await setCurrentWindowSize(targetWidth, originalHeight);
  }
  try {
    return await fn();
  } finally {
    if (needsResize) {
      await setCurrentWindowSize(originalWidth, originalHeight);
    }
  }
}

/* ═══ Component ═════════════════════════════════════════════════════ */

export default function ActiveRoom() {
  const hotkeys = useHotkeys();
  const location = useLocation();
  const navigate = useNavigate();
  // react-router types location.state as `unknown`; the cast below tells TS it's always our
  // shape, but navigating here directly (no state, e.g. a bookmark/reload) makes it genuinely
  // null at runtime.
  const locationState =
    // eslint-disable-next-line @typescript-eslint/no-unnecessary-condition
    (location.state as { channelId: string; channelName: string; channelRole: ChannelRole }) ?? {};
  const { channelId, channelName, channelRole } = locationState;

  const [roomState, setRoomState] = useState<VoiceRoomState | null>(null);
  const [countdownNowMs, setCountdownNowMs] = useState(() => Date.now());

  const [, setLeaving] = useState(false);
  const [cliInput, setCliInput] = useState('');
  const cliInputRef = useRef<HTMLInputElement>(null);
  const initRef = useRef(false);
  const [cliFocused, setCliFocused] = useState(false);
  const cliHistoryRef = useRef<string[]>([]);
  const cliHistoryIndexRef = useRef(-1);
  const cliDraftRef = useRef('');

  const [showSettings, setShowSettings] = useState(false);
  const [channelSwitcherOpen, setChannelSwitcherOpen] = useState(false);

  // Left column collapsible sections
  const [expandedSections, setExpandedSections] = useState<Record<string, boolean>>({
    you: true,
    participants: true,
  });
  const toggleSection = (key: string) =>
    setExpandedSections((prev) => {
      const current = prev[key] ?? true;
      return { ...prev, [key]: !current };
    });

  // Mobile tab state
  type MobileTab = 'participants' | 'chat' | 'log' | 'video';
  const [mobileTab, setMobileTab] = useState<MobileTab>('participants');
  type GroupedPanelTab = 'chat' | 'log' | 'video';
  const [groupedPanelTab, setGroupedPanelTab] = useState<GroupedPanelTab>('chat');

  const { navigateAwayFromRoom, allowNavigationRef, skipUnmountLeaveRef } = useRoomNavigationGuard({
    channelId,
    error: roomState?.error,
    rejectionReason: roomState?.rejectionReason,
  });

  const { resetNotificationCursor } = useRoomNotifications(roomState?.events);

  // Transient chat error display (auto-dismiss after 5s)
  const chatError = useTransientChatError(roomState?.lastChatError, mobileTab);

  useEffect(() => {
    const hasScheduledRemoval =
      roomState?.subRooms.some((subRoom) => subRoom.deleteAtMs !== null) ?? false;
    if (!hasScheduledRemoval) return;

    setCountdownNowMs(Date.now());
    const interval = window.setInterval(() => {
      setCountdownNowMs(Date.now());
    }, ROOM_REMOVAL_COUNTDOWN_INTERVAL_MS);

    return () => window.clearInterval(interval);
  }, [roomState?.subRooms]);

  // Session init + cleanup
  useEffect(() => {
    if (!channelId || initRef.current) return;
    initRef.current = true;
    allowNavigationRef.current = false;
    skipUnmountLeaveRef.current = false;
    initSession(channelId, channelName, channelRole, setRoomState);
    return () => {
      if (!skipUnmountLeaveRef.current) {
        leaveRoom();
      }
      skipUnmountLeaveRef.current = false;
      initRef.current = false;
      resetNotificationCursor();
    };
  }, [channelId, channelName, channelRole, resetNotificationCursor]);

  useTrayIntegration({
    roomState,
    channelId,
    onLeave: () => navigateAwayFromRoom('/', 'immediate'),
    onToggleMute: toggleSelfMute,
    onToggleDeafen: toggleSelfDeafen,
  });

  useCliFocusShortcut({
    cliInputRef,
    cliInput,
    setCliInput,
    onActivate: () => setMobileTab('log'),
  });

  const toggleWatchAllRef = useRef<() => void>(() => {});
  const groupedPanelVideoActivityKey = roomState
    ? Object.entries(roomState.videoTilesById)
        .filter(([, tile]) => !tile.isMuted)
        .map(([participantId]) => participantId)
        .sort()
        .join('|')
    : '';
  const groupedPanelVideoActivityRef = useRef(groupedPanelVideoActivityKey);

  useEffect(() => {
    const previous = groupedPanelVideoActivityRef.current;
    if (previous === groupedPanelVideoActivityKey) return;
    groupedPanelVideoActivityRef.current = groupedPanelVideoActivityKey;
    setGroupedPanelTab((current) => {
      if (groupedPanelVideoActivityKey !== '') return 'video';
      return current === 'video' ? 'chat' : current;
    });
  }, [groupedPanelVideoActivityKey]);

  const [shareQualityState, setShareQualityState] = useState<ShareQuality>('high');
  const [shareAudioOn, setShareAudioOn] = useState(false);
  const [showPostShareAudioPrompt, setShowPostShareAudioPrompt] = useState(false);
  const [showMacAudioHoverMessage, setShowMacAudioHoverMessage] = useState(false);
  // Screen share error toast (auto-dismisses after 5s)
  const [screenShareError, setScreenShareError] = useState<string | null>(null);
  const shareErrorTimerRef = useRef<ReturnType<typeof setTimeout> | null>(null);
  const shareEnumerating = useRef(false);
  // True while waiting for the OS screen picker to appear (macOS getDisplayMedia)
  const [sharePickerLoading, setSharePickerLoading] = useState(false);
  // True while a selected video share source is starting publication.
  const [shareStarting, setShareStarting] = useState(false);
  // Windows: inline share picker data (replaces getDisplayMedia to suppress WebView2 capture indicator)
  const [winSharePicker, setWinSharePicker] = useState<{
    enumResult: EnumerationResult | null;
    occupied: OccupiedSlots;
    isChangingSource?: boolean;
    initialWithAudio?: boolean;
  } | null>(null);
  // macOS audio driver install prompt state
  const [showDriverPrompt, setShowDriverPrompt] = useState(false);
  const pendingShareRef = useRef<boolean>(false);
  // Set to true when the user explicitly skips driver install so handleStartShare bypasses the check once.
  const skipDriverCheckRef = useRef(false);
  const wasSelfSharingRef = useRef(false);
  const selfSharingRef = useRef(false);
  const handleStartShareRef = useRef<() => void | Promise<void>>(() => {});
  const stopShareActionRef = useRef<() => void>(() => {});

  const showTransientScreenShareError = useCallback((message: string) => {
    if (shareErrorTimerRef.current) clearTimeout(shareErrorTimerRef.current);
    setScreenShareError(message);
    shareErrorTimerRef.current = setTimeout(() => {
      setScreenShareError(null);
      shareErrorTimerRef.current = null;
    }, 5000);
  }, []);

  // Self-share owner actions from child windows (quality / audio / source —
  // the viewer-window channels live in useShareViewerWindows).
  useEffect(() => {
    const cleanups: Array<() => void> = [];

    cleanups.push(
      onScreenShareQuality((quality) => {
        setShareQualityState(quality);
        void setShareQuality(quality);
      }),
    );
    cleanups.push(
      onScreenShareToggleAudio((withAudio) => {
        if (DEBUG_SHARE_AUDIO) {
          console.log('[wavis:active-room] [share-audio] screen-share:toggle-audio received', {
            withAudio,
            userActivationIsActive: (navigator as { userActivation?: { isActive: boolean } })
              .userActivation?.isActive,
          });
        }
        if (isMacPlatform && withAudio) {
          setShareAudioOn(false);
          return;
        }
        setShareAudioOn(withAudio);
        void toggleShareAudio(withAudio);
      }),
    );
    cleanups.push(
      onScreenShareChangeSource(() => {
        if (isWindowsPlatform) {
          const occupied = {
            videoOccupied: false,
            audioOccupied: roomStateRef.current?.activeAudioShare !== null,
          };
          const initialWithAudio = roomStateRef.current?.activeVideoShare?.withAudio ?? false;
          setWinSharePicker({
            enumResult: null,
            occupied,
            isChangingSource: true,
            initialWithAudio,
          });
          listShareSources()
            .then((enumResult) => {
              setWinSharePicker((current) => (current ? { ...current, enumResult } : current));
            })
            .catch((err: unknown) => {
              setWinSharePicker(null);
              const detail = err instanceof Error ? err.message : String(err);
              toast.error(`Screen sharing failed: ${detail}`);
            });
        } else {
          void withPickerResize(isMacPlatform, () => changeShareSource());
        }
      }),
    );
    return () => {
      for (const cleanup of cleanups) cleanup();
    };
  }, []);

  // Custom share picker + indicator event listeners
  useEffect(() => {
    const cleanups: Array<() => void> = [];

    // Share picker selection → start custom share
    cleanups.push(
      onSharePickerSelected((selection) => {
        setPendingSharePickerData(null);
        const showStartingIndicator = isVideoShareSelectionMode(selection.mode);
        if (showStartingIndicator) setShareStarting(true);
        startCustomShare(selection)
          .catch((err) => {
            const msg = err instanceof Error ? err.message : String(err);
            showTransientScreenShareError(msg);
          })
          .finally(() => {
            if (showStartingIndicator) setShareStarting(false);
          });
      }),
    );

    // Share picker cancelled → clear pending data
    cleanups.push(
      onSharePickerCancelled(() => {
        setPendingSharePickerData(null);
      }),
    );

    cleanups.push(
      onSharePickerUsePortal(() => {
        setPendingSharePickerData(null);
        setShareStarting(true);
        startPortalShare()
          .catch((err) => {
            const msg = err instanceof Error ? err.message : String(err);
            showTransientScreenShareError(msg);
          })
          .finally(() => {
            setShareStarting(false);
          });
      }),
    );

    // Share indicator stop button (now with target: 'video' | 'audio' | 'all')
    cleanups.push(
      onShareIndicatorStop((target) => {
        void stopCustomShare(target);
      }),
    );

    // Rust-side share error (PipeWire disconnect, window closed, etc.)
    cleanups.push(
      onNativeShareError((message) => {
        stopCustomShare()
          .catch((err) => {
            console.error('[wavis:active-room] failed to stop after native share error:', err);
          })
          .finally(() => {
            showTransientScreenShareError(message);
          });
      }),
    );

    return () => {
      for (const cleanup of cleanups) cleanup();
    };
  }, [showTransientScreenShareError]); // listeners surface share errors via shared timer helper

  // When the main window is actually closing (not minimized to tray),
  // tear down the voice session and close all child windows so nothing
  // is orphaned.
  useMainWindowLifecycle(() => {
    closeAllShareWindows();
    leaveRoom();
  });

  // Ref to latest roomState so the ready callback always reads fresh data
  const roomStateRef = useRef(roomState);
  roomStateRef.current = roomState;

  const { videoPopoutOpen, openVideoPopoutWindow, closeVideoPopoutWindow } = useVideoPopoutWindow({
    videoTilesById: roomState?.videoTilesById,
    getRoomSnapshot: () => roomStateRef.current,
  });

  // The grouped panel loses its video tab while the pop-out owns the grid.
  useEffect(() => {
    if (!videoPopoutOpen) return;
    setGroupedPanelTab((current) => (current === 'video' ? 'chat' : current));
  }, [videoPopoutOpen]);

  useRoomHotkeys({
    mediaConnected: roomState?.mediaState === 'connected',
    onToggleWatchAll: () => toggleWatchAllRef.current(),
  });

  // Platform check: Linux uses standalone window (PostMessage works fine there).
  const isLinuxPlatform = typeof navigator !== 'undefined' && /Linux/.test(navigator.userAgent);
  const isMacPlatform = typeof navigator !== 'undefined' && /Mac/.test(navigator.userAgent);
  const isWindowsPlatform = typeof navigator !== 'undefined' && /Windows/.test(navigator.userAgent);
  const supportedCapturePlatform =
    isWindowsPlatform || isMacPlatform || isLinuxPlatform || hasBrowserCameraMediaSupport();
  const macShareAudioDisabledMessage =
    'System audio on macOS requires the WavisAudioTap driver — stop sharing and restart to enable audio.';

  // macOS: check / install the WavisAudioTap HAL driver needed for echo-free audio share.
  const { driverState, installError, triggerInstall } = useAudioDriverInstall(isMacPlatform);

  /* â”€â”€ Derived â”€â”€ */
  const { showSecrets } = useDebug();
  const selfP = roomState?.participants.find((p) => p.id === roomState.selfParticipantId);
  const isHost = roomState?.selfIsHost ?? false;
  const selfSharing = selfP?.isSharing ?? false;
  // True when video capture (or fallback getDisplayMedia) is active — does NOT include audio-only share.
  // Used to decide whether the ◉ compact button and the expanded /stopshare button should be in stop-mode.
  const isVideoOrFallbackSharing =
    !!roomState?.activeVideoShare || (selfSharing && !roomState?.activeAudioShare);
  const voiceRoomConnected = roomState
    ? roomState.machineState === 'active' || roomState.machineState === 'reconnecting'
    : false;
  const showCameraButton = shouldMountCameraButton(voiceRoomConnected, supportedCapturePlatform);
  const cameraButtonDisabled = shouldDisableCameraButton(
    voiceRoomConnected,
    roomState?.mediaState,
    roomState?.joinedSubRoomId,
  );
  const videoButtonLabel = cameraButtonLabel(roomState?.cameraIntent ?? false);
  const cameraLabel = `/${videoButtonLabel}`;
  const sharers = roomState?.participants.filter((p) => p.isSharing) ?? [];
  const shareEnabled = roomState
    ? isShareEnabled(
        roomState.sharePermission,
        isHost,
        roomState.machineState,
        roomState.mediaState,
        roomState.joinedSubRoomId,
      )
    : false;
  const currentShareType = roomState
    ? activeShareType(roomState.activeVideoShare, roomState.activeAudioShare)
    : null;
  const stopShareAction = () => {
    const route = computeStopRoute(currentShareType, selfSharing);
    if (route === 'stop_custom') {
      // Stop only the video share when video is active — audio-only share stays running.
      // Stopping both is handled by /stop-audio for the audio slot.
      void stopCustomShare(roomState?.activeVideoShare !== null ? 'video' : 'audio');
    } else if (route === 'stop_fallback') void stopShare();
  };
  const {
    watchingShareIds,
    shareVolumes,
    watchAllOpen,
    getSavedShareVolume,
    getSavedShareMuted,
    syncScreenShareVolume,
    syncScreenShareMuted,
    openShareWindow,
    closeShareWindow,
    closeAllShareWindows,
    toggleWatchAllWindow,
  } = useShareViewerWindows({
    roomState,
    getRoomSnapshot: () => roomStateRef.current,
    shareUserState: {
      isMuted: selfP?.isMuted ?? false,
      isDeafened: roomState?.isDeafened ?? false,
      isSharing: selfSharing,
      shareEnabled,
    },
    onToggleSelfShare: () => {
      if (selfSharingRef.current) {
        stopShareActionRef.current();
      } else {
        void handleStartShareRef.current();
      }
    },
    onError: showTransientScreenShareError,
    closeVideoPopoutWindow,
  });

  // Keep ref in sync so hotkey callback never captures a stale closure
  toggleWatchAllRef.current = () => {
    void toggleWatchAllWindow();
  };

  /** Open custom share picker or invoke getDisplayMedia fallback based on platform. */
  const handleStartShare = async () => {
    if (!shareEnabled) return;
    if (shareEnumerating.current) return;
    shareEnumerating.current = true;
    if (!isLinuxPlatform) setSharePickerLoading(true);

    try {
      if (!isLinuxPlatform) {
        // macOS: uses getDisplayMedia() via startFallbackShare() below.
        // Windows: intercepted below by isWindowsPlatform — uses native Rust
        //   capture pipeline (inline SharePicker → WinCapture → startNativeCapture
        //   + startWasapiAudioBridge). getDisplayMedia() is never called on Windows.
        if (isMacPlatform) {
          const access = await ensureScreenRecordingAccess();

          if (!access.authorized) {
            const msg =
              'Screen sharing requires Screen Recording permission in System Settings > Privacy & Security > Screen Recording.';
            showTransientScreenShareError(msg);
            toast.error(msg);
            return;
          }

          if (access.restartRequired) {
            const msg =
              'Screen Recording permission was granted. Quit and reopen Wavis, then try screen sharing again.';
            showTransientScreenShareError(msg);
            toast.error(msg);
            return;
          }
        }

        // On macOS, show the driver install prompt before the first audio share
        // if the WavisAudioTap HAL driver is not installed. skipDriverCheckRef
        // is set when the user explicitly skips so we don't re-prompt.
        if (isMacPlatform && driverState === 'not_installed' && !skipDriverCheckRef.current) {
          pendingShareRef.current = true;
          setShowDriverPrompt(true);
          shareEnumerating.current = false;
          setSharePickerLoading(false);
          return;
        }
        skipDriverCheckRef.current = false;

        // Windows: use native Rust source picker to avoid getDisplayMedia() and
        // the WebView2 capture indicator bar it triggers.
        if (isWindowsPlatform) {
          const occupied: OccupiedSlots = {
            videoOccupied: roomState?.activeVideoShare !== null,
            audioOccupied: roomState?.activeAudioShare !== null,
          };
          setWinSharePicker({ enumResult: null, occupied });
          setSharePickerLoading(false);
          try {
            const enumResult = await listShareSources();
            setWinSharePicker((current) => (current ? { ...current, enumResult } : current));
          } catch (err) {
            setWinSharePicker(null);
            const detail = err instanceof Error ? err.message : String(err);
            showTransientScreenShareError(`Screen sharing failed: ${detail}`);
            toast.error(`Screen sharing failed: ${detail}`);
          }
          return;
        }

        try {
          await withPickerResize(isMacPlatform, async () => {
            const result = await startFallbackShare();
            if (result.started) {
              if (isMacPlatform) {
                if (result.withAudio) {
                  void toggleShareAudio(false);
                }
                setShareAudioOn(false);
                setShowPostShareAudioPrompt(true);
                return;
              }
              if (result.withAudio) {
                setShareAudioOn(true);
              } else {
                // No audio track yet — offer the native/browser audio toggle prompt.
                setShowPostShareAudioPrompt(true);
              }
            }
          });
        } catch (err) {
          const detail = err instanceof Error ? err.message : String(err);
          console.error('[wavis:active-room] screen share failed:', detail);
          const msg = isMacPlatform
            ? 'Screen sharing is blocked by macOS. Make sure Wavis is allowed in System Settings > Privacy & Security > Screen Recording, then quit and reopen Wavis.'
            : `Screen sharing failed: ${detail}`;
          showTransientScreenShareError(msg);
          toast.error(msg);
        }
        return;
      }

      // Linux: custom picker path — getDisplayMedia() doesn't work in WebKitGTK.
      // Open the picker before source enumeration so slow PipeWire/PulseAudio
      // discovery does not block the OS window from appearing.
      const occupied: OccupiedSlots = {
        videoOccupied: roomState?.activeVideoShare !== null,
        audioOccupied: roomState?.activeAudioShare !== null,
      };
      setPendingSharePickerData({
        enumResult: { sources: [], warnings: [], fallback_reason: null },
        occupied,
      });
      openSharePickerWindow({ enumResult: null, occupied });
    } catch (err) {
      const msg = err instanceof Error ? err.message : String(err);
      showTransientScreenShareError(msg);
    } finally {
      shareEnumerating.current = false;
      setSharePickerLoading(false);
    }
  };

  selfSharingRef.current = selfSharing;
  handleStartShareRef.current = handleStartShare;
  stopShareActionRef.current = stopShareAction;

  // Clean up share error timer on unmount
  useEffect(() => {
    return () => {
      if (shareErrorTimerRef.current) clearTimeout(shareErrorTimerRef.current);
    };
  }, []);

  useEffect(() => {
    const selfParticipant = roomState?.participants.find(
      (p) => p.id === roomState.selfParticipantId,
    );
    const isSelfSharing = selfParticipant?.isSharing ?? false;
    if (wasSelfSharingRef.current && !isSelfSharing) {
      setShareAudioOn(false);
      setShowPostShareAudioPrompt(false);
    }
    wasSelfSharingRef.current = isSelfSharing;
  }, [roomState?.participants, roomState?.selfParticipantId]);

  if (!channelId) return null;

  // Loading state
  if (!roomState) {
    return (
      <div className="h-full flex items-center justify-center bg-wavis-bg font-mono text-wavis-text-secondary">
        Connecting to voice room...
      </div>
    );
  }

  if (roomState.machineState === 'server_starting') {
    const waitMins = Math.ceil((roomState.serverStartingEstimatedWaitSecs ?? 120) / 60);
    return (
      <div className="h-full flex items-center justify-center bg-wavis-bg font-mono text-wavis-text">
        <div
          className="border border-wavis-text-secondary/60 bg-wavis-panel/90 shadow-[0_0_24px_rgba(0,0,0,0.45)]"
          style={{ width: 'min(88%, 24rem)', padding: '1rem 1.125rem' }}
          role="status"
        >
          <div className="flex items-center gap-2 text-[0.625rem] uppercase text-wavis-warn">
            <div className="flex items-center gap-1.5" aria-hidden="true">
              {[0, 1, 2].map((i) => (
                <span
                  key={i}
                  className="inline-block w-1 bg-wavis-purple animate-pulse"
                  style={{
                    height: '0.7rem',
                    animationDelay: `${i * 0.16}s`,
                  }}
                />
              ))}
            </div>
            <span>[starting]</span>
          </div>
          <div className="mt-2 text-sm text-wavis-text">voice server starting up</div>
          <div className="mt-1 text-xs leading-5 text-wavis-text-secondary">
            The server was offline and is booting. This takes up to {waitMins} minute
            {waitMins !== 1 ? 's' : ''}. Joining automatically when ready.
          </div>
          <button className="mt-3 text-xs text-wavis-text border border-wavis-text-secondary py-0.5 px-1 hover:bg-wavis-text-secondary hover:text-wavis-text-contrast transition-colors">
            /back
          </button>
        </div>
      </div>
    );
  }

  // Rejection state
  if (roomState.rejectionReason) {
    return (
      <div className="h-full flex flex-col bg-wavis-bg font-mono text-wavis-text">
        <div className="flex-1 flex items-center justify-center">
          <div className="text-center max-w-md">
            <div className="text-wavis-danger mb-4">{roomState.rejectionReason}</div>
            <div className="flex gap-4 justify-center">
              <button
                className="text-xs text-wavis-text border border-wavis-text-secondary py-0.5 px-1 text-center transition-colors hover:bg-wavis-text-secondary hover:text-wavis-text-contrast"
                onClick={() => {
                  initRef.current = false;
                  initSession(channelId, channelName, channelRole, setRoomState);
                  initRef.current = true;
                }}
              >
                /retry
              </button>
              <button className="text-xs text-wavis-text border border-wavis-text-secondary py-0.5 px-1 text-center transition-colors hover:bg-wavis-text-secondary hover:text-wavis-text-contrast">
                /back
              </button>
            </div>
          </div>
        </div>
      </div>
    );
  }

  // Error state (connection failure, not kicked)
  if (roomState.error && roomState.error !== 'You were kicked') {
    return (
      <div className="h-full flex flex-col bg-wavis-bg font-mono text-wavis-text">
        <div className="flex-1 flex items-center justify-center">
          <div className="text-center">
            <div className="text-wavis-danger mb-4">{roomState.error}</div>
            <div className="flex gap-4 justify-center">
              <button
                className="text-xs text-wavis-text border border-wavis-text-secondary py-0.5 px-1 text-center transition-colors hover:bg-wavis-text-secondary hover:text-wavis-text-contrast"
                onClick={() => {
                  initRef.current = false;
                  initSession(channelId, channelName, channelRole, setRoomState);
                  initRef.current = true;
                }}
              >
                /retry
              </button>
              <button
                className="text-xs text-wavis-danger border border-wavis-danger py-0.5 px-1 text-center transition-colors hover:bg-wavis-danger hover:text-wavis-bg"
                onClick={() => {
                  void clearLastChannel();
                  navigateAwayFromRoom('/', 'immediate');
                }}
              >
                /leave
              </button>
            </div>
          </div>
        </div>
      </div>
    );
  }

  // Kicked state
  if (roomState.error === 'You were kicked') {
    return (
      <div className="h-full flex flex-col items-center justify-center bg-wavis-bg font-mono text-wavis-danger gap-4">
        <span>you were kicked from the room</span>
        <button className="text-xs text-wavis-text border border-wavis-text-secondary py-0.5 px-1 text-center transition-colors hover:bg-wavis-text-secondary hover:text-wavis-text-contrast">
          /back
        </button>
      </div>
    );
  }

  /* ── Actions ── */
  const handleLeave = () => {
    setLeaving(true);
    navigateAwayFromRoom('/', 'immediate');
  };

  const handleChannelSwitch = async (ch: Channel) => {
    await setLastChannel(ch.id, ch.name, ch.role);
    setChannelSwitcherOpen(false);
    allowNavigationRef.current = true;
    leaveRoom();
    void navigate('/room', {
      state: { channelId: ch.id, channelName: ch.name, channelRole: ch.role },
    });
  };

  /* ── CLI autocomplete ── */
  const CLI_COMMANDS = [
    '/help',
    '/mute',
    '/deafen',
    '/kick',
    '/share',
    '/stopshare',
    '/revoke',
    '/stopall',
    '/shareperm',
    '/vol',
    '/watch-all',
    '/leave',
    '/reconnect-media',
    '/devices',
  ];

  const handleCliInputChange = (value: string) => {
    if (cliHistoryIndexRef.current !== -1) {
      const reset = resetCliHistoryNavigation();
      cliHistoryIndexRef.current = reset.historyIndex;
      cliDraftRef.current = reset.draft;
    }
    setCliInput(value);
  };

  const handleCliKeyDown = (e: React.KeyboardEvent<HTMLInputElement>) => {
    if (e.key === 'Enter') {
      handleCli();
      return;
    }
    if (e.key === 'ArrowUp' || e.key === 'ArrowDown') {
      const result = navigateCliHistory({
        currentInput: cliInput,
        history: cliHistoryRef.current,
        historyIndex: cliHistoryIndexRef.current,
        draft: cliDraftRef.current,
        direction: e.key === 'ArrowUp' ? 'older' : 'newer',
      });
      if (!result.handled) return;
      e.preventDefault();
      cliHistoryIndexRef.current = result.historyIndex;
      cliDraftRef.current = result.draft;
      setCliInput(result.nextInput);
      return;
    }
    if (e.key === 'Tab') {
      e.preventDefault();
      const raw = cliInput;
      // Only autocomplete the command portion (first token starting with /)
      if (!raw.startsWith('/')) return;
      const spaceIdx = raw.indexOf(' ');
      const prefix = spaceIdx === -1 ? raw : raw.slice(0, spaceIdx);
      if (spaceIdx !== -1) return; // already past the command token
      const matches = CLI_COMMANDS.filter((c) => c.startsWith(prefix.toLowerCase()));
      if (matches.length === 1) {
        setCliInput(matches[0] + ' ');
      } else if (matches.length > 1) {
        // Complete to longest common prefix
        let common = matches[0];
        for (const m of matches) {
          while (!m.startsWith(common)) common = common.slice(0, -1);
        }
        if (common.length > prefix.length) setCliInput(common);
      }
    }
  };

  /* ── CLI handler ── */
  const handleCli = () => {
    const raw = cliInput.trim();
    if (!raw) return;
    cliHistoryRef.current = pushCliHistory(cliHistoryRef.current, raw);
    const reset = resetCliHistoryNavigation();
    cliHistoryIndexRef.current = reset.historyIndex;
    cliDraftRef.current = reset.draft;
    setCliInput('');

    if (raw === '/help') {
      const help = [
        'available commands:',
        '  /help                        — show this list',
        '  /mute                        — toggle self mute',
        '  /mute <name>                 — host-mute a participant',
        '  /deafen                      — toggle deafen (mute + silence)',
        '  /kick <name>                 — kick a participant',
        '  /share                       — start screen share',
        '  /stopshare                   — stop your share',
        "  /revoke <name>               — stop a participant's share",
        '  /stopall                     — stop all shares',
        '  /shareperm anyone|host       — set share permission',
        '  /vol <0-100>                 — master volume',
        '  /vol <name> <0-100>          — per-peer volume',
        '  /reconnect-media             — reconnect media',
        '  /devices                     — toggle audio device panel',
        '  /watch-all                   — toggle watch all for your joined room',
        '  /leave                       — leave the room',
      ].join('\n');
      appendSystemEvent(help);
    } else if (raw === '/mute') {
      toggleSelfMute();
    } else if (raw === '/deafen') {
      toggleSelfDeafen();
    } else if (raw.startsWith('/kick ')) {
      const name = raw.replace('/kick ', '').trim();
      const p = roomState.participants.find((pp) => pp.displayName === name);
      if (p) kickParticipant(p.id);
    } else if (raw.startsWith('/mute ')) {
      const name = raw.replace('/mute ', '').trim();
      const p = roomState.participants.find((pp) => pp.displayName === name);
      if (p) muteParticipant(p.id);
    } else if (raw === '/share') {
      if (!shareEnabled) {
        setScreenShareError('screen share is host-only');
        if (shareErrorTimerRef.current) clearTimeout(shareErrorTimerRef.current);
        shareErrorTimerRef.current = setTimeout(() => {
          setScreenShareError(null);
          shareErrorTimerRef.current = null;
        }, 5000);
      } else {
        void handleStartShare();
      }
    } else if (raw === '/stopshare') {
      stopShareAction();
    } else if (raw.startsWith('/revoke ')) {
      const name = raw.replace('/revoke ', '').trim();
      const p = roomState.participants.find((pp) => pp.displayName === name);
      if (p) stopParticipantShare(p.id);
    } else if (raw === '/stopall') {
      stopAllShares();
    } else if (raw === '/shareperm anyone') {
      setSharePermission('anyone');
    } else if (raw === '/shareperm host') {
      setSharePermission('host_only');
    } else if (raw.startsWith('/vol ')) {
      const args = raw.replace('/vol ', '').trim().split(' ');
      if (args.length === 1) {
        const v = parseInt(args[0], 10);
        if (!isNaN(v)) setMasterVolume(v);
      } else if (args.length === 2) {
        const name = args[0];
        const v = parseInt(args[1], 10);
        const p = roomState.participants.find((pp) => pp.displayName === name);
        if (p && !isNaN(v)) setParticipantVolume(p.id, v);
      }
    } else if (raw === '/watch-all') {
      void toggleWatchAllWindow();
    } else if (raw === '/leave') {
      handleLeave();
    } else if (raw === '/reconnect-media') {
      void reconnectMedia();
    }
  };

  /* ── Reusable panel fragments ── */

  // Mobile compact header renders its own StatusDots directly (no room-name/badge/loss
  // row), so it needs sigDot/mediaDot independently of <RoomHeaderBar>, which
  // recomputes the same values internally for the intermediate/desktop layouts.
  const sigDot = signalingIndicator(roomState.machineState, roomState.lastRateLimitError);
  const mediaDot = mediaIndicator(roomState.mediaState, roomState.mediaError);

  const handleToggleChannelSwitcher = () => {
    setChannelSwitcherOpen((v) => !v);
    setShowSettings(false);
  };

  const roomHeader = (
    <RoomHeaderBar
      machineState={roomState.machineState}
      mediaState={roomState.mediaState}
      mediaError={roomState.mediaError}
      lastRateLimitError={roomState.lastRateLimitError}
      connectionMode={roomState.connectionMode}
      showSecrets={showSecrets}
      channelName={roomState.channelName}
      participantCount={Object.keys(roomState.participantSubRoomById).length}
      rttMs={roomState.networkStats.rttMs}
      packetLossPercent={roomState.networkStats.packetLossPercent}
      channelSwitcherOpen={channelSwitcherOpen}
      onToggleChannelSwitcher={handleToggleChannelSwitcher}
    />
  );

  const mediaStatusBanners = (
    <MediaStatusBanners
      machineState={roomState.machineState}
      mediaState={roomState.mediaState}
      mediaReconnectFailures={roomState.mediaReconnectFailures}
      lastRateLimitError={roomState.lastRateLimitError}
    />
  );

  const participantsViewModel: ParticipantRowViewModel[] = roomState.participants.map((p) => {
    const videoTile = roomState.videoTilesById[p.id];
    // A legacy/untyped signaling-only sharer (no shareType metadata at all)
    // is assumed to be video, matching prior behavior — see voice-room.ts's
    // share_started/share_state handlers for how shareType/audioOnlySharers
    // are kept independent for participants running both slots at once.
    const isAudioOnlySharer = roomState.audioOnlySharers.has(p.id);
    return {
      id: p.id,
      userId: p.userId,
      displayName: p.displayName,
      color: p.color,
      isMuted: p.isMuted,
      isHostMuted: p.isHostMuted,
      isDeafened: p.isDeafened,
      isSpeaking: p.isSpeaking,
      isSharing: p.isSharing,
      mediaConnected: p.mediaConnected,
      volume: p.volume,
      // Without noUncheckedIndexedAccess, TS types the videoTilesById index access as
      // always-defined even though a participant with no video tile entry makes it
      // genuinely undefined.
      // eslint-disable-next-line @typescript-eslint/no-unnecessary-condition
      cameraOn: !!videoTile && !videoTile.isMuted && !videoTile.hasError,
      isAudioOnlySharer,
      hasVideoShare: p.shareType !== undefined || !isAudioOnlySharer,
      hasScreenShareStream: roomState.screenShareStreams.has(p.id),
    };
  });

  const participantsPanel = (
    <ParticipantsPanel
      participants={participantsViewModel}
      subRooms={roomState.subRooms}
      joinedSubRoomId={roomState.joinedSubRoomId}
      selfParticipantId={roomState.selfParticipantId}
      passthrough={roomState.passthrough}
      passthroughEnabled={roomState.passthroughEnabled}
      isHost={isHost}
      sharersCount={sharers.length}
      countdownNowMs={countdownNowMs}
      selfIsDeafened={roomState.isDeafened}
      hasActiveVideoShare={roomState.activeVideoShare !== null}
      hasActiveAudioShare={roomState.activeAudioShare !== null}
      expandedSections={expandedSections}
      toggleSection={toggleSection}
      watchAllHotkeyLabel={hotkeys.watchAll}
      shareViewerWindows={{
        watchingShareIds,
        shareVolumes,
        watchAllOpen,
        getSavedShareVolume,
        getSavedShareMuted,
        syncScreenShareVolume,
        syncScreenShareMuted,
        openShareWindow,
        closeShareWindow,
        toggleWatchAllWindow,
      }}
      getRoomSnapshot={() => roomStateRef.current}
    />
  );

  const youBar = (
    <div className="shrink-0 p-4 border-t border-wavis-text-secondary">
      <div className="flex items-center gap-1 border-b border-wavis-text-secondary font-mono text-wavis-text">
        <button
          onClick={() => toggleSection('you')}
          className="bg-transparent outline-none px-1 py-1 text-xs text-wavis-text-secondary hover:opacity-80"
        >
          {expandedSections.you ? '[-]' : '[+]'}
        </button>
        <button
          onClick={() => toggleSection('you')}
          className="bg-transparent outline-none py-1 px-1 text-left flex items-center gap-2 hover:opacity-80"
        >
          <span style={{ color: selfP?.color }}>{selfP?.displayName}</span>
        </button>
        {!expandedSections.you && (
          <div className="ml-auto flex items-center leading-none text-xs">
            <button
              onClick={toggleSelfMute}
              disabled={!!selfP?.isHostMuted}
              className="px-1.5 h-5 flex items-center justify-center disabled:opacity-40 disabled:cursor-not-allowed hover:opacity-70 transition-opacity"
              style={{
                color: selfP?.isMuted ? 'var(--wavis-danger)' : 'var(--wavis-text-secondary)',
              }}
              title={selfP?.isMuted ? `/unmute (${hotkeys.mute})` : `/mute (${hotkeys.mute})`}
            >
              <span className="inline-flex w-3 h-3 items-center justify-center leading-none -translate-y-px">
                ○
              </span>
            </button>
            <span className="text-wavis-text-secondary opacity-30 select-none leading-none">│</span>
            <button
              onClick={toggleSelfDeafen}
              className="px-1.5 h-5 flex items-center justify-center hover:opacity-70 transition-opacity"
              style={{
                color: roomState.isDeafened ? 'var(--wavis-danger)' : 'var(--wavis-text-secondary)',
              }}
              title={roomState.isDeafened ? '/undeafen' : '/deafen'}
            >
              <span
                className="inline-flex w-3 h-3 items-center justify-center leading-none"
                style={{ fontSize: '1.1em' }}
              >
                ¤
              </span>
            </button>
            {showCameraButton && (
              <>
                <span className="text-wavis-text-secondary opacity-30 select-none leading-none">
                  │
                </span>
                <button
                  onClick={() => {
                    void toggleCameraIntent();
                  }}
                  disabled={cameraButtonDisabled}
                  className="px-1.5 h-5 flex items-center justify-center hover:opacity-70 transition-opacity disabled:opacity-40 disabled:cursor-not-allowed"
                  style={{
                    color: roomState.cameraIntent
                      ? 'var(--wavis-accent)'
                      : 'var(--wavis-text-secondary)',
                  }}
                  title={cameraLabel}
                  aria-label={videoButtonLabel}
                >
                  <span
                    style={{
                      display: 'inline-block',
                      width: '0.75rem',
                      height: '0.75rem',
                      backgroundColor: 'currentColor',
                      WebkitMaskImage: 'url(/video-camera.png)',
                      WebkitMaskSize: 'contain',
                      WebkitMaskRepeat: 'no-repeat',
                      WebkitMaskPosition: 'center',
                      maskImage: 'url(/video-camera.png)',
                      maskSize: 'contain',
                      maskRepeat: 'no-repeat',
                      maskPosition: 'center',
                    }}
                  />
                </button>
              </>
            )}
            <span className="text-wavis-text-secondary opacity-30 select-none leading-none">│</span>
            <button
              onClick={isVideoOrFallbackSharing ? stopShareAction : handleStartShare}
              disabled={
                !isVideoOrFallbackSharing && (!shareEnabled || sharePickerLoading || shareStarting)
              }
              className="px-1.5 h-5 flex items-center justify-center disabled:opacity-40 disabled:cursor-not-allowed hover:opacity-70 transition-opacity"
              style={{
                color: isVideoOrFallbackSharing
                  ? 'var(--wavis-danger)'
                  : 'var(--wavis-text-secondary)',
              }}
              title={isVideoOrFallbackSharing ? '/stopshare' : '/share'}
            >
              <span className="inline-flex w-3 h-3 items-center justify-center leading-none">
                ◉
              </span>
            </button>
          </div>
        )}
      </div>
      {expandedSections.you && (
        <div className="pt-2 pl-6 text-sm">
          <div className="flex flex-col gap-1 w-full">
            <div className="flex flex-col md:flex-row gap-1">
              <button
                onClick={toggleSelfMute}
                disabled={selfP?.isHostMuted}
                title={selfP?.isMuted ? `/unmute (${hotkeys.mute})` : `/mute (${hotkeys.mute})`}
                className={`flex-1 py-0.5 px-1 text-xs text-center transition-colors border disabled:opacity-40 disabled:cursor-not-allowed ${selfP?.isMuted ? 'border-wavis-danger text-wavis-danger bg-wavis-danger/8 hover:bg-wavis-danger hover:text-wavis-bg' : 'border-wavis-text-secondary text-wavis-text hover:bg-wavis-text-secondary hover:text-wavis-text-contrast'}`}
              >
                {selfP?.isMuted ? '/unmute' : '/mute'}
              </button>
              <button
                onClick={toggleSelfDeafen}
                className={`flex-1 py-0.5 px-1 text-xs text-center transition-colors border ${roomState.isDeafened ? 'border-wavis-purple text-wavis-purple hover:bg-wavis-purple hover:text-wavis-bg' : 'border-wavis-text-secondary text-wavis-text hover:bg-wavis-text-secondary hover:text-wavis-text-contrast'}`}
              >
                {roomState.isDeafened ? '/undeafen' : '/deafen'}
              </button>
            </div>
            {showCameraButton && (
              <button
                onClick={() => {
                  void toggleCameraIntent();
                }}
                disabled={cameraButtonDisabled}
                className={`w-full py-0.5 px-1 text-xs text-center transition-colors border disabled:opacity-40 disabled:cursor-not-allowed ${roomState.cameraIntent ? 'border-wavis-danger text-wavis-danger hover:bg-wavis-danger hover:text-wavis-bg' : 'border-wavis-text-secondary text-wavis-text hover:bg-wavis-text-secondary hover:text-wavis-text-contrast'}`}
              >
                {cameraLabel}
              </button>
            )}
            {(() => {
              const isVideoActive = roomState.activeVideoShare !== null;
              const isFallbackSharing =
                selfSharing && !isVideoActive && roomState.activeAudioShare === null;
              if (isVideoActive || isFallbackSharing) {
                return (
                  <>
                    <button
                      onClick={stopShareAction}
                      className="w-full py-0.5 px-1 text-xs text-center transition-colors border text-wavis-danger border-wavis-danger hover:bg-wavis-danger hover:text-wavis-bg"
                    >
                      /stopshare
                    </button>
                    {sharePickerLoading &&
                      shareLoadingNotice(
                        SHARE_PICKER_LOADING_LABEL,
                        '-mt-1 border-x border-b border-wavis-text-secondary/30 bg-wavis-panel p-2 text-xs flex items-center gap-2',
                      )}
                    {shareStarting &&
                      shareLoadingNotice(
                        SHARE_STARTING_LOADING_LABEL,
                        '-mt-1 border-x border-b border-wavis-text-secondary/30 bg-wavis-panel p-2 text-xs flex items-center gap-2',
                      )}
                    {isVideoActive && !shareStarting && (
                      <div className="border-x border-b border-wavis-text-secondary p-2 space-y-1 text-xs">
                        <div className="flex gap-1">
                          <button
                            onClick={() => {
                              if (isWindowsPlatform) {
                                void (async () => {
                                  const occupied = {
                                    videoOccupied: false,
                                    audioOccupied: roomState.activeAudioShare !== null,
                                  };
                                  const initialWithAudio =
                                    roomState.activeVideoShare?.withAudio ?? false;
                                  setWinSharePicker({
                                    enumResult: null,
                                    occupied,
                                    isChangingSource: true,
                                    initialWithAudio,
                                  });
                                  try {
                                    const enumResult = await listShareSources();
                                    setWinSharePicker((current) =>
                                      current ? { ...current, enumResult } : current,
                                    );
                                  } catch (err) {
                                    setWinSharePicker(null);
                                    const detail = err instanceof Error ? err.message : String(err);
                                    toast.error(`Screen sharing failed: ${detail}`);
                                  }
                                })();
                              } else {
                                void withPickerResize(isMacPlatform, () => changeShareSource());
                              }
                            }}
                            className="flex-1 py-0.5 px-1 text-xs text-center border border-wavis-text-secondary text-wavis-text transition-colors hover:bg-wavis-text-secondary hover:text-wavis-text-contrast"
                          >
                            /window
                          </button>
                          {(() => {
                            // Turning companion audio ON conflicts with a running audio-only share (same WASAPI device).
                            const companionBlocked =
                              !isMacPlatform &&
                              !shareAudioOn &&
                              roomState.activeAudioShare !== null;
                            const companionDisabled = isMacPlatform || companionBlocked;
                            return (
                              <button
                                onClick={() => {
                                  void (async () => {
                                    if (companionDisabled) return;
                                    const next = !shareAudioOn;
                                    const ok = await toggleShareAudio(next);
                                    if (ok) setShareAudioOn(next);
                                  })();
                                }}
                                disabled={companionDisabled}
                                title={
                                  companionBlocked
                                    ? 'audio device busy — stop your audio share first'
                                    : undefined
                                }
                                className={`flex-1 py-0.5 px-1 text-xs text-center border transition-colors ${
                                  companionDisabled
                                    ? 'cursor-not-allowed border-wavis-text-secondary text-wavis-text-secondary opacity-50'
                                    : shareAudioOn
                                      ? 'border-wavis-accent text-wavis-accent hover:bg-wavis-accent hover:text-wavis-bg'
                                      : 'border-wavis-text-secondary text-wavis-text hover:bg-wavis-text-secondary hover:text-wavis-text-contrast'
                                }`}
                              >
                                {shareAudioOn ? '/audio on' : '/audio off'}
                              </button>
                            );
                          })()}
                        </div>
                        <select
                          value={shareQualityState}
                          onChange={(e) => {
                            const q = e.target.value as 'low' | 'high' | 'max';
                            setShareQualityState(q);
                            void setShareQuality(q);
                            e.currentTarget.blur();
                          }}
                          onClick={(e) => {
                            if (e.currentTarget.dataset.open === 'true') {
                              e.currentTarget.blur();
                              e.currentTarget.dataset.open = 'false';
                            } else if (document.activeElement === e.currentTarget) {
                              e.currentTarget.dataset.open = 'true';
                            }
                          }}
                          onBlur={(e) => {
                            e.currentTarget.dataset.open = 'false';
                          }}
                          onKeyDown={(e) => {
                            if (e.key === 'Escape') e.currentTarget.blur();
                          }}
                          className="w-full bg-wavis-panel border border-wavis-text-secondary text-wavis-text text-xs py-0.5 px-1 cursor-pointer"
                        >
                          {(['low', 'high', 'max'] as const).map((q) => {
                            const label =
                              q === 'low'
                                ? 'Smooth  1080p @ 60fps'
                                : q === 'high'
                                  ? 'Sharp   1440p @ 30fps'
                                  : 'Max     1440p @ 60fps';
                            return (
                              <option key={q} value={q}>
                                {label}
                              </option>
                            );
                          })}
                        </select>
                      </div>
                    )}
                  </>
                );
              }
              const shareDisabled = !shareEnabled || sharePickerLoading || shareStarting;
              return (
                <>
                  <button
                    onClick={() => {
                      void handleStartShare();
                    }}
                    disabled={shareDisabled}
                    className="w-full py-0.5 px-1 text-xs text-center transition-colors border disabled:opacity-40 disabled:cursor-not-allowed border-wavis-purple text-wavis-purple hover:bg-wavis-purple hover:text-wavis-bg"
                  >
                    {shareButtonLabel(shareEnabled, false, roomState.sharePermission, isHost)}
                  </button>
                  {sharePickerLoading &&
                    shareLoadingNotice(
                      SHARE_PICKER_LOADING_LABEL,
                      '-mt-1 border-x border-b border-wavis-text-secondary/30 bg-wavis-panel p-2 text-xs flex items-center gap-2',
                    )}
                  {shareStarting &&
                    shareLoadingNotice(
                      SHARE_STARTING_LOADING_LABEL,
                      '-mt-1 border-x border-b border-wavis-text-secondary/30 bg-wavis-panel p-2 text-xs flex items-center gap-2',
                    )}
                </>
              );
            })()}
            {roomState.activeVideoShare !== null && roomState.activeAudioShare === null && (
              <button
                onClick={() => {
                  void handleStartShare();
                }}
                disabled={
                  !shareEnabled || sharePickerLoading || !!roomState.activeVideoShare.withAudio
                }
                title={
                  roomState.activeVideoShare.withAudio
                    ? 'audio device busy — turn off /audio first'
                    : undefined
                }
                className="w-full py-0.5 px-1 text-xs text-center transition-colors border disabled:opacity-40 disabled:cursor-not-allowed border-wavis-text-secondary text-wavis-text hover:bg-wavis-text-secondary hover:text-wavis-text-contrast"
              >
                /share audio
              </button>
            )}
            {roomState.activeAudioShare !== null && (
              <button
                onClick={() => {
                  void stopCustomShare('audio');
                }}
                className="w-full py-0.5 px-1 text-xs text-center transition-colors border border-wavis-danger text-wavis-danger hover:bg-wavis-danger hover:text-wavis-bg"
              >
                /stop-audio
              </button>
            )}
            <div className="mt-4 flex flex-col gap-1">
              <button
                onClick={() => {
                  if (showSettings) {
                    setShowSettings(false);
                  } else {
                    setShowSettings(true);
                    setChannelSwitcherOpen(false);
                  }
                }}
                className={`w-full border py-0.5 px-1 text-xs text-center transition-colors ${showSettings ? 'border-wavis-accent text-wavis-accent hover:bg-wavis-accent hover:text-wavis-bg' : 'border-wavis-text-secondary text-wavis-text hover:bg-wavis-text-secondary hover:text-wavis-text-contrast'}`}
              >
                {showSettings ? '/close-settings' : '/settings'}
              </button>
            </div>
          </div>
        </div>
      )}
      {!expandedSections.you &&
        sharePickerLoading &&
        shareLoadingNotice(
          SHARE_PICKER_LOADING_LABEL,
          'ml-6 mt-2 border border-wavis-text-secondary/30 bg-wavis-panel p-2 text-xs flex items-center gap-2',
        )}
      {!expandedSections.you &&
        shareStarting &&
        shareLoadingNotice(
          SHARE_STARTING_LOADING_LABEL,
          'ml-6 mt-2 border border-wavis-text-secondary/30 bg-wavis-panel p-2 text-xs flex items-center gap-2',
        )}
      {screenShareError && (
        <div className="mx-4 mt-2 border border-wavis-danger bg-wavis-panel p-2 text-xs text-wavis-danger flex items-start gap-2">
          <span className="flex-1 break-words">{screenShareError}</span>
          <button
            onClick={() => {
              setScreenShareError(null);
              if (shareErrorTimerRef.current) {
                clearTimeout(shareErrorTimerRef.current);
                shareErrorTimerRef.current = null;
              }
            }}
            className="shrink-0 hover:underline text-wavis-text-secondary"
            aria-label="Dismiss screen share error"
          >
            [x]
          </button>
        </div>
      )}
    </div>
  );

  const chatPanel = (
    <ChatPanel
      chatMessages={roomState.chatMessages}
      participants={roomState.participants}
      chatError={chatError}
    />
  );

  const currentPanelTab = roomState.roomPanelTab;
  const videoTilesById = roomState.videoTilesById;

  const logsContent = (
    <LogsPanel
      events={roomState.events}
      participants={roomState.participants}
      cliInput={cliInput}
      onCliInputChange={handleCliInputChange}
      onCliKeyDown={handleCliKeyDown}
      cliFocused={cliFocused}
      onCliFocus={() => setCliFocused(true)}
      onCliBlur={() => setCliFocused(false)}
      cliInputRef={cliInputRef}
    />
  );

  const videoPanel = (
    <div className="flex flex-col flex-1 min-h-0">
      <VideoTab videoTilesById={videoTilesById} />
      <div className="shrink-0 p-4 border-t border-wavis-text-secondary -translate-y-px">
        <button
          onClick={() => {
            void openVideoPopoutWindow();
          }}
          className={`w-full py-[7px] px-2 text-xs text-center transition-colors border ${videoPopoutOpen ? 'border-wavis-accent text-wavis-accent hover:bg-wavis-accent hover:text-wavis-bg' : 'border-wavis-text-secondary text-wavis-text hover:bg-wavis-text-secondary hover:text-wavis-text-contrast'}`}
        >
          /pop-out
        </button>
      </div>
    </div>
  );

  // Desktop right-panel: LOGS / VIDEOS tab switcher
  const logPanel = (
    <div className="flex-1 flex flex-col min-h-0">
      {/* ── Tab header ── */}
      <div className="flex h-[4.5rem] border-b border-wavis-text-secondary">
        {(['logs', 'video'] as const)
          .filter((tab) => !(tab === 'video' && videoPopoutOpen))
          .map((tab) => {
            const label = tab === 'logs' ? 'LOGS' : 'VIDEOS';
            const active = currentPanelTab === tab;
            return (
              <button
                key={tab}
                role="tab"
                aria-selected={active}
                onClick={() => selectRoomPanelTab(tab)}
                onDoubleClick={() => {
                  if (tab !== 'video') return;
                  selectRoomPanelTab('video');
                  void openVideoPopoutWindow();
                }}
                onKeyDown={(e) => {
                  if (e.key === 'Enter' || e.key === ' ') {
                    e.preventDefault();
                    selectRoomPanelTab(tab);
                  }
                }}
                className="flex-1 flex items-center justify-center font-bold text-sm border-r border-wavis-text-secondary last:border-r-0 transition-colors"
                style={{
                  color: active ? 'var(--wavis-accent)' : 'var(--wavis-text-secondary)',
                  backgroundColor: active ? 'rgba(46,160,67,0.08)' : 'transparent',
                }}
              >
                {label}
              </button>
            );
          })}
      </div>
      {/* ── Tab body ── */}
      {currentPanelTab === 'video' && !videoPopoutOpen ? videoPanel : logsContent}
    </div>
  );

  const groupedPanel = (
    <div className="flex-1 flex flex-col min-h-0">
      {showSettings ? (
        <Settings
          onClose={() => setShowSettings(false)}
          onNavigateAway={navigateAwayFromRoom}
          channelId={channelId}
        />
      ) : channelSwitcherOpen ? (
        <ChannelSwitcherPanel
          onChannelSelect={(ch) => {
            void handleChannelSwitch(ch);
          }}
          onClose={() => setChannelSwitcherOpen(false)}
          currentChannelId={channelId}
        />
      ) : (
        <>
          <div className="flex h-[4.5rem] border-b border-wavis-text-secondary bg-wavis-panel">
            {(['chat', 'log', 'video'] as const)
              .filter((tab) => !(tab === 'video' && videoPopoutOpen))
              .map((tab) => {
                const active = groupedPanelTab === tab;
                const label =
                  tab === 'chat'
                    ? `CHAT (${roomState.chatMessages.length})`
                    : tab === 'log'
                      ? `LOG (${roomState.events.length})`
                      : 'VIDEO';
                return (
                  <button
                    key={tab}
                    role="tab"
                    aria-selected={active}
                    onClick={() => setGroupedPanelTab(tab)}
                    onDoubleClick={() => {
                      if (tab !== 'video') return;
                      setGroupedPanelTab('video');
                      void openVideoPopoutWindow();
                    }}
                    onKeyDown={(e) => {
                      if (e.key === 'Enter' || e.key === ' ') {
                        e.preventDefault();
                        setGroupedPanelTab(tab);
                      }
                    }}
                    className="flex-1 flex items-center justify-center font-bold text-xs border-r border-wavis-text-secondary last:border-r-0 transition-colors"
                    style={{
                      color: active ? 'var(--wavis-accent)' : 'var(--wavis-text-secondary)',
                      backgroundColor: active ? 'rgba(46,160,67,0.08)' : 'transparent',
                    }}
                  >
                    {label}
                  </button>
                );
              })}
          </div>
          {groupedPanelTab === 'chat' && chatPanel}
          {groupedPanelTab === 'log' && logsContent}
          {groupedPanelTab === 'video' && !videoPopoutOpen && videoPanel}
        </>
      )}
    </div>
  );

  return (
    <div className="h-full flex flex-col bg-wavis-bg font-mono text-wavis-text">
      <Toaster
        className="wavis-room-toaster"
        position="top-center"
        duration={4000}
        closeButton
        offset={{ top: 12 }}
        mobileOffset={{ top: 8, left: 12, right: 12 }}
        gap={6}
        toastOptions={{
          closeButton: true,
          closeButtonAriaLabel: 'Close notification',
          style: { fontFamily: 'var(--font-mono)', fontSize: '0.875rem' },
        }}
      />
      {/* ═══ MOBILE LAYOUT (< md) ═══ */}
      <div className="flex flex-col flex-1 overflow-hidden md:hidden">
        {/* Compact header */}
        <div className="flex items-center gap-3 px-3 py-2 border-b border-wavis-text-secondary bg-wavis-panel">
          <div className="flex items-center gap-2 min-w-0 flex-1">
            <StatusDot color={sigDot.color} label={sigDot.label} />
            <StatusDot color={mediaDot.color} label={mediaDot.label} />
            <span className="truncate text-sm">{roomState.channelName}</span>
            <span className="shrink-0 text-[0.625rem] text-wavis-text-secondary">
              {Object.keys(roomState.participantSubRoomById).length}/6
            </span>
            <span
              className="shrink-0 text-[0.625rem]"
              style={{ color: rttColor(roomState.networkStats.rttMs) }}
            >
              {roomState.networkStats.rttMs}ms
            </span>
          </div>
          <ChannelSwitcherToggle
            channelSwitcherOpen={channelSwitcherOpen}
            onToggleChannelSwitcher={handleToggleChannelSwitcher}
          />
        </div>

        {mediaStatusBanners}

        {/* Tab bar */}
        <div className="flex border-b border-wavis-text-secondary bg-wavis-panel">
          {(['participants', 'chat', 'log', 'video'] as const)
            .filter((tab) => !(tab === 'video' && videoPopoutOpen))
            .map((tab) => {
              const active = mobileTab === tab;
              const color = active ? 'var(--wavis-accent)' : 'var(--wavis-text-secondary)';
              return (
                <button
                  key={tab}
                  onClick={() => setMobileTab(tab)}
                  onDoubleClick={() => {
                    if (tab !== 'video') return;
                    setMobileTab('video');
                    void openVideoPopoutWindow();
                  }}
                  className="flex-1 py-2 text-center border-r border-wavis-text-secondary last:border-r-0 text-xs"
                  style={{
                    color,
                    backgroundColor: active ? 'rgba(46,160,67,0.08)' : 'transparent',
                  }}
                >
                  {tab === 'participants'
                    ? `VOICE (${Object.keys(roomState.participantSubRoomById).length})`
                    : tab === 'chat'
                      ? `CHAT (${roomState.chatMessages.length})`
                      : tab === 'log'
                        ? `LOG (${roomState.events.length})`
                        : 'VIDEO'}
                </button>
              );
            })}
        </div>

        {/* Tab content */}
        <div className="flex-1 flex flex-col min-h-0 overflow-hidden">
          {showSettings ? (
            <Settings
              onClose={() => setShowSettings(false)}
              onNavigateAway={navigateAwayFromRoom}
              channelId={channelId}
            />
          ) : channelSwitcherOpen ? (
            <ChannelSwitcherPanel
              onChannelSelect={(ch) => {
                void handleChannelSwitch(ch);
              }}
              onClose={() => setChannelSwitcherOpen(false)}
              currentChannelId={channelId}
            />
          ) : (
            <>
              {mobileTab === 'participants' && (
                <div className="flex flex-col flex-1 min-h-0">
                  {participantsPanel}
                  {youBar}
                </div>
              )}
              {mobileTab === 'chat' && chatPanel}
              {mobileTab === 'log' && logsContent}
              {mobileTab === 'video' && !videoPopoutOpen && videoPanel}
            </>
          )}
        </div>
      </div>

      {/* Intermediate layout (md to 1038px) */}
      <div className="wavis-room-intermediate-layout flex-1 overflow-hidden">
        <div className="w-80 shrink-0 border-r border-wavis-text-secondary flex flex-col">
          {roomHeader}
          {mediaStatusBanners}
          {participantsPanel}
          {youBar}
        </div>
        <div className="flex-1 flex flex-col min-h-0 min-w-0 overflow-hidden">{groupedPanel}</div>
      </div>

      {/* Desktop layout (1039px+) */}
      <div className="hidden md:flex flex-1 overflow-hidden">
        <div className="w-80 border-r border-wavis-text-secondary flex flex-col">
          {roomHeader}
          {mediaStatusBanners}
          {participantsPanel}
          {youBar}
        </div>
        <div className="flex-1 flex flex-col min-h-0 min-w-0 overflow-hidden">
          {showSettings ? (
            <Settings
              onClose={() => setShowSettings(false)}
              onNavigateAway={navigateAwayFromRoom}
              channelId={channelId}
            />
          ) : channelSwitcherOpen ? (
            <ChannelSwitcherPanel
              onChannelSelect={(ch) => {
                void handleChannelSwitch(ch);
              }}
              onClose={() => setChannelSwitcherOpen(false)}
              currentChannelId={channelId}
            />
          ) : (
            chatPanel
          )}
        </div>
        <div className="w-80 border-l border-wavis-text-secondary flex flex-col">{logPanel}</div>
      </div>
      {showDriverPrompt && (
        <AudioDriverInstallPrompt
          state={driverState}
          installError={installError}
          onInstall={() => {
            void triggerInstall().then((_ok) => {
              setShowDriverPrompt(false);
              if (pendingShareRef.current) {
                pendingShareRef.current = false;
                void handleStartShare();
              }
            });
          }}
          onSkip={() => {
            skipDriverCheckRef.current = true;
            setShowDriverPrompt(false);
            if (pendingShareRef.current) {
              pendingShareRef.current = false;
              void handleStartShare();
            }
          }}
        />
      )}
      {winSharePicker && (
        <div className="fixed inset-0 z-50 flex items-center justify-center bg-wavis-bg/80 px-4">
          <div className="w-[640px] max-w-[90vw] h-[480px] max-h-[80vh] border border-wavis-text-secondary shadow-xl overflow-hidden flex flex-col">
            <SharePicker
              inline
              enumResult={winSharePicker.enumResult}
              occupied={winSharePicker.occupied}
              modeScope={winSharePicker.isChangingSource ? 'video_only' : 'all'}
              initialWithAudio={winSharePicker.initialWithAudio}
              onSelect={(selection) => {
                void (async () => {
                  const wasChangingSource = winSharePicker.isChangingSource ?? false;
                  const previousVideoShare = roomStateRef.current?.activeVideoShare ?? null;
                  setWinSharePicker(null);
                  const showStartingIndicator = isVideoShareSelectionMode(selection.mode);
                  if (showStartingIndicator) setShareStarting(true);
                  try {
                    const nextSelection = wasChangingSource
                      ? preserveVideoShareSelectionForSourceChange(selection, previousVideoShare)
                      : selection;
                    if (wasChangingSource) {
                      // keepPublication=true: skip unpublishTrack so the LiveKit
                      // publication stays alive for replaceNativeCaptureSource().
                      // Viewers never see a TrackUnpublished/TrackPublished cycle.
                      await stopCustomShare('video', {
                        suppressSignaling: true,
                        keepPublication: true,
                      });
                    }
                    await startCustomShare(nextSelection, { isSourceChange: wasChangingSource });
                    setShowPostShareAudioPrompt(false);
                    if (nextSelection.mode !== 'audio_only') {
                      setShareAudioOn(nextSelection.withAudio);
                    } else {
                      setShareAudioOn(false);
                    }
                  } catch (err) {
                    const detail = err instanceof Error ? err.message : String(err);
                    showTransientScreenShareError(`Screen sharing failed: ${detail}`);
                    toast.error(`Screen sharing failed: ${detail}`);
                  } finally {
                    if (showStartingIndicator) setShareStarting(false);
                  }
                })();
              }}
              onCancel={() => setWinSharePicker(null)}
            />
          </div>
        </div>
      )}
      {showPostShareAudioPrompt && (
        <div className="fixed inset-0 z-50 flex items-center justify-center bg-wavis-bg/80 px-4">
          <div className="w-full max-w-md border border-wavis-text-secondary bg-wavis-panel p-4 shadow-xl">
            <div className="text-sm font-bold text-wavis-text mb-2">Share system audio?</div>
            <div className="text-xs text-wavis-text-secondary mb-4">
              System audio is off by default. You can turn it on now or keep sharing video only.
            </div>
            <div className="flex gap-2 justify-end">
              <button
                onClick={() => setShowPostShareAudioPrompt(false)}
                className="border border-wavis-text-secondary text-wavis-text-secondary hover:opacity-80 transition-colors px-4 py-1 text-xs"
              >
                No
              </button>
              <div
                className="relative"
                onMouseEnter={() => {
                  if (isMacPlatform) setShowMacAudioHoverMessage(true);
                }}
                onMouseLeave={() => setShowMacAudioHoverMessage(false)}
              >
                <button
                  onClick={() => {
                    if (isMacPlatform) return;
                    setShowPostShareAudioPrompt(false);
                    setShareAudioOn(true);
                    void toggleShareAudio(true);
                  }}
                  disabled={isMacPlatform}
                  className={`border px-4 py-1 text-xs transition-colors ${
                    isMacPlatform
                      ? 'cursor-not-allowed border-wavis-text-secondary text-wavis-text-secondary opacity-50'
                      : 'border-wavis-accent text-wavis-accent hover:bg-wavis-accent hover:text-wavis-bg'
                  }`}
                >
                  Yes
                </button>
                {isMacPlatform && showMacAudioHoverMessage && (
                  <span className="absolute bottom-full right-0 mb-2 whitespace-nowrap border border-wavis-text-secondary bg-wavis-panel px-2 py-1 text-[10px] text-wavis-text shadow-lg">
                    {macShareAudioDisabledMessage}
                  </span>
                )}
              </div>
            </div>
          </div>
        </div>
      )}
    </div>
  );
}
