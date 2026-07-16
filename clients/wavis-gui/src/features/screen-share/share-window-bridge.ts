/**
 * Wavis Share Window Bridge
 *
 * Owns every Tauri surface of the screen-share / watch-all / camera-popout
 * child-window subsystem: WebviewWindow construction, the cross-window
 * event channels (`watch-all:*`, `screen-share:*`, `share:*`, `share-picker:*`,
 * `camera-popout:*`, `share-indicator:*`, `share_error`), and the native
 * viewer / source-enumeration IPC commands.
 *
 * UI components must use this module instead of importing @tauri-apps/*
 * directly (see eslint no-restricted-imports fence). Channel names are
 * private to this file — callers use the typed functions.
 */

import { invoke } from '@tauri-apps/api/core';
import { emit, emitTo, listen } from '@tauri-apps/api/event';
import { WebviewWindow } from '@tauri-apps/api/webviewWindow';
import { getCurrentWindow } from '@tauri-apps/api/window';
import type { EnumerationResult, ShareSelection } from './share-types';
import type { ShareQuality } from '@features/voice/voice-room';
import type { VideoTileSnapshot } from '@features/voice/camera-types';

const LOG = '[wavis:share-window-bridge]';

/* ─── Child window handle ───────────────────────────────────────── */

/**
 * Opaque wrapper around a Tauri WebviewWindow exposing only the operations
 * the room UI needs, so components never touch @tauri-apps types directly.
 */
export class ChildWindowHandle {
  constructor(private readonly win: WebviewWindow) {}

  get label(): string {
    return this.win.label;
  }

  close(): Promise<void> {
    return this.win.close();
  }

  setFocus(): Promise<void> {
    return this.win.setFocus();
  }

  isMinimized(): Promise<boolean> {
    return this.win.isMinimized();
  }

  unminimize(): Promise<void> {
    return this.win.unminimize();
  }

  onceError(handler: (detail: unknown) => void): void {
    void this.win.once('tauri://error', (event) => handler(event));
  }

  onceDestroyed(handler: () => void): void {
    void this.win.once('tauri://destroyed', () => handler());
  }

  /** Resolve when the OS window is destroyed, or after `timeoutMs` as a fallback. */
  waitForDestroyed(timeoutMs: number): Promise<void> {
    return new Promise<void>((resolve) => {
      const timeout = setTimeout(resolve, timeoutMs);
      void this.win.once('tauri://destroyed', () => {
        clearTimeout(timeout);
        resolve();
      });
    });
  }
}

const CHILD_WINDOW_BASE_OPTIONS = {
  resizable: true,
  decorations: false,
  center: true,
} as const;

function hashParams(params: unknown): string {
  return encodeURIComponent(JSON.stringify(params));
}

/* ─── Window construction ───────────────────────────────────────── */

export function screenShareWindowLabel(participantId: string): string {
  return `screen-share-${participantId}`;
}

export interface ScreenShareViewerParams {
  participantId: string;
  liveKitIdentity: string;
  username: string;
  userColor: string;
  isOwner: boolean;
  canvasFallback: boolean;
  initialVolume: number;
  initialMuted: boolean;
}

/** Open the per-sharer pop-out viewer window. */
export function openScreenShareViewerWindow(
  params: ScreenShareViewerParams,
  title: string,
): ChildWindowHandle {
  const win = new WebviewWindow(screenShareWindowLabel(params.participantId), {
    url: `/screen-share#${hashParams(params)}`,
    title,
    width: 800,
    height: 520,
    minWidth: 320,
    minHeight: 232,
    ...CHILD_WINDOW_BASE_OPTIONS,
  });
  return new ChildWindowHandle(win);
}

/** Open the Watch All grid window. */
export function openWatchAllGridWindow(channelName: string): ChildWindowHandle {
  const win = new WebviewWindow('watch-all', {
    url: `/watch-all#${hashParams({ channelName })}`,
    title: `Watch All — ${channelName}`,
    width: 960,
    height: 540,
    minWidth: 480,
    minHeight: 320,
    ...CHILD_WINDOW_BASE_OPTIONS,
  });
  return new ChildWindowHandle(win);
}

