import { describe, it, expect, vi, beforeEach } from 'vitest';
import { parseJwtExpiry, redactToken, validateServerUrl } from '../auth';

describe('parseJwtExpiry', () => {
  it('returns expiry ms from a valid JWT', () => {
    // payload: { "sub": "user1", "exp": 1700000000 }
    const payload = btoa(JSON.stringify({ sub: 'user1', exp: 1700000000 }));
    const jwt = `eyJhbGciOiJIUzI1NiJ9.${payload}.signature`;
    expect(parseJwtExpiry(jwt)).toBe(1700000000 * 1000);
  });

  it('returns null for missing exp field', () => {
    const payload = btoa(JSON.stringify({ sub: 'user1' }));
    const jwt = `header.${payload}.sig`;
    expect(parseJwtExpiry(jwt)).toBeNull();
  });

  it('returns null for wrong segment count', () => {
    expect(parseJwtExpiry('only.two')).toBeNull();
    expect(parseJwtExpiry('one')).toBeNull();
  });

  it('returns null for non-JSON payload', () => {
    const jwt = `header.${btoa('not json')}.sig`;
    expect(parseJwtExpiry(jwt)).toBeNull();
  });

  it('handles URL-safe base64 characters', () => {
    // Use raw base64url encoding with - and _
    const json = JSON.stringify({ exp: 1700000000 });
    const b64 = btoa(json).replace(/\+/g, '-').replace(/\//g, '_').replace(/=+$/, '');
    const jwt = `header.${b64}.sig`;
    expect(parseJwtExpiry(jwt)).toBe(1700000000 * 1000);
  });

  it('returns null when exp is not a number', () => {
    const payload = btoa(JSON.stringify({ exp: 'not-a-number' }));
    const jwt = `header.${payload}.sig`;
    expect(parseJwtExpiry(jwt)).toBeNull();
  });
});

describe('redactToken', () => {
  it('redacts tokens >= 16 chars', () => {
    const token = 'abcdefghijklmnopqrstuvwxyz';
    expect(redactToken(token)).toBe('abcdefghijklmnop...');
  });

  it('returns *** for short tokens', () => {
    expect(redactToken('short')).toBe('***');
    expect(redactToken('')).toBe('***');
  });

  it('handles exactly 16 chars', () => {
    const token = '1234567890123456';
    expect(redactToken(token)).toBe('1234567890123456...');
  });
});

describe('validateServerUrl', () => {
  it('accepts https URLs', () => {
    expect(validateServerUrl('https://example.com', false)).toEqual({ valid: true });
  });

  it('accepts http with insecureTls enabled', () => {
    expect(validateServerUrl('http://localhost:3000', true)).toEqual({ valid: true });
  });

  it('rejects http without insecureTls', () => {
    const result = validateServerUrl('http://localhost:3000', false);
    expect(result.valid).toBe(false);
    expect(result.reason).toContain('insecure');
  });

  it('rejects empty string', () => {
    const result = validateServerUrl('', false);
    expect(result.valid).toBe(false);
    expect(result.reason).toContain('empty');
  });

  it('rejects unsupported protocols', () => {
    const result = validateServerUrl('ftp://example.com', false);
    expect(result.valid).toBe(false);
    expect(result.reason).toContain('Unsupported protocol');
  });

  it('rejects malformed URLs', () => {
    const result = validateServerUrl('not a url at all', false);
    expect(result.valid).toBe(false);
    expect(result.reason).toContain('Malformed');
  });
});


/* ═══ Mocked Integration Tests ══════════════════════════════════════
 *
 * Tests below require mocks for Tauri store, keychain IPC, and tauriFetch.
 * They use vi.resetModules() + dynamic import('../auth') to pick up mocks.
 *
 * Validates: Requirements 2.1, 2.2, 2.3, 2.4, 2.5
 */

/* ─── Mock State ────────────────────────────────────────────────── */

let mockStore: Record<string, unknown> = {};
let mockKeychain: Record<string, string> = {};
let deleteTokenCalls: Array<{ key: string }> = [];
let mockFetchBehavior:
  | 'network_error'
  | '401'
  | '400'
  | '429'
  | '500'
  | '200' = '200';
let mockRetryAfterHeader: string | null = null;

/* ─── Module Mocks ──────────────────────────────────────────────── */

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
      deleteTokenCalls.push({ key: (args?.key as string) ?? '' });
      delete mockKeychain[(args?.key as string) ?? ''];
      return;
    }
    throw new Error(`Unknown IPC command: ${cmd}`);
  }),
}));

