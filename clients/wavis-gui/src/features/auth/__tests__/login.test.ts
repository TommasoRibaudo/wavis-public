/**
 * Login Component Logic Tests
 *
 * Tests the Login component's behavior by simulating its logic
 * (vitest env is 'node', no jsdom — same approach as auth-gate.test.ts).
 *
 * The Login component:
 * 1. On mount: check isDeviceRegistered() → if not, navigate to /setup
 * 2. Check getRefreshToken() → determines UI mode (reconnect vs re-register)
 * 3. Reconnect handler: calls refreshTokens(), handles result
 * 4. Re-register handler: calls registerUser(), handles result
 *
 * Validates: Requirements 2.6, 2.7, 2.9
 */

import { describe, it, expect, vi, beforeEach, afterEach } from 'vitest';

/* ─── Mock State ────────────────────────────────────────────────── */

let mockStore: Record<string, unknown> = {};
let mockKeychain: Record<string, string> = {};
let navigateTarget: string | null = null;
let mockFetchBehavior: 'network_error' | '401' | '400' | '429' | '500' | '200' | 'register_ok' | 'register_fail' = '200';

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
          ok: false, status: 401,
          headers: { get: () => null },
          json: async () => ({ error: 'unauthorized' }),
        };
      }
      if (mockFetchBehavior === '200') {
        const futureExp = Date.now() / 1000 + 900;
        return {
          ok: true, status: 200,
          headers: { get: () => null },
          json: async () => ({
            user_id: 'device-123',
            device_id: 'device-123',
            access_token: `h.${btoa(JSON.stringify({ sub: 'device-123', exp: futureExp }))}.s`,
            refresh_token: 'new-refresh-token',
          }),
        };
      }
    }
    if (typeof url === 'string' && url.includes('/auth/register')) {
      if (mockFetchBehavior === 'network_error') {
        throw new Error('Network error: connection refused');
      }
      if (mockFetchBehavior === 'register_ok') {
        const futureExp = Date.now() / 1000 + 900;
        return {
          ok: true, status: 200,
          headers: { get: () => null },
          json: async () => ({
            user_id: 'new-user-456',
            device_id: 'new-device-456',
            recovery_id: 'wvs-ABCD-1234',
            access_token: `h.${btoa(JSON.stringify({ sub: 'new-user-456', exp: futureExp }))}.s`,
            refresh_token: 'fresh-refresh-token',
          }),
        };
      }
      if (mockFetchBehavior === 'register_fail') {
        return {
          ok: false, status: 500,
          headers: { get: () => null },
          json: async () => ({ error: 'internal server error' }),
        };
      }
    }
    throw new Error(`Unexpected fetch URL: ${url}`);
  }),
}));

vi.mock('react-router', () => ({
  useNavigate: () => (target: string) => {
    navigateTarget = target;
  },
}));


/* ─── Helpers ───────────────────────────────────────────────────── */

/** Set up store with a registered device (session expired, has refresh token) */
function setupRegisteredWithRefreshToken(): void {
  mockStore = {
    device_id: 'device-123',
    server_url: 'https://wavis.example.com',
    display_name: 'TestUser',
    insecure_tls: false,
  };
  mockKeychain = {
    wavis_refresh_token: 'old-refresh-token',
  };
}

/** Set up store with a registered device but NO refresh token */
function setupRegisteredNoRefreshToken(): void {
  mockStore = {
    device_id: 'device-123',
    server_url: 'https://wavis.example.com',
    display_name: 'TestUser',
    insecure_tls: false,
  };
  mockKeychain = {};
}

/* ─── Login Logic Simulators ────────────────────────────────────── */

/**
 * Simulate Login component mount behavior:
 * 1. Check isDeviceRegistered() → if not, navigate to /setup
 * 2. Check getRefreshToken() → determines UI mode
 * 3. Get serverUrl for re-register mode
 */
async function simulateLoginMount(): Promise<{
  hasRefreshToken: boolean;
  serverUrl: string | null;
} | null> {
  const auth = await import('../auth');

  const registered = await auth.isDeviceRegistered();
  if (!registered) {
    navigateTarget = '/setup';
    return null; // redirected away
  }

  const rt = await auth.getRefreshToken();
  const url = await auth.getServerUrl();
  return { hasRefreshToken: rt !== null, serverUrl: url };
}

/**
 * Simulate the reconnect handler:
 * - Calls refreshTokens()
 * - On success → navigate to /
 * - On 401/400 → clearSessionFull(), switch to re-register UI
 * - On transient error → show inline error, stay on /login
 */
