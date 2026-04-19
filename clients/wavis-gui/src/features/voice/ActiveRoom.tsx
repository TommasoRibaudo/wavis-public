import { useState, useEffect, useRef, useCallback } from 'react';
import { VolumeSlider } from '@shared/VolumeSlider';
import { useBlocker, useLocation, useNavigate } from 'react-router';
import type { ChannelRole } from '@features/channels/channels';
import type {
  VoiceRoomState,
  VoiceRoomMachineState,
  RoomParticipant,
  RoomEventType,
  RoomEvent,
  ShareQuality,
} from './voice-room';
import type { MediaState } from './livekit-media';
import {
  initSession,
  leaveRoom,
  toggleSelfMute,
  toggleSelfDeafen,
  stopShare,
  startCustomShare,
  stopCustomShare,
  setParticipantVolume,
  setMasterVolume,
  kickParticipant,
  muteParticipant,
  unmuteParticipant,
  createSubRoom,
  joinSubRoom,
  leaveSubRoom,
  stopParticipantShare,
  stopAllShares,
  setSharePermission,
  sendChatMessage,
  reconnectMedia,
  resetMediaReconnectFailures,
  setShareQuality,
  toggleShareAudio,
  changeShareSource,
  attachScreenShareAudio,
  detachScreenShareAudio,
  setScreenShareAudioVolume,
  activeShareType,
  computeStopRoute,
  isShareButtonDisabled,
  startFallbackShare,
  startPortalShare,
  setPendingSharePickerData,
} from './voice-room';
import type { ShareSelection, EnumerationResult } from '@features/screen-share/share-types';
import type { OccupiedSlots } from '@features/screen-share/SharePicker';
import { invoke } from '@tauri-apps/api/core';
import { getCurrentWindow } from '@tauri-apps/api/window';
import { LogicalSize } from '@tauri-apps/api/dpi';
import { Tooltip, TooltipTrigger, TooltipContent } from '../../components/ui/tooltip';
import { WebviewWindow } from '@tauri-apps/api/webviewWindow';
import { emit, emitTo, listen } from '@tauri-apps/api/event';
import { startSending, stopSending, stopSendingForWindow, stopAllSending, resendStream } from '@features/screen-share/screen-share-viewer';
import { getWatchAllHotkey } from '@features/settings/settings-store';

const DEBUG_SHARE_VIEW = import.meta.env.VITE_DEBUG_SCREEN_SHARE_VIEW === 'true';
const DEBUG_SHARE_AUDIO = import.meta.env.VITE_DEBUG_SHARE_AUDIO === 'true';
const LOG_SS = '[wavis:active-room:screen-share]';
import { registerWatchAllHotkey, unregisterWatchAllHotkey } from '@shared/hotkey-bridge';
import { listenTrayEvents, updateTrayState } from './tray-bridge';
import type { TrayAction } from './tray-bridge';
import { useDebug } from '@shared/debug-context';
import {
  connectionModeBadgeText,
  toastMessageForEvent,
  toastColorForEvent,
  eventToToggleKey,
  shouldBlockRoomNavigation,
  shouldPreventRoomNavigationGesture,
} from '@shared/helpers';
import { isShareEnabled, shareButtonLabel, appendSystemEvent } from './voice-room';
import { isNotificationEnabled } from '@features/settings/settings-store';
import {
  navigateCliHistory,
  pushCliHistory,
  resetCliHistoryNavigation,
} from './cli-history';
import { Toaster, toast } from 'sonner';
import { sendWavisNotification } from '@shared/notification-bridge';
import Settings from '@features/settings/Settings';
import { useAudioDriverInstall } from '@features/screen-share/useAudioDriverInstall';
import { AudioDriverInstallPrompt } from '@features/screen-share/AudioDriverInstallPrompt';
/* ─── Helpers ───────────────────────────────────────────────────── */

function voiceIcon(p: RoomParticipant, isDeafened?: boolean): { char: string; color: string; strikethrough?: boolean; transform?: string } {
  if (isDeafened) return { char: '¤', color: 'var(--wavis-danger)', transform: 'scale(1.25) translateY(8%)' };
  if (p.isMuted) return { char: '○', color: 'var(--wavis-danger)' };
  if (p.isSpeaking) return { char: '●', color: 'var(--wavis-accent)' };
  return { char: '○', color: 'var(--wavis-text-secondary)' };
}

function getEventColor(type: RoomEventType): string {
  switch (type) {
    case 'join': return 'var(--wavis-accent)';
    case 'leave':
    case 'kicked': return 'var(--wavis-danger)';
    case 'host-mute': return 'var(--wavis-warn)';
    case 'host-unmute': return 'var(--wavis-accent)';
    case 'share-start':
    case 'share-stop': return 'var(--wavis-purple)';
    case 'share-permission': return 'var(--wavis-warn)';
    case 'deafen': return 'var(--wavis-warn)';
    case 'undeafen': return 'var(--wavis-accent)';
    case 'muted':
    case 'unmuted': return 'var(--wavis-text)';
    default: return 'var(--wavis-text)';
  }
}

function rttColor(rttMs: number): string {
  if (rttMs < 100) return 'var(--wavis-accent)';
  if (rttMs <= 300) return 'var(--wavis-warn)';
  return 'var(--wavis-danger)';
}

function formatTime(isoString: string): string {
  try {
    const d = new Date(isoString);
    return d.toLocaleTimeString('en-US', { hour12: false });
  } catch {
    return '??:??:??';
  }
}

function getUserColor(participants: RoomParticipant[], participantId?: string): string {
  if (!participantId) return 'var(--wavis-text)';
  const p = participants.find((pp) => pp.id === participantId);
  return p?.color ?? 'var(--wavis-text)';
}

function getEventUsername(event: RoomEvent): string | null {
  const msg = event.message;
  const patterns = [' joined', ' muted', ' unmuted', ' started', ' stopped', ' was kicked', ' was muted', ' was unmuted'];
  for (const pat of patterns) {
    const idx = msg.indexOf(pat);
    if (idx > 0) return msg.slice(0, idx);
  }
  return null;
}

type ShareViewerScope = 'direct' | 'watch-all';

interface ShareViewerWindow {
  scope: ShareViewerScope;
  window: WebviewWindow;
}

/* ─── Sub-components ────────────────────────────────────────────── */

function signalingIndicator(state: VoiceRoomMachineState): { color: string; label: string } {
  switch (state) {
    case 'active': return { color: 'var(--wavis-accent)', label: 'Signaling: connected' };
    case 'connecting':
    case 'authenticated':
    case 'joining': return { color: 'var(--wavis-warn)', label: 'Signaling: connecting...' };
    case 'reconnecting': return { color: 'var(--wavis-warn)', label: 'Signaling: reconnecting...' };
    case 'idle':
    default: return { color: 'var(--wavis-text-secondary)', label: 'Signaling: disconnected' };
  }
}

function mediaIndicator(state: MediaState, error: string | null): { color: string; label: string } {
  switch (state) {
    case 'connected': return { color: 'var(--wavis-accent)', label: 'Media: connected' };
    case 'connecting': return { color: 'var(--wavis-warn)', label: 'Media: connecting...' };
    case 'failed': return { color: 'var(--wavis-danger)', label: `Media: failed${error ? ` — ${error}` : ''}` };
    case 'disconnected':
    default: return { color: 'var(--wavis-text-secondary)', label: 'Media: disconnected' };
  }
}

function combinedStatusBadge(
  machine: VoiceRoomMachineState,
  media: MediaState,
): { text: string; color: string } {
  // Failed media takes priority
  if (media === 'failed') return { text: 'FAILED', color: 'var(--wavis-danger)' };
  // Both fully connected = live
  if (machine === 'active' && media === 'connected') return { text: 'LIVE', color: 'var(--wavis-accent)' };
  // Reconnecting signaling
  if (machine === 'reconnecting') return { text: 'RECONNECTING', color: 'var(--wavis-warn)' };
  // Any connecting state
  if (
    machine === 'connecting' || machine === 'authenticated' || machine === 'joining' ||
    media === 'connecting'
  ) return { text: 'CONNECTING', color: 'var(--wavis-warn)' };
  // Idle / disconnected
  return { text: 'OFFLINE', color: 'var(--wavis-text-secondary)' };
}

function StatusDot({ color, label }: { color: string; label: string }) {
  const isAnimating = label.includes('connecting') || label.includes('reconnecting');
  return (
    <Tooltip>
      <TooltipTrigger asChild>
        <span
          className="inline-block w-2 h-2 rounded-full cursor-default"
          style={{
            backgroundColor: color,
            boxShadow: color === 'var(--wavis-accent)' ? `0 0 6px ${color}` : undefined,
            animation: isAnimating ? 'pulse 3s ease-in-out infinite' : undefined,
          }}
          aria-label={label}
        />
      </TooltipTrigger>
      <TooltipContent side="bottom" className="bg-wavis-panel text-wavis-text border border-wavis-text-secondary font-mono text-xs">
        {label}
      </TooltipContent>
    </Tooltip>
  );
}


/**
 * Temporarily expands the Tauri window before a native getDisplayMedia picker
 * dialog opens so the picker's 2-column grid is never clipped on narrow windows,
 * then restores the original size when the dialog closes (or if it throws).
 * No-op on macOS — that platform opens getDisplayMedia as a system dialog
 * outside the WebView, so window width is irrelevant there.
 */
async function withPickerResize<T>(isMacPlatform: boolean, fn: () => Promise<T>): Promise<T> {
  const MIN_NATIVE_PICKER_WIDTH = 700;
  const originalWidth = window.innerWidth;
  const originalHeight = window.innerHeight;
  const targetWidth = Math.min(MIN_NATIVE_PICKER_WIDTH, window.screen.availWidth - 20);
  const needsResize = !isMacPlatform && originalWidth < targetWidth;
  if (needsResize) {
    await getCurrentWindow().setSize(new LogicalSize(targetWidth, originalHeight));
  }
  try {
    return await fn();
  } finally {
    if (needsResize) {
      await getCurrentWindow().setSize(new LogicalSize(originalWidth, originalHeight));
    }
  }
}

/* ═══ Component ═════════════════════════════════════════════════════ */

