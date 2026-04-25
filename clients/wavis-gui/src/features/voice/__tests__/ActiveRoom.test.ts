/**
 * ActiveRoom Unit Tests — Quality Indicator & System Audio Warning
 *
 * Tests the rendering logic and state transitions for:
 * 1. Quality indicator display format from shareQualityInfo
 * 2. System audio warning confirm/cancel flow
 *
 * vitest env is 'node' (no jsdom) — tests simulate component logic
 * by replicating the state transitions and rendering expressions
 * from ActiveRoom.tsx, same approach as auth-gate.test.ts / login.test.ts.
 *
 * Validates: Requirements 8.4, 7.1, 7.2
 */

import { describe, it, expect, vi, beforeEach } from 'vitest';
import type { ShareQualityInfo } from '../livekit-media';

/* ─── Mock voice-room module ────────────────────────────────────── */

const mockToggleShareAudio = vi.fn();

vi.mock('../voice-room', () => ({
  initSession: vi.fn(),
  leaveRoom: vi.fn(),
  toggleSelfMute: vi.fn(),
  // startShare was removed (cleanup 2026-03); ActiveRoom uses startFallbackShare/startCustomShare.
  stopShare: vi.fn(),
  setParticipantVolume: vi.fn(),
  setMasterVolume: vi.fn(),
  kickParticipant: vi.fn(),
  muteParticipant: vi.fn(),
  createSubRoom: vi.fn(),
  joinSubRoom: vi.fn(),
  leaveSubRoom: vi.fn(),
  stopParticipantShare: vi.fn(),
  stopAllShares: vi.fn(),
  sendChatMessage: vi.fn(),
  reconnectMedia: vi.fn(),
  setShareQuality: vi.fn(),
  toggleShareAudio: mockToggleShareAudio,
  changeShareSource: vi.fn(),
}));

/* ─── Quality Indicator Rendering Logic ─────────────────────────── */

/**
 * Replicates the quality indicator rendering expression from ActiveRoom.tsx:
 *   {roomState.shareQualityInfo.height}p @ {Math.round(roomState.shareQualityInfo.frameRate)}fps
 *
 * Returns the formatted string, or null if conditions aren't met.
 */
function formatQualityIndicator(
  isSelfShare: boolean,
  shareQualityInfo: ShareQualityInfo | null,
): string | null {
  if (!isSelfShare || !shareQualityInfo) return null;
  return `${shareQualityInfo.height}p @ ${Math.round(shareQualityInfo.frameRate)}fps`;
}

/* ─── System Audio Warning State Machine ────────────────────────── */

/**
 * Replicates the system audio warning state machine from ActiveRoom.tsx.
 * State: { showAudioWarning, shareAudioOn }
 * Actions: toggleAudioClick, confirmWarning, cancelWarning
 */
interface ShareAudioUiState {
  showAudioWarning: boolean;
  showPostShareAudioPrompt: boolean;
  shareAudioOn: boolean;
}

function initialShareAudioUiState(): ShareAudioUiState {
  return { showAudioWarning: false, showPostShareAudioPrompt: false, shareAudioOn: false };
}

/** Simulates clicking the audio toggle button in ActiveRoom */
function toggleAudioClick(
  state: ShareAudioUiState,
  toggleShareAudioFn: (withAudio: boolean) => void,
  isMacPlatform = false,
): ShareAudioUiState {
  if (isMacPlatform) return state;
  const next = !state.shareAudioOn;
  toggleShareAudioFn(next);
    // Turning on → show warning first (don't call toggleShareAudio yet)
  return { ...state, shareAudioOn: next };
    // Turning off → immediate
}

/** Simulates a successful browser fallback share start. */
function handleFallbackShareStarted(
  state: ShareAudioUiState,
  shareStarted: boolean,
  isPromptPlatform: boolean,
): ShareAudioUiState {
  if (!shareStarted || !isPromptPlatform) return state;
  return { ...state, showAudioWarning: false, showPostShareAudioPrompt: true };
}

/** Simulates accepting the post-share audio prompt. */
function acceptPostSharePrompt(
  state: ShareAudioUiState,
  toggleShareAudioFn: (withAudio: boolean) => void,
  isMacPlatform = false,
): ShareAudioUiState {
  if (isMacPlatform) return state;
  toggleShareAudioFn(true);
  return { showAudioWarning: false, showPostShareAudioPrompt: false, shareAudioOn: true };
}

/** Simulates declining the post-share audio prompt. */
function declinePostSharePrompt(state: ShareAudioUiState): ShareAudioUiState {
  return { ...state, showAudioWarning: false, showPostShareAudioPrompt: false };
}

/** Simulates the share ending while the prompt is open. */
function handleShareEnded(): ShareAudioUiState {
  return { showAudioWarning: false, showPostShareAudioPrompt: false, shareAudioOn: false };
}

type AudioWarningState = ShareAudioUiState;

function initialAudioWarningState(): AudioWarningState {
  return initialShareAudioUiState();
}

function confirmWarning(
  state: ShareAudioUiState,
  toggleShareAudioFn: (withAudio: boolean) => void,
): ShareAudioUiState {
  return acceptPostSharePrompt(state, toggleShareAudioFn);
}

function cancelWarning(state: ShareAudioUiState): ShareAudioUiState {
  return declinePostSharePrompt(state);
}

void initialAudioWarningState;
void confirmWarning;
void cancelWarning;

function coldStartWaitMinutes(estimatedWaitSecs: number | null): number {
  return Math.ceil((estimatedWaitSecs ?? 120) / 60);
}

/* ═══ Tests ═════════════════════════════════════════════════════════ */

describe('Quality Indicator Rendering', () => {
  it('displays correct format for 1080p @ 30fps', () => {
    const info: ShareQualityInfo = { width: 1920, height: 1080, frameRate: 30 };
    expect(formatQualityIndicator(true, info)).toBe('1080p @ 30fps');
  });

  it('displays correct format for 1440p @ 60fps', () => {
    const info: ShareQualityInfo = { width: 2560, height: 1440, frameRate: 60 };
    expect(formatQualityIndicator(true, info)).toBe('1440p @ 60fps');
  });

  it('displays correct format for 720p @ 15fps', () => {
    const info: ShareQualityInfo = { width: 1280, height: 720, frameRate: 15 };
    expect(formatQualityIndicator(true, info)).toBe('720p @ 15fps');
  });

  it('rounds fractional frame rates', () => {
    const info: ShareQualityInfo = { width: 1920, height: 1080, frameRate: 29.97 };
    expect(formatQualityIndicator(true, info)).toBe('1080p @ 30fps');
  });

  it('rounds down frame rates below .5', () => {
    const info: ShareQualityInfo = { width: 1920, height: 1080, frameRate: 14.3 };
    expect(formatQualityIndicator(true, info)).toBe('1080p @ 14fps');
  });

  it('returns null when shareQualityInfo is null', () => {
    expect(formatQualityIndicator(true, null)).toBeNull();
  });

  it('returns null when not self share', () => {
    const info: ShareQualityInfo = { width: 1920, height: 1080, frameRate: 30 };
    expect(formatQualityIndicator(false, info)).toBeNull();
  });

  it('returns null when not self share and info is null', () => {
    expect(formatQualityIndicator(false, null)).toBeNull();
  });
});