/** Open the camera video pop-out window. */
export function openCameraPopoutWindow(channelName: string): ChildWindowHandle {
  const win = new WebviewWindow('camera-popout', {
    url: `/camera-popout#${hashParams({ channelName })}`,
    title: `Camera — ${channelName}`,
    width: 960,
    height: 540,
    minWidth: 480,
    minHeight: 320,
    ...CHILD_WINDOW_BASE_OPTIONS,
  });
  return new ChildWindowHandle(win);
}

/** Open the Linux standalone share picker window. */
export function openSharePickerWindow(payload: {
  enumResult: EnumerationResult | null;
  occupied: { videoOccupied: boolean; audioOccupied: boolean };
}): ChildWindowHandle {
  const win = new WebviewWindow('share-picker', {
    url: `/share-picker#${hashParams(payload)}`,
    title: 'Wavis — Share Picker',
    width: 640,
    height: 480,
    minWidth: 360,
    minHeight: 320,
    ...CHILD_WINDOW_BASE_OPTIONS,
  });
  return new ChildWindowHandle(win);
}

/* ─── Native IPC commands ───────────────────────────────────────── */

/** Enumerate shareable sources via the native picker pipeline. */
export function listShareSources(): Promise<EnumerationResult> {
  return invoke<EnumerationResult>('list_share_sources');
}

export interface ScreenRecordingAccess {
  authorized: boolean;
  promptShown: boolean;
  restartRequired: boolean;
}

/** macOS: check/request the Screen Recording permission. */
export function ensureScreenRecordingAccess(): Promise<ScreenRecordingAccess> {
  return invoke<ScreenRecordingAccess>('ensure_screen_recording_access');
}

/** Open the Rust-native (non-webview) share viewer keyed by LiveKit identity. */
export function openNativeScreenShareViewer(identity: string, title: string): Promise<void> {
  return invoke('media_open_native_screen_share_viewer', { identity, title });
}

/** Close the Rust-native share viewer; swallows errors (viewer may be gone). */
export function closeNativeScreenShareViewer(identity: string): void {
  invoke('media_close_native_screen_share_viewer', { identity }).catch(() => {});
}

/* ─── Current (child) window lifecycle ──────────────────────────── */

/** Close the window this code is currently running in. */
export function closeCurrentChildWindow(): Promise<void> {
  return getCurrentWindow().close();
}

/** Subscribe to the current window's own close-requested event (e.g. OS close button). */
export function onCurrentChildWindowCloseRequested(
  handler: () => Promise<void> | void,
): () => void {
  const unlisten = getCurrentWindow().onCloseRequested(async () => {
    await handler();
  });
  let disposed = false;
  let fn: (() => void) | null = null;
  unlisten
    .then((f) => {
      if (disposed) f();
      else fn = f;
    })
    .catch((err) => console.error(LOG, 'failed to listen for close-requested:', err));
  return () => {
    disposed = true;
    fn?.();
  };
}

/* ─── Event plumbing helpers ────────────────────────────────────── */

function subscribe<T>(channel: string, handler: (payload: T) => void): () => void {
  let unlisten: (() => void) | null = null;
  let disposed = false;
  listen<T>(channel, (event) => handler(event.payload))
    .then((fn) => {
      if (disposed) fn();
      else unlisten = fn;
    })
    .catch((err) => {
      console.error(LOG, `failed to listen for ${channel}:`, err);
    });
  return () => {
    disposed = true;
    unlisten?.();
  };
}

function fireAndForget(channel: string, payload?: unknown): void {
  emit(channel, payload).catch((err) => {
    console.error(LOG, `failed to emit ${channel}:`, err);
  });
}

/* ─── Emits: main window → child windows ────────────────────────── */

export interface ShareRestoreVolumePayload {
  participantId: string;
  volume: number;
  muted: boolean;
}

export function emitShareRestoreVolume(payload: ShareRestoreVolumePayload): void {
  fireAndForget('watch-all:restore-volume', payload);
  fireAndForget('screen-share:restore-volume', payload);
}