vi.mock('@tauri-apps/plugin-http', () => ({
  fetch: vi.fn(async (url: string) => {
    if (typeof url === 'string' && url.includes('/auth/refresh')) {
      if (mockFetchBehavior === 'network_error') {
        throw new Error('Network error: connection refused');
      }
      if (mockFetchBehavior === '401') {
        return {
          ok: false,
          status: 401,
          headers: { get: () => null },
          json: async () => ({ error: 'unauthorized' }),
        };
      }
      if (mockFetchBehavior === '400') {
        return {
          ok: false,
          status: 400,
          headers: { get: () => null },
          json: async () => ({ error: 'bad request' }),
        };
      }
      if (mockFetchBehavior === '429') {
        return {
          ok: false,
          status: 429,
          headers: {
            get: (key: string) =>
              key === 'Retry-After' ? mockRetryAfterHeader : null,
          },
          json: async () => ({}),
        };
      }
      if (mockFetchBehavior === '500') {
        return {
          ok: false,
          status: 500,
          headers: { get: () => null },
          json: async () => ({ error: 'internal server error' }),
        };
      }
      if (mockFetchBehavior === '200') {
        const futureExp = Date.now() / 1000 + 900;
        return {
          ok: true,
          status: 200,
          headers: { get: () => null },
          json: async () => ({
            user_id: 'device-123',
            access_token: `header.${btoa(JSON.stringify({ sub: 'device-123', exp: futureExp }))}.sig`,
            refresh_token: 'new-refresh-token',
          }),
        };
      }
    }
    throw new Error(`Unexpected fetch URL: ${url}`);
  }),
}));

/* ─── Shared Setup ──────────────────────────────────────────────── */

function setupFullStore(): void {
  mockStore = {
    device_id: 'device-123',
    server_url: 'https://wavis.example.com',
    display_name: 'TestUser',
    access_token: 'some-access-token',
    access_token_exp: Date.now() + 600_000,
    insecure_tls: false,
  };
  mockKeychain = {
    wavis_refresh_token: 'old-refresh-token',
  };
}

/* ═══ clearAccessTokens Tests ═══════════════════════════════════════
 *
 * Validates: Requirements 2.1, 2.2
 */
describe('clearAccessTokens', () => {
  beforeEach(() => {
    mockStore = {};
    mockKeychain = {};
    deleteTokenCalls = [];
    vi.resetModules();
  });

  it('deletes access_token and access_token_exp from store', async () => {
    setupFullStore();
    const auth = await import('../auth');

    await auth.clearAccessTokens();

    expect(mockStore['access_token']).toBeUndefined();
    expect(mockStore['access_token_exp']).toBeUndefined();
  });

  it('does NOT delete device_id, server_url, display_name, insecure_tls from store', async () => {
    setupFullStore();
    const auth = await import('../auth');

    await auth.clearAccessTokens();

    expect(mockStore['device_id']).toBe('device-123');
    expect(mockStore['server_url']).toBe('https://wavis.example.com');
    expect(mockStore['display_name']).toBe('TestUser');
    expect(mockStore['insecure_tls']).toBe(false);
  });

  it('does NOT call invoke(delete_token) — refresh token preserved in keychain', async () => {
    setupFullStore();
    const auth = await import('../auth');

    await auth.clearAccessTokens();

    expect(deleteTokenCalls).toHaveLength(0);
    expect(mockKeychain['wavis_refresh_token']).toBe('old-refresh-token');
  });
});

/* ═══ clearSessionFull Tests ════════════════════════════════════════
 *
 * Validates: Requirements 2.1, 2.2
 */
describe('clearSessionFull', () => {
  beforeEach(() => {
    mockStore = {};
    mockKeychain = {};
    deleteTokenCalls = [];
    vi.resetModules();
  });

  it('deletes access_token and access_token_exp from store AND calls invoke(delete_token) for refresh token', async () => {
    setupFullStore();
    const auth = await import('../auth');

    await auth.clearSessionFull();

    expect(mockStore['access_token']).toBeUndefined();
    expect(mockStore['access_token_exp']).toBeUndefined();
    expect(deleteTokenCalls).toContainEqual({ key: 'wavis_refresh_token' });
  });

  it('does NOT delete device_id, server_url, display_name, insecure_tls from store', async () => {
    setupFullStore();
    const auth = await import('../auth');

    await auth.clearSessionFull();

    expect(mockStore['device_id']).toBe('device-123');
    expect(mockStore['server_url']).toBe('https://wavis.example.com');
    expect(mockStore['display_name']).toBe('TestUser');
    expect(mockStore['insecure_tls']).toBe(false);
  });
});

/* ═══ refreshTokens RefreshResult Mapping Tests ═════════════════════
 *
 * Validates: Requirements 2.1, 2.2, 2.3, 2.4, 2.5
 */
