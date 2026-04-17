import { beforeEach, describe, expect, it, vi } from 'vitest';

class MockMediaStream {}
globalThis.MediaStream = MockMediaStream as unknown as typeof MediaStream;

interface ViewerReadyPayload {
  participantId: string;
  windowLabel: string;
}

interface ViewerAudioState {
  activeShares: Set<string>;
  shareWindows: Map<string, string>;
  watchAllOpen: boolean;
  watchAllReady: boolean;
  savedVolumes: Map<string, number>;
  watchAllAttachedAudio: Set<string>;
}

function createState(overrides: Partial<ViewerAudioState> = {}): ViewerAudioState {
  return {
    activeShares: new Set(),
    shareWindows: new Map(),
    watchAllOpen: false,
    watchAllReady: false,
    savedVolumes: new Map(),
    watchAllAttachedAudio: new Set(),
    ...overrides,
  };
}

function onPopOutWindowCreated(
  participantId: string,
  stream: MediaStream | null,
  deps: {
    startSending: (participantId: string, windowLabel: string, stream: MediaStream) => void;
    attachScreenShareAudio: (participantId: string) => void;
  },
): void {
  if (stream) {
    deps.startSending(participantId, `screen-share-${participantId}`, stream);
  }
}

function onWatchAllShareAdded(
  state: ViewerAudioState,
  participantId: string,
  stream: MediaStream | null,
  deps: {
    startSending: (participantId: string, windowLabel: string, stream: MediaStream) => void;
    attachScreenShareAudio: (participantId: string) => void;
    emit: (eventName: string, payload: unknown) => void;
  },
): void {
  if (!state.watchAllOpen || !state.watchAllReady) return;
  if (state.shareWindows.has(participantId)) return;

  if (stream) {
    deps.startSending(participantId, 'watch-all', stream);
  }

  deps.emit('watch-all:share-added', {
    participantId,
    displayName: 'Alice',
    color: '#E06C75',
    canvasFallback: stream === null,
  });
}

function onViewerReady(
  state: ViewerAudioState,
  event: ViewerReadyPayload,
  deps: {
    attachScreenShareAudio: (participantId: string) => void;
    setScreenShareAudioVolume: (participantId: string, volume: number) => void;
  },
): void {
  if (!state.activeShares.has(event.participantId)) return;

  if (event.windowLabel === 'watch-all') {
    if (!state.watchAllOpen || !state.watchAllReady) return;
    if (state.shareWindows.has(event.participantId)) return;
    deps.attachScreenShareAudio(event.participantId);
    deps.setScreenShareAudioVolume(event.participantId, state.savedVolumes.get(event.participantId) ?? 70);
    state.watchAllAttachedAudio.add(event.participantId);
    return;
  }

  const currentWindowLabel = state.shareWindows.get(event.participantId);
  if (currentWindowLabel !== event.windowLabel) return;

  deps.attachScreenShareAudio(event.participantId);
  deps.setScreenShareAudioVolume(event.participantId, state.savedVolumes.get(event.participantId) ?? 70);
}

describe('ActiveRoom viewer audio orchestration', () => {
  const startSending = vi.fn();
  const attachScreenShareAudio = vi.fn();
  const setScreenShareAudioVolume = vi.fn();
  const emit = vi.fn();

  beforeEach(() => {
    startSending.mockReset();
    attachScreenShareAudio.mockReset();
    setScreenShareAudioVolume.mockReset();
    emit.mockReset();
  });

  it('does not attach pop-out audio on tauri://created', () => {
    const stream = new MediaStream();

    onPopOutWindowCreated('user-1', stream, {
      startSending,
      attachScreenShareAudio,
    });

    expect(startSending).toHaveBeenCalledWith('user-1', 'screen-share-user-1', stream);
    expect(attachScreenShareAudio).not.toHaveBeenCalled();
  });

  it('attaches pop-out audio only after the matching viewer-ready event', () => {
    const state = createState({
      activeShares: new Set(['user-1']),
      shareWindows: new Map([['user-1', 'screen-share-user-1']]),
      savedVolumes: new Map([['user-1', 55]]),
    });

    onViewerReady(state, { participantId: 'user-1', windowLabel: 'screen-share-user-1' }, {
      attachScreenShareAudio,
      setScreenShareAudioVolume,
    });

    expect(attachScreenShareAudio).toHaveBeenCalledWith('user-1');
    expect(setScreenShareAudioVolume).toHaveBeenCalledWith('user-1', 55);
  });

  it('ignores stale pop-out ready events after the window is gone', () => {
    const state = createState({
      activeShares: new Set(['user-1']),
      shareWindows: new Map(),
    });

    onViewerReady(state, { participantId: 'user-1', windowLabel: 'screen-share-user-1' }, {
      attachScreenShareAudio,
      setScreenShareAudioVolume,
    });

    expect(attachScreenShareAudio).not.toHaveBeenCalled();
    expect(setScreenShareAudioVolume).not.toHaveBeenCalled();
  });

  it('does not attach watch-all audio when the tile is created', () => {
    const state = createState({
      activeShares: new Set(['user-1']),
      watchAllOpen: true,
      watchAllReady: true,
    });
    const stream = new MediaStream();

    onWatchAllShareAdded(state, 'user-1', stream, {
      startSending,
      attachScreenShareAudio,
      emit,
    });

    expect(startSending).toHaveBeenCalledWith('user-1', 'watch-all', stream);
    expect(emit).toHaveBeenCalledWith('watch-all:share-added', {
      participantId: 'user-1',
      displayName: 'Alice',
      color: '#E06C75',
      canvasFallback: false,
    });
    expect(attachScreenShareAudio).not.toHaveBeenCalled();
  });

  it('attaches watch-all audio only after the corresponding viewer-ready event', () => {
    const state = createState({
      activeShares: new Set(['user-1']),
      watchAllOpen: true,
      watchAllReady: true,
      savedVolumes: new Map([['user-1', 33]]),
    });

    onViewerReady(state, { participantId: 'user-1', windowLabel: 'watch-all' }, {
      attachScreenShareAudio,
      setScreenShareAudioVolume,
    });

    expect(attachScreenShareAudio).toHaveBeenCalledWith('user-1');
    expect(setScreenShareAudioVolume).toHaveBeenCalledWith('user-1', 33);
    expect(state.watchAllAttachedAudio.has('user-1')).toBe(true);
  });

  it('ignores watch-all ready events for shares that already moved to a pop-out', () => {
    const state = createState({
      activeShares: new Set(['user-1']),
      shareWindows: new Map([['user-1', 'screen-share-user-1']]),
      watchAllOpen: true,
      watchAllReady: true,
    });

    onViewerReady(state, { participantId: 'user-1', windowLabel: 'watch-all' }, {
      attachScreenShareAudio,
      setScreenShareAudioVolume,
    });

    expect(attachScreenShareAudio).not.toHaveBeenCalled();
    expect(setScreenShareAudioVolume).not.toHaveBeenCalled();
  });
});
