/**
 * Unit tests for share-window-bridge.ts - verifies the bridge maps typed
 * functions to the right Tauri channels/commands and payload shapes, and
 * that subscriptions clean up (including the register-then-dispose race).
 */

import { describe, it, expect, vi, beforeEach } from 'vitest';

const h = vi.hoisted(() => {
  const constructedWindows: Array<{ label: string; options: Record<string, unknown> }> = [];

  class MockWebviewWindow {
    label: string;
    closed = false;
    focused = false;
    private onceHandlers = new Map<string, (event: unknown) => void>();

    constructor(label: string, options: Record<string, unknown>) {
      this.label = label;
      constructedWindows.push({ label, options });
    }

    close() {
      this.closed = true;
      return Promise.resolve();
    }

    setFocus() {
      this.focused = true;
      return Promise.resolve();
    }

    isMinimized() {
      return Promise.resolve(false);
    }

    unminimize() {
      return Promise.resolve();
    }

    once(event: string, handler: (event: unknown) => void) {
      this.onceHandlers.set(event, handler);
      return Promise.resolve(() => {});
    }

    fire(event: string, payload?: unknown) {
      this.onceHandlers.get(event)?.(payload);
    }
  }

  return {
    constructedWindows,
    MockWebviewWindow,
    emitMock: vi.fn().mockResolvedValue(undefined),
    emitToMock: vi.fn().mockResolvedValue(undefined),
    unlistenMock: vi.fn(),
    listenMock: vi.fn(),
    invokeMock: vi.fn().mockResolvedValue(undefined),
  };
});

const { constructedWindows, emitMock, emitToMock, unlistenMock, listenMock, invokeMock } = h;

vi.mock('@tauri-apps/api/event', () => ({
  emit: (...args: unknown[]): Promise<void> => h.emitMock(...args) as Promise<void>,
  emitTo: (...args: unknown[]): Promise<void> => h.emitToMock(...args) as Promise<void>,
  listen: (...args: unknown[]): Promise<() => void> => h.listenMock(...args) as Promise<() => void>,
}));

vi.mock('@tauri-apps/api/core', () => ({
  invoke: (...args: unknown[]): Promise<unknown> => h.invokeMock(...args) as Promise<unknown>,
}));

vi.mock('@tauri-apps/api/webviewWindow', () => ({
  WebviewWindow: h.MockWebviewWindow,
}));

import {
  ChildWindowHandle,
  closeNativeScreenShareViewer,
  emitShareRestoreVolume,
  emitShareUserState,
  emitWatchAllShareAdded,
  emitWatchAllShareRemoved,
  listShareSources,
  onScreenShareClosed,
  onShareIndicatorStop,
  onWatchAllVolumeChange,
  openNativeScreenShareViewer,
  openScreenShareViewerWindow,
  openWatchAllGridWindow,
  requestScreenShareWindowClose,
  screenShareWindowLabel,
} from '../share-window-bridge';

beforeEach(() => {
  emitMock.mockClear();
  emitToMock.mockClear();
  invokeMock.mockClear();
  listenMock.mockReset();
  unlistenMock.mockClear();
  listenMock.mockResolvedValue(unlistenMock);
  constructedWindows.length = 0;
});

describe('window construction', () => {
  it('opens the pop-out viewer with a participant-derived label and hash params', () => {
    const handle = openScreenShareViewerWindow(
      {
        participantId: 'p1',
        liveKitIdentity: 'user-1',
        username: 'alice',
        userColor: '#fff',
        isOwner: false,
        canvasFallback: true,
        initialVolume: 70,
        initialMuted: false,
      },
      'alice \u2014 screen share',
    );
    expect(handle).toBeInstanceOf(ChildWindowHandle);
    expect(handle.label).toBe('screen-share-p1');
    expect(constructedWindows).toHaveLength(1);
    const { options } = constructedWindows[0];
    expect(options.title).toBe('alice \u2014 screen share');
    const url = options.url as string;
    expect(url.startsWith('/screen-share#')).toBe(true);
    const params = JSON.parse(decodeURIComponent(url.split('#')[1])) as Record<string, unknown>;
    expect(params.liveKitIdentity).toBe('user-1');
    expect(params.canvasFallback).toBe(true);
  });

  it('opens the watch-all grid window with the channel name', () => {
    const handle = openWatchAllGridWindow('my-channel');
    expect(handle.label).toBe('watch-all');
    expect(constructedWindows[0].options.title).toBe('Watch All \u2014 my-channel');
  });

  it('waitForDestroyed resolves on the destroyed event', async () => {
    openWatchAllGridWindow('c');
    // Reach into the mock to fire the event.
    const handle = openWatchAllGridWindow('c2');
    const win = (handle as unknown as { win: InstanceType<typeof h.MockWebviewWindow> }).win;
    const resolved = vi.fn();
    const promise = handle.waitForDestroyed(5_000).then(resolved);
    await Promise.resolve(); // let once() registration settle
    win.fire('tauri://destroyed');
    await promise;
    expect(resolved).toHaveBeenCalled();
  });
});

