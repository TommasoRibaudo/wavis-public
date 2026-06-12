import { describe, it, expect, vi, beforeEach } from 'vitest';
import { invoke } from '@tauri-apps/api/core';

let mockStore: Record<string, unknown> = {};
let mockKeychain: Record<string, string> = {};
let mockFetchMode: 'register_ok' | 'recover_ok' = 'register_ok';
let fetchBodies: unknown[] = [];

vi.mock('@tauri-apps/plugin-store', () => ({
  load: vi.fn(async () => ({
    get: vi.fn(async (key: string) => mockStore[key] ?? null),
    set: vi.fn(async (key: string, value: unknown) => {
      mockStore[key] = value;
    }),
    delete: vi.fn(async (key: string) => {
      delete mockStore[key];
    }),
  })),
}));

vi.mock('@tauri-apps/api/core', () => ({
  invoke: vi.fn(async (cmd: string, args?: Record<string, unknown>) => {
    if (cmd === 'get_token') {
      return mockKeychain[(args?.key as string) ?? ''] ?? null;
    }
    if (cmd === 'store_token') {
      mockKeychain[(args?.key as string) ?? ''] = (args?.value as string) ?? '';
      return;
    }
    if (cmd === 'delete_token') {
      delete mockKeychain[(args?.key as string) ?? ''];
      return;
    }
    throw new Error(`Unknown IPC command: ${cmd}`);
  }),
}));

vi.mock('@tauri-apps/plugin-http', () => ({
  fetch: vi.fn(async (url: string, init?: RequestInit) => {
    const futureExp = Date.now() / 1000 + 900;
    if (init?.body) fetchBodies.push(JSON.parse(init.body as string));
    if (url.includes('/auth/register') && mockFetchMode === 'register_ok') {
      return {
        ok: true,
        status: 200,
        headers: { get: () => null },
        json: async () => ({
          user_id: 'user-1',
          device_id: 'device-1',
          recovery_id: 'wvs-REG-0001',
          access_token: `h.${btoa(JSON.stringify({ exp: futureExp }))}.s`,
          refresh_token: 'refresh-register',
        }),
      };
    }
    if (url.includes('/auth/recover') && mockFetchMode === 'recover_ok') {
      return {
        ok: true,
        status: 200,
        headers: { get: () => null },
        json: async () => ({
          user_id: 'user-1',
          device_id: 'device-2',
          access_token: `h.${btoa(JSON.stringify({ exp: futureExp }))}.s`,
          refresh_token: 'refresh-recover',
        }),
      };
    }
    throw new Error(`Unexpected fetch URL: ${url}`);
  }),
}));

describe('recovery ID storage helpers', () => {
  beforeEach(() => {
    mockStore = {};
    mockKeychain = {};
    mockFetchMode = 'register_ok';
    fetchBodies = [];
    vi.clearAllMocks();
    vi.resetModules();
  });

  it('storeRecoveryId / getStoredRecoveryId / deleteStoredRecoveryId round-trip', async () => {
    const auth = await import('../auth');

    await auth.storeRecoveryId('wvs-ABCD-1234');
    expect(mockStore['recovery_id']).toBe('wvs-ABCD-1234');
    expect(await auth.getStoredRecoveryId()).toBe('wvs-ABCD-1234');

    await auth.deleteStoredRecoveryId();
    expect(mockStore['recovery_id']).toBeUndefined();
    expect(await auth.getStoredRecoveryId()).toBeNull();
  });

  it('recoverAccount stores the trimmed recovery ID after success', async () => {
    mockFetchMode = 'recover_ok';
    const auth = await import('../auth');

    const result = await auth.recoverAccount(
      'https://wavis.example.com',
      '  wvs-TRIM-0001  ',
      'passphrase',
      'user',
      false,
      () => {},
    );

    expect(result.success).toBe(true);
    expect(mockStore['recovery_id']).toBe('wvs-TRIM-0001');
    expect(fetchBodies).toMatchObject([
      { recovery_id: 'wvs-TRIM-0001', phrase: 'passphrase' },
    ]);
    expect(fetchBodies[0]).not.toHaveProperty('username');
  });

  it('recoverAccount can skip storing the recovery ID', async () => {
    mockFetchMode = 'recover_ok';
    mockStore = { recovery_id: 'wvs-OLD-0001' };
    const auth = await import('../auth');

    const result = await auth.recoverAccount(
      'https://wavis.example.com',
      'wvs-NOTRUST-1',
      'passphrase',
      '',
      false,
      () => {},
      false,
    );

    expect(result.success).toBe(true);
    expect(mockStore['recovery_id']).toBeUndefined();
  });

  it('registerUser stores the response recovery ID after success', async () => {
    mockFetchMode = 'register_ok';
    const auth = await import('../auth');

    const result = await auth.registerUser(
      'https://wavis.example.com',
      'passphrase',
      'user',
      false,
      () => {},
    );

    expect(result).toEqual({ success: true, recovery_id: 'wvs-REG-0001' });
    expect(mockStore['recovery_id']).toBe('wvs-REG-0001');
  });

  it('resetAuth deletes the stored recovery ID', async () => {
    mockStore = {
      server_url: 'https://wavis.example.com',
      device_id: 'device-1',
      access_token: 'token',
      access_token_exp: Date.now() + 60_000,
      username: 'user',
      recovery_id: 'wvs-OLD-0001',
    };
    mockKeychain = {
      wavis_refresh_token: 'refresh-token',
    };
    const auth = await import('../auth');

    await auth.resetAuth();

    expect(mockKeychain['wavis_refresh_token']).toBeUndefined();
    expect(mockStore['recovery_id']).toBeUndefined();
    expect(invoke).toHaveBeenCalledWith('delete_token', { key: 'wavis_refresh_token' });
  });
});