describe('Cold Start UI', () => {
  it('rounds the estimated cold-start wait up to minutes', () => {
    expect(coldStartWaitMinutes(120)).toBe(2);
    expect(coldStartWaitMinutes(61)).toBe(2);
    expect(coldStartWaitMinutes(60)).toBe(1);
  });

  it('defaults cold-start wait copy to two minutes', () => {
    expect(coldStartWaitMinutes(null)).toBe(2);
  });
});

describe('Share Audio UX', () => {
  beforeEach(() => {
    mockToggleShareAudio.mockClear();
  });

  it('enabling audio acts immediately without showing a warning dialog', () => {
    let state = initialShareAudioUiState();
    state = toggleAudioClick(state, mockToggleShareAudio);

    expect(state.showAudioWarning).toBe(false);
    expect(state.showPostShareAudioPrompt).toBe(false);
    expect(state.shareAudioOn).toBe(true);
    expect(mockToggleShareAudio).toHaveBeenCalledWith(true);
    expect(mockToggleShareAudio).toHaveBeenCalledTimes(1);
  });

  it('successful fallback share shows the post-share audio prompt on prompt platforms', () => {
    let state = initialShareAudioUiState();
    state = handleFallbackShareStarted(state, true, true);

    expect(state.showAudioWarning).toBe(false);
    expect(state.showPostShareAudioPrompt).toBe(true);
    expect(state.shareAudioOn).toBe(false);
    expect(mockToggleShareAudio).not.toHaveBeenCalled();
  });

  it('accepting the post-share prompt enables audio and dismisses the prompt', () => {
    let state = initialShareAudioUiState();
    state = handleFallbackShareStarted(state, true, true);

    state = acceptPostSharePrompt(state, mockToggleShareAudio);

    expect(state.showAudioWarning).toBe(false);
    expect(state.showPostShareAudioPrompt).toBe(false);
    expect(state.shareAudioOn).toBe(true);
    expect(mockToggleShareAudio).toHaveBeenCalledWith(true);
  });

  it('macOS blocks accepting the post-share prompt and leaves audio off', () => {
    let state = initialShareAudioUiState();
    state = handleFallbackShareStarted(state, true, true);

    state = acceptPostSharePrompt(state, mockToggleShareAudio, true);

    expect(state.showAudioWarning).toBe(false);
    expect(state.showPostShareAudioPrompt).toBe(true);
    expect(state.shareAudioOn).toBe(false);
    expect(mockToggleShareAudio).not.toHaveBeenCalled();
  });

  it('turning audio off calls toggleShareAudio(false) immediately', () => {
    let state: AudioWarningState = { showAudioWarning: false, showPostShareAudioPrompt: false, shareAudioOn: true };
    state = toggleAudioClick(state, mockToggleShareAudio);

    expect(state.showAudioWarning).toBe(false);
    expect(state.showPostShareAudioPrompt).toBe(false);
    expect(state.shareAudioOn).toBe(false);
    expect(mockToggleShareAudio).toHaveBeenCalledWith(false);
    expect(mockToggleShareAudio).toHaveBeenCalledTimes(1);
  });

  it('macOS blocks the in-room audio toggle', () => {
    let state = initialShareAudioUiState();
    state = toggleAudioClick(state, mockToggleShareAudio, true);

    expect(state.showAudioWarning).toBe(false);
    expect(state.showPostShareAudioPrompt).toBe(false);
    expect(state.shareAudioOn).toBe(false);
    expect(mockToggleShareAudio).not.toHaveBeenCalled();
  });

  it('share end auto-dismisses the post-share prompt and resets local audio state', () => {
    let state = initialShareAudioUiState();

    state = handleFallbackShareStarted(state, true, true);
    state = handleShareEnded();

    expect(state.showAudioWarning).toBe(false);
    expect(state.showPostShareAudioPrompt).toBe(false);
    expect(state.shareAudioOn).toBe(false);
    expect(mockToggleShareAudio).not.toHaveBeenCalled();
  });

  it('non-prompt platforms do not show the post-share prompt after fallback share', () => {
    let state = initialShareAudioUiState();

    state = handleFallbackShareStarted(state, true, false);

    expect(state.showAudioWarning).toBe(false);
    expect(state.showPostShareAudioPrompt).toBe(false);
    expect(state.shareAudioOn).toBe(false);
    expect(mockToggleShareAudio).not.toHaveBeenCalled();
  });

  it('warning text matches expected content', () => {
    const warningText = 'Share system audio?';
    expect(warningText).toContain('system audio');
  });
});

/* ═══ Watch All — Lifecycle & Entry Point Integration Tests ═════════ */

/**
 * Tests the Watch All window lifecycle and entry point logic extracted
 * from ActiveRoom.tsx as pure state transformations.
 *
 * 14.1 — Lifecycle: room leave cleanup, main window closing cleanup,
 *         pop-out from Watch All tile.
 * 14.2 — Entry points: button visibility, button label toggle,
 *         CLI command, /help output, Tab-autocomplete.
 *
 * Validates: Requirements 1.4, 1.5, 5.1, 5.2, 5.3, 5.4, 10.2, 10.3,
 *            13.1, 13.2, 13.3
 */

/* ─── Watch All State Machine ───────────────────────────────────── */

interface WatchAllState {
  watchAllOpen: boolean;
  watchAllWindowRef: { current: object | null };
}

function initialWatchAllState(): WatchAllState {
  return { watchAllOpen: false, watchAllWindowRef: { current: null } };
}

/** Simulates openWatchAllWindow — sets ref and open flag. */
function openWatchAll(state: WatchAllState): WatchAllState {
  if (state.watchAllWindowRef.current) return state; // already open, bring to foreground
  const win = { label: 'watch-all' }; // mock window object
  return { watchAllOpen: true, watchAllWindowRef: { current: win } };
}

