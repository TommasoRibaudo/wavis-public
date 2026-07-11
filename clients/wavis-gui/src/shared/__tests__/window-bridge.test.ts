/**
 * Unit tests for window-bridge.ts — command names, event channels, cleanup.
 */

import { describe, it, expect, vi, beforeEach } from 'vitest';

const invokeMock = vi.fn().mockResolvedValue(undefined);
const listenMock = vi.fn();
const unlistenMock = vi.fn();
const setSizeMock = vi.fn().mockResolvedValue(undefined);

vi.mock('@tauri-apps/api/core', () => ({
  invoke: (...args: unknown[]): Promise<unknown> => invokeMock(...args) as Promise<unknown>,
}));

vi.mock('@tauri-apps/api/event', () => ({
  listen: (...args: unknown[]): Promise<() => void> => listenMock(...args) as Promise<() => void>,
}));

vi.mock('@tauri-apps/api/window', () => ({
  getCurrentWindow: () => ({ setSize: setSizeMock }),
}));

vi.mock('@tauri-apps/api/dpi', () => ({
  LogicalSize: class {
    constructor(
      public width: number,
      public height: number,
    ) {}
  },
}));

import {
  closeMainWindow,
  onFocusMainWindow,
  onMainWindowClosing,
  setCurrentWindowSize,
  showMainWindow,
} from '../window-bridge';

beforeEach(() => {
  invokeMock.mockClear();
  setSizeMock.mockClear();
  listenMock.mockReset();
  unlistenMock.mockClear();
  listenMock.mockResolvedValue(unlistenMock);
});

describe('window commands', () => {
  it('show/close map to the Rust IPC commands', async () => {
    await showMainWindow();
    expect(invokeMock).toHaveBeenCalledWith('show_main_window');
    await closeMainWindow();
    expect(invokeMock).toHaveBeenCalledWith('close_main_window');
  });

  it('setCurrentWindowSize resizes with a logical size', async () => {
    await setCurrentWindowSize(700, 500);
    expect(setSizeMock).toHaveBeenCalledTimes(1);
    const size = setSizeMock.mock.calls[0][0] as { width: number; height: number };
    expect(size.width).toBe(700);
    expect(size.height).toBe(500);
  });
});

describe('window event subscriptions', () => {
  it('subscribes to the lifecycle channels and cleans up', async () => {
    const closing = vi.fn();
    const cleanupClosing = onMainWindowClosing(closing);
    expect(listenMock).toHaveBeenCalledWith('main-window-closing', expect.any(Function));

    const focus = vi.fn();
    onFocusMainWindow(focus);
    expect(listenMock).toHaveBeenCalledWith('focus-main-window', expect.any(Function));

    (listenMock.mock.calls[0][1] as () => void)();
    expect(closing).toHaveBeenCalledTimes(1);
    (listenMock.mock.calls[1][1] as () => void)();
    expect(focus).toHaveBeenCalledTimes(1);

    await Promise.resolve();
    cleanupClosing();
    expect(unlistenMock).toHaveBeenCalledTimes(1);
  });
});