export function emitWatchAllRestoreVolume(payload: ShareRestoreVolumePayload): void {
  fireAndForget('watch-all:restore-volume', payload);
}

export interface ShareUserState {
  isMuted: boolean;
  isDeafened: boolean;
  isSharing: boolean;
  shareEnabled: boolean;
}

export function emitShareUserState(state: ShareUserState): void {
  fireAndForget('share:user-state', state);
}

export interface ShareVoiceParticipant {
  id: string;
  name: string;
  color: string;
  volume: number;
  muted: boolean;
}

export function emitShareVoiceParticipants(payload: {
  participants: ShareVoiceParticipant[];
}): void {
  fireAndForget('share:voice-participants', payload);
}

export function emitWatchAllVoiceParticipants(payload: {
  participants: ShareVoiceParticipant[];
}): void {
  fireAndForget('watch-all:voice-participants', payload);
}

export interface WatchAllShareAddedPayload {
  participantId: string;
  liveKitIdentity: string;
  displayName: string;
  color: string;
  canvasFallback: boolean;
}

export function emitWatchAllShareAdded(payload: WatchAllShareAddedPayload): void {
  fireAndForget('watch-all:share-added', payload);
}

export function emitWatchAllShareRemoved(participantId: string): void {
  fireAndForget('watch-all:share-removed', { participantId });
}

export function emitWatchAllShareUpdated(payload: {
  participantId: string;
  displayName: string;
  color: string;
}): void {
  fireAndForget('watch-all:share-updated', payload);
}

export interface WatchAllAudioShareAddedPayload {
  participantId: string;
  displayName: string;
  color: string;
  volume: number;
  muted: boolean;
}

export function emitWatchAllAudioShareAdded(payload: WatchAllAudioShareAddedPayload): void {
  fireAndForget('watch-all:audio-share-added', payload);
}

export function emitWatchAllAudioShareRemoved(participantId: string): void {
  fireAndForget('watch-all:audio-share-removed', { participantId });
}

export function emitCameraPopoutTileAdded(snapshot: VideoTileSnapshot): void {
  fireAndForget('camera-popout:tile-added', snapshot);
}

export function emitCameraPopoutTileUpdated(snapshot: VideoTileSnapshot): void {
  fireAndForget('camera-popout:tile-updated', snapshot);
}

export function emitCameraPopoutTileRemoved(participantId: string): void {
  fireAndForget('camera-popout:tile-removed', { participantId });
}

/** Tell a specific pop-out viewer window to close itself. */
export function requestScreenShareWindowClose(participantId: string): void {
  emitTo(screenShareWindowLabel(participantId), 'screen-share:close', {}).catch(() => {});
}

/* ─── Listens: child windows → main window ──────────────────────── */

export function onScreenShareClosed(handler: (participantId: string) => void): () => void {
  return subscribe<{ participantId: string }>('screen-share:closed', (p) =>
    handler(p.participantId),
  );
}

export function onScreenShareQuality(handler: (quality: ShareQuality) => void): () => void {
  return subscribe<{ quality: ShareQuality }>('screen-share:quality', (p) => handler(p.quality));
}

export function onScreenShareToggleAudio(handler: (withAudio: boolean) => void): () => void {
  return subscribe<{ withAudio: boolean }>('screen-share:toggle-audio', (p) =>
    handler(p.withAudio),
  );
}

export function onScreenShareChangeSource(handler: () => void): () => void {
  return subscribe('screen-share:change-source', () => handler());
}

export function onWatchAllVolumeChange(
  handler: (participantId: string, volume: number) => void,
): () => void {
  return subscribe<{ participantId: string; volume: number }>('watch-all:volume-change', (p) =>
    handler(p.participantId, p.volume),
  );
}

export function onScreenShareVolumeChange(
  handler: (participantId: string, volume: number) => void,
): () => void {
  return subscribe<{ participantId: string; volume: number }>('screen-share:volume-change', (p) =>
    handler(p.participantId, p.volume),
  );
}

export function onWatchAllMuteChange(
  handler: (participantId: string, muted: boolean) => void,
): () => void {
  return subscribe<{ participantId: string; muted: boolean }>('watch-all:mute-change', (p) =>
    handler(p.participantId, p.muted),
  );
}