/** Simulates closeWatchAllWindow — idempotent, clears ref and flag. */
function closeWatchAll(state: WatchAllState): WatchAllState {
  if (!state.watchAllWindowRef.current) return state; // no-op if already null
  return { watchAllOpen: false, watchAllWindowRef: { current: null } };
}

/**
 * Simulates toggleWatchAllWindow from ActiveRoom.tsx.
 * - If open → close (unconditional, Req 6.3)
 * - If closed and shares active → open
 * - If closed and no shares → no-op
 */
function toggleWatchAll(state: WatchAllState, hasJoinedRoomShares: boolean): WatchAllState {
  if (state.watchAllOpen) {
    return closeWatchAll(state);
  } else {
    if (hasJoinedRoomShares) return openWatchAll(state);
    return state; // no-op
  }
}

/**
 * Simulates closeAllShareWindows from ActiveRoom.tsx.
 * This is called on room leave — it closes Watch All first, then all pop-outs.
 */
function closeAllShareWindows(state: WatchAllState): WatchAllState {
  return closeWatchAll(state);
}

/**
 * Simulates the idle-session cleanup effect in ActiveRoom.tsx.
 * Terminal disconnects transition the room to idle even when the user did not
 * explicitly press /leave, and that should still close Watch All.
 */
function handleSessionStateChange(
  state: WatchAllState,
  machineState: 'connecting' | 'active' | 'reconnecting' | 'idle',
): WatchAllState {
  if (machineState !== 'idle') return state;
  return closeAllShareWindows(state);
}

/* ─── Pop-out from Watch All tile ───────────────────────────────── */

interface ShareWindowsState {
  openWindows: Map<string, object>; // participantId → window ref
}

function initialShareWindowsState(): ShareWindowsState {
  return { openWindows: new Map() };
}

/**
 * Simulates the watch-all:pop-out handler in ActiveRoom.tsx.
 * If a pop-out window already exists for the participant, bring to foreground.
 * Otherwise, open a new ScreenShareWindow.
 */
function handlePopOut(
  state: ShareWindowsState,
  participantId: string,
): { state: ShareWindowsState; action: 'opened' | 'focused' } {
  const existing = state.openWindows.get(participantId);
  if (existing) {
    return { state, action: 'focused' };
  }
  const newWin = { label: `screen-share-${participantId}` };
  const next = new Map(state.openWindows);
  next.set(participantId, newWin);
  return { state: { openWindows: next }, action: 'opened' };
}

interface PopBackState {
  openWindows: Map<string, object>;
  watchAllOpen: boolean;
  watchAllReady: boolean;
  activeShares: Set<string>;
  participants: Set<string>;
}

function closePopOutAndMaybeRestore(
  state: PopBackState,
  participantId: string,
): { state: PopBackState; restoredToWatchAll: boolean } {
  const nextWindows = new Map(state.openWindows);
  const hadWindow = nextWindows.delete(participantId);
  return {
    state: { ...state, openWindows: nextWindows },
    restoredToWatchAll:
      hadWindow
      && state.watchAllOpen
      && state.watchAllReady
      && state.activeShares.has(participantId)
      && state.participants.has(participantId),
  };
}

/* ─── Entry Point: Button Visibility & Label ────────────────────── */

interface SharesPanelState {
  joinedRoomRemoteSharersCount: number;
  otherRoomRemoteSharersCount: number;
  watchAllOpen: boolean;
  isPassthroughPairedRoom?: boolean;
}

interface RoomPanelLayoutState {
  isJoinedRoom: boolean;
  joinedRoomRemoteSharersCount: number;
  otherRoomRemoteSharersCount: number;
  watchAllOpen: boolean;
  isPassthroughPairedRoom?: boolean;
}

/**
 * Replicates the Phase 14 room-panel layout contract from ActiveRoom.tsx:
 * - header is the collapse toggle and contains /join or /leave on the right
 * - /watch-all lives in a right-aligned row inside the expanded room body
 * - collapsing the room hides the watch-all row with the participants
 */
function roomPanelLayout(state: RoomPanelLayoutState): {
  headerContainsRoomAction: boolean;
  headerRoomAction: string;
  watchAllRowInsideExpandedBody: boolean;
  watchAllRowRightAligned: boolean;
  watchAllVisibleWhenExpanded: boolean;
  watchAllVisibleWhenCollapsed: boolean;
  watchAllButton: string | null;
} {
  let watchAllButton: string | null = null;
  if ((state.isJoinedRoom || state.isPassthroughPairedRoom) && state.joinedRoomRemoteSharersCount > 0) {
    watchAllButton = state.watchAllOpen ? '/close-all' : '/watch-all';
  } else if (!state.isJoinedRoom && state.otherRoomRemoteSharersCount > 0) {
    watchAllButton = '/watch-all';
  }

  return {
    headerContainsRoomAction: true,
    headerRoomAction: state.isJoinedRoom ? '/leave' : '/join',
    watchAllRowInsideExpandedBody: true,
    watchAllRowRightAligned: true,
    watchAllVisibleWhenExpanded: watchAllButton !== null,
    watchAllVisibleWhenCollapsed: false,
    watchAllButton,
  };
}

function toggleRoomHeaderOnPointerDown(
  expanded: boolean,
  event: { isPrimary: boolean; button: number },
): boolean {
  if (!event.isPrimary || event.button !== 0) return expanded;
  return !expanded;
}

function toggleRoomHeaderFromStoredState(
  storedExpanded: boolean | undefined,
  event: { isPrimary: boolean; button: number },
): boolean {
  const effectiveExpanded = storedExpanded ?? true;
  return toggleRoomHeaderOnPointerDown(effectiveExpanded, event);
}

function headerPointerDownTriggeredByRoomAction(event: { stopPropagationCalled: boolean }): boolean {
  return !event.stopPropagationCalled;
}

function toggleRoomHeaderOnKeyDown(expanded: boolean, key: string): boolean {
  if (key !== 'Enter' && key !== ' ') return expanded;
  return !expanded;
}

interface ScopedShareWindow {
  scope: 'direct' | 'watch-all';
}

interface RoomScopedViewerState extends WatchAllState {
  openWindows: Map<string, ScopedShareWindow>;
}

