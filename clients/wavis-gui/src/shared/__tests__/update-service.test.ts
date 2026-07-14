import { beforeEach, describe, expect, it, vi } from 'vitest';

vi.mock('@tauri-apps/plugin-updater', () => ({
  check: vi.fn(),
}));

vi.mock('@tauri-apps/plugin-process', () => ({
  relaunch: vi.fn(),
}));

vi.mock('@tauri-apps/api/core', () => ({
  invoke: vi.fn(),
}));

const getStateMock = vi.fn();
const leaveRoomMock = vi.fn();
const waitForMediaTeardownMock = vi.fn<() => Promise<void>>().mockResolvedValue(undefined);

vi.mock('@features/voice/voice-room', () => ({
  getState: () => getStateMock(),
  leaveRoom: () => leaveRoomMock(),
  waitForMediaTeardown: () => waitForMediaTeardownMock(),
}));

import {
  leaveActiveVoiceRoomForUpdate,
  updateProgressLabel,
  updateProgressPercent,
} from '../update-service';

describe('update progress formatting', () => {
  it('returns null percent and generic label when total bytes are unknown', () => {
    const progress = { downloadedBytes: 256, totalBytes: null };

    expect(updateProgressPercent(progress)).toBeNull();
    expect(updateProgressLabel(progress)).toBe('Downloading update...');
  });

  it('rounds progress percentage when total bytes are known', () => {
    const progress = { downloadedBytes: 512, totalBytes: 1024 };

    expect(updateProgressPercent(progress)).toBe(50);
    expect(updateProgressLabel(progress)).toBe('Downloading update... 50%');
  });

  it('caps progress percentage at 100', () => {
    const progress = { downloadedBytes: 1536, totalBytes: 1024 };

    expect(updateProgressPercent(progress)).toBe(100);
    expect(updateProgressLabel(progress)).toBe('Downloading update... 100%');
  });
});

describe('leaveActiveVoiceRoomForUpdate', () => {
  beforeEach(() => {
    getStateMock.mockReset();
    leaveRoomMock.mockReset();
    waitForMediaTeardownMock.mockReset();
    waitForMediaTeardownMock.mockResolvedValue(undefined);
  });

  it('leaves the room and waits for media teardown when a voice session is active', async () => {
    getStateMock.mockReturnValue({ machineState: 'active', mediaState: 'connected' });

    await leaveActiveVoiceRoomForUpdate();

    expect(leaveRoomMock).toHaveBeenCalledTimes(1);
    expect(waitForMediaTeardownMock).toHaveBeenCalledTimes(1);
    // The mic must be released after leaveRoom, before the install can start.
    expect(leaveRoomMock.mock.invocationCallOrder[0]).toBeLessThan(
      waitForMediaTeardownMock.mock.invocationCallOrder[0],
    );
  });

  it('waits for teardown while media is still connecting', async () => {
    getStateMock.mockReturnValue({ machineState: 'idle', mediaState: 'connecting' });

    await leaveActiveVoiceRoomForUpdate();

    expect(leaveRoomMock).toHaveBeenCalledTimes(1);
    expect(waitForMediaTeardownMock).toHaveBeenCalledTimes(1);
  });

  it('does nothing when no voice session is active', async () => {
    getStateMock.mockReturnValue({ machineState: 'idle', mediaState: 'disconnected' });

    await leaveActiveVoiceRoomForUpdate();

    expect(leaveRoomMock).not.toHaveBeenCalled();
    expect(waitForMediaTeardownMock).not.toHaveBeenCalled();
  });
});