export default function ActiveRoom() {
  const location = useLocation();
  const navigate = useNavigate();
  const { channelId, channelName, channelRole } =
    (location.state as { channelId: string; channelName: string; channelRole: ChannelRole }) ?? {};

  const [roomState, setRoomState] = useState<VoiceRoomState | null>(null);

  const [leaving, setLeaving] = useState(false);
  const [cliInput, setCliInput] = useState('');
  const [chatInput, setChatInput] = useState('');
  const logEndRef = useRef<HTMLDivElement>(null);
  const chatEndRef = useRef<HTMLDivElement>(null);
  const cliInputRef = useRef<HTMLInputElement>(null);
  const pendingCliFocus = useRef(false);

  // Focus whichever CLI input is currently visible in the DOM.
  // Both mobile and desktop layouts may render logPanel simultaneously (same JSX const,
  // same cliInputRef). When the mobile logPanel mounts it captures cliInputRef.current,
  // but in desktop mode that element lives inside an md:hidden (display:none) container
  // and browsers silently ignore focus() on hidden elements. offsetParent === null when
  // an element or any ancestor has display:none, so we use it as a visibility guard and
  // fall back to a data-attribute DOM query to find the actually-visible input.
  function focusCliInput() {
    const el = cliInputRef.current;
    if (el && el.offsetParent !== null) {
      el.focus();
      return;
    }
    const inputs = document.querySelectorAll<HTMLInputElement>('[data-cli-input]');
    for (const input of inputs) {
      if (input.offsetParent !== null) {
        input.focus();
        return;
      }
    }
  }
  const initRef = useRef(false);
  const allowNavigationRef = useRef(false);
  const chatThrottledRef = useRef(false);
  const [cliFocused, setCliFocused] = useState(false);
  const cliHistoryRef = useRef<string[]>([]);
  const cliHistoryIndexRef = useRef(-1);
  const cliDraftRef = useRef('');

  const [showSettings, setShowSettings] = useState(false);

  // Transient chat error display (auto-dismiss after 5s)
  const [chatError, setChatError] = useState<string | null>(null);
  const chatErrorTimerRef = useRef<ReturnType<typeof setTimeout> | null>(null);
  const prevChatErrorRef = useRef<string | null>(null);

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

  // Per-participant expanded host controls
  const [expandedUser, setExpandedUser] = useState<string | null>(null);

  // Mobile tab state
  type MobileTab = 'participants' | 'chat' | 'log';
  const [mobileTab, setMobileTab] = useState<MobileTab>('participants');

  const blocker = useBlocker(({ currentLocation, nextLocation }) =>
    shouldBlockRoomNavigation(
      currentLocation.pathname,
      nextLocation.pathname,
      allowNavigationRef.current,
    ));

  const navigateAwayFromRoom = useCallback(
    (target: string, shouldLeave = false) => {
      allowNavigationRef.current = true;
      if (shouldLeave) leaveRoom();
      navigate(target);
    },
    [navigate],
  );

  // Guard: no channelId → redirect home
  useEffect(() => {
    if (!channelId) {
      allowNavigationRef.current = true;
      navigate('/');
    }
  }, [channelId, navigate]);

  // Keep the room mounted unless the app explicitly allows navigation out.
  useEffect(() => {
    if (blocker.state === 'blocked') {
      blocker.reset();
    }
  }, [blocker]);

  // Suppress hardware/browser back gestures before they trigger navigation.
  useEffect(() => {
    const onMouseNavigation = (event: MouseEvent) => {
      if (!shouldPreventRoomNavigationGesture({ button: event.button })) return;
      event.preventDefault();
      event.stopPropagation();
    };

    const onKeyNavigation = (event: KeyboardEvent) => {
      if (!shouldPreventRoomNavigationGesture({ key: event.key, altKey: event.altKey })) return;
      event.preventDefault();
      event.stopPropagation();
    };

    window.addEventListener('mousedown', onMouseNavigation, true);
    window.addEventListener('mouseup', onMouseNavigation, true);
    window.addEventListener('auxclick', onMouseNavigation, true);
    window.addEventListener('keydown', onKeyNavigation, true);
    return () => {
      window.removeEventListener('mousedown', onMouseNavigation, true);
      window.removeEventListener('mouseup', onMouseNavigation, true);
      window.removeEventListener('auxclick', onMouseNavigation, true);
      window.removeEventListener('keydown', onKeyNavigation, true);
    };
  }, []);

  // Session init + cleanup
  useEffect(() => {
    if (!channelId || initRef.current) return;
    initRef.current = true;
    allowNavigationRef.current = false;
    initSession(channelId, channelName, channelRole, setRoomState);
    return () => {
      leaveRoom();
      initRef.current = false;
      prevEventsLenRef.current = 0;
    };
  }, [channelId, channelName, channelRole]);

  // Event log auto-scroll
  useEffect(() => {
    logEndRef.current?.scrollIntoView({ behavior: 'smooth' });
  }, [roomState?.events.length]);

  // Chat auto-scroll
  useEffect(() => {
    chatEndRef.current?.scrollIntoView({ behavior: 'smooth' });
  }, [roomState?.chatMessages.length]);

  // Transient chat error: show when chat panel is visible, auto-dismiss after 5s
  useEffect(() => {
    const err = roomState?.lastChatError ?? null;
    if (err === prevChatErrorRef.current) return;
    prevChatErrorRef.current = err;
    if (!err) return;

    // On mobile, drop errors when chat tab is not active
    // On desktop (md+), chat panel is always visible — use matchMedia to detect
    const isDesktop = window.matchMedia('(min-width: 768px)').matches;
    if (!isDesktop && mobileTab !== 'chat') {
      return; // silently drop
    }

    // Clear any existing timer
    if (chatErrorTimerRef.current) clearTimeout(chatErrorTimerRef.current);

    setChatError(err);
    chatErrorTimerRef.current = setTimeout(() => {
      setChatError(null);
      chatErrorTimerRef.current = null;
    }, 5000);
  }, [roomState?.lastChatError, mobileTab]);

  // Clean up chat error timer on unmount
  useEffect(() => {
    return () => {
      if (chatErrorTimerRef.current) clearTimeout(chatErrorTimerRef.current);
    };
  }, []);

  // Tray event wiring: dispatch tray menu actions to voice room
  useEffect(() => {
    const cleanup = listenTrayEvents((action: TrayAction) => {
      switch (action) {
        case 'toggle-mute':
          toggleSelfMute();
          break;
        case 'leave':
          navigateAwayFromRoom('/', true);
          break;
        case 'show':
          // handled by Rust side (window.show + set_focus)
          break;
      }
    });
    return cleanup;
  }, [channelId, navigateAwayFromRoom]);

  // Tray state sync: update tray menu items when voice/mute state changes
  useEffect(() => {
    if (!roomState) return;
    const selfP = roomState.participants.find((p) => p.id === roomState.selfParticipantId);
    const inVoice = roomState.machineState === 'active';
    updateTrayState({
      inVoiceSession: inVoice,
      isMuted: selfP?.isMuted ?? false,
    });
  }, [roomState?.machineState, roomState?.participants, roomState?.selfParticipantId]);

  // Send "not in voice" on unmount so tray items get disabled
  useEffect(() => {
    return () => {
      updateTrayState({ inVoiceSession: false, isMuted: false });
    };
  }, []);

  // Toast notifications for new room events
  useEffect(() => {
    if (!roomState) return;
    const events = roomState.events;
    const prevLen = prevEventsLenRef.current;
    prevEventsLenRef.current = events.length;
    if (prevLen === 0 || events.length <= prevLen) return;
    const newEvents = events.slice(prevLen);
    for (const ev of newEvents) {
      const name = ev.message.split(' ')[0] ?? '';
      const msg = toastMessageForEvent(ev.type, name);
      if (!msg) continue;
      const toggleKey = eventToToggleKey(ev.type);
      if (toggleKey) {
        isNotificationEnabled(toggleKey).then((enabled) => {
          if (!enabled) return;
          toast(msg, {
            style: { borderLeft: `3px solid ${toastColorForEvent(ev.type)}`, fontFamily: 'var(--font-mono)', fontSize: '0.875rem' },
          });
        });
        // Also send native notification (gated by visibility + toggle inside sendWavisNotification)
        sendWavisNotification(toggleKey, msg);
      } else {
        toast(msg, {
          style: { borderLeft: `3px solid ${toastColorForEvent(ev.type)}`, fontFamily: 'var(--font-mono)', fontSize: '0.875rem' },
        });
      }
    }
  }, [roomState?.events.length]);

  // Global `/` shortcut: focus CLI input from anywhere (unless chat is focused)
  useEffect(() => {
    const handler = (e: KeyboardEvent) => {
      if (e.key !== '/') return;
      // Don't steal focus when the bug report modal is open.
      if (document.querySelector('[data-bug-report-modal]')) return;
      const active = document.activeElement;
      // If typing in the chat input and it's empty, redirect `/` to the CLI input.
      // If the chat input already has text, let the user type normally.
      if (active instanceof HTMLInputElement || active instanceof HTMLTextAreaElement) {
        // Already on a CLI input → let the user type normally.
        // Use the data attribute rather than cliInputRef.current because both
        // layouts render logPanel; the ref may point to the hidden one.
        if (!active.hasAttribute('data-cli-input') && (active as HTMLInputElement).value === '') {
          e.preventDefault();
          (active as HTMLElement).blur();
          pendingCliFocus.current = true;
          setCliInput('/');
          setMobileTab('log');
          // Fallback: direct focus after React commit + paint
          requestAnimationFrame(() => focusCliInput());
        }
        return;
      }
      e.preventDefault();
      pendingCliFocus.current = true;
      setCliInput('/');
      // In mobile/tabbed layout the CLI input lives in the log tab —
      // switch to it first so the input is rendered and visible.
      setMobileTab('log');
      // Fallback: direct focus after React commit + paint.
      // Covers the edge case where cliInput was already '/' (no state
      // change → useEffect doesn't re-fire), and also races the effect
      // to whichever lands first in Tauri's webview.
      requestAnimationFrame(() => focusCliInput());
    };
    window.addEventListener('keydown', handler);
    return () => window.removeEventListener('keydown', handler);
  }, []);

  // Drive CLI focus from React's commit phase.
  // Two paths ensure focus lands reliably in Tauri's webview:
  // 1. This effect fires when cliInput changes (covers the normal case).
  // 2. The keydown handler also schedules a rAF + microtask focus as a
  //    fallback — covers the case where setCliInput('/') is a no-op
  //    (value already '/') so this effect never re-runs.
  useEffect(() => {
    if (pendingCliFocus.current) {
      pendingCliFocus.current = false;
      focusCliInput();
    }
  }, [cliInput]);

  // Watch All window state
  const watchAllWindowRef = useRef<WebviewWindow | null>(null);
  const watchAllReadyUnlistenRef = useRef<(() => void) | null>(null);
  const [watchAllOpen, setWatchAllOpen] = useState(false);
  const watchAllReadyRef = useRef(false);
  const watchAllHotkeyRef = useRef<string | null>(null);
  const toggleWatchAllRef = useRef<() => void>(() => { });

  // Screen share window state (multi-window: one per sharer)
  const [watchingShareIds, setWatchingShareIds] = useState<Set<string>>(new Set());
  const [shareVolumes, setShareVolumes] = useState<Map<string, number>>(new Map());
  const shareVolumesRef = useRef(shareVolumes);
  const watchAllVolumesRef = useRef<Map<string, number>>(new Map());
  const watchAllAttachedAudioRef = useRef<Set<string>>(new Set());
  const [shareQualityState, setShareQualityState] = useState<ShareQuality>('high');
  const [shareAudioOn, setShareAudioOn] = useState(false);
  const [showPostShareAudioPrompt, setShowPostShareAudioPrompt] = useState(false);
  const [showMacAudioHoverMessage, setShowMacAudioHoverMessage] = useState(false);
// Screen share error toast (auto-dismisses after 5s)
  const [screenShareError, setScreenShareError] = useState<string | null>(null);
  const shareErrorTimerRef = useRef<ReturnType<typeof setTimeout> | null>(null);
  const shareEnumerating = useRef(false);
  // True while waiting for the OS screen picker to appear (macOS/Windows getDisplayMedia)
  const [sharePickerLoading, setSharePickerLoading] = useState(false);
  // macOS audio driver install prompt state
  const [showDriverPrompt, setShowDriverPrompt] = useState(false);
  const pendingShareRef = useRef<boolean>(false);
  // Set to true when the user explicitly skips driver install so handleStartShare bypasses the check once.
  const skipDriverCheckRef = useRef(false);
  const wasSelfSharingRef = useRef(false);
  // Toast notification tracking
  const prevEventsLenRef = useRef(0);
  // Refs to the screen share OS windows (keyed by participantId)
  const shareWindowsRef = useRef<Map<string, ShareViewerWindow>>(new Map());
  const selfSharingRef = useRef(false);
  const handleStartShareRef = useRef<() => void | Promise<void>>(() => {});
  const stopShareActionRef = useRef<() => void>(() => {});
  const shareUserStateRef = useRef({
    isMuted: false,
    isDeafened: false,
    isSharing: false,
    shareEnabled: false,
  });
  const voiceParticipantsRef = useRef({
    participants: [] as Array<{
      id: string;
      name: string;
      color: string;
      volume: number;
      muted: boolean;
    }>,
  });
  const watchAllVoiceParticipantsRef = useRef({
    participants: [] as Array<{
      id: string;
      name: string;
      color: string;
      volume: number;
      muted: boolean;
    }>,
  });

  const showTransientScreenShareError = useCallback((message: string) => {
    if (shareErrorTimerRef.current) clearTimeout(shareErrorTimerRef.current);
    setScreenShareError(message);
    shareErrorTimerRef.current = setTimeout(() => {
      setScreenShareError(null);
      shareErrorTimerRef.current = null;
    }, 5000);
  }, []);

  // Close share windows when watched participants stop sharing
  useEffect(() => {
    if (!roomState) return;
    for (const id of watchingShareIds) {
      const stillSharing = roomState.participants.some((p) => p.id === id && p.isSharing);
      if (!stillSharing) {
        closeShareWindow(id);
      }
    }
  }, [watchingShareIds, roomState?.participants]);

  // Re-send stream through loopback bridge when the underlying MediaStream
  // changes for an already-open viewer window (e.g. after LiveKit adaptive
  // stream pause→resume re-emits onScreenShareSubscribed with a fresh stream).
  useEffect(() => {
    shareVolumesRef.current = shareVolumes;
  }, [shareVolumes]);

  const prevStreamsRef = useRef<Map<string, MediaStream | null>>(new Map());
  useEffect(() => {
    if (!roomState) return;
    for (const id of watchingShareIds) {
      const current = roomState.screenShareStreams.get(id) ?? null;
      const prev = prevStreamsRef.current.get(id) ?? null;
      if (current && current !== prev) {
        console.log(LOG_SS, `resendStream(${id}, screen-share-${id}) — stream: ${current?.id}, prevStream: ${prev?.id ?? 'none'}, active: ${current?.active}, ts: ${Date.now()}`);
        resendStream(id, `screen-share-${id}`, current);
      }
    }
    prevStreamsRef.current = new Map(roomState.screenShareStreams);
  }, [watchingShareIds, roomState?.screenShareStreams]);

  const getSavedShareVolume = useCallback((participantId: string) => {
    return shareVolumesRef.current.get(participantId) ?? watchAllVolumesRef.current.get(participantId) ?? 70;
  }, []);

  const syncScreenShareVolume = useCallback((participantId: string, volume: number) => {
    setShareVolumes((prev) => {
      if (prev.get(participantId) === volume) return prev;
      const next = new Map(prev);
      next.set(participantId, volume);
      return next;
    });
    watchAllVolumesRef.current.set(participantId, volume);
    setScreenShareAudioVolume(participantId, volume);
    emit('watch-all:restore-volume', { participantId, volume });
    emit('screen-share:restore-volume', { participantId, volume });
  }, []);

  const emitWatchAllRestoreVolume = useCallback((participantId: string) => {
    emit('watch-all:restore-volume', {
      participantId,
      volume: getSavedShareVolume(participantId),
    });
  }, [getSavedShareVolume]);

  const getWatchAllScope = useCallback((currentState: VoiceRoomState | null) => {
    if (!currentState || !currentState.joinedSubRoomId) {
      return {
        participantIds: new Set<string>(),
        participants: [] as RoomParticipant[],
        remoteSharers: [] as RoomParticipant[],
        streams: new Map<string, MediaStream | null>(),
      };
    }

    const currentRoom = currentState.subRooms.find((subRoom) => subRoom.id === currentState.joinedSubRoomId);
    const participantIds = new Set(currentRoom?.participantIds ?? []);
    const participants = currentState.participants.filter((participant) => participantIds.has(participant.id));
    const remoteSharers = participants.filter((participant) => participant.isSharing && participant.id !== currentState.selfParticipantId);
    const streams = new Map(
      [...currentState.screenShareStreams].filter(([participantId]) => participantIds.has(participantId)),
    );

    return { participantIds, participants, remoteSharers, streams };
  }, []);

  const handleViewerReady = useCallback((participantId: string, windowLabel: string) => {
    const rs = roomStateRef.current;
    if (!rs || !rs.screenShareStreams.has(participantId)) return;

    if (windowLabel === 'watch-all') {
      if (!watchAllWindowRef.current || !watchAllReadyRef.current) return;
      if (shareWindowsRef.current.has(participantId)) return;
      attachScreenShareAudio(participantId);
      setScreenShareAudioVolume(participantId, getSavedShareVolume(participantId));
      watchAllAttachedAudioRef.current.add(participantId);
      void emit('share:user-state', shareUserStateRef.current);
      void emit('watch-all:voice-participants', watchAllVoiceParticipantsRef.current);
      return;
    }

    const shareWindow = shareWindowsRef.current.get(participantId);
    if (!shareWindow || shareWindow.window.label !== windowLabel) return;
    attachScreenShareAudio(participantId);
    setScreenShareAudioVolume(participantId, getSavedShareVolume(participantId));
    void emit('share:user-state', shareUserStateRef.current);
    void emit('share:voice-participants', voiceParticipantsRef.current);
  }, [getSavedShareVolume]);

  /** Re-add a participant's stream to the Watch All grid after their pop-out closes. */
  const reAddStreamToWatchAll = (participantId: string) => {
    if (!watchAllWindowRef.current || !watchAllReadyRef.current) return;
    const rs = roomStateRef.current;
    const scope = getWatchAllScope(rs);
    if (!scope.streams.has(participantId)) return;
    const stream = scope.streams.get(participantId) ?? null;
    const participant = scope.participants.find((p) => p.id === participantId);
    if (!participant) return;
    if (stream) {
      if (DEBUG_SHARE_VIEW) console.log(LOG_SS, `startSending(${participantId}, 'watch-all') — stream: ${stream?.id}, active: ${stream?.active}`);
      startSending(participantId, 'watch-all', stream);
    }
    emit('watch-all:share-added', {
      participantId,
      displayName: participant.displayName,
      color: participant.color,
      canvasFallback: stream === null,
    });
    emitWatchAllRestoreVolume(participantId);
    prevWatchAllStreamsRef.current.set(participantId, stream);
  };

  const handleShareWindowClosed = (participantId: string) => {
    // The map entry marks which pop-out currently owns this participant.
    // If another close path already removed it, skip duplicate cleanup.
    if (!shareWindowsRef.current.delete(participantId)) return;
    stopSending(participantId, `screen-share-${participantId}`);
    detachScreenShareAudio(participantId);
    setWatchingShareIds((prev) => {
      const next = new Set(prev);
      next.delete(participantId);
      return next;
    });
    reAddStreamToWatchAll(participantId);
  };

  // Listen for child windows closing themselves
  useEffect(() => {
    const unlisten = listen<{ participantId: string }>('screen-share:closed', (event) => {
      const pid = event.payload.participantId;
      // Gate on delete — if closeShareWindow already handled this pid,
      // delete() returns false and we skip to avoid double-add.
      if (!shareWindowsRef.current.delete(pid)) return;
      stopSending(pid, `screen-share-${pid}`);
      detachScreenShareAudio(pid);
      setWatchingShareIds((prev) => {
        const next = new Set(prev);
        next.delete(pid);
        return next;
      });
      reAddStreamToWatchAll(pid);
    });
    return () => { unlisten.then((fn) => fn()); };
  }, []);

  // Listen for owner actions from the child window
  useEffect(() => {
    const cleanups: Array<Promise<() => void>> = [];

    cleanups.push(
      listen<{ quality: ShareQuality }>('screen-share:quality', (event) => {
        setShareQualityState(event.payload.quality);
        setShareQuality(event.payload.quality);
      }),
    );
    cleanups.push(
      listen<{ withAudio: boolean }>('screen-share:toggle-audio', (event) => {
        if (DEBUG_SHARE_AUDIO) {
          console.log('[wavis:active-room] [share-audio] screen-share:toggle-audio received', {
            withAudio: event.payload.withAudio,
            userActivationIsActive: (navigator as { userActivation?: { isActive: boolean } }).userActivation?.isActive,
          });
        }
        if (isMacPlatform && event.payload.withAudio) {
          setShareAudioOn(false);
          return;
        }
        setShareAudioOn(event.payload.withAudio);
        toggleShareAudio(event.payload.withAudio);
      }),
    );
    cleanups.push(
      listen('screen-share:change-source', async () => {
        await withPickerResize(isMacPlatform, () => changeShareSource());
      }),
    );
    cleanups.push(
      listen<{ participantId: string; volume: number }>('watch-all:volume-change', (event) => {
        const { participantId, volume } = event.payload;
        syncScreenShareVolume(participantId, volume);
      }),
    );
    cleanups.push(
      listen<{ participantId: string; volume: number }>('screen-share:volume-change', (event) => {
        const { participantId, volume } = event.payload;
        syncScreenShareVolume(participantId, volume);
      }),
    );
    cleanups.push(
      listen<{ participantId: string; volume: number }>('share:voice-volume-change', (event) => {
        const { participantId, volume } = event.payload;
        setParticipantVolume(participantId, volume);
      }),
    );
    cleanups.push(
      listen('share:toggle-mute', () => {
        toggleSelfMute();
      }),
    );
    cleanups.push(
      listen('share:toggle-deafen', () => {
        toggleSelfDeafen();
      }),
    );
    cleanups.push(
      listen('share:toggle-share', () => {
        if (selfSharingRef.current) {
          stopShareActionRef.current();
        } else {
          void handleStartShareRef.current();
        }
      }),
    );

    return () => {
      for (const p of cleanups) p.then((fn) => fn());
    };
  }, [syncScreenShareVolume]); // syncScreenShareVolume is stable (useCallback[]) but listed for exhaustive-deps

  useEffect(() => {
    const unlisten = listen<{ participantId: string; windowLabel: string }>('screen-share-viewer:ready', (event) => {
      handleViewerReady(event.payload.participantId, event.payload.windowLabel);
    });
    return () => { unlisten.then((fn) => fn()); };
  }, [handleViewerReady]);

  // Watch All: listen for close event from WatchAllPage
  useEffect(() => {
    const unlisten = listen('watch-all:closed', () => {
      closeWatchAllWindow();
    });
    return () => { unlisten.then((fn) => fn()); };
  }, []);

  // Screen share: listen for pop-back-in request from ScreenSharePage
  // Only acts when Watch All is open — otherwise double-click is a no-op.
  useEffect(() => {
    const unlisten = listen<{ participantId: string }>('screen-share:pop-back-in', (event) => {
      const pid = event.payload.participantId;
      if (!watchAllWindowRef.current || !watchAllReadyRef.current) return;
      handleShareWindowClosed(pid); // deletes from shareWindowsRef, re-adds to watch-all
      // Tell the child window to close itself — emitTo is reliable; win.close() from parent is not.
      // screen-share:closed will fire but is a no-op since the map entry was already deleted above.
      emitTo(`screen-share-${pid}`, 'screen-share:close', {}).catch(() => {});
    });
    return () => { unlisten.then((fn) => fn()); };
  }, []);

  // Watch All: listen for pop-out request from WatchAllPage
  useEffect(() => {
    const unlisten = listen<{ participantId: string; volume?: number }>('watch-all:pop-out', (event) => {
      const pid = event.payload.participantId;
      if (typeof event.payload.volume === 'number') {
        syncScreenShareVolume(pid, event.payload.volume);
      }
      const rs = roomStateRef.current;
      const participant = rs?.participants.find((p) => p.id === pid);
      if (!participant) return;
      // If already open, bring to foreground
      const existingWin = shareWindowsRef.current.get(pid);
      if (existingWin) {
        existingWin.window.setFocus();
        return;
      }
      // openShareWindow handles removing the tile from Watch All grid
      openShareWindow(pid, participant, rs?.screenShareStreams.get(pid) ?? null, 'watch-all');
    });
    return () => { unlisten.then((fn) => fn?.()); };
  }, [syncScreenShareVolume]);

  useEffect(() => {
    const unlisten = listen<{ participantId: string }>('watch-all:request-resend', (event) => {
      const pid = event.payload.participantId;
      if (!watchAllWindowRef.current) return;
      if (shareWindowsRef.current.has(pid)) return;
      const rs = roomStateRef.current;
      const scope = getWatchAllScope(rs);
      if (!scope.streams.has(pid)) return;
      const stream = scope.streams.get(pid) ?? null;
      if (!stream) return;
      if (DEBUG_SHARE_VIEW) console.log(LOG_SS, `watch-all resend requested for ${pid} â€” stream: ${stream.id}, active: ${stream.active}`);
      resendStream(pid, 'watch-all', stream).catch((err) => {
        console.warn('[wavis:active-room] watch-all resend failed:', err);
      });
    });
    return () => { unlisten.then((fn) => fn?.()); };
  }, []);

  // Dynamic share tracking for Watch All window
  const prevWatchAllStreamsRef = useRef<Map<string, MediaStream | null>>(new Map());
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
        // their stream is already being sent to the pop-out window.
        if (shareWindowsRef.current.has(pid)) continue;
        // New participant started sharing
        if (stream) {
          if (DEBUG_SHARE_VIEW) console.log(LOG_SS, `startSending(${pid}, 'watch-all') — stream: ${stream?.id}, active: ${stream?.active}`);
          startSending(pid, 'watch-all', stream);
        }
        const participant = scope.participants.find((p) => p.id === pid);
        if (participant) {
          emit('watch-all:share-added', {
            participantId: pid,
            displayName: participant.displayName,
            color: participant.color,
            canvasFallback: stream === null,
          });
          emitWatchAllRestoreVolume(pid);
        }
      } else {
        // Existing participant — check if stream reference changed
        // Skip if this participant has an individual pop-out window
        if (shareWindowsRef.current.has(pid)) continue;
        const prevStream = prevStreams.get(pid) ?? null;
        if (stream && stream !== prevStream) {
          console.log(LOG_SS, `resendStream(${pid}, 'watch-all') — stream: ${stream?.id}, prevStream: ${prevStream?.id ?? 'none'}, active: ${stream?.active}, ts: ${Date.now()}`);
          resendStream(pid, 'watch-all', stream);
        }
      }
    }

    // Removed shares: in prev but not in current
    for (const pid of prevStreams.keys()) {
      if (!currentStreams.has(pid)) {
        detachScreenShareAudio(pid);
        stopSending(pid, 'watch-all');
        watchAllAttachedAudioRef.current.delete(pid);
        emit('watch-all:share-removed', { participantId: pid });
      }
    }

    prevWatchAllStreamsRef.current = new Map(currentStreams);
  }, [getWatchAllScope, watchAllOpen, roomState?.screenShareStreams, roomState?.participants, roomState?.joinedSubRoomId, roomState?.subRooms]);

  // Watch All: emit share-updated when participant info changes
  const prevParticipantsRef = useRef<Map<string, { displayName: string; color: string }>>(new Map());
  useEffect(() => {
    if (!roomState || !watchAllOpen) return;

    const sharers = getWatchAllScope(roomState).remoteSharers;
    for (const p of sharers) {
      const prev = prevParticipantsRef.current.get(p.id);
      if (prev && (prev.displayName !== p.displayName || prev.color !== p.color)) {
        emit('watch-all:share-updated', {
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
  }, [getWatchAllScope, watchAllOpen, roomState?.participants, roomState?.joinedSubRoomId, roomState?.subRooms]);

  // Custom share picker + indicator event listeners
  useEffect(() => {
    const cleanups: Array<Promise<() => void>> = [];

    // Share picker selection → start custom share
    cleanups.push(
      listen<ShareSelection>('share-picker:selected', async (event) => {
        setPendingSharePickerData(null);
        try {
          await startCustomShare(event.payload);
        } catch (err) {
          const msg = err instanceof Error ? err.message : String(err);
          showTransientScreenShareError(msg);
        }
      }),
    );

    // Share picker cancelled → clear pending data
    cleanups.push(
      listen('share-picker:cancelled', () => {
        setPendingSharePickerData(null);
      }),
    );

    // Share indicator stop button (now with target: 'video' | 'audio' | 'all')
    cleanups.push(
      listen<{ target?: 'video' | 'audio' | 'all' }>('share-indicator:stop', (event) => {
        const target = event.payload?.target ?? 'all';
        stopCustomShare(target);
      }),
    );

    // Rust-side share error (PipeWire disconnect, window closed, etc.)
    cleanups.push(
      listen<string>('share_error', async (event) => {
        await stopCustomShare();
        showTransientScreenShareError(event.payload);
      }),
    );

    return () => {
      for (const p of cleanups) p.then((fn) => fn());
    };
  }, [showTransientScreenShareError]); // listeners surface share errors via shared timer helper

  // Cleanup all share windows on unmount / leave
  useEffect(() => {
    return () => {
      closeAllShareWindows();
    };
  }, []);

  // When the main window is actually closing (not minimized to tray),
  // tear down the voice session and close all child windows so nothing
  // is orphaned. The Rust on_window_event handler emits this event.
  useEffect(() => {
    const unlisten = listen('main-window-closing', () => {
      closeAllShareWindows();
      leaveRoom();
    });
    return () => { unlisten.then((fn) => fn()); };
  }, []);

  /** Open a real OS window for a screen share viewer. Supports multiple simultaneous windows. */
  const openShareWindow = async (
    participantId: string,
    participant: RoomParticipant,
    stream: MediaStream | null,
    scope: ShareViewerScope = 'direct',
  ) => {
    // If already watching this participant, close it first and wait for Tauri to
    // destroy the webview before creating a new one with the same label.
    if (shareWindowsRef.current.has(participantId)) {
      const oldWin = shareWindowsRef.current.get(participantId)!;
      closeShareWindow(participantId);
      await new Promise<void>((resolve) => {
        const timeout = setTimeout(resolve, 1000);
        oldWin.window.once('tauri://destroyed', () => { clearTimeout(timeout); resolve(); });
      });
    }

    const isSelf = participantId === roomState?.selfParticipantId;
    const params = {
      participantId,
      username: participant.displayName,
      userColor: participant.color,
      isOwner: isSelf,
      canvasFallback: stream === null,
      initialVolume: getSavedShareVolume(participantId),
    };
    const hash = encodeURIComponent(JSON.stringify(params));
    const windowLabel = `screen-share-${participantId}`;

    try {
      const win = new WebviewWindow(windowLabel, {
        url: `/screen-share#${hash}`,
        title: `${participant.displayName} — screen share`,
        width: 800,
        height: 520,
        minWidth: 320,
        minHeight: 232,
        resizable: true,
        decorations: false,
        center: true,
      });

      win.once('tauri://created', () => {
        // Primary path: pipe MediaStream through loopback bridge
        // Fallback path (stream is null): child window listens for
        // screen_share_frame events directly — no bridge needed
        if (stream) {
          startSending(participantId, windowLabel, stream);
        }
      });

      win.once('tauri://error', (e) => {
        console.error('[wavis:active-room] screen share window error:', e);
        setWatchingShareIds((prev) => {
          const next = new Set(prev);
          next.delete(participantId);
          return next;
        });
      });

      // Defense-in-depth: restore the tile even if the page-level close event
      // is missed and only the native window destruction fires.
      win.once('tauri://destroyed', () => {
        handleShareWindowClosed(participantId);
      });

      shareWindowsRef.current.set(participantId, { window: win, scope });
      setWatchingShareIds((prev) => new Set(prev).add(participantId));

      // If Watch All is open, remove this tile from the grid — the pop-out owns it now
      if (watchAllWindowRef.current && watchAllReadyRef.current) {
        detachScreenShareAudio(participantId);
        watchAllAttachedAudioRef.current.delete(participantId);
        stopSending(participantId, 'watch-all');
        prevWatchAllStreamsRef.current.delete(participantId);
        emit('watch-all:share-removed', { participantId });
      }
    } catch (err) {
      console.error('[wavis:active-room] failed to open screen share window:', err);
    }
  };

  /** Close a specific screen share OS window and clean up the bridge. */
  const closeShareWindow = (participantId: string) => {
    stopSending(participantId, `screen-share-${participantId}`);
    detachScreenShareAudio(participantId);
    const shareWindow = shareWindowsRef.current.get(participantId);
    if (shareWindow) {
      // Delete BEFORE win.close() so the screen-share:closed handler
      // sees delete() return false and skips its re-add (no double-fire).
      shareWindowsRef.current.delete(participantId);
      shareWindow.window.close().catch(() => { });
    }
    setWatchingShareIds((prev) => {
      const next = new Set(prev);
      next.delete(participantId);
      return next;
    });
    reAddStreamToWatchAll(participantId);
  };

  /** Close all share windows. */
  const closeAllShareWindows = () => {
    closeWatchAllWindow(); // close Watch All window first
    stopAllSending();
    for (const [pid, shareWindow] of shareWindowsRef.current) {
      detachScreenShareAudio(pid);
      shareWindow.window.close().catch(() => { });
    }
    shareWindowsRef.current.clear();
    setWatchingShareIds(new Set());
  };

  // Ref to latest roomState so the ready callback always reads fresh data
  const roomStateRef = useRef(roomState);
  roomStateRef.current = roomState;

  /** Open the Watch All window showing all active screen shares in a grid. */
  const openWatchAllWindow = async () => {
    // If already open, bring to foreground
    if (watchAllWindowRef.current) {
      watchAllWindowRef.current.setFocus();
      return;
    }

    if (!roomState) return;

    // Close any existing individual pop-out windows — WatchAll subsumes them.
    // We close the windows but don't detach audio (WatchAll doesn't handle
    // per-stream audio — the main window's audio attachment is independent).
    for (const [pid, shareWindow] of [...shareWindowsRef.current.entries()]) {
      stopSending(pid, `screen-share-${pid}`);
      detachScreenShareAudio(pid);
      shareWindow.window.close().catch(() => { });
    }
    shareWindowsRef.current.clear();
    setWatchingShareIds(new Set());

    const params = { channelName: roomState.channelName };
    const hash = encodeURIComponent(JSON.stringify(params));

    try {
      // Await the ready listener registration so it's guaranteed to be
      // active before the child window can emit watch-all:ready.
      // Previous bug: listen() returns a Promise — calling it without
      // await meant the listener wasn't registered yet when the child
      // window mounted and emitted the ready event.
      watchAllReadyRef.current = false;
      const unlistenReady = await listen('watch-all:ready', () => {
        console.log('[wavis:active-room] watch-all:ready received, readyRef was:', watchAllReadyRef.current);
        if (watchAllReadyRef.current) return; // idempotent
        watchAllReadyRef.current = true;
        // Read fresh roomState via ref — the closure captured at
        // openWatchAllWindow time may be stale by now.
        const rs = roomStateRef.current;
        if (!rs) {
          console.warn('[wavis:active-room] watch-all:ready fired but roomStateRef is null');
          return;
        }
        const scope = getWatchAllScope(rs);
        console.log('[wavis:active-room] watch-all:ready: screenShareStreams size =', scope.streams.size);
        for (const [pid, stream] of scope.streams) {
          if (stream) {
            startSending(pid, 'watch-all', stream);
          }
          const participant = scope.participants.find((p) => p.id === pid);
          if (participant) {
            emit('watch-all:share-added', {
              participantId: pid,
              displayName: participant.displayName,
              color: participant.color,
              canvasFallback: stream === null,
            });
            emitWatchAllRestoreVolume(pid);
          }
        }
        // Seed the dynamic tracking ref so the useEffect doesn't
        // re-emit these same shares as "new".
        prevWatchAllStreamsRef.current = new Map(scope.streams);
      });
      watchAllReadyUnlistenRef.current = unlistenReady;

      const win = new WebviewWindow('watch-all', {
        url: `/watch-all#${hash}`,
        title: `Watch All — ${roomState.channelName}`,
        width: 960,
        height: 540,
        minWidth: 480,
        minHeight: 320,
        resizable: true,
        decorations: false,
        center: true,
      });

      win.once('tauri://error', (e) => {
        console.error('[wavis:active-room] watch-all window error:', e);
      });

      // Defense-in-depth: tauri://destroyed fires even if watch-all:closed doesn't
      win.once('tauri://destroyed', () => {
        closeWatchAllWindow();
      });

      watchAllWindowRef.current = win;
      setWatchAllOpen(true);
    } catch (err) {
      console.error('[wavis:active-room] failed to open watch-all window:', err);
    }
  };

  /** Close the Watch All window and clean up bridge senders. */
  const closeWatchAllWindow = () => {
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
    stopSendingForWindow('watch-all');
    watchAllWindowRef.current.close().catch(() => { });
    watchAllWindowRef.current = null;
    setWatchAllOpen(false);
  };

  const previousJoinedSubRoomIdRef = useRef<string | null>(null);
  useEffect(() => {
    const previousJoinedSubRoomId = previousJoinedSubRoomIdRef.current;
    const nextJoinedSubRoomId = roomState?.joinedSubRoomId ?? null;
    previousJoinedSubRoomIdRef.current = nextJoinedSubRoomId;

    if (previousJoinedSubRoomId === nextJoinedSubRoomId) return;

    const scopeParticipantIds = getWatchAllScope(roomState).participantIds;
    closeWatchAllWindow();

    for (const [participantId, shareWindow] of [...shareWindowsRef.current.entries()]) {
      if (shareWindow.scope !== 'watch-all') continue;
      if (scopeParticipantIds.has(participantId)) continue;
      closeShareWindow(participantId);
    }
  }, [getWatchAllScope, roomState, roomState?.joinedSubRoomId]);

  /** Toggle the Watch All window open/closed. */
  const toggleWatchAllWindow = () => {
    if (watchAllOpen) {
      closeWatchAllWindow(); // unconditional close (Req 6.3)
    } else {
      // Only open if the joined room has active remote shares.
      const hasShares = roomState ? getWatchAllScope(roomState).remoteSharers.length > 0 : false;
      if (hasShares) {
        openWatchAllWindow();
      }
    }
  };

  // Keep ref in sync so hotkey callback never captures a stale closure
  toggleWatchAllRef.current = toggleWatchAllWindow;

  // Register Watch All hotkey when media connects
  useEffect(() => {
    if (roomState?.mediaState !== 'connected') return;

    let cancelled = false;
    getWatchAllHotkey().then((hotkey) => {
      if (cancelled) return;
      watchAllHotkeyRef.current = hotkey;
      registerWatchAllHotkey(hotkey, () => toggleWatchAllRef.current());
    });

    return () => {
      cancelled = true;
      if (watchAllHotkeyRef.current) {
        unregisterWatchAllHotkey(watchAllHotkeyRef.current);
        watchAllHotkeyRef.current = null;
      }
    };
  }, [roomState?.mediaState]);

  // Platform check: Linux uses standalone window (PostMessage works fine there).
  const isLinuxPlatform = typeof navigator !== 'undefined' && /Linux/.test(navigator.userAgent);
  const isMacPlatform = typeof navigator !== 'undefined' && /Mac/.test(navigator.userAgent);
  const macShareAudioDisabledMessage = "mac sucks and we can't make this feature work yet";

  // macOS: check / install the WavisAudioTap HAL driver needed for echo-free audio share.
  const { driverState, installError, triggerInstall } = useAudioDriverInstall(isMacPlatform);

  /* â”€â”€ Derived â”€â”€ */
  const { showSecrets } = useDebug();
  const selfP = roomState?.participants.find((p) => p.id === roomState.selfParticipantId);
  const isHost = roomState?.selfIsHost ?? false;
  const selfSharing = selfP?.isSharing ?? false;
  const sharers = roomState?.participants.filter((p) => p.isSharing) ?? [];
  const joinedSubRoom = roomState?.subRooms.find((subRoom) => subRoom.id === roomState.joinedSubRoomId) ?? null;
  const joinedSubRoomParticipantIds = new Set(joinedSubRoom?.participantIds ?? []);
  const joinedRoomParticipants = roomState?.participants.filter((participant) => joinedSubRoomParticipantIds.has(participant.id)) ?? [];
  const shareEnabled = roomState
    ? isShareEnabled(roomState.sharePermission, isHost, roomState.machineState, roomState.mediaState)
    : false;
  const currentShareType = roomState
    ? activeShareType(roomState.activeVideoShare, roomState.activeAudioShare)
    : null;
  const stopShareAction = () => {
    const route = computeStopRoute(currentShareType, selfSharing);
    if (route === 'stop_custom') stopCustomShare('all');
    else if (route === 'stop_fallback') stopShare();
  };
  shareUserStateRef.current = {
    isMuted: selfP?.isMuted ?? false,
    isDeafened: roomState?.isDeafened ?? false,
    isSharing: selfSharing,
    shareEnabled,
  };
  voiceParticipantsRef.current = {
    participants: roomState?.participants
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
    participants: joinedRoomParticipants
      .filter((participant) => participant.id !== roomState?.selfParticipantId)
      .map((participant) => ({
        id: participant.id,
        name: participant.displayName,
        color: participant.color,
        volume: participant.volume,
        muted: participant.volume === 0,
      })),
  };

  useEffect(() => {
    if (!roomState) return;
    void emit('share:user-state', shareUserStateRef.current);
  }, [roomState, selfP?.isMuted, roomState?.isDeafened, selfSharing, shareEnabled]);

  useEffect(() => {
    if (!roomState) return;
    void emit('share:voice-participants', voiceParticipantsRef.current);
  }, [roomState, roomState?.participants, roomState?.selfParticipantId]);

  useEffect(() => {
    if (!roomState) return;
    void emit('watch-all:voice-participants', watchAllVoiceParticipantsRef.current);
  }, [roomState, roomState?.participants, roomState?.selfParticipantId, roomState?.joinedSubRoomId, roomState?.subRooms]);

  /** Open custom share picker or invoke getDisplayMedia fallback based on platform. */
  const handleStartShare = async () => {
    if (shareEnumerating.current) return;
    shareEnumerating.current = true;
    if (!isLinuxPlatform) setSharePickerLoading(true);

    try {
      if (!isLinuxPlatform) {
        // Windows/macOS: use the browser/WebView getDisplayMedia path, not the
        // custom native-source path below. If share diagnostics or leak logging
        // are changed, this branch must be updated too — otherwise logs will only
        // appear for Linux/native-picker flows and Windows repros will miss them.
        //
        // This path is what normal Windows `/share` currently exercises.
        if (isMacPlatform) {
          const access = await invoke<{
            authorized: boolean;
            promptShown: boolean;
            restartRequired: boolean;
          }>('ensure_screen_recording_access');

          if (!access.authorized) {
            const msg = 'Screen sharing requires Screen Recording permission in System Settings > Privacy & Security > Screen Recording.';
            showTransientScreenShareError(msg);
            toast.error(msg);
            return;
          }

          if (access.restartRequired) {
            const msg = 'Screen Recording permission was granted. Quit and reopen Wavis, then try screen sharing again.';
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

      const captureAuthStatus = await invoke<{
        display_server: string;
        authorized: boolean;
        needs_auth: boolean;
        was_attempted: boolean;
      }>('get_capture_auth_status');

      if (captureAuthStatus.display_server === 'wayland') {
        await startPortalShare();
        return;
      }

      // Linux: custom picker path — getDisplayMedia() doesn't work in WebKitGTK.
      const result = await invoke<EnumerationResult>('list_share_sources');

      if (result.sources.length > 0 || result.fallback_reason === 'portal') {
        const occupied: OccupiedSlots = {
          videoOccupied: roomState?.activeVideoShare !== null,
          audioOccupied: roomState?.activeAudioShare !== null,
        };

        // Linux: standalone OS window — PostMessage works fine on WebKitGTK.
        setPendingSharePickerData({ enumResult: result, occupied });
        const pickerPayload = encodeURIComponent(
          JSON.stringify({ enumResult: result, occupied }),
        );
        new WebviewWindow('share-picker', {
          url: `/share-picker#${pickerPayload}`,
          title: 'Wavis — Share Picker',
          width: 640,
          height: 480,
          minWidth: 360,
          minHeight: 320,
          resizable: true,
          decorations: false,
          center: true,
        });
      } else if (result.fallback_reason === 'get_display_media' && roomState?.connectionMode === 'livekit') {
        await startFallbackShare();
      } else {
        toast.error('No shareable sources found');
      }
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
    const selfParticipant = roomState?.participants.find((p) => p.id === roomState.selfParticipantId);
    const isSelfSharing = selfParticipant?.isSharing ?? false;
    if (wasSelfSharingRef.current && !isSelfSharing) {
      setShareAudioOn(false);
      setShowPostShareAudioPrompt(false);
    }
    wasSelfSharingRef.current = isSelfSharing;
  }, [roomState?.participants, roomState?.selfParticipantId]);

  // Self-kick navigation
  useEffect(() => {
    if (roomState?.error === 'You were kicked') {
      const timer = setTimeout(() => navigateAwayFromRoom(`/channel/${channelId}`), 2000);
      return () => clearTimeout(timer);
    }
  }, [roomState?.error, channelId, navigateAwayFromRoom]);

  // Close Watch All and any share pop-outs when the room session fully ends.
  // This covers disconnect/error paths that transition to idle without going
  // through the explicit leave flow, such as the session being displaced.
  useEffect(() => {
    if (roomState?.machineState !== 'idle') return;
    closeAllShareWindows();
  }, [roomState?.machineState]);

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
            The server was offline and is booting. This takes up to {waitMins} minute{waitMins !== 1 ? 's' : ''}.
            Joining automatically when ready.
          </div>
          <button
            className="mt-3 text-xs text-wavis-text border border-wavis-text-secondary py-0.5 px-1 hover:bg-wavis-text-secondary hover:text-wavis-text-contrast transition-colors"
            onClick={() => navigateAwayFromRoom(`/channel/${channelId}`, true)}
          >
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
              <button className="text-xs text-wavis-text border border-wavis-text-secondary py-0.5 px-1 text-center transition-colors hover:bg-wavis-text-secondary hover:text-wavis-text-contrast" onClick={() => { initRef.current = false; initSession(channelId, channelName, channelRole, setRoomState); initRef.current = true; }}>/retry</button>
              <button className="text-xs text-wavis-text border border-wavis-text-secondary py-0.5 px-1 text-center transition-colors hover:bg-wavis-text-secondary hover:text-wavis-text-contrast" onClick={() => navigateAwayFromRoom(`/channel/${channelId}`)}>/back</button>
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
              <button className="text-xs text-wavis-text border border-wavis-text-secondary py-0.5 px-1 text-center transition-colors hover:bg-wavis-text-secondary hover:text-wavis-text-contrast" onClick={() => { initRef.current = false; initSession(channelId, channelName, channelRole, setRoomState); initRef.current = true; }}>/retry</button>
              <button className="text-xs text-wavis-danger border border-wavis-danger py-0.5 px-1 text-center transition-colors hover:bg-wavis-danger hover:text-wavis-bg" onClick={() => navigateAwayFromRoom('/', true)}>/leave</button>
            </div>
          </div>
        </div>
      </div>
    );
  }

  // Kicked state
  if (roomState.error === 'You were kicked') {
    return (
      <div className="h-full flex items-center justify-center bg-wavis-bg font-mono text-wavis-danger">
        you were kicked from the room
      </div>
    );
  }

  /* ── Actions ── */
  const handleLeave = () => {
    setLeaving(true);
    navigateAwayFromRoom('/', true);
  };

  const handleSendChat = () => {
    if (chatThrottledRef.current) return;
    const text = chatInput.trim();
    if (!text) return;
    setChatInput('');
    sendChatMessage(text);
    chatThrottledRef.current = true;
    setTimeout(() => { chatThrottledRef.current = false; }, 200);
  };

  /* ── CLI autocomplete ── */
  const CLI_COMMANDS = [
    '/help', '/mute', '/deafen', '/kick', '/share', '/stopshare', '/revoke',
    '/stopall', '/shareperm', '/vol', '/watch-all', '/leave', '/reconnect-media', '/devices',
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
        '  /revoke <name>               — stop a participant\'s share',
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
        handleStartShare();
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
      toggleWatchAllWindow();
    } else if (raw === '/leave') {
      handleLeave();
    } else if (raw === '/reconnect-media') {
      reconnectMedia();
    }
  };

  /* ── Reusable panel fragments ── */

  const sigDot = signalingIndicator(roomState.machineState);
  const mediaDot = mediaIndicator(roomState.mediaState, roomState.mediaError);
  const statusBadge = combinedStatusBadge(roomState.machineState, roomState.mediaState);

  const roomHeader = (
    <div className="px-3 py-3 border-b border-wavis-text-secondary h-[4.5rem] flex flex-col justify-center gap-0.5 overflow-hidden">
      <div className="flex items-center gap-2">
        <StatusDot color={sigDot.color} label={sigDot.label} />
        <StatusDot color={mediaDot.color} label={mediaDot.label} />
        {(() => {
          const badge = connectionModeBadgeText(showSecrets, roomState.connectionMode);
          return badge ? <span className="text-[0.625rem] text-wavis-purple">[{badge}]</span> : null;
        })()}
        <span className="text-sm" style={{ color: statusBadge.color }}>{statusBadge.text}</span>
        <span className="text-[0.625rem] text-wavis-text-secondary">{roomState.participants.length}/6</span>
        <span className="text-[0.625rem]" style={{ color: rttColor(roomState.networkStats.rttMs) }}>{roomState.networkStats.rttMs}ms</span>
        <span className="text-[0.625rem] text-wavis-text-secondary">{roomState.networkStats.packetLossPercent.toFixed(1)}% loss</span>
      </div>
      <div
        className={`font-bold truncate min-w-0${roomState.channelName.length > 20 ? ' text-xs' : ' text-sm'}`}
        title={roomState.channelName}
      >
        {roomState.channelName}
      </div>
    </div>
  );

  const mediaRetryBanner = roomState.mediaState === 'failed' && roomState.mediaReconnectFailures > 0 ? (
    <div className="px-3 py-2 border-b border-wavis-danger bg-wavis-panel text-xs flex items-center justify-between gap-2">
      <span className="text-wavis-danger">media disconnected — automatic retries exhausted</span>
      <button
        className="border border-wavis-accent text-wavis-accent hover:bg-wavis-accent hover:text-wavis-bg transition-colors px-1 py-0.5 text-xs text-center shrink-0"
        onClick={() => { resetMediaReconnectFailures(); reconnectMedia(); }}
      >
        /retry
      </button>
    </div>
  ) : null;

  const renderParticipantRow = (p: RoomParticipant) => {
    const isSelf = p.id === roomState.selfParticipantId;
    const icon = voiceIcon(p, isSelf ? roomState.isDeafened : p.isDeafened);

    return (
      <div key={p.id} className="pl-2">
        <div
          role="button"
          tabIndex={isSelf ? -1 : 0}
          onClick={() => { if (!isSelf) setExpandedUser((prev) => (prev === p.id ? null : p.id)); }}
          onKeyDown={(e) => { if (!isSelf && (e.key === 'Enter' || e.key === ' ')) setExpandedUser((prev) => (prev === p.id ? null : p.id)); }}
          className="w-full text-left flex items-center gap-2 hover:opacity-80"
          style={{ cursor: isSelf ? 'default' : 'pointer' }}
        >
          {isSelf ? <span className="text-xs text-wavis-accent inline-block w-6 text-center flex-none">&gt;</span> : <span className="text-[0.625rem] text-wavis-text-secondary inline-block w-6 text-center flex-none">{expandedUser === p.id ? '[-]' : '[+]'}</span>}
          {p.role === 'host' && <span className="text-xs text-wavis-text-secondary">[HOST]</span>}
          <span style={{
            color: p.color,
            animation: p.isSpeaking && !p.isMuted ? 'pulse 3s ease-in-out infinite' : 'none',
            filter: p.isSpeaking && !p.isMuted ? 'brightness(1.5)' : 'brightness(0.7)',
          }}>{p.displayName}</span>
          <span style={{ color: icon.color, textDecoration: icon.strikethrough ? 'line-through' : undefined, ...(icon.transform ? { display: 'inline-block', transform: icon.transform } : {}) }}>{icon.char}</span>
          <div className="ml-auto flex items-center gap-1">
            {isSelf && p.isSharing && (
              <span
                className="text-sm leading-none"
                style={{ color: 'var(--wavis-danger)', animation: 'watchPulse 2s ease-in-out infinite' }}
                title="you are sharing"
              >
                {"\u25C9"}
              </span>
            )}
            {!isSelf && p.isSharing && (() => {
              const hasStream = roomState.screenShareStreams.has(p.id);
              const isWatching = watchingShareIds.has(p.id);
              return (
                <button
                  onClick={(e) => {
                    e.stopPropagation();
                    if (!hasStream) return;
                    if (isWatching) {
                      closeShareWindow(p.id);
                    } else {
                      openShareWindow(p.id, p, roomState.screenShareStreams.get(p.id)!);
                    }
                  }}
                  className="text-sm leading-none"
                  style={isWatching
                    ? { color: 'var(--wavis-danger)' }
                    : hasStream
                      ? { color: 'var(--wavis-danger)', animation: 'watchPulse 2s ease-in-out infinite' }
                      : { color: 'var(--wavis-text-secondary)', opacity: 0.4 }}
                  title={isWatching ? 'close share' : hasStream ? 'watch share' : 'waiting for stream...'}
                >
                  {isWatching ? "\u25CE" : "\u25C9"}
                </button>
              );
            })()}
          </div>
        </div>
        {expandedUser === p.id && !isSelf && (
          <div className="pl-6 py-1 space-y-0.5 text-xs">
            <div className="flex items-center gap-2">
              <span className="text-wavis-text-secondary shrink-0">voice vol</span>
              <div className="flex-1">
                <VolumeSlider value={p.volume} onChange={(v) => setParticipantVolume(p.id, v)} color={p.color} />
              </div>
              <span className="text-wavis-text-secondary w-6 text-right">{p.volume}</span>
            </div>
            {p.isSharing && (
              <div className="flex items-center gap-2 mt-1">
                <span className="text-wavis-text-secondary shrink-0">share vol</span>
                <div className="flex-1">
                  <VolumeSlider value={shareVolumes.get(p.id) ?? 70} onChange={(v) => syncScreenShareVolume(p.id, v)} color={p.color} />
                </div>
                <span className="text-wavis-text-secondary w-6 text-right">{shareVolumes.get(p.id) ?? 70}</span>
              </div>
            )}
            {isHost && (
              <>
                <button onClick={() => kickParticipant(p.id)} className="block w-full text-left border border-wavis-danger text-wavis-danger px-3 py-2 transition-colors hover:opacity-70">/kick {p.displayName}</button>
                {p.isHostMuted
                  ? <button onClick={() => unmuteParticipant(p.id)} className="block w-full text-left border border-wavis-accent text-wavis-accent px-3 py-2 transition-colors hover:opacity-70">/unmute {p.displayName}</button>
                  : !p.isMuted && <button onClick={() => muteParticipant(p.id)} className="block w-full text-left border px-3 py-2 transition-colors hover:opacity-70" style={{ color: 'var(--wavis-warn)', borderColor: 'var(--wavis-warn)' }}>/mute {p.displayName}</button>
                }
                {p.isSharing && <button onClick={() => stopParticipantShare(p.id)} className="block w-full text-left border border-wavis-danger text-wavis-danger px-3 py-2 transition-colors hover:opacity-70">/revoke {p.displayName}</button>}
              </>
            )}
          </div>
        )}
      </div>
    );
  };

  const participantsSections = (
    <div className="flex-1 overflow-y-auto">
      {roomState.subRooms.map((subRoom) => {
        const sectionKey = `sub-room:${subRoom.id}`;
        const roomPanelId = `sub-room-panel-${subRoom.id}`;
        const isExpanded = expandedSections[sectionKey] ?? true;
        const roomParticipantIds = new Set(subRoom.participantIds);
        const roomParticipants = roomState.participants.filter((participant) => roomParticipantIds.has(participant.id));
        const roomRemoteSharers = roomParticipants.filter(
          (participant) => participant.isSharing && participant.id !== roomState.selfParticipantId,
        );
        const isJoinedRoom = roomState.joinedSubRoomId === subRoom.id;
        const showJoinedRoomWatchAll = isJoinedRoom && roomRemoteSharers.length > 0;
        const showDisabledWatchAll = !isJoinedRoom && roomRemoteSharers.length > 0;
        const roomActionButton = isJoinedRoom ? (
          <button
            type="button"
            onPointerDown={(event) => {
              event.stopPropagation();
            }}
            onClick={(event) => {
              event.stopPropagation();
              leaveSubRoom();
            }}
            className="text-xs py-0.5 px-1 border border-wavis-danger text-wavis-danger transition-colors hover:bg-wavis-danger hover:text-wavis-bg cursor-pointer"
          >
            /leave
          </button>
        ) : (
          <button
            type="button"
            onPointerDown={(event) => {
              event.stopPropagation();
            }}
            onClick={(event) => {
              event.stopPropagation();
              joinSubRoom(subRoom.id);
            }}
            className="text-xs py-0.5 px-1 border border-wavis-accent text-wavis-accent transition-colors hover:bg-wavis-accent hover:text-wavis-bg cursor-pointer"
          >
            /join
          </button>
        );

        return (
          <div key={subRoom.id} className="border-b border-wavis-text-secondary">
            <div
              role="button"
              tabIndex={0}
              aria-expanded={isExpanded}
              aria-controls={roomPanelId}
              onPointerDown={(event) => {
                if (!event.isPrimary || event.button !== 0) return;
                event.preventDefault();
                event.currentTarget.focus();
                toggleSection(sectionKey);
              }}
              onKeyDown={(event) => {
                if (event.key !== 'Enter' && event.key !== ' ') return;
                event.preventDefault();
                toggleSection(sectionKey);
              }}
              className="w-full px-3 py-2 flex items-center gap-2 text-sm text-left hover:opacity-80 cursor-pointer"
            >
              <div className="flex items-center gap-2 min-w-0 flex-1">
                <span className="text-wavis-text-secondary">{isExpanded ? '[-]' : '[+]'}</span>
                <span>{`ROOM ${subRoom.roomNumber}`}</span>
                <span className="text-wavis-text-secondary">({roomParticipants.length})</span>
              </div>
              <div className="shrink-0">
                {roomActionButton}
              </div>
            </div>
            {isExpanded && (
              <div id={roomPanelId} className="px-3 py-2 space-y-1 text-sm">
                {roomParticipants.length > 0 ? roomParticipants.map(renderParticipantRow) : (
                  <div className="pl-8 text-xs text-wavis-text-secondary">no participants in this room</div>
                )}
                {(showJoinedRoomWatchAll || showDisabledWatchAll) && (
                  <div className="pt-2 flex items-center justify-end gap-2">
                    {showJoinedRoomWatchAll && (
                      <button
                        type="button"
                        onClick={toggleWatchAllWindow}
                        className={`text-xs py-0.5 px-1 border transition-colors cursor-pointer ${watchAllOpen ? 'border-wavis-purple text-wavis-purple hover:bg-wavis-purple hover:text-wavis-bg' : 'border-wavis-text-secondary text-wavis-text hover:bg-wavis-text-secondary hover:text-wavis-text-contrast'}`}
                      >
                        {watchAllOpen ? '/close-all' : '/watch-all'}
                      </button>
                    )}
                    {showDisabledWatchAll && (
                      <button
                        type="button"
                        disabled
                        aria-disabled="true"
                        title="Join this room to watch all streams together."
                        className="text-xs py-0.5 px-1 border border-wavis-text-secondary text-wavis-text-secondary opacity-60 cursor-not-allowed"
                      >
                        /watch-all
                      </button>
                    )}
                  </div>
                )}
              </div>
            )}
          </div>
        );
      })}
      <div className="px-3 py-2 flex items-center justify-between gap-2 border-b border-wavis-text-secondary">
        <button
          onClick={() => createSubRoom()}
          className="text-xs py-0.5 px-1 border border-wavis-accent text-wavis-accent transition-colors hover:bg-wavis-accent hover:text-wavis-bg"
        >
          /create room
        </button>
        {isHost && sharers.length > 1 ? (
          <div className="flex items-center gap-2">
            <button
              onClick={stopAllShares}
              className="text-wavis-danger text-xs border border-wavis-danger py-0.5 px-1 hover:bg-wavis-danger hover:text-wavis-bg transition-colors"
            >
              /stopall
            </button>
          </div>
        ) : <span />}
      </div>
    </div>
  );

  const youBar = (
    <div className="p-4 border-t border-wavis-text-secondary">
      <div className="flex items-center gap-1 border-b border-wavis-text-secondary font-mono text-wavis-text">
        <button onClick={() => toggleSection('you')} className="bg-transparent outline-none px-1 py-1 text-xs text-wavis-text-secondary hover:opacity-80">
          {expandedSections.you ? '[-]' : '[+]'}
        </button>
        <button onClick={() => toggleSection('you')} className="bg-transparent outline-none py-1 px-1 text-left flex items-center gap-2 hover:opacity-80">
          <span style={{ color: selfP?.color }}>{selfP?.displayName}</span>
          {selfP?.role === 'host' && <span className="text-[0.625rem] text-wavis-text-secondary">[HOST]</span>}
        </button>
        {!expandedSections.you && (
          <div className="ml-auto flex items-center leading-none text-xs">
            <button
              onClick={toggleSelfMute}
              disabled={!!selfP?.isHostMuted}
              className="px-1.5 flex items-center justify-center disabled:opacity-40 disabled:cursor-not-allowed hover:opacity-70 transition-opacity"
              style={{ color: selfP?.isMuted ? 'var(--wavis-danger)' : 'var(--wavis-text-secondary)' }}
              title={selfP?.isMuted ? '/unmute' : '/mute'}
            >○</button>
            <span className="text-wavis-text-secondary opacity-30 select-none leading-none">│</span>
            <button
              onClick={toggleSelfDeafen}
              className="px-1.5 flex items-center justify-center hover:opacity-70 transition-opacity"
              style={{ color: roomState.isDeafened ? 'var(--wavis-danger)' : 'var(--wavis-text-secondary)' }}
              title={roomState.isDeafened ? '/undeafen' : '/deafen'}
            ><span style={{ display: 'inline-block', transform: 'scale(1.25) translateY(8%)' }}>¤</span></button>
            <span className="text-wavis-text-secondary opacity-30 select-none leading-none">│</span>
            <button
              onClick={selfSharing ? stopShareAction : handleStartShare}
              disabled={!selfSharing && (!shareEnabled || sharePickerLoading)}
              className="px-1.5 flex items-center justify-center disabled:opacity-40 disabled:cursor-not-allowed hover:opacity-70 transition-opacity"
              style={{ color: selfSharing ? 'var(--wavis-danger)' : 'var(--wavis-text-secondary)' }}
              title={selfSharing ? '/stopshare' : '/share'}
            ><span style={{ display: 'inline-block', transform: 'translateY(8%)' }}>◉</span></button>
          </div>
        )}
      </div>
      {expandedSections.you && (
        <div className="pt-2 pl-6 text-sm">
          <div className="flex flex-col gap-1 w-full">
            <div className="flex flex-col md:flex-row gap-1">
              <button onClick={toggleSelfMute} disabled={selfP?.isHostMuted} className={`flex-1 py-0.5 px-1 text-xs text-center transition-colors border disabled:opacity-40 disabled:cursor-not-allowed ${selfP?.isMuted ? 'border-wavis-danger text-wavis-danger bg-wavis-danger/8 hover:bg-wavis-danger hover:text-wavis-bg' : 'border-wavis-text-secondary text-wavis-text hover:bg-wavis-text-secondary hover:text-wavis-text-contrast'}`}>{selfP?.isMuted ? '/unmute' : '/mute'}</button>
              <button onClick={toggleSelfDeafen} className={`flex-1 py-0.5 px-1 text-xs text-center transition-colors border ${roomState.isDeafened ? 'border-wavis-purple text-wavis-purple hover:bg-wavis-purple hover:text-wavis-bg' : 'border-wavis-text-secondary text-wavis-text hover:bg-wavis-text-secondary hover:text-wavis-text-contrast'}`}>{roomState.isDeafened ? '/undeafen' : '/deafen'}</button>
            </div>
            {(selfSharing || !(roomState.activeVideoShare && roomState.activeAudioShare)) && (() => {
              const shareDisabled = !shareEnabled || isShareButtonDisabled(currentShareType, selfSharing) || sharePickerLoading;
              return (
                <>
                  <button
                    onClick={selfSharing ? stopShareAction : handleStartShare}
                    disabled={selfSharing ? false : shareDisabled}
                    className={`w-full py-0.5 px-1 text-xs text-center transition-colors border disabled:opacity-40 disabled:cursor-not-allowed ${selfSharing ? 'text-wavis-danger border-wavis-danger hover:bg-wavis-danger hover:text-wavis-bg' : 'border-wavis-purple text-wavis-purple hover:bg-wavis-purple hover:text-wavis-bg'}`}
                  >
                    {shareButtonLabel(shareEnabled, selfSharing, roomState.sharePermission, isHost)}
                  </button>
                  {sharePickerLoading && (
                    <div className="-mt-1 border-x border-b border-wavis-text-secondary/30 bg-wavis-panel p-2 text-xs flex items-center gap-2">
                      <div className="flex items-center gap-1">
                        {[0, 1, 2].map((bar) => (
                          <span
                            key={bar}
                            className="inline-block w-1 bg-wavis-purple"
                            style={{
                              height: '0.55rem',
                              animation: 'pulse 1.2s ease-in-out infinite',
                              animationDelay: `${bar * 0.16}s`,
                            }}
                          />
                        ))}
                      </div>
                      <span className="text-wavis-text-secondary">waiting for screen picker...</span>
                    </div>
                  )}
                  {selfSharing && (
                    <div className="border-x border-b border-wavis-text-secondary p-2 space-y-1 text-xs">
                      <div className="flex gap-1">
                        <button
                          onClick={() => { void withPickerResize(isMacPlatform, () => changeShareSource()); }}
                          className="flex-1 py-0.5 px-1 text-xs text-center border border-wavis-text-secondary text-wavis-text transition-colors hover:bg-wavis-text-secondary hover:text-wavis-text-contrast"
                        >
                          /window
                        </button>
                        <button
                          onClick={() => {
                            if (isMacPlatform) return;
                            const next = !shareAudioOn;
                            setShareAudioOn(next);
                            void toggleShareAudio(next);
                          }}
                          disabled={isMacPlatform}
                          className={`flex-1 py-0.5 px-1 text-xs text-center border transition-colors ${isMacPlatform
                            ? 'cursor-not-allowed border-wavis-text-secondary text-wavis-text-secondary opacity-50'
                            : shareAudioOn
                              ? 'border-wavis-accent text-wavis-accent hover:bg-wavis-accent hover:text-wavis-bg'
                              : 'border-wavis-text-secondary text-wavis-text hover:bg-wavis-text-secondary hover:text-wavis-text-contrast'
                            }`}
                        >
                          {shareAudioOn ? '/audio on' : '/audio off'}
                        </button>
                      </div>
                      <select
                        value={shareQualityState}
                        onChange={(e) => {
                          const q = e.target.value as 'low' | 'high' | 'max';
                          setShareQualityState(q);
                          setShareQuality(q);
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
                        onBlur={(e) => { e.currentTarget.dataset.open = 'false'; }}
                        onKeyDown={(e) => { if (e.key === 'Escape') e.currentTarget.blur(); }}
                        className="w-full bg-wavis-panel border border-wavis-text-secondary text-wavis-text text-xs py-0.5 px-1 cursor-pointer"
                      >
                        {(['low', 'high', 'max'] as const).map((q) => {
                          const label = q === 'low' ? 'Smooth  1080p @ 60fps' : q === 'high' ? 'Sharp   1440p @ 30fps' : 'Max     1440p @ 60fps';
                          return <option key={q} value={q}>{label}</option>;
                        })}
                      </select>
                    </div>
                  )}
                </>
              );
            })()}
            <div className="mt-4 flex flex-col gap-1">
              <button onClick={() => setShowSettings(true)} className="w-full text-wavis-text border border-wavis-text-secondary py-0.5 px-1 text-xs text-center transition-colors hover:bg-wavis-text-secondary hover:text-wavis-text-contrast">/settings</button>
              <button onClick={handleLeave} disabled={leaving} className="w-full text-wavis-danger border border-wavis-danger py-0.5 px-1 text-xs text-center transition-colors hover:bg-wavis-danger hover:text-wavis-bg disabled:opacity-40 disabled:cursor-not-allowed">{leaving ? 'leaving...' : '/leave'}</button>
            </div>
          </div>
        </div>
      )}
      {!expandedSections.you && sharePickerLoading && (
        <div className="ml-6 mt-2 border border-wavis-text-secondary/30 bg-wavis-panel p-2 text-xs flex items-center gap-2">
          <div className="flex items-center gap-1">
            {[0, 1, 2].map((bar) => (
              <span
                key={bar}
                className="inline-block w-1 bg-wavis-purple"
                style={{
                  height: '0.55rem',
                  animation: 'pulse 1.2s ease-in-out infinite',
                  animationDelay: `${bar * 0.16}s`,
                }}
              />
            ))}
          </div>
          <span className="text-wavis-text-secondary">waiting for screen picker...</span>
        </div>
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
    <div className="flex-1 flex flex-col min-h-0">
      <div className="px-3 py-3 border-b border-wavis-text-secondary h-[4.5rem] flex flex-col justify-center">
        <div className="font-bold text-sm">CHAT</div>
      </div>
      <div className="flex-1 overflow-y-auto overflow-x-hidden p-4 space-y-1 text-sm">
        {roomState.chatMessages.length === 0 && (
          <div className="text-wavis-text-secondary">No messages yet</div>
        )}
        {roomState.chatMessages.map((msg) =>
          msg.isDivider ? (
            <div key={msg.id} className="text-wavis-text-secondary text-xs py-1 text-center">
              {'─'.repeat(12)} Earlier messages {'─'.repeat(12)}
            </div>
          ) : (
            <div key={msg.id} className="break-all">
              <span className="text-wavis-text-secondary">[{formatTime(msg.timestamp)}]</span>{' '}
              <span style={{ color: msg.color }}>{msg.displayName}</span>
              <span>: {msg.text}</span>
            </div>
          )
        )}
        {chatError && (
          <div className="text-wavis-text-secondary italic text-xs">
            {chatError}
          </div>
        )}
        <div ref={chatEndRef} />
      </div>
      <div className="p-4 border-t border-wavis-text-secondary">
        <div className="flex items-center gap-2">
          <span className="text-wavis-accent">&gt;</span>
          <input
            type="text"
            value={chatInput}
            onChange={(e) => setChatInput(e.target.value)}
            onKeyDown={(e) => e.key === 'Enter' && handleSendChat()}
            maxLength={2000}
            className="flex-1 bg-transparent border-b border-wavis-text-secondary outline-none px-2 py-1 font-mono text-wavis-text"
            placeholder="type message..."
          />
        </div>
      </div>
    </div>
  );

  const logPanel = (
    <div className="flex-1 flex flex-col min-h-0">
      <div className="px-3 py-3 border-b border-wavis-text-secondary h-[4.5rem] flex flex-col justify-center">
        <div className="font-bold text-sm">LOGS</div>
      </div>
      <div className="flex-1 overflow-y-auto p-4 space-y-1 text-sm">
        {roomState.events.map((evt) => {
          const username = getEventUsername(evt);
          const userColor = getUserColor(roomState.participants, evt.participantId);
          return (
            <div key={evt.id} style={{ whiteSpace: evt.message.includes('\n') ? 'pre-line' : undefined }}>
              <span className="text-wavis-text-secondary">[{formatTime(evt.timestamp)}]</span>{' '}
              {username && evt.participantId ? (
                <><span style={{ color: userColor }}>{username}</span>{' '}<span style={{ color: getEventColor(evt.type) }}>{evt.message.slice(username.length + 1)}</span></>
              ) : (
                <span style={{ color: getEventColor(evt.type) }}>{evt.message}</span>
              )}
            </div>
          );
        })}
        <div ref={logEndRef} />
      </div>
      <div className="p-4 border-t border-wavis-text-secondary">
        <div className="flex items-center gap-2">
          <span className="text-wavis-accent">&gt;</span>
          <input
            type="text"
            value={cliInput}
            onChange={(e) => handleCliInputChange(e.target.value)}
            onKeyDown={handleCliKeyDown}
            onFocus={() => setCliFocused(true)}
            onBlur={() => setCliFocused(false)}
            ref={cliInputRef}
            data-cli-input="true"
            className="flex-1 bg-transparent border-b border-wavis-text-secondary outline-none px-2 py-1 font-mono text-wavis-text"
            placeholder={cliFocused ? '' : 'type command... try /help'}
            autoFocus
          />
        </div>
      </div>
    </div>
  );

  return (
    <div className="h-full flex flex-col bg-wavis-bg font-mono text-wavis-text">
      <Toaster
        position="bottom-right"
        duration={4000}
        toastOptions={{
          style: { fontFamily: 'var(--font-mono)', fontSize: '0.875rem' },
        }}
      />
      {/* ═══ MOBILE LAYOUT (< md) ═══ */}
      <div className="flex flex-col flex-1 overflow-hidden md:hidden">
        {/* Compact header */}
        <div className="flex items-center justify-between px-3 py-2 border-b border-wavis-text-secondary bg-wavis-panel">
          <div className="flex items-center gap-2 min-w-0">
            <StatusDot color={sigDot.color} label={sigDot.label} />
            <StatusDot color={mediaDot.color} label={mediaDot.label} />
            <span className="truncate text-sm">{roomState.channelName}</span>
            <span className="shrink-0 text-[0.625rem] text-wavis-text-secondary">{roomState.participants.length}/6</span>
            <span className="shrink-0 text-[0.625rem]" style={{ color: rttColor(roomState.networkStats.rttMs) }}>{roomState.networkStats.rttMs}ms</span>
          </div>
          <div className="flex items-center gap-2 shrink-0">
            <button onClick={toggleSelfMute} disabled={selfP?.isHostMuted} className={`px-2 py-1.5 border text-[0.625rem] transition-colors text-center disabled:opacity-40 disabled:cursor-not-allowed ${selfP?.isMuted ? 'border-wavis-danger text-wavis-danger bg-wavis-danger/8 hover:bg-wavis-danger hover:text-wavis-bg' : 'border-wavis-text-secondary text-wavis-text hover:bg-wavis-text-secondary hover:text-wavis-text-contrast'}`}>
              {selfP?.isMuted ? '/unmute' : '/mute'}
            </button>
            <button onClick={toggleSelfDeafen} className={`px-2 py-1.5 border text-[0.625rem] transition-colors text-center ${roomState.isDeafened ? 'border-wavis-purple text-wavis-purple hover:bg-wavis-purple hover:text-wavis-bg' : 'border-wavis-text-secondary text-wavis-text hover:bg-wavis-text-secondary hover:text-wavis-text-contrast'}`}>
              {roomState.isDeafened ? '/undeafen' : '/deafen'}
            </button>
            <button onClick={handleLeave} className="px-2 py-1.5 border border-wavis-danger text-wavis-danger text-[0.625rem] transition-colors text-center hover:bg-wavis-danger hover:text-wavis-bg">/leave</button>
          </div>
        </div>

        {mediaRetryBanner}

        {/* Tab bar */}
        <div className="flex border-b border-wavis-text-secondary bg-wavis-panel">
          {(['participants', 'chat', 'log'] as const).map((tab) => (
            <button
              key={tab}
              onClick={() => setMobileTab(tab)}
              className="flex-1 py-2 text-center border-r border-wavis-text-secondary last:border-r-0 text-xs"
              style={{
                color: mobileTab === tab ? 'var(--wavis-accent)' : 'var(--wavis-text-secondary)',
                backgroundColor: mobileTab === tab ? 'rgba(46,160,67,0.08)' : 'transparent',
              }}
            >
              {tab === 'participants' ? `VOICE (${roomState.participants.length})` : tab === 'chat' ? `CHAT (${roomState.chatMessages.length})` : `LOG (${roomState.events.length})`}
            </button>
          ))}
        </div>

        {/* Tab content */}
        <div className="flex-1 flex flex-col min-h-0 overflow-hidden">
          {showSettings ? (
            <Settings onClose={() => setShowSettings(false)} onNavigateAway={navigateAwayFromRoom} channelId={channelId} />
          ) : (
            <>
              {mobileTab === 'participants' && <div className="flex flex-col flex-1 min-h-0">{participantsSections}{youBar}</div>}
              {mobileTab === 'chat' && chatPanel}
              {mobileTab === 'log' && logPanel}
            </>
          )}
        </div>
      </div>

      {/* ═══ DESKTOP LAYOUT (md+) ═══ */}
      <div className="hidden md:flex flex-1 overflow-hidden">
        <div className="w-80 border-r border-wavis-text-secondary flex flex-col">
          {roomHeader}
          {mediaRetryBanner}
          {participantsSections}
          {youBar}
        </div>
        <div className="flex-1 flex flex-col min-h-0 min-w-0 overflow-hidden">
          {showSettings ? <Settings onClose={() => setShowSettings(false)} onNavigateAway={navigateAwayFromRoom} channelId={channelId} /> : chatPanel}
        </div>
        <div className="w-80 border-l border-wavis-text-secondary flex flex-col">
          {logPanel}
        </div>
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
                  className={`border px-4 py-1 text-xs transition-colors ${isMacPlatform
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