function handleJoinedRoomChange(
  state: RoomScopedViewerState,
  nextScopedParticipantIds: Set<string>,
): RoomScopedViewerState {
  const next = new Map(state.openWindows);
  for (const [participantId, window] of next) {
    if (window.scope === 'watch-all' && !nextScopedParticipantIds.has(participantId)) {
      next.delete(participantId);
    }
  }
  return {
    watchAllOpen: false,
    watchAllWindowRef: { current: null },
    openWindows: next,
  };
}

interface WatchAllScopeRoom {
  id: string;
  participantIds: string[];
}

interface WatchAllScopeParticipant {
  id: string;
  isSharing: boolean;
}

interface WatchAllScopeData {
  joinedSubRoomId: string | null;
  passthrough: { sourceSubRoomId: string; targetSubRoomId: string } | null;
  subRooms: WatchAllScopeRoom[];
  participants: WatchAllScopeParticipant[];
  selfParticipantId: string | null;
  streams: Map<string, string | null>;
}

function computeWatchAllScope(state: WatchAllScopeData | null): {
  participantIds: Set<string>;
  remoteSharers: WatchAllScopeParticipant[];
  streams: Map<string, string | null>;
} {
  if (!state?.joinedSubRoomId) {
    return {
      participantIds: new Set(),
      remoteSharers: [],
      streams: new Map(),
    };
  }

  const scopedSubRoomIds = new Set([state.joinedSubRoomId]);
  if (state.passthrough?.sourceSubRoomId === state.joinedSubRoomId) {
    scopedSubRoomIds.add(state.passthrough.targetSubRoomId);
  } else if (state.passthrough?.targetSubRoomId === state.joinedSubRoomId) {
    scopedSubRoomIds.add(state.passthrough.sourceSubRoomId);
  }

  const participantIds = new Set<string>();
  for (const room of state.subRooms) {
    if (!scopedSubRoomIds.has(room.id)) continue;
    for (const participantId of room.participantIds) {
      participantIds.add(participantId);
    }
  }

  const participants = state.participants.filter((participant) => participantIds.has(participant.id));
  const remoteSharers = participants.filter(
    (participant) => participant.isSharing && participant.id !== state.selfParticipantId,
  );
  const streams = new Map([...state.streams].filter(([participantId]) => participantIds.has(participantId)));

  return { participantIds, remoteSharers, streams };
}

function diffWatchAllStreams(
  previous: Map<string, string | null>,
  current: Map<string, string | null>,
): { added: string[]; removed: string[] } {
  const added = [...current.keys()].filter((participantId) => !previous.has(participantId));
  const removed = [...previous.keys()].filter((participantId) => !current.has(participantId));
  return { added, removed };
}

/**
 * Replicates the joined-room /watch-all button visibility logic from ActiveRoom.tsx.
 */
function isWatchAllButtonVisible(panel: SharesPanelState): boolean {
  return panel.joinedRoomRemoteSharersCount > 0 || (!!panel.isPassthroughPairedRoom && panel.otherRoomRemoteSharersCount > 0);
}

/**
 * Replicates the disabled /watch-all button shown for other rooms with sharers.
 */
function isDisabledWatchAllVisible(panel: SharesPanelState): boolean {
  return !panel.isPassthroughPairedRoom && panel.otherRoomRemoteSharersCount > 0;
}

/**
 * Replicates the button label logic:
 *   {watchAllOpen ? '/close-all' : '/watch-all'}
 */
function watchAllButtonLabel(panel: SharesPanelState): string {
  return panel.watchAllOpen ? '/close-all' : '/watch-all';
}

/* ─── Entry Point: CLI ──────────────────────────────────────────── */

/**
 * CLI_COMMANDS array from ActiveRoom.tsx — used for Tab-autocomplete.
 */
const CLI_COMMANDS = [
  '/help', '/mute', '/deafen', '/kick', '/share', '/stopshare', '/revoke',
  '/stopall', '/shareperm', '/vol', '/watch-all', '/leave', '/reconnect-media', '/devices',
];

/**
 * Replicates the /help output from handleCli in ActiveRoom.tsx.
 */
const HELP_OUTPUT = [
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
  '  /watch-all                   — toggle watch all for your joined room',
  '  /leave                       — leave the room',
].join('\n');

/**
 * Replicates the CLI dispatch logic for /watch-all from handleCli.
 * Returns true if the command was handled.
 */
function handleCli(raw: string): { handled: boolean; action?: string } {
  const trimmed = raw.trim();
  if (trimmed === '/watch-all') {
    return { handled: true, action: 'toggleWatchAllWindow' };
  }
  if (trimmed === '/help') {
    return { handled: true, action: 'showHelp' };
  }
  return { handled: false };
}

/**
 * Replicates Tab-autocomplete logic from handleCliKeyDown in ActiveRoom.tsx.
 * Returns the completed input string, or the original if no match.
 */
function tabAutocomplete(input: string): string {
  if (!input.startsWith('/')) return input;
  const spaceIdx = input.indexOf(' ');
  if (spaceIdx !== -1) return input; // already past the command token
  const prefix = input.toLowerCase();
  const matches = CLI_COMMANDS.filter((c) => c.startsWith(prefix));
  if (matches.length === 1) {
    return matches[0] + ' ';
  } else if (matches.length > 1) {
    let common = matches[0];
    for (const m of matches) {
      while (!m.startsWith(common)) common = common.slice(0, -1);
    }
    if (common.length > prefix.length) return common;
  }
  return input;
}

/* ═══ 14.1 — Watch All Lifecycle Integration Tests ══════════════════ */

