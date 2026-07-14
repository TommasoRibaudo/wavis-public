import { emit, listen } from '@tauri-apps/api/event';

export const WATCH_ALL_DIAGNOSTICS_WINDOW_LABEL = 'watch-all-diagnostics';
export const WATCH_ALL_TEST_READY_EVENT = 'watch-all:test-ready';
export const WATCH_ALL_TEST_STATE_EVENT = 'watch-all:test-state';
export const WATCH_ALL_TEST_CHANNEL_NAME = 'Diagnostics';

export interface WatchAllParams {
  channelName: string;
  testSessionId?: string;
}

export interface WatchAllTestTile {
  participantId: string;
  displayName: string;
  color: string;
  width: number;
  height: number;
}

export interface WatchAllTestState {
  sessionId: string;
  channelName: string;
  tiles: WatchAllTestTile[];
}

export function encodeWatchAllHash(params: WatchAllParams): string {
  return encodeURIComponent(JSON.stringify(params));
}

/**
 * Watch All (child) window: subscribe to diagnostics test-state pushes from
 * DiagnosticsPage. Returns the awaited Promise<UnlistenFn> directly (same
 * contract as share-window-bridge's listenWatchAllReady) so a caller can
 * await registration before emitting test-ready — DiagnosticsPage pushes
 * state immediately on seeing test-ready, so registering after would race it.
 */
export function onWatchAllTestState(
  handler: (state: WatchAllTestState) => void,
): Promise<() => void> {
  return listen<WatchAllTestState>(WATCH_ALL_TEST_STATE_EVENT, (event) => handler(event.payload));
}

/** Watch All (child) window: tell DiagnosticsPage its test-state listener is registered. */
export function emitWatchAllTestReady(sessionId: string): void {
  emit(WATCH_ALL_TEST_READY_EVENT, { sessionId }).catch((err) => {
    console.error(
      '[wavis:watch-all-test-mode]',
      `failed to emit ${WATCH_ALL_TEST_READY_EVENT}:`,
      err,
    );
  });
}