export function onScreenShareMuteChange(
  handler: (participantId: string, muted: boolean) => void,
): () => void {
  return subscribe<{ participantId: string; muted: boolean }>('screen-share:mute-change', (p) =>
    handler(p.participantId, p.muted),
  );
}

export function onShareVoiceVolumeChange(
  handler: (participantId: string, volume: number) => void,
): () => void {
  return subscribe<{ participantId: string; volume: number }>('share:voice-volume-change', (p) =>
    handler(p.participantId, p.volume),
  );
}

/** Child windows (ScreenSharePage, WatchAllPage): report a voice-participant volume change. */
export function emitShareVoiceVolumeChange(participantId: string, volume: number): void {
  fireAndForget('share:voice-volume-change', { participantId, volume });
}

export function onShareToggleMute(handler: () => void): () => void {
  return subscribe('share:toggle-mute', () => handler());
}

export function onShareToggleDeafen(handler: () => void): () => void {
  return subscribe('share:toggle-deafen', () => handler());
}

export function onShareToggleShare(handler: () => void): () => void {
  return subscribe('share:toggle-share', () => handler());
}

export function onScreenShareViewerReady(
  handler: (participantId: string, windowLabel: string) => void,
): () => void {
  return subscribe<{ participantId: string; windowLabel: string }>(
    'screen-share-viewer:ready',
    (p) => handler(p.participantId, p.windowLabel),
  );
}

export function onWatchAllClosed(handler: () => void): () => void {
  return subscribe('watch-all:closed', () => handler());
}

export function onScreenSharePopBackIn(handler: (participantId: string) => void): () => void {
  return subscribe<{ participantId: string }>('screen-share:pop-back-in', (p) =>
    handler(p.participantId),
  );
}

export interface WatchAllPopOutPayload {
  participantId: string;
  volume?: number;
  muted?: boolean;
}

export function onWatchAllPopOut(handler: (payload: WatchAllPopOutPayload) => void): () => void {
  return subscribe<WatchAllPopOutPayload>('watch-all:pop-out', handler);
}

export function onCameraPopoutClosed(handler: () => void): () => void {
  return subscribe('camera-popout:closed', () => handler());
}

export function onSharePickerSelected(handler: (selection: ShareSelection) => void): () => void {
  return subscribe<ShareSelection>('share-picker:selected', handler);
}

export function onSharePickerCancelled(handler: () => void): () => void {
  return subscribe('share-picker:cancelled', () => handler());
}

export function onSharePickerUsePortal(handler: () => void): () => void {
  return subscribe('share-picker:use-portal', () => handler());
}

export function onShareIndicatorStop(
  handler: (target: 'video' | 'audio' | 'all') => void,
): () => void {
  return subscribe<{ target?: 'video' | 'audio' | 'all' } | undefined>(
    'share-indicator:stop',
    (p) => handler(p?.target ?? 'all'),
  );
}

/** Rust-side share error (PipeWire disconnect, captured window closed, …). */
export function onNativeShareError(handler: (message: string) => void): () => void {
  return subscribe<string>('share_error', handler);
}

/* ─── Ready listeners (await registration before opening window) ── */

/**
 * Register the watch-all ready listener. Await this BEFORE constructing the
 * watch-all window: listen() is async, and a listener registered after the
 * child window mounts can miss its ready event.
 */
export function listenWatchAllReady(handler: () => void): Promise<() => void> {
  return listen('watch-all:ready', () => handler());
}

/** Same contract as listenWatchAllReady, for the camera pop-out window. */
export function listenCameraPopoutReady(handler: () => void): Promise<() => void> {
  return listen('camera-popout:ready', () => handler());
}

/* ─── Child window: Watch All page ──────────────────────────────── *
 * Mirror image of the "Emits: main → child" / "Listens: child → main"
 * sections above, from the Watch All child window's own point of view. */

export interface WatchAllShareUpdatedPayload {
  participantId: string;
  displayName: string;
  color: string;
}