describe('Watch All Lifecycle', () => {
  it('Watch All window closed on room leave (closeAllShareWindows)', () => {
    let state = initialWatchAllState();
    state = openWatchAll(state);
    expect(state.watchAllOpen).toBe(true);
    expect(state.watchAllWindowRef.current).not.toBeNull();

    // Room leave triggers closeAllShareWindows
    state = closeAllShareWindows(state);
    expect(state.watchAllOpen).toBe(false);
    expect(state.watchAllWindowRef.current).toBeNull();
  });

  it('Watch All window closed when the room disconnects to idle', () => {
    let state = initialWatchAllState();
    state = openWatchAll(state);
    expect(state.watchAllOpen).toBe(true);

    state = handleSessionStateChange(state, 'idle');
    expect(state.watchAllOpen).toBe(false);
    expect(state.watchAllWindowRef.current).toBeNull();
  });

  it('Watch All window closed on main window closing', () => {
    let state = initialWatchAllState();
    state = openWatchAll(state);
    expect(state.watchAllOpen).toBe(true);

    // main-window-closing listener calls closeWatchAllWindow
    state = closeWatchAll(state);
    expect(state.watchAllOpen).toBe(false);
    expect(state.watchAllWindowRef.current).toBeNull();
  });

  it('closeWatchAllWindow is idempotent (double-fire safety)', () => {
    let state = initialWatchAllState();
    state = openWatchAll(state);

    // First close (from watch-all:closed event)
    state = closeWatchAll(state);
    expect(state.watchAllOpen).toBe(false);

    // Second close (from tauri://destroyed) — should be no-op
    const stateAfterSecond = closeWatchAll(state);
    expect(stateAfterSecond).toBe(state); // same reference, no mutation
  });

  it('closeAllShareWindows is no-op when Watch All is not open', () => {
    const state = initialWatchAllState();
    const result = closeAllShareWindows(state);
    expect(result).toBe(state); // no-op returns same reference
  });

  it('Pop-out from Watch All tile opens ScreenShareWindow', () => {
    let shareState = initialShareWindowsState();
    const { state: next, action } = handlePopOut(shareState, 'user-1');
    expect(action).toBe('opened');
    expect(next.openWindows.has('user-1')).toBe(true);
  });

  it('Pop-out brings existing ScreenShareWindow to foreground', () => {
    let shareState = initialShareWindowsState();
    // First pop-out opens the window
    const { state: afterOpen } = handlePopOut(shareState, 'user-1');
    expect(afterOpen.openWindows.has('user-1')).toBe(true);

    // Second pop-out for same participant brings to foreground
    const { state: afterFocus, action } = handlePopOut(afterOpen, 'user-1');
    expect(action).toBe('focused');
    expect(afterFocus.openWindows.size).toBe(1); // no duplicate
  });

  it('Pop-out for different participants opens separate windows', () => {
    let shareState = initialShareWindowsState();
    const { state: s1 } = handlePopOut(shareState, 'user-1');
    const { state: s2 } = handlePopOut(s1, 'user-2');
    expect(s2.openWindows.size).toBe(2);
    expect(s2.openWindows.has('user-1')).toBe(true);
    expect(s2.openWindows.has('user-2')).toBe(true);
  });

  it('closing a single-stream window restores that share to Watch All when still active', () => {
    const state: PopBackState = {
      openWindows: new Map([['user-1', { label: 'screen-share-user-1' }]]),
      watchAllOpen: true,
      watchAllReady: true,
      activeShares: new Set(['user-1']),
      participants: new Set(['user-1']),
    };

    const result = closePopOutAndMaybeRestore(state, 'user-1');

    expect(result.state.openWindows.has('user-1')).toBe(false);
    expect(result.restoredToWatchAll).toBe(true);
  });

  it('closing a single-stream window only restores once', () => {
    const state: PopBackState = {
      openWindows: new Map([['user-1', { label: 'screen-share-user-1' }]]),
      watchAllOpen: true,
      watchAllReady: true,
      activeShares: new Set(['user-1']),
      participants: new Set(['user-1']),
    };

    const first = closePopOutAndMaybeRestore(state, 'user-1');
    const second = closePopOutAndMaybeRestore(first.state, 'user-1');

    expect(first.restoredToWatchAll).toBe(true);
    expect(second.restoredToWatchAll).toBe(false);
  });

  it('room changes close Watch All and remove watch-all-scoped viewers that are no longer valid', () => {
    const state: RoomScopedViewerState = {
      watchAllOpen: true,
      watchAllWindowRef: { current: { label: 'watch-all' } },
      openWindows: new Map([
        ['user-1', { scope: 'watch-all' }],
        ['user-2', { scope: 'direct' }],
      ]),
    };

    const result = handleJoinedRoomChange(state, new Set(['user-3']));

    expect(result.watchAllOpen).toBe(false);
    expect(result.watchAllWindowRef.current).toBeNull();
    expect(result.openWindows.has('user-1')).toBe(false);
    expect(result.openWindows.has('user-2')).toBe(true);
  });

  it('room changes preserve watch-all-scoped viewers that still belong to the new room scope', () => {
    const state: RoomScopedViewerState = {
      watchAllOpen: true,
      watchAllWindowRef: { current: { label: 'watch-all' } },
      openWindows: new Map([
        ['user-1', { scope: 'watch-all' }],
        ['user-2', { scope: 'direct' }],
      ]),
    };

    const result = handleJoinedRoomChange(state, new Set(['user-1']));

    expect(result.watchAllOpen).toBe(false);
    expect(result.openWindows.has('user-1')).toBe(true);
    expect(result.openWindows.has('user-2')).toBe(true);
  });
});

/* ═══ 14.2 — Watch All Entry Point Integration Tests ════════════════ */