describe('native IPC commands', () => {
  it('maps to the Rust command names', async () => {
    await openNativeScreenShareViewer('user-1', 'title');
    expect(invokeMock).toHaveBeenCalledWith('media_open_native_screen_share_viewer', {
      identity: 'user-1',
      title: 'title',
    });
    closeNativeScreenShareViewer('user-1');
    expect(invokeMock).toHaveBeenCalledWith('media_close_native_screen_share_viewer', {
      identity: 'user-1',
    });
    await listShareSources();
    expect(invokeMock).toHaveBeenCalledWith('list_share_sources');
  });
});

describe('emits', () => {
  it('emitShareRestoreVolume fans out to both viewer channels', () => {
    const payload = { participantId: 'p1', volume: 40, muted: false };
    emitShareRestoreVolume(payload);
    expect(emitMock).toHaveBeenCalledWith('watch-all:restore-volume', payload);
    expect(emitMock).toHaveBeenCalledWith('screen-share:restore-volume', payload);
  });

  it('emits user state and share-added/removed on their channels', () => {
    const state = { isMuted: true, isDeafened: false, isSharing: false, shareEnabled: true };
    emitShareUserState(state);
    expect(emitMock).toHaveBeenCalledWith('share:user-state', state);

    const added = {
      participantId: 'p1',
      liveKitIdentity: 'u1',
      displayName: 'alice',
      color: '#fff',
      canvasFallback: false,
    };
    emitWatchAllShareAdded(added);
    expect(emitMock).toHaveBeenCalledWith('watch-all:share-added', added);

    emitWatchAllShareRemoved('p1');
    expect(emitMock).toHaveBeenCalledWith('watch-all:share-removed', { participantId: 'p1' });
  });

  it('requestScreenShareWindowClose targets the pop-out window label', () => {
    requestScreenShareWindowClose('p9');
    expect(emitToMock).toHaveBeenCalledWith('screen-share-p9', 'screen-share:close', {});
    expect(screenShareWindowLabel('p9')).toBe('screen-share-p9');
  });
});

describe('subscriptions', () => {
  it('unwraps payloads into handler arguments', () => {
    const volumes: Array<[string, number]> = [];
    onWatchAllVolumeChange((pid, volume) => volumes.push([pid, volume]));
    expect(listenMock).toHaveBeenCalledWith('watch-all:volume-change', expect.any(Function));
    const registered = listenMock.mock.calls[0][1] as (event: { payload: unknown }) => void;
    registered({ payload: { participantId: 'p1', volume: 55 } });
    expect(volumes).toEqual([['p1', 55]]);

    const closed: string[] = [];
    onScreenShareClosed((pid) => closed.push(pid));
    const closedHandler = listenMock.mock.calls[1][1] as (event: { payload: unknown }) => void;
    closedHandler({ payload: { participantId: 'p2' } });
    expect(closed).toEqual(['p2']);
  });

  it('share-indicator stop defaults the target to all', () => {
    const targets: string[] = [];
    onShareIndicatorStop((target) => targets.push(target));
    const handler = listenMock.mock.calls[0][1] as (event: { payload: unknown }) => void;
    handler({ payload: undefined });
    handler({ payload: { target: 'video' } });
    expect(targets).toEqual(['all', 'video']);
  });

  it('cleanup unsubscribes, including when listen resolves after disposal', async () => {
    const cleanup = onScreenShareClosed(() => {});
    await Promise.resolve(); // listen() resolved, unlisten stored
    cleanup();
    expect(unlistenMock).toHaveBeenCalledTimes(1);

    // Race: dispose before listen() resolves - the resolved unlisten must
    // still be invoked so no dangling listener leaks.
    unlistenMock.mockClear();
    let resolveListen: (fn: () => void) => void = () => {};
    listenMock.mockImplementationOnce(
      () =>
        new Promise((resolve) => {
          resolveListen = resolve;
        }),
    );
    const cleanupEarly = onScreenShareClosed(() => {});
    cleanupEarly();
    resolveListen(unlistenMock);
    await Promise.resolve();
    await Promise.resolve();
    expect(unlistenMock).toHaveBeenCalledTimes(1);
  });
});