/**
 * The next 6 listeners back the "register everything before signaling
 * readiness" contract WatchAllPage relies on (see emitWatchAllReady) — each
 * returns the awaited Promise<UnlistenFn> directly, same contract as
 * listenWatchAllReady, so a caller can Promise.all them all before emitting
 * ready and be certain none of them can miss an early event.
 */

export function onWatchAllShareAdded(
  handler: (payload: WatchAllShareAddedPayload) => void,
): Promise<() => void> {
  return listen<WatchAllShareAddedPayload>('watch-all:share-added', (event) =>
    handler(event.payload),
  );
}

export function onWatchAllShareRemoved(
  handler: (participantId: string) => void,
): Promise<() => void> {
  return listen<{ participantId: string }>('watch-all:share-removed', (event) =>
    handler(event.payload.participantId),
  );
}

export function onWatchAllShareUpdated(
  handler: (payload: WatchAllShareUpdatedPayload) => void,
): Promise<() => void> {
  return listen<WatchAllShareUpdatedPayload>('watch-all:share-updated', (event) =>
    handler(event.payload),
  );
}

export function onWatchAllRestoreVolume(
  handler: (payload: ShareRestoreVolumePayload) => void,
): Promise<() => void> {
  return listen<ShareRestoreVolumePayload>('watch-all:restore-volume', (event) =>
    handler(event.payload),
  );
}

export function onWatchAllAudioShareAdded(
  handler: (payload: WatchAllAudioShareAddedPayload) => void,
): Promise<() => void> {
  return listen<WatchAllAudioShareAddedPayload>('watch-all:audio-share-added', (event) =>
    handler(event.payload),
  );
}

export function onWatchAllAudioShareRemoved(
  handler: (participantId: string) => void,
): Promise<() => void> {
  return listen<{ participantId: string }>('watch-all:audio-share-removed', (event) =>
    handler(event.payload.participantId),
  );
}

export function onShareUserState(handler: (state: ShareUserState) => void): () => void {
  return subscribe<ShareUserState>('share:user-state', handler);
}

export function onWatchAllVoiceParticipantsUpdate(
  handler: (payload: { participants: ShareVoiceParticipant[] }) => void,
): () => void {
  return subscribe<{ participants: ShareVoiceParticipant[] }>(
    'watch-all:voice-participants',
    handler,
  );
}

/** Main window telling this Watch All window to close itself. */
export function onWatchAllCloseCommand(handler: () => void): () => void {
  return subscribe('watch-all:close', () => handler());
}

export function onVoiceSessionEnded(handler: () => void): () => void {
  return subscribe('voice-session:ended', () => handler());
}

/** Tell the main window this Watch All window finished closing. Awaitable so
 *  callers can hold an onCloseRequested handler open until the emit lands. */
export function emitWatchAllClosed(): Promise<void> {
  return emit('watch-all:closed', {});
}

/** Tell the main window the child listeners are registered and ready for share-added events. */
export function emitWatchAllReady(): void {
  fireAndForget('watch-all:ready', {});
}

export function emitScreenShareViewerReady(participantId: string, windowLabel: string): void {
  emitTo('main', 'screen-share-viewer:ready', { participantId, windowLabel }).catch((err) => {
    console.error(LOG, 'failed to emit screen-share-viewer:ready:', err);
  });
}

/** Tell the main window a viewer subscribed to a specific sharer's stream. */
export function emitViewerSubscribed(targetId: string): void {
  emitTo('main', 'viewer-subscribed', { targetId }).catch((err) => {
    console.error(LOG, 'failed to emit viewer-subscribed:', err);
  });
}

export function emitWatchAllMuteChange(participantId: string, muted: boolean): void {
  fireAndForget('watch-all:mute-change', { participantId, muted });
}

export function emitWatchAllVolumeChange(participantId: string, volume: number): void {
  fireAndForget('watch-all:volume-change', { participantId, volume });
}

export function emitWatchAllPopOut(payload: WatchAllPopOutPayload): void {
  fireAndForget('watch-all:pop-out', payload);
}

export function emitShareToggleMute(): void {
  fireAndForget('share:toggle-mute');
}

export function emitShareToggleDeafen(): void {
  fireAndForget('share:toggle-deafen');
}