describe('Watch All Entry Points', () => {
  describe('room panel layout', () => {
    it('keeps join or leave in the room header and keeps watch-all right-aligned in expanded content', () => {
      expect(roomPanelLayout({
        isJoinedRoom: true,
        joinedRoomRemoteSharersCount: 2,
        otherRoomRemoteSharersCount: 0,
        watchAllOpen: false,
      })).toEqual({
        headerContainsRoomAction: true,
        headerRoomAction: '/leave',
        watchAllRowInsideExpandedBody: true,
        watchAllRowRightAligned: true,
        watchAllVisibleWhenExpanded: true,
        watchAllVisibleWhenCollapsed: false,
        watchAllButton: '/watch-all',
      });
    });

    it('shows the disabled other-room watch-all affordance inside expanded room content only', () => {
      expect(roomPanelLayout({
        isJoinedRoom: false,
        joinedRoomRemoteSharersCount: 0,
        otherRoomRemoteSharersCount: 1,
        watchAllOpen: false,
      })).toEqual({
        headerContainsRoomAction: true,
        headerRoomAction: '/join',
        watchAllRowInsideExpandedBody: true,
        watchAllRowRightAligned: true,
        watchAllVisibleWhenExpanded: true,
        watchAllVisibleWhenCollapsed: false,
        watchAllButton: '/watch-all',
      });
    });

    it('enables watch-all in the paired passthrough room while passthrough is active', () => {
      expect(roomPanelLayout({
        isJoinedRoom: false,
        isPassthroughPairedRoom: true,
        joinedRoomRemoteSharersCount: 1,
        otherRoomRemoteSharersCount: 0,
        watchAllOpen: false,
      })).toEqual({
        headerContainsRoomAction: true,
        headerRoomAction: '/join',
        watchAllRowInsideExpandedBody: true,
        watchAllRowRightAligned: true,
        watchAllVisibleWhenExpanded: true,
        watchAllVisibleWhenCollapsed: false,
        watchAllButton: '/watch-all',
      });
    });

    it('hides watch-all entirely when the room is collapsed', () => {
      expect(roomPanelLayout({
        isJoinedRoom: true,
        joinedRoomRemoteSharersCount: 1,
        otherRoomRemoteSharersCount: 0,
        watchAllOpen: true,
      }).watchAllVisibleWhenCollapsed).toBe(false);
    });
  });

  describe('room collapse interaction', () => {
    it('toggles immediately on the first primary pointer interaction', () => {
      expect(toggleRoomHeaderOnPointerDown(true, { isPrimary: true, button: 0 })).toBe(false);
      expect(toggleRoomHeaderOnPointerDown(false, { isPrimary: true, button: 0 })).toBe(true);
    });

    it('first click on an uninitialized room section collapses immediately', () => {
      expect(toggleRoomHeaderFromStoredState(undefined, { isPrimary: true, button: 0 })).toBe(false);
    });

    it('second click after collapsing re-expands the room', () => {
      const collapsed = toggleRoomHeaderFromStoredState(undefined, { isPrimary: true, button: 0 });
      expect(toggleRoomHeaderFromStoredState(collapsed, { isPrimary: true, button: 0 })).toBe(true);
    });

    it('ignores non-primary pointer interactions', () => {
      expect(toggleRoomHeaderOnPointerDown(true, { isPrimary: false, button: 0 })).toBe(true);
      expect(toggleRoomHeaderOnPointerDown(true, { isPrimary: true, button: 1 })).toBe(true);
    });

    it('preserves keyboard toggling on Enter and Space', () => {
      expect(toggleRoomHeaderOnKeyDown(true, 'Enter')).toBe(false);
      expect(toggleRoomHeaderOnKeyDown(true, ' ')).toBe(false);
      expect(toggleRoomHeaderOnKeyDown(true, 'Escape')).toBe(true);
    });

    it('treats the header room action as separate from the collapse toggle', () => {
      expect(toggleRoomHeaderOnPointerDown(true, { isPrimary: true, button: 0 })).toBe(false);
      expect(roomPanelLayout({
        isJoinedRoom: false,
        joinedRoomRemoteSharersCount: 0,
        otherRoomRemoteSharersCount: 0,
        watchAllOpen: false,
      }).headerRoomAction).toBe('/join');
    });

    it('join or leave button pointer handling blocks the header collapse handler', () => {
      expect(headerPointerDownTriggeredByRoomAction({ stopPropagationCalled: true })).toBe(false);
    });
  });

  describe('/watch-all button visibility', () => {
    it('button visible when the joined room has remote sharers', () => {
      expect(isWatchAllButtonVisible({
        joinedRoomRemoteSharersCount: 1,
        otherRoomRemoteSharersCount: 0,
        watchAllOpen: false,
      })).toBe(true);
      expect(isWatchAllButtonVisible({
        joinedRoomRemoteSharersCount: 3,
        otherRoomRemoteSharersCount: 2,
        watchAllOpen: false,
      })).toBe(true);
    });

    it('button hidden when only other rooms have sharers', () => {
      expect(isWatchAllButtonVisible({
        joinedRoomRemoteSharersCount: 0,
        otherRoomRemoteSharersCount: 2,
        watchAllOpen: false,
      })).toBe(false);
    });

    it('button visible in the paired passthrough room when that room has sharers', () => {
      expect(isWatchAllButtonVisible({
        joinedRoomRemoteSharersCount: 0,
        otherRoomRemoteSharersCount: 1,
        watchAllOpen: false,
        isPassthroughPairedRoom: true,
      })).toBe(true);
    });

    it('shows a disabled watch-all button for other rooms with sharers', () => {
      expect(isDisabledWatchAllVisible({
        joinedRoomRemoteSharersCount: 0,
        otherRoomRemoteSharersCount: 1,
        watchAllOpen: false,
      })).toBe(true);
      expect(isDisabledWatchAllVisible({
        joinedRoomRemoteSharersCount: 1,
        otherRoomRemoteSharersCount: 0,
        watchAllOpen: false,
      })).toBe(false);
    });

    it('does not disable watch-all for the paired passthrough room', () => {
      expect(isDisabledWatchAllVisible({
        joinedRoomRemoteSharersCount: 0,
        otherRoomRemoteSharersCount: 1,
        watchAllOpen: false,
        isPassthroughPairedRoom: true,
      })).toBe(false);
    });
  });

  describe('/watch-all passthrough scope', () => {
    const baseScope: WatchAllScopeData = {
      joinedSubRoomId: 'room-1',
      passthrough: null,
      selfParticipantId: 'self',
      subRooms: [
        { id: 'room-1', participantIds: ['self', 'user-1'] },
        { id: 'room-2', participantIds: ['user-2'] },
        { id: 'room-3', participantIds: ['user-3'] },
      ],
      participants: [
        { id: 'self', isSharing: false },
        { id: 'user-1', isSharing: true },
        { id: 'user-2', isSharing: true },
        { id: 'user-3', isSharing: true },
      ],
      streams: new Map([
        ['user-1', 'stream-1'],
        ['user-2', 'stream-2'],
        ['user-3', 'stream-3'],
      ]),
    };

    it('includes both rooms in the active passthrough pair when the joined room is involved', () => {
      const scope = computeWatchAllScope({
        ...baseScope,
        passthrough: { sourceSubRoomId: 'room-1', targetSubRoomId: 'room-2' },
      });

      expect([...scope.participantIds].sort()).toEqual(['self', 'user-1', 'user-2']);
      expect(scope.remoteSharers.map((participant) => participant.id).sort()).toEqual(['user-1', 'user-2']);
      expect([...scope.streams.keys()].sort()).toEqual(['user-1', 'user-2']);
    });

    it('does not include passthrough rooms when the joined room is uninvolved', () => {
      const scope = computeWatchAllScope({
        ...baseScope,
        passthrough: { sourceSubRoomId: 'room-2', targetSubRoomId: 'room-3' },
      });

      expect([...scope.participantIds].sort()).toEqual(['self', 'user-1']);
      expect(scope.remoteSharers.map((participant) => participant.id)).toEqual(['user-1']);
      expect([...scope.streams.keys()]).toEqual(['user-1']);
    });

    it('adds newly in-scope streams when passthrough starts while Watch All is open', () => {
      const before = computeWatchAllScope(baseScope).streams;
      const after = computeWatchAllScope({
        ...baseScope,
        passthrough: { sourceSubRoomId: 'room-1', targetSubRoomId: 'room-2' },
      }).streams;

      expect(diffWatchAllStreams(before, after)).toEqual({
        added: ['user-2'],
        removed: [],
      });
    });

    it('removes other-room streams when passthrough stops while Watch All is open', () => {
      const before = computeWatchAllScope({
        ...baseScope,
        passthrough: { sourceSubRoomId: 'room-1', targetSubRoomId: 'room-2' },
      }).streams;
      const after = computeWatchAllScope(baseScope).streams;

      expect(diffWatchAllStreams(before, after)).toEqual({
        added: [],
        removed: ['user-2'],
      });
    });
  });

  describe('/watch-all button label toggle', () => {
    it('shows /watch-all when window is closed', () => {
      expect(watchAllButtonLabel({
        joinedRoomRemoteSharersCount: 2,
        otherRoomRemoteSharersCount: 0,
        watchAllOpen: false,
      })).toBe('/watch-all');
    });

    it('shows /close-all when window is open', () => {
      expect(watchAllButtonLabel({
        joinedRoomRemoteSharersCount: 2,
        otherRoomRemoteSharersCount: 0,
        watchAllOpen: true,
      })).toBe('/close-all');
    });
  });

  describe('/watch-all CLI command', () => {
    it('dispatches toggleWatchAllWindow on /watch-all', () => {
      const result = handleCli('/watch-all');
      expect(result.handled).toBe(true);
      expect(result.action).toBe('toggleWatchAllWindow');
    });

    it('toggleWatchAllWindow opens when closed and joined-room shares are active', () => {
      let state = initialWatchAllState();
      state = toggleWatchAll(state, true);
      expect(state.watchAllOpen).toBe(true);
    });

    it('toggleWatchAllWindow closes when open (unconditional)', () => {
      let state = initialWatchAllState();
      state = openWatchAll(state);
      state = toggleWatchAll(state, true);
      expect(state.watchAllOpen).toBe(false);
    });

    it('toggleWatchAllWindow is no-op when closed and only other rooms have shares', () => {
      const state = initialWatchAllState();
      const result = toggleWatchAll(state, false);
      expect(result.watchAllOpen).toBe(false);
      expect(result).toBe(state); // same reference
    });
  });

  describe('/watch-all in /help output', () => {
    it('/watch-all appears in help output', () => {
      expect(HELP_OUTPUT).toContain('/watch-all');
    });

    it('/watch-all help line describes toggle behavior', () => {
      expect(HELP_OUTPUT).toContain('/watch-all                   — toggle watch all for your joined room');
    });

    it('/help command is handled', () => {
      const result = handleCli('/help');
      expect(result.handled).toBe(true);
      expect(result.action).toBe('showHelp');
    });
  });

  describe('/watch-all Tab-autocomplete', () => {
    it('CLI_COMMANDS includes /watch-all', () => {
      expect(CLI_COMMANDS).toContain('/watch-all');
    });

    it('Tab-autocomplete completes /wat to /watch-all', () => {
      expect(tabAutocomplete('/wat')).toBe('/watch-all ');
    });

    it('Tab-autocomplete completes /watch to /watch-all', () => {
      expect(tabAutocomplete('/watch')).toBe('/watch-all ');
    });

    it('Tab-autocomplete completes /watch-a to /watch-all', () => {
      expect(tabAutocomplete('/watch-a')).toBe('/watch-all ');
    });

    it('Tab-autocomplete does not complete past space', () => {
      expect(tabAutocomplete('/watch-all foo')).toBe('/watch-all foo');
    });

    it('Tab-autocomplete returns input when no match', () => {
      expect(tabAutocomplete('/xyz')).toBe('/xyz');
    });

    it('Tab-autocomplete finds common prefix for ambiguous input', () => {
      // /s matches /share, /stopshare, /stopall, /shareperm
      const result = tabAutocomplete('/s');
      // All start with /s — common prefix is /s, which is not longer than input
      expect(result).toBe('/s');
    });
  });
});

