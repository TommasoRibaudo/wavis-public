import { describe, expect, it, vi } from 'vitest';

vi.mock('@tauri-apps/plugin-updater', () => ({
  check: vi.fn(),
}));

vi.mock('@tauri-apps/plugin-process', () => ({
  relaunch: vi.fn(),
}));

vi.mock('@tauri-apps/api/core', () => ({
  invoke: vi.fn(),
}));

import { updateProgressLabel, updateProgressPercent } from '../update-service';

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
