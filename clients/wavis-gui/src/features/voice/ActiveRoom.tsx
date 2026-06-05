import { type ReactNode, useState, useEffect, useRef, useCallback } from 'react';
import { VolumeSlider } from '@shared/VolumeSlider';
import { useBlocker, useLocation, useNavigate } from 'react-router';
import type { ChannelRole } from '@features/channels/channels';
import type { Channel } from '@features/channels/channels';
import { ChannelSwitcherPanel } from '@features/channels/ChannelSwitcherPanel';
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
  scheduleLeaveRoom,
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
  unmuteParticipant,
  createSubRoom,
  joinSubRoom,
  leaveSubRoom,
  setPassthrough,
  clearPassthrough,
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
  persistStreamVolume,
  getPersistedStreamVolume,
  persistStreamMuted,
  getPersistedStreamMuted,
  activeShareType,
  computeStopRoute,
  startFallbackShare,
  startPortalShare,
  setPendingSharePickerData,
  buildChatDisplayItems,
  resolveChatMessageDisplayColor,
  preserveVideoShareSelectionForSourceChange,
} from './voice-room';
import type { ShareSelection, EnumerationResult } from '@features/screen-share/share-types';
import SharePicker from '@features/screen-share/SharePicker';
import type { OccupiedSlots } from '@features/screen-share/SharePicker';
import { invoke } from '@tauri-apps/api/core';
import { getCurrentWindow } from '@tauri-apps/api/window';
import { LogicalSize } from '@tauri-apps/api/dpi';
import { open } from '@tauri-apps/plugin-shell';
import { Tooltip, TooltipTrigger, TooltipContent } from '../../components/ui/tooltip';
import { WebviewWindow } from '@tauri-apps/api/webviewWindow';
import { emit, emitTo, listen } from '@tauri-apps/api/event';
import { startSending, stopSending, stopSendingForWindow, stopAllSending, resendStream } from '@features/screen-share/screen-share-viewer';
import { getWatchAllHotkey, getFocusMainHotkey, setLastChannel, clearLastChannel } from '@features/settings/settings-store';

const DEBUG_SHARE_VIEW = import.meta.env.VITE_DEBUG_SCREEN_SHARE_VIEW === 'true';
const DEBUG_SHARE_AUDIO = import.meta.env.VITE_DEBUG_SHARE_AUDIO === 'true';
const LOG_SS = '[wavis:active-room:screen-share]';
const ROOM_REMOVAL_COUNTDOWN_INTERVAL_MS = 10_000;
const NOT_AUTHORIZED_REJECTION_PREFIX = 'Unable to join voice.';
import { registerWatchAllHotkey, unregisterWatchAllHotkey, registerFocusMainHotkey, unregisterFocusMainHotkey } from '@shared/hotkey-bridge';
import { useHotkeys } from '@shared/useHotkeys';
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
import { selectRoomPanelTab } from './voice-room';
import { VideoTab } from './VideoTab';
import type { VideoTileSnapshot, VideoTileViewModel } from './camera-types';
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

function roomRemovalCountdownText(deleteAtMs: number | null, nowMs: number): string | null {
  if (deleteAtMs === null) return null;
  const remainingMs = Math.max(0, deleteAtMs - nowMs);
  const seconds = Math.max(
    10,
    Math.ceil(remainingMs / ROOM_REMOVAL_COUNTDOWN_INTERVAL_MS)
      * (ROOM_REMOVAL_COUNTDOWN_INTERVAL_MS / 1000),
  );
  return `Removing in less than ${seconds} seconds`;
}

function formatTime(isoString: string): string {
  try {
    const d = new Date(isoString);
    return d.toLocaleTimeString('en-US', { hour12: false });
  } catch {
    return '??:??:??';
  }
}

const CHAT_LINK_RE = /(https?:\/\/[^\s<]+|www\.[^\s<]+)/gi;
const TRAILING_LINK_PUNCTUATION_RE = /[.,!?;:)\]]+$/;

function normalizeChatLink(raw: string): string {
  return raw.toLowerCase().startsWith('www.') ? `https://${raw}` : raw;
}

function splitChatLink(raw: string): { hrefText: string; trailingText: string } {
  const trailingText = raw.match(TRAILING_LINK_PUNCTUATION_RE)?.[0] ?? '';
  return trailingText
    ? { hrefText: raw.slice(0, -trailingText.length), trailingText }
    : { hrefText: raw, trailingText: '' };
}