async function simulateReconnect(): Promise<{
  success: boolean;
  error: string | null;
  switchedToReregister: boolean;
}> {
  const auth = await import('../auth');
  const result = await auth.refreshTokens();

  if (result.status === 'success') {
    navigateTarget = '/';
    return { success: true, error: null, switchedToReregister: false };
  }

  if (result.status === 'unauthorized' || result.status === 'bad_request') {
    await auth.clearSessionFull();
    return {
      success: false,
      error: result.status === 'unauthorized'
        ? 'Session expired — please re-register'
        : 'Session invalid — please re-register',
      switchedToReregister: true,
    };
  }

  // Transient errors: show inline error, stay on /login
  const errorMsg =
    result.status === 'network_error' ? 'Network error — could not reach server'
    : result.status === 'server_error' ? 'Server error — try again later'
    : result.status === 'rate_limited' ? 'Too many requests — try again later'
    : 'Unexpected error';

  return { success: false, error: errorMsg, switchedToReregister: false };
}

/**
 * Simulate the re-register handler:
 * - Calls registerUser() with stored serverUrl
 * - On success → navigate to /
 * - On failure → show inline error, stay on /login
 */
async function simulateReregister(serverUrl: string): Promise<{
  success: boolean;
  error: string | null;
}> {
  const auth = await import('../auth');
  const insecure = await auth.getInsecureTls();
  // TODO(task-22): pass phrase and deviceName from UI inputs
  const result = await auth.registerUser(serverUrl, '', '', insecure, () => {});

  if (result.success) {
    navigateTarget = '/';
    return { success: true, error: null };
  }

  return { success: false, error: result.error ?? 'Registration failed' };
}

/* ═══ Tests ══════════════════════════════════════════════════════════ */

describe('Login Component Logic', () => {
  beforeEach(() => {
    mockStore = {};
    mockKeychain = {};
    navigateTarget = null;
    mockFetchBehavior = '200';
  });

  afterEach(() => {
    vi.restoreAllMocks();
  });

  /* ── Mount behavior ── */

  it('unregistered device → redirects to /setup', async () => {
    // No device_id in store
    mockStore = {};
    mockKeychain = {};

    const result = await simulateLoginMount();

    expect(result).toBeNull();
    expect(navigateTarget).toBe('/setup');
  });

  it('refresh token exists → reports hasRefreshToken=true (shows Reconnect button)', async () => {
    setupRegisteredWithRefreshToken();

    const result = await simulateLoginMount();

    expect(result).not.toBeNull();
    expect(result!.hasRefreshToken).toBe(true);
    expect(navigateTarget).toBeNull(); // no redirect
  });

  it('no refresh token → reports hasRefreshToken=false with stored server_url (shows Re-register button)', async () => {
    setupRegisteredNoRefreshToken();

    const result = await simulateLoginMount();

    expect(result).not.toBeNull();
    expect(result!.hasRefreshToken).toBe(false);
    expect(result!.serverUrl).toBe('https://wavis.example.com');
    expect(navigateTarget).toBeNull();
  });

  /* ── Reconnect handler ── */

  it('reconnect success → navigates to /', async () => {
    setupRegisteredWithRefreshToken();
    mockFetchBehavior = '200';

    const result = await simulateReconnect();

    expect(result.success).toBe(true);
    expect(navigateTarget).toBe('/');
  });

  it('reconnect failure (401) → calls clearSessionFull(), switches to re-register UI', async () => {
    setupRegisteredWithRefreshToken();
    mockFetchBehavior = '401';

    const result = await simulateReconnect();

    expect(result.success).toBe(false);
    expect(result.switchedToReregister).toBe(true);
    expect(result.error).toContain('re-register');
    // clearSessionFull deletes refresh token from keychain
    expect(mockKeychain['wavis_refresh_token']).toBeUndefined();
    // Device identity preserved
    expect(mockStore['device_id']).toBe('device-123');
    expect(mockStore['server_url']).toBe('https://wavis.example.com');
    // No navigation — stays on /login
    expect(navigateTarget).toBeNull();
  });

  it('reconnect failure (network_error) → shows inline error, Reconnect button remains', async () => {
    setupRegisteredWithRefreshToken();
    mockFetchBehavior = 'network_error';

    const result = await simulateReconnect();

    expect(result.success).toBe(false);
    expect(result.switchedToReregister).toBe(false);
    expect(result.error).toContain('Network error');
    // Refresh token preserved (transient failure)
    expect(mockKeychain['wavis_refresh_token']).toBe('old-refresh-token');
    // No navigation — stays on /login
    expect(navigateTarget).toBeNull();
  });

  /* ── Re-register handler ── */

  it('re-register success → navigates to /', async () => {
    setupRegisteredNoRefreshToken();
    mockFetchBehavior = 'register_ok';

    const result = await simulateReregister('https://wavis.example.com');

    expect(result.success).toBe(true);
    expect(navigateTarget).toBe('/');
  });

  it('re-register failure → shows inline error, stays on /login', async () => {
    setupRegisteredNoRefreshToken();
    mockFetchBehavior = 'register_fail';

    const result = await simulateReregister('https://wavis.example.com');

    expect(result.success).toBe(false);
    expect(result.error).toBeDefined();
    expect(navigateTarget).toBeNull(); // stays on /login
  });
});
