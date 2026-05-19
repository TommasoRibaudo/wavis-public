import { beforeEach, describe, expect, it, vi } from 'vitest';
import type { Update } from '@tauri-apps/plugin-updater';
import { check } from '@tauri-apps/plugin-updater';
import { checkForUpdate } from '../update-service';

vi.mock('@tauri-apps/plugin-updater', () => ({
  check: vi.fn(),
}));

vi.mock('@tauri-apps/plugin-process', () => ({
  relaunch: vi.fn(),
}));

beforeEach(() => {
  vi.clearAllMocks();
  vi.unstubAllEnvs();
});

describe('checkForUpdate', () => {
  it('returns none in dev mode', async () => {
    vi.stubEnv('DEV', true);

    await expect(checkForUpdate()).resolves.toEqual({ kind: 'none' });
  });

  it('does not call the updater in dev mode', async () => {
    vi.stubEnv('DEV', true);

    await checkForUpdate();

    expect(check).not.toHaveBeenCalled();
  });

  it('returns available when a non-dev update is found', async () => {
    vi.stubEnv('DEV', false);
    const update = { version: '0.2.0' } as Update;
    vi.mocked(check).mockResolvedValueOnce(update);

    await expect(checkForUpdate()).resolves.toEqual({ kind: 'available', update });
    expect(check).toHaveBeenCalledWith({ timeout: 15_000 });
  });

  it('returns none when a non-dev update is not found', async () => {
    vi.stubEnv('DEV', false);
    vi.mocked(check).mockResolvedValueOnce(null);

    await expect(checkForUpdate()).resolves.toEqual({ kind: 'none' });
    expect(check).toHaveBeenCalledWith({ timeout: 15_000 });
  });

  it('maps non-dev updater failures to an error result', async () => {
    vi.stubEnv('DEV', false);
    vi.mocked(check).mockRejectedValueOnce(new Error('updater unavailable'));

    await expect(checkForUpdate()).resolves.toEqual({
      kind: 'error',
      message: 'updater unavailable',
    });
  });

  it('maps non-Error updater failures to an error result', async () => {
    vi.stubEnv('DEV', false);
    vi.mocked(check).mockRejectedValueOnce('offline');

    await expect(checkForUpdate()).resolves.toEqual({
      kind: 'error',
      message: 'offline',
    });
  });
});