function renderChatText(text: string): ReactNode[] {
  const nodes: ReactNode[] = [];
  let lastIndex = 0;

  for (const match of text.matchAll(CHAT_LINK_RE)) {
    const raw = match[0];
    const index = match.index ?? 0;
    if (index > lastIndex) nodes.push(text.slice(lastIndex, index));

    const { hrefText, trailingText } = splitChatLink(raw);
    const href = normalizeChatLink(hrefText);
    nodes.push(
      <a
        key={`link-${index}-${hrefText}`}
        href={href}
        className="text-wavis-accent underline underline-offset-2 break-words hover:opacity-80"
        onClick={(event) => {
          event.preventDefault();
          void open(href);
        }}
        rel="noreferrer"
        title={href}
      >
        {hrefText}
      </a>,
    );
    if (trailingText) nodes.push(trailingText);
    lastIndex = index + raw.length;
  }

  if (lastIndex < text.length) nodes.push(text.slice(lastIndex));
  return nodes;
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

function buildVideoTileSnapshot(tile: VideoTileViewModel): VideoTileSnapshot {
  return {
    participantId: tile.participantId,
    displayName: tile.displayName,
    color: tile.color,
    hasTrack: tile.track !== null,
    isSelf: tile.isSelf,
    isMuted: tile.isMuted,
    hasError: tile.hasError,
  };
}

function areVideoTileSnapshotsEqual(a: VideoTileSnapshot, b: VideoTileSnapshot): boolean {
  return a.participantId === b.participantId
    && a.displayName === b.displayName
    && a.color === b.color
    && a.hasTrack === b.hasTrack
    && a.isSelf === b.isSelf
    && a.isMuted === b.isMuted
    && a.hasError === b.hasError;
}

/* ─── Sub-components ────────────────────────────────────────────── */

function signalingIndicator(
  state: VoiceRoomMachineState,
  lastRateLimitError: string | null,
): { color: string; label: string } {
  switch (state) {
    case 'active': return { color: 'var(--wavis-accent)', label: 'Signaling: connected' };
    case 'connecting':
    case 'authenticated':
    case 'joining': return { color: 'var(--wavis-warn)', label: 'Signaling: connecting...' };
    case 'reconnecting':
      return {
        color: 'var(--wavis-warn)',
        label: lastRateLimitError ? 'Signaling: reconnecting after rate limit...' : 'Signaling: reconnecting...',
      };
    case 'idle':
    default: return { color: 'var(--wavis-text-secondary)', label: 'Signaling: disconnected' };
  }
}

function mediaIndicator(state: MediaState, error: string | null): { color: string; label: string } {
  switch (state) {
    case 'connected': return { color: 'var(--wavis-accent)', label: 'Media: connected' };
    case 'connecting': return { color: 'var(--wavis-warn)', label: 'Media: connecting...' };
    case 'reconnecting': return { color: 'var(--wavis-warn)', label: 'Media: reconnecting...' };
    case 'failed': return { color: 'var(--wavis-danger)', label: `Media: failed${error ? ` — ${error}` : ''}` };
    case 'disconnected':
    default: return { color: 'var(--wavis-text-secondary)', label: 'Media: disconnected' };
  }
}

function shareLoadingNotice(label: string, className: string): ReactNode {
  return (
    <div className={className}>
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
      <span className="text-wavis-text-secondary">{label}</span>
    </div>
  );
}

function combinedStatusBadge(
  machine: VoiceRoomMachineState,
  media: MediaState,
): { text: string; color: string } {
  // Failed media takes priority
  if (media === 'failed') return { text: 'FAILED', color: 'var(--wavis-danger)' };
  // Media reconnecting takes priority over live/connected state
  if (media === 'reconnecting') return { text: 'RECONNECTING', color: 'var(--wavis-warn)' };
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
  const hotkeys = useHotkeys();
  const location = useLocation();
  const navigate = useNavigate();
  const { channelId, channelName, channelRole } =
    (location.state as { channelId: string; channelName: string; channelRole: ChannelRole }) ?? {};

  const [roomState, setRoomState] = useState<VoiceRoomState | null>(null);
  const [countdownNowMs, setCountdownNowMs] = useState(() => Date.now());

  const [, setLeaving] = useState(false);
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
  const skipUnmountLeaveRef = useRef(false);
  const chatThrottledRef = useRef(false);
  const [cliFocused, setCliFocused] = useState(false);
  const cliHistoryRef = useRef<string[]>([]);
  const cliHistoryIndexRef = useRef(-1);
  const cliDraftRef = useRef('');

  const [showSettings, setShowSettings] = useState(false);
  const [channelSwitcherOpen, setChannelSwitcherOpen] = useState(false);

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
  type MobileTab = 'participants' | 'chat' | 'log' | 'video';
  const [mobileTab, setMobileTab] = useState<MobileTab>('participants');

  const blocker = useBlocker(({ currentLocation, nextLocation }) =>
    shouldBlockRoomNavigation(
      currentLocation.pathname,
      nextLocation.pathname,
      allowNavigationRef.current,
    ));

  const leaveNavigationMode = roomState && (
    roomState.machineState === 'active'
    || roomState.machineState === 'reconnecting'
  ) ? 'deferred' : 'immediate';

  const navigateAwayFromRoom = useCallback(
    (target: string, leaveMode: 'none' | 'immediate' | 'deferred' = 'none') => {
      allowNavigationRef.current = true;
      if (leaveMode === 'deferred') {
        skipUnmountLeaveRef.current = true;
        scheduleLeaveRoom();
      } else {
        skipUnmountLeaveRef.current = false;
        if (leaveMode === 'immediate') leaveRoom();
      }
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

  useEffect(() => {
    const hasScheduledRemoval = roomState?.subRooms.some((subRoom) => subRoom.deleteAtMs !== null) ?? false;
    if (!hasScheduledRemoval) return;

    setCountdownNowMs(Date.now());
    const interval = window.setInterval(() => {
      setCountdownNowMs(Date.now());
    }, ROOM_REMOVAL_COUNTDOWN_INTERVAL_MS);

    return () => window.clearInterval(interval);
  }, [roomState?.subRooms]);

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
    skipUnmountLeaveRef.current = false;
    initSession(channelId, channelName, channelRole, setRoomState);
    return () => {
      if (!skipUnmountLeaveRef.current) {
        leaveRoom();
      }
      skipUnmountLeaveRef.current = false;
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

  // Clear persisted last channel when the saved channel is no longer safe to auto-join.
  useEffect(() => {
    if (
      roomState?.error === 'You were kicked'
      || roomState?.rejectionReason?.startsWith(NOT_AUTHORIZED_REJECTION_PREFIX)
    ) {
      void clearLastChannel();
    }
  }, [roomState?.error, roomState?.rejectionReason]);

  // Tray event wiring: dispatch tray menu actions to voice room
  useEffect(() => {
    const cleanup = listenTrayEvents((action: TrayAction) => {
      switch (action) {
        case 'toggle-mute':
          toggleSelfMute();
          break;
        case 'leave':
          navigateAwayFromRoom('/', leaveNavigationMode);
          break;
        case 'show':
          // handled by Rust side (window.show + set_focus)
          break;
      }
    });
    return cleanup;
  }, [channelId, leaveNavigationMode, navigateAwayFromRoom]);

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
      if (ev.shouldToast === false) continue;
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
  const videoPopoutWindowRef = useRef<WebviewWindow | null>(null);
  const videoPopoutReadyUnlistenRef = useRef<(() => void) | null>(null);
  const [videoPopoutOpen, setVideoPopoutOpen] = useState(false);
  const videoPopoutReadyRef = useRef(false);
  const watchAllHotkeyRef = useRef<string | null>(null);
  const focusMainHotkeyRef = useRef<string | null>(null);
  const toggleWatchAllRef = useRef<() => void>(() => { });

  // Screen share window state (multi-window: one per sharer)
  const [watchingShareIds, setWatchingShareIds] = useState<Set<string>>(new Set());
  const [shareVolumes, setShareVolumes] = useState<Map<string, number>>(new Map());
  const shareVolumesRef = useRef(shareVolumes);
  const watchAllVolumesRef = useRef<Map<string, number>>(new Map());
  const [shareMuted, setShareMuted] = useState<Map<string, boolean>>(new Map());
  const shareMutedRef = useRef(shareMuted);
  // Local mute state: key = participantId, value = pre-mute volume (presence means muted).
  const [localMicMuted, setLocalMicMuted] = useState<Map<string, number>>(new Map());
  const watchAllAttachedAudioRef = useRef<Set<string>>(new Set());
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
    enumResult: EnumerationResult;
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
  // Toast notification tracking
  const prevEventsLenRef = useRef(0);
  // Refs to the screen share OS windows (keyed by participantId)
  const shareWindowsRef = useRef<Map<string, ShareViewerWindow>>(new Map());
  const nativeShareViewersRef = useRef<Set<string>>(new Set());
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

  useEffect(() => {
    shareMutedRef.current = shareMuted;
  }, [shareMuted]);

  const prevStreamsRef = useRef<Map<string, MediaStream | null>>(new Map());
  useEffect(() => {
    if (!roomState) return;
    for (const id of watchingShareIds) {
      const current = roomState.screenShareStreams.get(id) ?? null;
      const prev = prevStreamsRef.current.get(id) ?? null;
      if (current && current !== prev) {
        console.log(LOG_SS, `resendStream(${id}, screen-share-${id}) — stream: ${current?.id}, prevStream: ${prev?.id ?? 'none'}, active: ${current?.active}, ts: ${Date.now()}`);
        resendStream(id, `screen-share-${id}`, current);
        attachScreenShareAudio(id);
        const volume =
          watchAllVolumesRef.current.get(id) ??
          shareVolumesRef.current.get(id) ??
          getPersistedStreamVolume(id) ??
          70;
        const muted = shareMutedRef.current.get(id) ?? getPersistedStreamMuted(id) ?? (volume === 0);
        setScreenShareAudioVolume(id, muted ? 0 : volume);
      }
    }
    prevStreamsRef.current = new Map(roomState.screenShareStreams);
  }, [watchingShareIds, roomState?.screenShareStreams]);

  useEffect(() => {
    const unlisten = listen('camera-popout:closed', () => {
      closeVideoPopoutWindow(true);
    });
    return () => { unlisten.then((fn) => fn?.()); };
  }, []);

  const getSavedShareVolume = useCallback((participantId: string) => {
    // watchAllVolumesRef is updated synchronously by syncScreenShareVolume;
    // shareVolumesRef lags by one render cycle (useEffect). Prefer the sync ref.
    return watchAllVolumesRef.current.get(participantId) ?? shareVolumesRef.current.get(participantId) ?? getPersistedStreamVolume(participantId) ?? 70;
  }, []);

  const getSavedShareMuted = useCallback((participantId: string) => {
    return shareMutedRef.current.get(participantId) ?? getPersistedStreamMuted(participantId) ?? (getSavedShareVolume(participantId) === 0);
  }, [getSavedShareVolume]);

  const applySavedScreenShareAudio = useCallback((participantId: string) => {
    const volume = getSavedShareVolume(participantId);
    setScreenShareAudioVolume(participantId, getSavedShareMuted(participantId) ? 0 : volume);
  }, [getSavedShareMuted, getSavedShareVolume]);

  const syncScreenShareMuted = useCallback((participantId: string, muted: boolean) => {
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
    setScreenShareAudioVolume(participantId, muted ? 0 : restoredVolume);
    emit('watch-all:restore-volume', { participantId, volume: restoredVolume, muted });
    emit('screen-share:restore-volume', { participantId, volume: restoredVolume, muted });
  }, [getSavedShareVolume]);

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
    setScreenShareAudioVolume(participantId, muted ? 0 : volume);
    persistStreamVolume(participantId, volume);
    persistStreamMuted(participantId, muted);
    emit('watch-all:restore-volume', { participantId, volume, muted });
    emit('screen-share:restore-volume', { participantId, volume, muted });
  }, []);

  const toggleLocalMicMute = useCallback((participantId: string, currentVolume: number) => {
    setLocalMicMuted((prev) => {
      const next = new Map(prev);
      if (next.has(participantId)) {
        const saved = next.get(participantId)!;
        next.delete(participantId);
        setParticipantVolume(participantId, saved);
      } else {
        next.set(participantId, currentVolume || 70);
        setParticipantVolume(participantId, 0);
      }
      return next;
    });
  }, []);

  const emitWatchAllRestoreVolume = useCallback((participantId: string) => {
    emit('watch-all:restore-volume', {
      participantId,
      volume: getSavedShareVolume(participantId),
      muted: getSavedShareMuted(participantId),
    });
  }, [getSavedShareMuted, getSavedShareVolume]);

  const getWatchAllScope = useCallback((currentState: VoiceRoomState | null) => {
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
      if (!watchAllWindowRef.current || !watchAllReadyRef.current) {
        console.log('[wavis:active-room] handleViewerReady: watch-all skipped — window:', !!watchAllWindowRef.current, 'ready:', watchAllReadyRef.current);
        return;
      }
      if (shareWindowsRef.current.has(participantId)) {
        console.log('[wavis:active-room] handleViewerReady: watch-all skipped — pop-out window exists for', participantId);
        return;
      }
      console.log('[wavis:active-room] handleViewerReady: attaching watch-all audio for', participantId);
      attachScreenShareAudio(participantId);
      applySavedScreenShareAudio(participantId);
      watchAllAttachedAudioRef.current.add(participantId);
      void emit('share:user-state', shareUserStateRef.current);
      void emit('watch-all:voice-participants', watchAllVoiceParticipantsRef.current);
      return;
    }

    const shareWindow = shareWindowsRef.current.get(participantId);
    if (!shareWindow || shareWindow.window.label !== windowLabel) return;
    attachScreenShareAudio(participantId);
    applySavedScreenShareAudio(participantId);
    void emit('share:user-state', shareUserStateRef.current);
    void emit('share:voice-participants', voiceParticipantsRef.current);
  }, [applySavedScreenShareAudio]);

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
    // Attach audio directly. If the tile already exists in Watch All,
    // share-added is a no-op and viewer-ready never fires, so audio would be
    // left unattached. This covers both the new-tile path (idempotent with
    // the viewer-ready attach) and the existing-tile path.
    if (!shareWindowsRef.current.has(participantId)) {
      attachScreenShareAudio(participantId);
      applySavedScreenShareAudio(participantId);
      watchAllAttachedAudioRef.current.add(participantId);
    }
  };

  const handleShareWindowClosed = (participantId: string) => {
    // The map entry marks which pop-out currently owns this participant.
    // If another close path already removed it, skip duplicate cleanup.
    if (!shareWindowsRef.current.delete(participantId)) return;
    stopSending(participantId, `screen-share-${participantId}`);
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
  };

  // Listen for child windows closing themselves
  useEffect(() => {
    const unlisten = listen<{ participantId: string }>('screen-share:closed', (event) => {
      const pid = event.payload.participantId;
      // Gate on delete — if closeShareWindow already handled this pid,
      // delete() returns false and we skip to avoid double-add.
      if (!shareWindowsRef.current.delete(pid)) return;
      stopSending(pid, `screen-share-${pid}`);
      if (!watchAllWindowRef.current || !watchAllReadyRef.current) {
        detachScreenShareAudio(pid);
      }
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
        if (isWindowsPlatform) {
          try {
            const enumResult = await invoke<EnumerationResult>('list_share_sources');
            setWinSharePicker({
              enumResult,
              occupied: { videoOccupied: false, audioOccupied: roomStateRef.current?.activeAudioShare !== null },
              isChangingSource: true,
              initialWithAudio: roomStateRef.current?.activeVideoShare?.withAudio ?? false,
            });
          } catch (err) {
            const detail = err instanceof Error ? err.message : String(err);
            toast.error(`Screen sharing failed: ${detail}`);
          }
        } else {
          await withPickerResize(isMacPlatform, () => changeShareSource());
        }
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
      listen<{ participantId: string; muted: boolean }>('watch-all:mute-change', (event) => {
        syncScreenShareMuted(event.payload.participantId, event.payload.muted);
      }),
    );
    cleanups.push(
      listen<{ participantId: string; muted: boolean }>('screen-share:mute-change', (event) => {
        syncScreenShareMuted(event.payload.participantId, event.payload.muted);
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
  }, [syncScreenShareMuted, syncScreenShareVolume]);

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
    const unlisten = listen<{ participantId: string; volume?: number; muted?: boolean }>('watch-all:pop-out', (event) => {
      const pid = event.payload.participantId;
      if (typeof event.payload.volume === 'number') {
        syncScreenShareVolume(pid, event.payload.volume);
      }
      if (typeof event.payload.muted === 'boolean') {
        syncScreenShareMuted(pid, event.payload.muted);
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
  }, [syncScreenShareMuted, syncScreenShareVolume]);

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
  const prevAudioOnlySharersRef = useRef<Set<string>>(new Set());
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
        stopSending(pid, 'watch-all');
        watchAllAttachedAudioRef.current.delete(pid);
        emit('watch-all:share-removed', { participantId: pid });
      }
    }

    prevWatchAllStreamsRef.current = new Map(currentStreams);
  }, [getWatchAllScope, watchAllOpen, roomState?.screenShareStreams, roomState?.participants, roomState?.joinedSubRoomId, roomState?.subRooms, roomState?.passthrough]);

  // Watch All: sync audio-only sharer additions/removals
  useEffect(() => {
    if (!roomState || !watchAllOpen || !watchAllReadyRef.current) return;
    const curr = roomState.audioOnlySharers;
    const prev = prevAudioOnlySharersRef.current;
    for (const identity of curr) {
      if (prev.has(identity)) continue;
      const participant = roomState.participants.find((p) => p.id === identity);
      if (!participant) continue;
      const vol = getSavedShareVolume(identity);
      const muted = getSavedShareMuted(identity);
      applySavedScreenShareAudio(identity);
      void emit('watch-all:audio-share-added', { participantId: identity, displayName: participant.displayName, color: participant.color, volume: vol, muted });
    }
    for (const identity of prev) {
      if (curr.has(identity)) continue;
      void emit('watch-all:audio-share-removed', { participantId: identity });
    }
    prevAudioOnlySharersRef.current = new Set(curr);
  }, [applySavedScreenShareAudio, getSavedShareMuted, getSavedShareVolume, watchAllOpen, roomState?.audioOnlySharers, roomState?.participants]);

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
  }, [getWatchAllScope, watchAllOpen, roomState?.participants, roomState?.joinedSubRoomId, roomState?.subRooms, roomState?.passthrough]);

  // Custom share picker + indicator event listeners
  useEffect(() => {
    const cleanups: Array<Promise<() => void>> = [];

    // Share picker selection → start custom share
    cleanups.push(
      listen<ShareSelection>('share-picker:selected', async (event) => {
        setPendingSharePickerData(null);
        const showStartingIndicator = isVideoShareSelectionMode(event.payload.mode);
        if (showStartingIndicator) setShareStarting(true);
        try {
          await startCustomShare(event.payload);
        } catch (err) {
          const msg = err instanceof Error ? err.message : String(err);
          showTransientScreenShareError(msg);
        } finally {
          if (showStartingIndicator) setShareStarting(false);
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
    const unlisten = listen('main-window-closing', async () => {
      closeAllShareWindows();
      leaveRoom();
      // Give the JS event loop a tick so room.disconnect() can flush its
      // WebSocket Leave signal to the LiveKit SFU before the webview is
      // destroyed. The Rust side uses prevent_close() until this resolves.
      await invoke('close_main_window').catch(() => {});
    });
    return () => { unlisten.then((fn) => fn()); };
  }, []);

  // Child windows (watch-all, screen-share) emit this to restore and focus the
  // main window. The main window restores itself — more reliable than calling
  // unminimize/setFocus on another window from a child webview.
  useEffect(() => {
    console.log('[wavis:focus-main] registering focus-main-window listener');
    const unlisten = listen('focus-main-window', async () => {
      console.log('[wavis:focus-main] event received — calling unminimize + setFocus');
      try {
        const win = getCurrentWindow();
        await win.unminimize();
        console.log('[wavis:focus-main] unminimize done');
        await win.setFocus();
        console.log('[wavis:focus-main] setFocus done');
      } catch (e) {
        console.error('[wavis:focus-main] error:', e);
      }
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
    if (nativeShareViewersRef.current.has(participantId)) {
      closeShareWindow(participantId);
    }

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
      initialMuted: getSavedShareMuted(participantId),
    };
    const hash = encodeURIComponent(JSON.stringify(params));
    const windowLabel = `screen-share-${participantId}`;

    try {
      if (stream === null) {
        await invoke('media_open_native_screen_share_viewer', {
          identity: participantId,
          title: `${participant.displayName} — screen share`,
        });

        nativeShareViewersRef.current.add(participantId);
        attachScreenShareAudio(participantId);
        applySavedScreenShareAudio(participantId);
        setWatchingShareIds((prev) => new Set(prev).add(participantId));

        if (watchAllWindowRef.current && watchAllReadyRef.current) {
          watchAllAttachedAudioRef.current.delete(participantId);
          prevWatchAllStreamsRef.current.delete(participantId);
          emit('watch-all:share-removed', { participantId });
        }
        return;
      }

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

      // If Watch All is open, remove this tile from the grid — the pop-out owns it now.
      // Do NOT detach audio here: the gain node and subscription remain alive so the
      // pop-out's handleViewerReady finds them intact and only needs to set the volume.
      // closeShareWindow handles detach when the pop-out is actually closed.
      if (watchAllWindowRef.current && watchAllReadyRef.current) {
        watchAllAttachedAudioRef.current.delete(participantId);
        stopSending(participantId, 'watch-all');
        prevWatchAllStreamsRef.current.delete(participantId);
        emit('watch-all:share-removed', { participantId });
      }
    } catch (err) {
      console.error('[wavis:active-room] failed to open screen share window:', err);
      showTransientScreenShareError(err instanceof Error ? err.message : String(err));
    }
  };

  /** Close a specific screen share OS window and clean up the bridge. */
  const closeShareWindow = (participantId: string) => {
    stopSending(participantId, `screen-share-${participantId}`);
    if (!watchAllWindowRef.current || !watchAllReadyRef.current) {
      detachScreenShareAudio(participantId);
    }
    if (nativeShareViewersRef.current.delete(participantId)) {
      invoke('media_close_native_screen_share_viewer', { identity: participantId }).catch(() => {});
    }
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
    closeVideoPopoutWindow();
    closeWatchAllWindow(); // close Watch All window first
    stopAllSending();
    for (const pid of nativeShareViewersRef.current) {
      detachScreenShareAudio(pid);
      invoke('media_close_native_screen_share_viewer', { identity: pid }).catch(() => {});
    }
    nativeShareViewersRef.current.clear();
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
  const prevVideoPopoutTracksRef = useRef<Map<string, MediaStreamTrack | null>>(new Map());
  const prevVideoPopoutTilesRef = useRef<Map<string, VideoTileSnapshot>>(new Map());

  const closeVideoPopoutWindow = (alreadyDestroyed = false) => {
    if (!videoPopoutWindowRef.current) return;
    if (videoPopoutReadyUnlistenRef.current) {
      videoPopoutReadyUnlistenRef.current();
      videoPopoutReadyUnlistenRef.current = null;
    }
    videoPopoutReadyRef.current = false;
    stopSendingForWindow('camera-popout');
    if (!alreadyDestroyed) {
      videoPopoutWindowRef.current.close().catch(() => { });
    }
    videoPopoutWindowRef.current = null;
    setVideoPopoutOpen(false);
    prevVideoPopoutTracksRef.current = new Map();
    prevVideoPopoutTilesRef.current = new Map();
  };

  const syncVideoPopoutSnapshot = useCallback((nextTilesById: Record<string, VideoTileViewModel>) => {
    const prevTracks = prevVideoPopoutTracksRef.current;
    const prevTiles = prevVideoPopoutTilesRef.current;
    const nextTracks = new Map<string, MediaStreamTrack | null>();
    const nextTiles = new Map<string, VideoTileSnapshot>();

    for (const tile of Object.values(nextTilesById)) {
      const snapshot = buildVideoTileSnapshot(tile);
      const sendableTrack = snapshot.hasTrack && !snapshot.isMuted && !snapshot.hasError ? tile.track : null;
      nextTracks.set(tile.participantId, sendableTrack);
      nextTiles.set(tile.participantId, snapshot);

      const prevSnapshot = prevTiles.get(tile.participantId);
      if (!prevSnapshot) {
        void emit('camera-popout:tile-added', snapshot);
      } else if (!areVideoTileSnapshotsEqual(prevSnapshot, snapshot)) {
        void emit('camera-popout:tile-updated', snapshot);
      }

      const prevTrack = prevTracks.get(tile.participantId) ?? null;
      if (sendableTrack && !prevTrack) {
        void startSending(tile.participantId, 'camera-popout', new MediaStream([sendableTrack]));
      } else if (sendableTrack && prevTrack !== sendableTrack) {
        void resendStream(tile.participantId, 'camera-popout', new MediaStream([sendableTrack]));
      } else if (!sendableTrack && prevTrack) {
        stopSending(tile.participantId, 'camera-popout');
      }
    }

    for (const participantId of prevTiles.keys()) {
      if (!nextTiles.has(participantId)) {
        stopSending(participantId, 'camera-popout');
        void emit('camera-popout:tile-removed', { participantId });
      }
    }

    prevVideoPopoutTracksRef.current = nextTracks;
    prevVideoPopoutTilesRef.current = nextTiles;
  }, []);

  const openVideoPopoutWindow = useCallback(async () => {
    if (videoPopoutWindowRef.current) {
      videoPopoutWindowRef.current.setFocus().catch(() => { });
      return;
    }

    const rs = roomStateRef.current;
    if (!rs) return;

    const hash = encodeURIComponent(JSON.stringify({ channelName: rs.channelName }));

    try {
      videoPopoutReadyRef.current = false;
      const unlistenReady = await listen('camera-popout:ready', () => {
        if (videoPopoutReadyRef.current) return;
        videoPopoutReadyRef.current = true;
        syncVideoPopoutSnapshot(roomStateRef.current?.videoTilesById ?? {});
      });
      videoPopoutReadyUnlistenRef.current = unlistenReady;

      const win = new WebviewWindow('camera-popout', {
        url: `/camera-popout#${hash}`,
        title: `Camera — ${rs.channelName}`,
        width: 960,
        height: 540,
        minWidth: 480,
        minHeight: 320,
        resizable: true,
        decorations: false,
        center: true,
      });

      win.once('tauri://error', (event) => {
        console.error('[wavis:active-room] video popout window error:', event);
        closeVideoPopoutWindow(true);
      });

      win.once('tauri://destroyed', () => {
        closeVideoPopoutWindow(true);
      });

      videoPopoutWindowRef.current = win;
      setVideoPopoutOpen(true);
      // Switch away from the video tab — it's now in the pop-out window
      selectRoomPanelTab('logs');
    } catch (err) {
      console.error('[wavis:active-room] failed to open video popout window:', err);
    }
  }, [syncVideoPopoutSnapshot]);

  useEffect(() => {
    if (!videoPopoutOpen) {
      prevVideoPopoutTracksRef.current = new Map();
      prevVideoPopoutTilesRef.current = new Map();
      return;
    }
    if (!videoPopoutReadyRef.current) return;
    syncVideoPopoutSnapshot(roomState?.videoTilesById ?? {});
  }, [roomState?.videoTilesById, syncVideoPopoutSnapshot, videoPopoutOpen]);

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
        watchAllWindowRef.current?.setFocus();
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
        // Seed audio-only sharers into Watch All
        for (const identity of rs.audioOnlySharers) {
          const participant = scope.participants.find((p) => p.id === identity);
          if (!participant) continue;
          const vol = getSavedShareVolume(identity);
          const muted = getSavedShareMuted(identity);
          applySavedScreenShareAudio(identity);
          void emit('watch-all:audio-share-added', { participantId: identity, displayName: participant.displayName, color: participant.color, volume: vol, muted });
        }
        prevAudioOnlySharersRef.current = new Set(rs.audioOnlySharers);
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

  /** Toggle the Watch All window:
   *  - closed          → open + focus
   *  - open + visible  → close
   *  - open + minimized → restore + focus (don't close)
   */
  const toggleWatchAllWindow = async () => {
    if (!watchAllWindowRef.current) {
      const hasShares = roomState ? getWatchAllScope(roomState).remoteSharers.length > 0 : false;
      if (hasShares) {
        openWatchAllWindow();
      }
      return;
    }
    const minimized = await watchAllWindowRef.current.isMinimized();
    if (minimized) {
      await watchAllWindowRef.current.unminimize();
      await watchAllWindowRef.current.setFocus();
    } else {
      closeWatchAllWindow();
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

  // Register Focus Main hotkey when media connects
  useEffect(() => {
    if (roomState?.mediaState !== 'connected') return;

    let cancelled = false;
    getFocusMainHotkey().then((hotkey) => {
      if (cancelled) return;
      focusMainHotkeyRef.current = hotkey;
      registerFocusMainHotkey(hotkey, () => { console.log('[wavis:focus-main] hotkey fired'); void getCurrentWindow().unminimize().then(() => getCurrentWindow().setFocus()).catch((e) => console.error('[wavis:focus-main] hotkey error:', e)); });
    });

    return () => {
      cancelled = true;
      if (focusMainHotkeyRef.current) {
        unregisterFocusMainHotkey(focusMainHotkeyRef.current);
        focusMainHotkeyRef.current = null;
      }
    };
  }, [roomState?.mediaState]);

  // Platform check: Linux uses standalone window (PostMessage works fine there).
  const isLinuxPlatform = typeof navigator !== 'undefined' && /Linux/.test(navigator.userAgent);
  const isMacPlatform = typeof navigator !== 'undefined' && /Mac/.test(navigator.userAgent);
  const isWindowsPlatform = typeof navigator !== 'undefined' && /Windows/.test(navigator.userAgent);
  const supportedCapturePlatform = isWindowsPlatform || isMacPlatform || isLinuxPlatform || hasBrowserCameraMediaSupport();
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
  const isVideoOrFallbackSharing = !!roomState?.activeVideoShare || (selfSharing && !roomState?.activeAudioShare);
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
  const watchAllScope = getWatchAllScope(roomState);
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
    } else if (route === 'stop_fallback') stopShare();
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
    participants: watchAllScope.participants
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
  }, [roomState, roomState?.participants, roomState?.selfParticipantId, roomState?.joinedSubRoomId, roomState?.subRooms, roomState?.passthrough]);

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

        // Windows: use native Rust source picker to avoid getDisplayMedia() and
        // the WebView2 capture indicator bar it triggers.
        if (isWindowsPlatform) {
          try {
            const enumResult = await invoke<EnumerationResult>('list_share_sources');
            const occupied: OccupiedSlots = {
              videoOccupied: roomState?.activeVideoShare !== null,
              audioOccupied: roomState?.activeAudioShare !== null,
            };
            setWinSharePicker({ enumResult, occupied });
          } catch (err) {
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
            onClick={() => navigateAwayFromRoom(`/channel/${channelId}`, 'immediate')}
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
              <button className="text-xs text-wavis-danger border border-wavis-danger py-0.5 px-1 text-center transition-colors hover:bg-wavis-danger hover:text-wavis-bg" onClick={() => { void clearLastChannel(); navigateAwayFromRoom('/', leaveNavigationMode); }}>/leave</button>
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
        <button
          className="text-xs text-wavis-text border border-wavis-text-secondary py-0.5 px-1 text-center transition-colors hover:bg-wavis-text-secondary hover:text-wavis-text-contrast"
          onClick={() => navigateAwayFromRoom('/')}
        >/back</button>
      </div>
    );
  }

  /* ── Actions ── */
  const handleLeave = () => {
    setLeaving(true);
    navigateAwayFromRoom('/', leaveNavigationMode);
  };

  const handleChannelSwitch = async (ch: Channel) => {
    await setLastChannel(ch.id, ch.name, ch.role);
    setChannelSwitcherOpen(false);
    allowNavigationRef.current = true;
    leaveRoom();
    navigate('/room', { state: { channelId: ch.id, channelName: ch.name, channelRole: ch.role } });
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

  const sigDot = signalingIndicator(roomState.machineState, roomState.lastRateLimitError);
  const mediaDot = mediaIndicator(roomState.mediaState, roomState.mediaError);
  const statusBadge = combinedStatusBadge(roomState.machineState, roomState.mediaState);

  const renderChannelSwitcherToggle = () => (
    <button
      onClick={() => { setChannelSwitcherOpen((v) => !v); setShowSettings(false); }}
      className={`shrink-0 border px-2 py-1 text-xs transition-colors ${
        channelSwitcherOpen
          ? 'border-wavis-accent text-wavis-accent hover:bg-wavis-accent hover:text-wavis-bg'
          : 'border-wavis-text-secondary text-wavis-text-secondary hover:border-wavis-accent hover:text-wavis-accent'
      }`}
      title="Change channel"
    >{channelSwitcherOpen ? '<' : '>'}</button>
  );

  const roomHeader = (
    <div className="px-3 py-3 border-b border-wavis-text-secondary h-[4.5rem] flex items-center gap-3 overflow-hidden">
      <div className="flex-1 flex flex-col justify-center gap-0.5 min-w-0">
        <div className="flex items-center gap-2">
          <StatusDot color={sigDot.color} label={sigDot.label} />
          <StatusDot color={mediaDot.color} label={mediaDot.label} />
          {(() => {
            const badge = connectionModeBadgeText(showSecrets, roomState.connectionMode);
            return badge ? <span className="text-[0.625rem] text-wavis-purple">[{badge}]</span> : null;
          })()}
          <span className="text-sm" style={{ color: statusBadge.color }}>{statusBadge.text}</span>
          <span className="text-[0.625rem] text-wavis-text-secondary">{Object.keys(roomState.participantSubRoomById).length}/6</span>
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
      {renderChannelSwitcherToggle()}
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

  const mediaReconnectingBanner = roomState.mediaState === 'reconnecting' ? (
    <div className="px-3 py-2 border-b border-wavis-warn bg-wavis-panel text-xs text-wavis-warn">
      Audio reconnecting... still in room
    </div>
  ) : null;

  const reconnectBanner = roomState.lastRateLimitError && (
    roomState.machineState === 'reconnecting' ||
    roomState.machineState === 'authenticated' ||
    roomState.machineState === 'joining'
  ) ? (
    <div className="px-3 py-2 border-b border-wavis-warn bg-wavis-panel text-xs text-wavis-warn">
      {roomState.lastRateLimitError}
    </div>
  ) : null;

  const renderParticipantRow = (p: RoomParticipant) => {
    const isSelf = p.id === roomState.selfParticipantId;
    const icon = voiceIcon(p, isSelf ? roomState.isDeafened : p.isDeafened);
    const videoTile = roomState.videoTilesById[p.id];
    const cameraOn = !!videoTile && !videoTile.isMuted && !videoTile.hasError;

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
          <span style={{
            color: p.color,
            animation: p.isSpeaking && !p.isMuted ? 'pulse 3s ease-in-out infinite' : 'none',
            filter: p.isSpeaking && !p.isMuted ? 'brightness(1.5)' : 'brightness(0.7)',
          }}>{p.displayName}</span>
          <span style={{ color: icon.color, textDecoration: icon.strikethrough ? 'line-through' : undefined, ...(icon.transform ? { display: 'inline-block', transform: icon.transform } : {}) }}>{icon.char}</span>
          <div className="ml-auto flex items-center gap-1">
            {cameraOn && (
              <span
                aria-label={isSelf ? 'your camera is on' : `${p.displayName}'s camera is on`}
                title={isSelf ? 'your camera is on' : `${p.displayName}'s camera is on`}
                style={{
                  display: 'inline-block',
                  width: '0.75rem',
                  height: '0.75rem',
                  backgroundColor: 'var(--wavis-accent)',
                  WebkitMaskImage: 'url(/video-camera.png)',
                  WebkitMaskSize: 'contain',
                  WebkitMaskRepeat: 'no-repeat',
                  WebkitMaskPosition: 'center',
                  maskImage: 'url(/video-camera.png)',
                  maskSize: 'contain',
                  maskRepeat: 'no-repeat',
                  maskPosition: 'center',
                  pointerEvents: 'none',
                }}
              />
            )}
            {isSelf && p.isSharing && (
              <>
                {(roomState.activeVideoShare !== null || roomState.activeAudioShare === null) && (
                  <span
                    className="text-sm leading-none"
                    style={{ color: 'var(--wavis-danger)', animation: 'watchPulse 2s ease-in-out infinite' }}
                    title="you are sharing"
                  >
                    {"\u25C9"}
                  </span>
                )}
                {roomState.activeAudioShare !== null && (
                  <span
                    className="text-sm leading-none"
                    style={{ color: 'var(--wavis-danger)', animation: 'watchPulse 2s ease-in-out infinite' }}
                    title="you are sharing audio"
                  >
                    {"\u266A"}
                  </span>
                )}
              </>
            )}
            {!isSelf && p.isSharing && (() => {
              const isAudioOnly = roomState.audioOnlySharers.has(p.id);
              if (isAudioOnly) {
                return (
                  <span
                    className="text-sm leading-none"
                    style={{ color: 'var(--wavis-danger)', animation: 'watchPulse 2s ease-in-out infinite' }}
                    title="sharing audio"
                  >
                    {"\u266A"}
                  </span>
                );
              }
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
              <span className="text-wavis-text-secondary shrink-0">mic</span>
              <div className="flex-1">
                <VolumeSlider
                  value={p.volume}
                  onChange={(v) => {
                    if (localMicMuted.has(p.id)) setLocalMicMuted((prev) => { const next = new Map(prev); next.delete(p.id); return next; });
                    setParticipantVolume(p.id, v);
                  }}
                  color={p.color}
                />
              </div>
              <span className="text-wavis-text-secondary w-6 text-right">{localMicMuted.has(p.id) ? 0 : p.volume}</span>
              <button
                onClick={() => toggleLocalMicMute(p.id, p.volume)}
                className="text-xs border px-1 py-0.5 transition-colors hover:opacity-70 shrink-0"
                style={localMicMuted.has(p.id) ? { color: 'var(--wavis-warn)', borderColor: 'var(--wavis-warn)' } : undefined}
                title={localMicMuted.has(p.id) ? 'unmute mic (local)' : 'mute mic (local)'}
              >{localMicMuted.has(p.id) ? '/unmute' : '/mute'}</button>
            </div>
            {p.isSharing && (
              <div className="flex items-center gap-2 mt-1">
                <span className="text-wavis-text-secondary shrink-0">share vol</span>
                <div className="flex-1">
                  <VolumeSlider
                    value={shareVolumes.get(p.id) ?? getSavedShareVolume(p.id)}
                    onChange={(v) => {
                      syncScreenShareVolume(p.id, v);
                    }}
                    color={p.color}
                  />
                </div>
                <span className="text-wavis-text-secondary w-6 text-right">{getSavedShareMuted(p.id) ? 0 : (shareVolumes.get(p.id) ?? getSavedShareVolume(p.id))}</span>
                <button
                  onClick={() => syncScreenShareMuted(p.id, !getSavedShareMuted(p.id))}
                  className="text-xs border px-1 py-0.5 transition-colors hover:opacity-70 shrink-0"
                  style={getSavedShareMuted(p.id) ? { color: 'var(--wavis-warn)', borderColor: 'var(--wavis-warn)' } : undefined}
                  title={getSavedShareMuted(p.id) ? 'unmute sys audio (local)' : 'mute sys audio (local)'}
                >{getSavedShareMuted(p.id) ? '/unmute' : '/mute'}</button>
              </div>
            )}
            {isHost && (
              <div className="flex flex-wrap items-center justify-center gap-2">
                <button onClick={() => kickParticipant(p.id)} className="text-xs text-center border border-wavis-danger text-wavis-danger px-1 py-0.5 transition-colors hover:opacity-70">/kick</button>
                {p.isHostMuted
                  ? <button onClick={() => unmuteParticipant(p.id)} className="text-xs text-center border border-wavis-accent text-wavis-accent px-1 py-0.5 transition-colors hover:opacity-70">/unmute</button>
                  : !p.isMuted && <button onClick={() => muteParticipant(p.id)} className="text-xs text-center border px-1 py-0.5 transition-colors hover:opacity-70" style={{ color: 'var(--wavis-warn)', borderColor: 'var(--wavis-warn)' }}>/mute</button>
                }
                {p.isSharing && <button onClick={() => stopParticipantShare(p.id)} className="text-xs text-center border border-wavis-danger text-wavis-danger px-1 py-0.5 transition-colors hover:opacity-70">/revoke</button>}
              </div>
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
        const passthrough = roomState.passthrough;
        const pairedSubRoomId = passthrough?.sourceSubRoomId === roomState.joinedSubRoomId
          ? passthrough.targetSubRoomId
          : passthrough?.targetSubRoomId === roomState.joinedSubRoomId
            ? passthrough.sourceSubRoomId
            : null;
        const isWatchAllScopedRoom = isJoinedRoom || pairedSubRoomId === subRoom.id;
        const showEnabledWatchAll = isWatchAllScopedRoom && roomRemoteSharers.length > 0;
        const showDisabledWatchAll = !isWatchAllScopedRoom && roomRemoteSharers.length > 0;
        const roomRemovalText = roomParticipants.length === 0
          ? roomRemovalCountdownText(subRoom.deleteAtMs, countdownNowMs)
          : null;
        const activePassthrough = roomState.passthrough;
        const activePassthroughInvolvesRoom = !!activePassthrough
          && (activePassthrough.sourceSubRoomId === subRoom.id || activePassthrough.targetSubRoomId === subRoom.id);
        const activePassthroughInvolvesLocalRoom = !!activePassthrough
          && !!roomState.joinedSubRoomId
          && (
            activePassthrough.sourceSubRoomId === roomState.joinedSubRoomId
            || activePassthrough.targetSubRoomId === roomState.joinedSubRoomId
          );
        const canSetPassthrough = roomState.passthroughEnabled && !activePassthrough && !!roomState.joinedSubRoomId && !isJoinedRoom;
        const canClearPassthrough = activePassthroughInvolvesRoom && activePassthroughInvolvesLocalRoom;
        const passthroughDisabled = !(canSetPassthrough || canClearPassthrough);
        const passthroughLabel = activePassthroughInvolvesRoom && activePassthrough?.label
          ? `“${activePassthrough.label}”`
          : '“ ”';
        const passthroughClassName = activePassthroughInvolvesRoom
          ? 'border-wavis-danger text-wavis-danger hover:bg-wavis-danger hover:text-wavis-bg'
          : passthroughDisabled
            ? 'border-wavis-text-secondary text-wavis-text-secondary opacity-60 cursor-not-allowed'
            : 'border-wavis-accent text-wavis-accent hover:bg-wavis-accent hover:text-wavis-bg';
        const passthroughButton = (
          <Tooltip>
            <TooltipTrigger asChild>
              <button
                type="button"
                disabled={passthroughDisabled}
                aria-label="Passthrough: listen and talk to this room at a lower volume"
                onPointerDown={(event) => {
                  event.stopPropagation();
                }}
                onClick={(event) => {
                  event.stopPropagation();
                  if (canClearPassthrough) {
                    clearPassthrough();
                  } else if (canSetPassthrough) {
                    setPassthrough(subRoom.id);
                  }
                }}
                className={`min-w-7 text-xs py-0.5 px-1 border transition-colors cursor-pointer disabled:cursor-not-allowed ${passthroughClassName}`}
              >
                {passthroughLabel}
              </button>
            </TooltipTrigger>
            <TooltipContent side="bottom" className="bg-wavis-panel text-wavis-text border border-wavis-text-secondary font-mono text-xs">
              Passthrough: listen and talk to this room at a lower volume
            </TooltipContent>
          </Tooltip>
        );
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
              <div className="shrink-0 flex items-center gap-1">
                {passthroughButton}
                {roomActionButton}
              </div>
            </div>
            {isExpanded && (
              <div id={roomPanelId} className="px-3 py-2 space-y-1 text-sm">
                {roomParticipants.length > 0 ? roomParticipants.map(renderParticipantRow) : (
                  <div className="pl-8 text-xs text-wavis-text-secondary space-y-1">
                    <div>No participants in this room.</div>
                    {roomRemovalText && <div>{roomRemovalText}</div>}
                  </div>
                )}
                {(showEnabledWatchAll || showDisabledWatchAll) && (
                  <div className="pt-2 flex items-center justify-end gap-2">
                    {showEnabledWatchAll && (
                      <button
                        type="button"
                        onClick={toggleWatchAllWindow}
                        title={`${watchAllOpen ? '/close-all' : '/watch-all'} (${hotkeys.watchAll})`}
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
    <div className="shrink-0 p-4 border-t border-wavis-text-secondary">
      <div className="flex items-center gap-1 border-b border-wavis-text-secondary font-mono text-wavis-text">
        <button onClick={() => toggleSection('you')} className="bg-transparent outline-none px-1 py-1 text-xs text-wavis-text-secondary hover:opacity-80">
          {expandedSections.you ? '[-]' : '[+]'}
        </button>
        <button onClick={() => toggleSection('you')} className="bg-transparent outline-none py-1 px-1 text-left flex items-center gap-2 hover:opacity-80">
          <span style={{ color: selfP?.color }}>{selfP?.displayName}</span>
        </button>
        {!expandedSections.you && (
          <div className="ml-auto flex items-center leading-none text-xs">
            <button
              onClick={toggleSelfMute}
              disabled={!!selfP?.isHostMuted}
              className="px-1.5 h-5 flex items-center justify-center disabled:opacity-40 disabled:cursor-not-allowed hover:opacity-70 transition-opacity"
              style={{ color: selfP?.isMuted ? 'var(--wavis-danger)' : 'var(--wavis-text-secondary)' }}
              title={selfP?.isMuted ? `/unmute (${hotkeys.mute})` : `/mute (${hotkeys.mute})`}
            ><span className="inline-flex w-3 h-3 items-center justify-center leading-none">○</span></button>
            <span className="text-wavis-text-secondary opacity-30 select-none leading-none">│</span>
            <button
              onClick={toggleSelfDeafen}
              className="px-1.5 h-5 flex items-center justify-center hover:opacity-70 transition-opacity"
              style={{ color: roomState.isDeafened ? 'var(--wavis-danger)' : 'var(--wavis-text-secondary)' }}
              title={roomState.isDeafened ? '/undeafen' : '/deafen'}
            ><span className="inline-flex w-3 h-3 items-center justify-center leading-none" style={{ fontSize: '1.1em' }}>¤</span></button>
            {showCameraButton && (
              <>
                <span className="text-wavis-text-secondary opacity-30 select-none leading-none">│</span>
                <button
                  onClick={toggleCameraIntent}
                  disabled={cameraButtonDisabled}
                  className="px-1.5 h-5 flex items-center justify-center hover:opacity-70 transition-opacity disabled:opacity-40 disabled:cursor-not-allowed"
                  style={{ color: roomState.cameraIntent ? 'var(--wavis-danger)' : 'var(--wavis-text-secondary)' }}
                  title={cameraLabel}
                  aria-label={videoButtonLabel}
                ><span style={{
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
                  }} /></button>
              </>
            )}
            <span className="text-wavis-text-secondary opacity-30 select-none leading-none">│</span>
            <button
              onClick={isVideoOrFallbackSharing ? stopShareAction : handleStartShare}
              disabled={!isVideoOrFallbackSharing && (!shareEnabled || sharePickerLoading || shareStarting)}
              className="px-1.5 h-5 flex items-center justify-center disabled:opacity-40 disabled:cursor-not-allowed hover:opacity-70 transition-opacity"
              style={{ color: isVideoOrFallbackSharing ? 'var(--wavis-danger)' : 'var(--wavis-text-secondary)' }}
              title={isVideoOrFallbackSharing ? '/stopshare' : '/share'}
            ><span className="inline-flex w-3 h-3 items-center justify-center leading-none">◉</span></button>
          </div>
        )}
      </div>
      {expandedSections.you && (
        <div className="pt-2 pl-6 text-sm">
          <div className="flex flex-col gap-1 w-full">
            <div className="flex flex-col md:flex-row gap-1">
              <button onClick={toggleSelfMute} disabled={selfP?.isHostMuted} title={selfP?.isMuted ? `/unmute (${hotkeys.mute})` : `/mute (${hotkeys.mute})`} className={`flex-1 py-0.5 px-1 text-xs text-center transition-colors border disabled:opacity-40 disabled:cursor-not-allowed ${selfP?.isMuted ? 'border-wavis-danger text-wavis-danger bg-wavis-danger/8 hover:bg-wavis-danger hover:text-wavis-bg' : 'border-wavis-text-secondary text-wavis-text hover:bg-wavis-text-secondary hover:text-wavis-text-contrast'}`}>{selfP?.isMuted ? '/unmute' : '/mute'}</button>
              <button onClick={toggleSelfDeafen} className={`flex-1 py-0.5 px-1 text-xs text-center transition-colors border ${roomState.isDeafened ? 'border-wavis-purple text-wavis-purple hover:bg-wavis-purple hover:text-wavis-bg' : 'border-wavis-text-secondary text-wavis-text hover:bg-wavis-text-secondary hover:text-wavis-text-contrast'}`}>{roomState.isDeafened ? '/undeafen' : '/deafen'}</button>
            </div>
            {showCameraButton && (
              <button
                onClick={toggleCameraIntent}
                disabled={cameraButtonDisabled}
                className={`w-full py-0.5 px-1 text-xs text-center transition-colors border disabled:opacity-40 disabled:cursor-not-allowed ${roomState.cameraIntent ? 'border-wavis-danger text-wavis-danger hover:bg-wavis-danger hover:text-wavis-bg' : 'border-wavis-text-secondary text-wavis-text hover:bg-wavis-text-secondary hover:text-wavis-text-contrast'}`}
              >
                {cameraLabel}
              </button>
            )}
            {(() => {
              const isVideoActive = roomState.activeVideoShare !== null;
              const isFallbackSharing = selfSharing && !isVideoActive && roomState.activeAudioShare === null;
              if (isVideoActive || isFallbackSharing) {
                return (
                  <>
                    <button
                      onClick={stopShareAction}
                      className="w-full py-0.5 px-1 text-xs text-center transition-colors border text-wavis-danger border-wavis-danger hover:bg-wavis-danger hover:text-wavis-bg"
                    >
                      /stopshare
                    </button>
                    {sharePickerLoading && shareLoadingNotice(
                      SHARE_PICKER_LOADING_LABEL,
                      '-mt-1 border-x border-b border-wavis-text-secondary/30 bg-wavis-panel p-2 text-xs flex items-center gap-2',
                    )}
                    {shareStarting && shareLoadingNotice(
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
                                  try {
                                    const enumResult = await invoke<EnumerationResult>('list_share_sources');
                                    setWinSharePicker({
                                      enumResult,
                                      occupied: { videoOccupied: false, audioOccupied: roomState.activeAudioShare !== null },
                                      isChangingSource: true,
                                      initialWithAudio: roomState.activeVideoShare?.withAudio ?? false,
                                    });
                                  } catch (err) {
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
                            const companionBlocked = !isMacPlatform && !shareAudioOn && roomState.activeAudioShare !== null;
                            const companionDisabled = isMacPlatform || companionBlocked;
                            return (
                              <button
                                onClick={async () => {
                                  if (companionDisabled) return;
                                  const next = !shareAudioOn;
                                  const ok = await toggleShareAudio(next);
                                  if (ok) setShareAudioOn(next);
                                }}
                                disabled={companionDisabled}
                                title={companionBlocked ? 'audio device busy — stop your audio share first' : undefined}
                                className={`flex-1 py-0.5 px-1 text-xs text-center border transition-colors ${companionDisabled
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
                            const label = q === 'low' ? 'Smooth  1080p @ 60fps' : q === 'high' ? 'Sharp   1440p @ 60fps' : 'Max     1440p @ 60fps';
                            return <option key={q} value={q}>{label}</option>;
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
                    onClick={handleStartShare}
                    disabled={shareDisabled}
                    className="w-full py-0.5 px-1 text-xs text-center transition-colors border disabled:opacity-40 disabled:cursor-not-allowed border-wavis-purple text-wavis-purple hover:bg-wavis-purple hover:text-wavis-bg"
                  >
                    {shareButtonLabel(shareEnabled, false, roomState.sharePermission, isHost)}
                  </button>
                  {sharePickerLoading && shareLoadingNotice(
                    SHARE_PICKER_LOADING_LABEL,
                    '-mt-1 border-x border-b border-wavis-text-secondary/30 bg-wavis-panel p-2 text-xs flex items-center gap-2',
                  )}
                  {shareStarting && shareLoadingNotice(
                    SHARE_STARTING_LOADING_LABEL,
                    '-mt-1 border-x border-b border-wavis-text-secondary/30 bg-wavis-panel p-2 text-xs flex items-center gap-2',
                  )}
                </>
              );
            })()}
            {roomState.activeVideoShare !== null && roomState.activeAudioShare === null && (
              <button
                onClick={handleStartShare}
                disabled={!shareEnabled || sharePickerLoading || !!roomState.activeVideoShare.withAudio}
                title={roomState.activeVideoShare.withAudio ? 'audio device busy — turn off /audio first' : undefined}
                className="w-full py-0.5 px-1 text-xs text-center transition-colors border disabled:opacity-40 disabled:cursor-not-allowed border-wavis-text-secondary text-wavis-text hover:bg-wavis-text-secondary hover:text-wavis-text-contrast"
              >
                /share audio
              </button>
            )}
            {roomState.activeAudioShare !== null && (
              <button
                onClick={() => { void stopCustomShare('audio'); }}
                className="w-full py-0.5 px-1 text-xs text-center transition-colors border border-wavis-danger text-wavis-danger hover:bg-wavis-danger hover:text-wavis-bg"
              >
                /stop-audio
              </button>
            )}
            <div className="mt-4 flex flex-col gap-1">
              <button
                onClick={() => { if (showSettings) { setShowSettings(false); } else { setShowSettings(true); setChannelSwitcherOpen(false); } }}
                className={`w-full border py-0.5 px-1 text-xs text-center transition-colors ${showSettings ? 'border-wavis-accent text-wavis-accent hover:bg-wavis-accent hover:text-wavis-bg' : 'border-wavis-text-secondary text-wavis-text hover:bg-wavis-text-secondary hover:text-wavis-text-contrast'}`}
              >
                {showSettings ? '/close-settings' : '/settings'}
              </button>
            </div>
          </div>
        </div>
      )}
      {!expandedSections.you && sharePickerLoading && shareLoadingNotice(
        SHARE_PICKER_LOADING_LABEL,
        'ml-6 mt-2 border border-wavis-text-secondary/30 bg-wavis-panel p-2 text-xs flex items-center gap-2',
      )}
      {!expandedSections.you && shareStarting && shareLoadingNotice(
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
    <div className="flex-1 flex flex-col min-h-0">
      <div className="px-3 py-3 border-b border-wavis-text-secondary h-[4.5rem] flex flex-col justify-center">
        <div className="font-bold text-sm">CHAT</div>
      </div>
      <div className="flex-1 overflow-y-auto overflow-x-hidden p-4 space-y-1 text-sm">
        {roomState.chatMessages.length === 0 && (
          <div className="text-wavis-text-secondary">No messages yet</div>
        )}
        {buildChatDisplayItems(roomState.chatMessages).map((item) =>
          item.type === 'date-divider' ? (
            <div key={item.id} className="text-wavis-text-secondary text-xs py-1 text-center">
              {'─'.repeat(12)} {item.label} {'─'.repeat(12)}
            </div>
          ) : (
            <div key={item.message.id} className="break-words">
              <span className="text-wavis-text-secondary">[{formatTime(item.message.timestamp)}]</span>{' '}
              <span style={{ color: resolveChatMessageDisplayColor(item.message, roomState.participants) }}>{item.message.displayName}</span>
              <span>: {renderChatText(item.message.text)}</span>
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

  const currentPanelTab = roomState?.roomPanelTab ?? 'logs';
  const videoTilesById = roomState?.videoTilesById ?? {};

  const logsContent = (
    <>
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
    </>
  );

  // Desktop right-panel: LOGS / VIDEOS tab switcher
  const logPanel = (
    <div className="flex-1 flex flex-col min-h-0">
      {/* ── Tab header ── */}
      <div className="flex h-[4.5rem] border-b border-wavis-text-secondary">
        {(['logs', 'video'] as const).filter((tab) => !(tab === 'video' && videoPopoutOpen)).map((tab) => {
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
      {currentPanelTab === 'video' && !videoPopoutOpen ? (
        <>
          <VideoTab videoTilesById={videoTilesById} />
          <div className="shrink-0 p-4 border-t border-wavis-text-secondary -translate-y-px">
            <button
              onClick={() => { void openVideoPopoutWindow(); }}
              className={`w-full py-[7px] px-2 text-xs text-center transition-colors border ${videoPopoutOpen ? 'border-wavis-accent text-wavis-accent hover:bg-wavis-accent hover:text-wavis-bg' : 'border-wavis-text-secondary text-wavis-text hover:bg-wavis-text-secondary hover:text-wavis-text-contrast'}`}
            >
              /pop-out
            </button>
          </div>
        </>
      ) : (
        logsContent
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
            <span className="shrink-0 text-[0.625rem] text-wavis-text-secondary">{Object.keys(roomState.participantSubRoomById).length}/6</span>
            <span className="shrink-0 text-[0.625rem]" style={{ color: rttColor(roomState.networkStats.rttMs) }}>{roomState.networkStats.rttMs}ms</span>
          </div>
          {renderChannelSwitcherToggle()}
        </div>

        {mediaReconnectingBanner}
        {mediaRetryBanner}
        {reconnectBanner}

        {/* Tab bar */}
        <div className="flex border-b border-wavis-text-secondary bg-wavis-panel">
          {(['participants', 'chat', 'log', 'video'] as const).filter((tab) => !(tab === 'video' && videoPopoutOpen)).map((tab) => {
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
                style={{ color, backgroundColor: active ? 'rgba(46,160,67,0.08)' : 'transparent' }}
              >
                {tab === 'participants' ? `VOICE (${Object.keys(roomState.participantSubRoomById).length})`
                  : tab === 'chat' ? `CHAT (${roomState.chatMessages.length})`
                  : tab === 'log' ? `LOG (${roomState.events.length})`
                  : 'VIDEO'
                }
              </button>
            );
          })}
        </div>

        {/* Tab content */}
        <div className="flex-1 flex flex-col min-h-0 overflow-hidden">
          {showSettings ? (
            <Settings onClose={() => setShowSettings(false)} onNavigateAway={navigateAwayFromRoom} channelId={channelId} />
          ) : channelSwitcherOpen ? (
            <ChannelSwitcherPanel
              onChannelSelect={handleChannelSwitch}
              onClose={() => setChannelSwitcherOpen(false)}
              currentChannelId={channelId}
            />
          ) : (
            <>
              {mobileTab === 'participants' && <div className="flex flex-col flex-1 min-h-0">{participantsSections}{youBar}</div>}
              {mobileTab === 'chat' && chatPanel}
              {mobileTab === 'log' && logsContent}
              {mobileTab === 'video' && !videoPopoutOpen && (
                <div className="flex flex-col flex-1 min-h-0">
                  <VideoTab videoTilesById={videoTilesById} />
                  <div className="shrink-0 p-4 border-t border-wavis-text-secondary -translate-y-px">
                    <button
                      onClick={() => { void openVideoPopoutWindow(); }}
                      className={`w-full py-[7px] px-2 text-xs text-center transition-colors border ${videoPopoutOpen ? 'border-wavis-accent text-wavis-accent hover:bg-wavis-accent hover:text-wavis-bg' : 'border-wavis-text-secondary text-wavis-text hover:bg-wavis-text-secondary hover:text-wavis-text-contrast'}`}
                    >
                      /pop-out
                    </button>
                  </div>
                </div>
              )}
            </>
          )}
        </div>
      </div>

      {/* ═══ DESKTOP LAYOUT (md+) ═══ */}
      <div className="hidden md:flex flex-1 overflow-hidden">
        <div className="w-80 border-r border-wavis-text-secondary flex flex-col">
          {roomHeader}
          {mediaReconnectingBanner}
          {mediaRetryBanner}
          {reconnectBanner}
          {participantsSections}
          {youBar}
        </div>
        <div className="flex-1 flex flex-col min-h-0 min-w-0 overflow-hidden">
          {showSettings ? <Settings onClose={() => setShowSettings(false)} onNavigateAway={navigateAwayFromRoom} channelId={channelId} /> : channelSwitcherOpen ? (
            <ChannelSwitcherPanel
              onChannelSelect={handleChannelSwitch}
              onClose={() => setChannelSwitcherOpen(false)}
              currentChannelId={channelId}
            />
          ) : chatPanel}
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
      {winSharePicker && (
        <div className="fixed inset-0 z-50 flex items-center justify-center bg-wavis-bg/80 px-4">
          <div className="w-[640px] max-w-[90vw] h-[480px] max-h-[80vh] border border-wavis-text-secondary shadow-xl overflow-hidden flex flex-col">
            <SharePicker
              enumResult={winSharePicker.enumResult}
              occupied={winSharePicker.occupied}
              modeScope={winSharePicker.isChangingSource ? 'video_only' : 'all'}
              initialWithAudio={winSharePicker.initialWithAudio}
              onSelect={async (selection) => {
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
                    await stopCustomShare('video', { suppressSignaling: true, keepPublication: true });
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
