/**
 * Unit tests for autostart-bridge.ts — plugin call mapping and error fallback.
 */

import { describe, it, expect, vi, beforeEach } from 'vitest';

const enableMock = vi.fn().mockResolvedValue(undefined);
const disableMock = vi.fn().mockResolvedValue(undefined);
const isEnabledMock = vi.fn().mockResolvedValue(false);

vi.mock('@tauri-apps/plugin-autostart', () => ({
  enable: (...args: unknown[]): Promise<void> => enableMock(...args) as Promise<void>,
  disable: (...args: unknown[]): Promise<void> => disableMock(...args) as Promise<void>,
  isEnabled: (...args: unknown[]): Promise<boolean> => isEnabledMock(...args) as Promise<boolean>,
}));

import { isLaunchOnStartupEnabled, setLaunchOnStartupEnabled } from '../autostart-bridge';

beforeEach(() => {
  enableMock.mockClear();
  disableMock.mockClear();
  isEnabledMock.mockClear();
});

describe('isLaunchOnStartupEnabled', () => {
  it('returns the plugin state', async () => {
    isEnabledMock.mockResolvedValueOnce(true);
    await expect(isLaunchOnStartupEnabled()).resolves.toBe(true);
  });

  it('falls back to false if the plugin call throws', async () => {
    isEnabledMock.mockRejectedValueOnce(new Error('boom'));
    await expect(isLaunchOnStartupEnabled()).resolves.toBe(false);
  });
});

describe('setLaunchOnStartupEnabled', () => {
  it('calls enable() when turning on', async () => {
    await setLaunchOnStartupEnabled(true);
    expect(enableMock).toHaveBeenCalledTimes(1);
    expect(disableMock).not.toHaveBeenCalled();
  });

  it('calls disable() when turning off', async () => {
    await setLaunchOnStartupEnabled(false);
    expect(disableMock).toHaveBeenCalledTimes(1);
    expect(enableMock).not.toHaveBeenCalled();
  });

  it('does not throw if the plugin call rejects', async () => {
    enableMock.mockRejectedValueOnce(new Error('boom'));
    await expect(setLaunchOnStartupEnabled(true)).resolves.toBeUndefined();
  });
});
