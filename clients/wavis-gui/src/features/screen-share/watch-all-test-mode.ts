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