describe('refreshTokens RefreshResult mapping', () => {
  beforeEach(() => {
    mockStore = {};
    mockKeychain = {};
    deleteTokenCalls = [];
    mockFetchBehavior = '200';
    mockRetryAfterHeader = null;
    vi.resetModules();
  });

  it('200 → { status: "success" }', async () => {
    mockStore = { server_url: 'https://wavis.example.com' };
    mockKeychain = { wavis_refresh_token: 'valid-token' };
    mockFetchBehavior = '200';

    const auth = await import('../auth');
    const result = await auth.refreshTokens();

    expect(result.status).toBe('success');
  });

  it('401 → { status: "unauthorized" }', async () => {
    mockStore = { server_url: 'https://wavis.example.com' };
    mockKeychain = { wavis_refresh_token: 'valid-token' };
    mockFetchBehavior = '401';

    const auth = await import('../auth');
    const result = await auth.refreshTokens();

    expect(result.status).toBe('unauthorized');
  });

  it('400 → { status: "bad_request" }', async () => {
    mockStore = { server_url: 'https://wavis.example.com' };
    mockKeychain = { wavis_refresh_token: 'valid-token' };
    mockFetchBehavior = '400';

    const auth = await import('../auth');
    const result = await auth.refreshTokens();

    expect(result.status).toBe('bad_request');
  });

  it('429 → { status: "rate_limited" } with retryAfter parsed from Retry-After header', async () => {
    mockStore = { server_url: 'https://wavis.example.com' };
    mockKeychain = { wavis_refresh_token: 'valid-token' };
    mockFetchBehavior = '429';
    mockRetryAfterHeader = '5';

    const auth = await import('../auth');
    const result = await auth.refreshTokens();

    expect(result.status).toBe('rate_limited');
    if (result.status === 'rate_limited') {
      // 5 seconds → 5000ms, clamped to [250, 30000]
      expect(result.retryAfter).toBe(5000);
    }
  });

  it('429 → retryAfter clamped to 250ms minimum', async () => {
    mockStore = { server_url: 'https://wavis.example.com' };
    mockKeychain = { wavis_refresh_token: 'valid-token' };
    mockFetchBehavior = '429';
    mockRetryAfterHeader = '0.1'; // 100ms → clamped to 250ms

    const auth = await import('../auth');
    const result = await auth.refreshTokens();

    expect(result.status).toBe('rate_limited');
    if (result.status === 'rate_limited') {
      expect(result.retryAfter).toBe(250);
    }
  });

  it('429 → retryAfter clamped to 30s maximum', async () => {
    mockStore = { server_url: 'https://wavis.example.com' };
    mockKeychain = { wavis_refresh_token: 'valid-token' };
    mockFetchBehavior = '429';
    mockRetryAfterHeader = '60'; // 60s → clamped to 30000ms

    const auth = await import('../auth');
    const result = await auth.refreshTokens();

    expect(result.status).toBe('rate_limited');
    if (result.status === 'rate_limited') {
      expect(result.retryAfter).toBe(30_000);
    }
  });

  it('500 → { status: "server_error", httpStatus: 500 }', async () => {
    mockStore = { server_url: 'https://wavis.example.com' };
    mockKeychain = { wavis_refresh_token: 'valid-token' };
    mockFetchBehavior = '500';

    const auth = await import('../auth');
    const result = await auth.refreshTokens();

    expect(result.status).toBe('server_error');
    if (result.status === 'server_error') {
      expect(result.httpStatus).toBe(500);
    }
  });

  it('network throw → { status: "network_error", message: ... }', async () => {
    mockStore = { server_url: 'https://wavis.example.com' };
    mockKeychain = { wavis_refresh_token: 'valid-token' };
    mockFetchBehavior = 'network_error';

    const auth = await import('../auth');
    const result = await auth.refreshTokens();

    expect(result.status).toBe('network_error');
    if (result.status === 'network_error') {
      expect(result.message).toContain('Network error');
    }
  });

  it('no server_url → { status: "no_server_url" }', async () => {
    mockStore = {}; // no server_url
    mockKeychain = { wavis_refresh_token: 'valid-token' };

    const auth = await import('../auth');
    const result = await auth.refreshTokens();

    expect(result.status).toBe('no_server_url');
  });

  it('no refresh token → { status: "no_refresh_token" }', async () => {
    mockStore = { server_url: 'https://wavis.example.com' };
    mockKeychain = {}; // no refresh token

    const auth = await import('../auth');
    const result = await auth.refreshTokens();

    expect(result.status).toBe('no_refresh_token');
  });

  it('inflight dedup — multiple concurrent callers get same result', async () => {
    mockStore = { server_url: 'https://wavis.example.com' };
    mockKeychain = { wavis_refresh_token: 'valid-token' };
    mockFetchBehavior = '200';

    const auth = await import('../auth');

    // Call refreshTokens() twice concurrently
    const [result1, result2] = await Promise.all([
      auth.refreshTokens(),
      auth.refreshTokens(),
    ]);

    // Both should get the same result
    expect(result1.status).toBe('success');
    expect(result2.status).toBe('success');
    // Both should be the exact same promise resolution
    expect(result1).toEqual(result2);
  });
});