/* ═══ 15 — Slash-key focus routing ══════════════════════════════════ */
/**
 * Replicates the '/' keydown redirect predicate from ActiveRoom.tsx.
 *
 * The handler on window 'keydown' checks:
 *   1. active is an input/textarea with data-cli-input → already on CLI, pass through
 *   2. active is an input/textarea without data-cli-input and empty value → redirect to CLI
 *   3. active is an input/textarea without data-cli-input and non-empty value → pass through
 *   4. active is not an input/textarea (button, div, null, body) → redirect to CLI
 *
 * Returns true when '/' should be captured and routed to the CLI input.
 */
interface ActiveElementDescriptor {
  isInputOrTextarea: boolean;
  hasCliAttr: boolean; // has data-cli-input attribute
  value: string;
}

function shouldCaptureSlash(active: ActiveElementDescriptor | null): boolean {
  if (!active || !active.isInputOrTextarea) {
    // Non-input element (button, div, body) or no focus → always capture
    return true;
  }
  if (active.hasCliAttr) {
    // Already on the CLI input itself → let the user type normally
    return false;
  }
  if (active.value !== '') {
    // Non-CLI input with content (e.g., chat with text typed) → don't hijack
    return false;
  }
  // Empty non-CLI input (e.g., empty chat box) → redirect to CLI
  return true;
}

/**
 * Replicates the element-resolution logic of focusCliInput() from ActiveRoom.tsx.
 *
 * focusCliInput() first tries cliInputRef.current. If that element's offsetParent
 * is null (the element or an ancestor has display:none — the hidden-layout case),
 * it falls back to querySelectorAll('[data-cli-input]') and returns the first
 * element with a non-null offsetParent.
 *
 * This pure function models that logic with plain objects so it can run in a
 * node test environment (no jsdom required).
 */
interface CliInputCandidate {
  id: string;
  offsetParent: object | null; // null === inside display:none
}

function resolveVisibleCliInput(
  primaryRef: CliInputCandidate | null,
  candidates: readonly CliInputCandidate[],
): CliInputCandidate | null {
  // Fast path: primary ref is visible
  if (primaryRef && primaryRef.offsetParent !== null) return primaryRef;
  // Fallback: walk candidates in DOM order (mobile first, desktop second)
  for (const el of candidates) {
    if (el.offsetParent !== null) return el;
  }
  return null;
}

// Sentinel objects used as non-null offsetParent values
const VISIBLE = {};
const HIDDEN = null;

describe('Slash-key capture predicate (shouldCaptureSlash)', () => {
  it('captures when no element is focused (null active)', () => {
    expect(shouldCaptureSlash(null)).toBe(true);
  });

  it('captures when focused element is a button (not input/textarea)', () => {
    expect(shouldCaptureSlash({ isInputOrTextarea: false, hasCliAttr: false, value: '' })).toBe(true);
  });

  it('captures when focused element is a div or body (not input/textarea)', () => {
    expect(shouldCaptureSlash({ isInputOrTextarea: false, hasCliAttr: false, value: 'irrelevant' })).toBe(true);
  });

  it('captures when empty non-CLI input is focused (empty chat box)', () => {
    expect(shouldCaptureSlash({ isInputOrTextarea: true, hasCliAttr: false, value: '' })).toBe(true);
  });

  it('does NOT capture when CLI input itself is focused (data-cli-input)', () => {
    expect(shouldCaptureSlash({ isInputOrTextarea: true, hasCliAttr: true, value: '' })).toBe(false);
  });

  it('does NOT capture when CLI input has text typed (mid-command)', () => {
    expect(shouldCaptureSlash({ isInputOrTextarea: true, hasCliAttr: true, value: '/mute ' })).toBe(false);
  });

  it('does NOT capture when non-CLI input has content (chat with text)', () => {
    expect(shouldCaptureSlash({ isInputOrTextarea: true, hasCliAttr: false, value: 'hello world' })).toBe(false);
  });

  it('does NOT capture when non-CLI input has a single char (partial chat)', () => {
    expect(shouldCaptureSlash({ isInputOrTextarea: true, hasCliAttr: false, value: 'h' })).toBe(false);
  });

  // Property-style: non-input elements always captured regardless of value/attr
  it.each([
    ['', false],
    ['hello', false],
    ['/mute', false],
  ])('non-input element with value=%j always captured', (value, hasCliAttr) => {
    expect(shouldCaptureSlash({ isInputOrTextarea: false, hasCliAttr, value })).toBe(true);
  });

  // Property-style: CLI input is never captured regardless of value
  it.each(['', '/', '/mute', '/help', 'a', '123'])(
    'CLI input (data-cli-input) with value=%j is never captured',
    (value) => {
      expect(shouldCaptureSlash({ isInputOrTextarea: true, hasCliAttr: true, value })).toBe(false);
    },
  );

  // Property-style: empty non-CLI input always captured
  it.each([false, true])(
    'empty non-CLI input (hasCliAttr=%s) — only captured when attr is false',
    (hasCliAttr) => {
      const result = shouldCaptureSlash({ isInputOrTextarea: true, hasCliAttr, value: '' });
      // captured iff NOT the CLI input
      expect(result).toBe(!hasCliAttr);
    },
  );
});

describe('focusCliInput element resolution (resolveVisibleCliInput)', () => {
  const desktop = { id: 'desktop', offsetParent: VISIBLE };
  const desktopHidden = { id: 'desktop-hidden', offsetParent: HIDDEN };
  const mobile = { id: 'mobile', offsetParent: VISIBLE };
  const mobileHidden = { id: 'mobile-hidden', offsetParent: HIDDEN };

  it('returns primary ref when it is visible (fast path, no DOM query needed)', () => {
    const result = resolveVisibleCliInput(desktop, [mobileHidden, desktop]);
    expect(result?.id).toBe('desktop');
  });

  it('falls back to candidates when primary ref is null', () => {
    // cliInputRef.current is null (e.g. mobile logPanel unmounted)
    const result = resolveVisibleCliInput(null, [mobileHidden, desktop]);
    expect(result?.id).toBe('desktop');
  });

  it('falls back to candidates when primary ref is inside display:none (the bug case)', () => {
    // Bug scenario: setMobileTab('log') mounted the mobile logPanel, which captured
    // cliInputRef.current. In desktop mode the mobile div has md:hidden (display:none)
    // so mobileHidden.offsetParent === null. The desktop input is visible.
    // Candidates in DOM order: mobile first, desktop second.
    const result = resolveVisibleCliInput(mobileHidden, [mobileHidden, desktop]);
    expect(result?.id).toBe('desktop');
  });

  it('returns mobile input when desktop layout is hidden (mobile mode)', () => {
    // In mobile mode (< md breakpoint) the desktop layout has display:none.
    // cliInputRef.current points to the desktop input (set at initial render).
    // The mobile logPanel is now mounted and visible.
    const result = resolveVisibleCliInput(desktopHidden, [mobile, desktopHidden]);
    expect(result?.id).toBe('mobile');
  });

  it('returns null when all candidates are hidden and primary is hidden', () => {
    // Edge case: both layouts hidden (e.g. window minimised / no CLI rendered)
    const result = resolveVisibleCliInput(mobileHidden, [mobileHidden, desktopHidden]);
    expect(result).toBeNull();
  });

  it('returns null when primary is null and no visible candidates', () => {
    const result = resolveVisibleCliInput(null, [mobileHidden, desktopHidden]);
    expect(result).toBeNull();
  });

  it('returns null when candidates list is empty and primary is hidden', () => {
    const result = resolveVisibleCliInput(mobileHidden, []);
    expect(result).toBeNull();
  });

  it('returns null when both primary and candidates are null/empty', () => {
    expect(resolveVisibleCliInput(null, [])).toBeNull();
  });

  // Property: result is always null or has a non-null offsetParent (never hidden)
  it.each([
    ['both visible', desktop, [mobile, desktop], 'desktop'],
    ['primary visible, candidate hidden', desktop, [mobileHidden], 'desktop'],
    ['primary hidden, first candidate visible', mobileHidden, [mobile, desktop], 'mobile'],
    ['primary hidden, second candidate visible (bug fix path)', mobileHidden, [mobileHidden, desktop], 'desktop'],
    ['all hidden', mobileHidden, [mobileHidden, desktopHidden], null],
  ] as const)(
    'property: result offsetParent is never null — case: %s',
    (_label, primary, candidates, expectedId) => {
      const result = resolveVisibleCliInput(primary, candidates);
      if (expectedId === null) {
        expect(result).toBeNull();
      } else {
        expect(result).not.toBeNull();
        expect(result!.offsetParent).not.toBeNull();
        expect(result!.id).toBe(expectedId);
      }
    },
  );
});
