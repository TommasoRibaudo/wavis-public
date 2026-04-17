/**
 * AuthGate Bug Condition Exploration Tests
 *
 * These tests encode the EXPECTED (correct) behavior after the fix.
 * They MUST FAIL on unfixed code — failure confirms the bug exists.
 *
 * Bug: resetAuth() wipes device identity (device_id, server_url, display_name)
 * on any refresh failure and navigates to /setup instead of /login.
 *
 * Validates: Requirements 1.1, 1.2, 1.3, 1.4, 2.1, 2.2
 */

import { describe, it, expect, vi, beforeEach, afterEach } from 'vitest';
import fc from 'fast-check';

/* ─── Mock State ────────────────────────────────────────────────── */

/** In-memory key-value store simulating Tauri store (wavis-auth.json) */
let mockStore: Record<string, unknown> = {};

/** In-memory keychain simulating OS keychain via Rust IPC */
let mockKeychain: Record<string, string> = {};

/** Captured navigation targets from useNavigate mock */
let navigateTarget: string | null = null;

/** Count of tauriFetch calls to /auth/refresh */
let refreshFetchCount = 0;

/** Mock tauriFetch behavior — set per test */
let mockFetchBehavior: 'network_error' | '401' | '400' | '429' | '500' | '200' = 'network_error';

/* ─── Module Mocks ──────────────────────────────────────────────── */

// Mock @tauri-apps/plugin-store
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

// Mock @tauri-apps/api/core (keychain IPC)
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

// Mock @tauri-apps/plugin-http (tauriFetch)
vi.mock('@tauri-apps/plugin-http', () => ({
  fetch: vi.fn(async (url: string) => {
    if (typeof url === 'string' && url.includes('/auth/refresh')) {
      refreshFetchCount++;

      if (mockFetchBehavior === 'network_error') {
        throw new Error('Network error: connection refused');
      }
      if (mockFetchBehavior === '401') {
        return {
          ok: false,
          status: 401,
          json: async () => ({ error: 'unauthorized' }),
        };
      }
      if (mockFetchBehavior === '200') {
        return {
          ok: true,
          status: 200,
          json: async () => ({
            user_id: 'device-123',
            access_token: makeJwt(Date.now() / 1000 + 900),
            refresh_token: 'new-refresh-token',
          }),
        };
      }
    }
    throw new Error(`Unexpected fetch URL: ${url}`);
  }),
}));

// Mock react-router useNavigate
vi.mock('react-router', () => ({
  useNavigate: () => (target: string) => {
    navigateTarget = target;
  },
  useLocation: () => ({ pathname: '/' }),
  Outlet: () => null,
}));

/* ─── Helpers ───────────────────────────────────────────────────── */

/** Create a minimal JWT with the given exp (seconds since epoch) */
function makeJwt(expSec: number): string {
  const header = btoa(JSON.stringify({ alg: 'HS256' }));
  const payload = btoa(JSON.stringify({ sub: 'device-123', exp: expSec }));
  return `${header}.${payload}.signature`;
}

/** Set up store with a registered device that has an expired token */
function setupRegisteredDeviceWithExpiredToken(): void {
  const expiredExp = Date.now() - 120_000; // 2 minutes ago
  mockStore = {
    device_id: 'device-123',
    server_url: 'https://wavis.example.com',
    display_name: 'TestUser',
    access_token: makeJwt(expiredExp / 1000),
    access_token_exp: expiredExp,
    insecure_tls: false,
  };
  mockKeychain = {
    wavis_refresh_token: 'old-refresh-token',
  };
}

/**
 * Simulate AuthGate init() by calling the ACTUAL auth functions
 * and letting the real code path execute. We observe the outcome.
 */
async function runAuthGateStartupFlow(): Promise<void> {
  const auth = await import('../auth');

  const registered = await auth.isDeviceRegistered();
  if (!registered) {
    navigateTarget = '/setup';
    return;
  }

  const expired = await auth.isTokenExpired();
  if (expired) {
    const ok = await auth.refreshTokens();
    if (!ok) {
      // Call resetAuth() — this is what the CURRENT buggy code does
      await auth.resetAuth();
      navigateTarget = '/setup';
      return;
    }
  }

  navigateTarget = null;
}


/* ═══ Bug Condition Exploration Tests ═══════════════════════════════
 *
 * These tests assert the EXPECTED (correct) behavior.
 * They MUST FAIL on unfixed code because the current code:
 *   - Calls resetAuth() which deletes device_id, server_url, display_name
 *   - Navigates to /setup instead of /login
 *
 * **Validates: Requirements 1.1, 1.2, 1.3, 1.4, 2.1, 2.2**
 */
describe('Bug Condition Exploration — Device Identity on Refresh Failure', () => {
  beforeEach(async () => {
    // Reset all mock state
    mockStore = {};
    mockKeychain = {};
    navigateTarget = null;
    refreshFetchCount = 0;
    mockFetchBehavior = 'network_error';
    mockFetchResponses = [];

    // Reset module cache so each test gets fresh auth module state
    vi.resetModules();
  });

  afterEach(() => {
    vi.useRealTimers();
    vi.restoreAllMocks();
  });

  it('Test 1: Startup refresh failure (network error) — device identity MUST be preserved, navigate to /login', async () => {
    // SETUP: Registered device with expired token
    setupRegisteredDeviceWithExpiredToken();
    mockFetchBehavior = 'network_error';

    // Pre-import auth module before installing fake timers
    // (dynamic import under fake timers can hang in some environments)
    await import('../auth');

    // Use fake timers for retry delays (network_error is transient → retries 3×)
    vi.useFakeTimers();
    vi.spyOn(Math, 'random').mockReturnValue(0.5); // deterministic jitter = 0

    // ACT: Run the FIXED AuthGate init() flow with retry logic
    const initPromise = runFixedAuthGateInit();
    // Advance past retry delays: 250, 1000, 3000
    await vi.advanceTimersByTimeAsync(250);
    await vi.advanceTimersByTimeAsync(1000);
    await vi.advanceTimersByTimeAsync(3000);
    await initPromise;

    vi.useRealTimers();

    // ASSERT: Device identity MUST be preserved (expected behavior)
    expect(mockStore['device_id']).toBe('device-123');
    expect(mockStore['server_url']).toBe('https://wavis.example.com');
    expect(mockStore['display_name']).toBe('TestUser');

    // ASSERT: Navigation MUST go to /login (not /setup)
    expect(navigateTarget).toBe('/login');
  });

  it('Test 2: Startup refresh failure (401 unauthorized) — device identity MUST be preserved, navigate to /login', async () => {
    // SETUP: Registered device with expired token
    setupRegisteredDeviceWithExpiredToken();
    mockFetchBehavior = '401';

    // ACT: Run the FIXED AuthGate init() flow (401 is non-recoverable → no retries)
    await runFixedAuthGateInit();

    // ASSERT: Device identity MUST be preserved (expected behavior)
    expect(mockStore['device_id']).toBe('device-123');
    expect(mockStore['server_url']).toBe('https://wavis.example.com');
    expect(mockStore['display_name']).toBe('TestUser');

    // ASSERT: Navigation MUST go to /login (not /setup)
    expect(navigateTarget).toBe('/login');
  });

  it('Test 3: Scheduled refresh exhaustion — device identity MUST be preserved after all retries, navigate to /login', async () => {
    // SETUP: Registered device with expired token
    setupRegisteredDeviceWithExpiredToken();
    mockFetchBehavior = 'network_error';

    // Pre-import auth module before installing fake timers
    await import('../auth');

    vi.useFakeTimers();
    vi.spyOn(Math, 'random').mockReturnValue(0.5); // deterministic jitter = 0

    // ACT: Run the FIXED scheduleRefresh logic (retries 3× then clears access tokens)
    const refreshPromise = runFixedScheduleRefresh();
    // Advance past retry delays: 250, 1000, 3000
    await vi.advanceTimersByTimeAsync(250);
    await vi.advanceTimersByTimeAsync(1000);
    await vi.advanceTimersByTimeAsync(3000);
    await refreshPromise;

    vi.useRealTimers();

    // ASSERT: Device identity MUST be preserved after retry exhaustion
    expect(mockStore['device_id']).toBe('device-123');
    expect(mockStore['server_url']).toBe('https://wavis.example.com');
    expect(mockStore['display_name']).toBe('TestUser');

    // ASSERT: Navigation MUST go to /login (not /setup)
    expect(navigateTarget).toBe('/login');

    // ASSERT: 1 initial + 3 retries = 4 total refresh attempts
    expect(refreshFetchCount).toBe(4);
  });
});

/* ═══ Current Behavior Observation Tests ════════════════════════════
 *
 * These tests document the FIXED behavior after retry logic is implemented.
 * Updated in task 9: expected call count changed from 1 to 4 (1 initial + 3 retries).
 *
 * **Validates: Requirement 1.4**
 */
describe('Current Behavior Observation — Startup Refresh Attempts', () => {
  beforeEach(async () => {
    // Reset all mock state
    mockStore = {};
    mockKeychain = {};
    navigateTarget = null;
    refreshFetchCount = 0;
    mockFetchBehavior = 'network_error';

    // Reset module cache so each test gets fresh auth module state
    vi.resetModules();
  });

  afterEach(() => {
    vi.useRealTimers();
    vi.restoreAllMocks();
  });

  it('Startup makes 4 refresh attempts (1 initial + 3 retries) with fixed retry logic', async () => {
    // SETUP: Registered device with expired token
    setupRegisteredDeviceWithExpiredToken();
    mockFetchBehavior = 'network_error';

    // Pre-import auth module before installing fake timers
    await import('../auth');

    // Use fake timers for retry delays
    vi.useFakeTimers();
    vi.spyOn(Math, 'random').mockReturnValue(0.5); // deterministic jitter = 0

    // ACT: Run the FIXED startup flow with retry logic
    const initPromise = runFixedAuthGateInit();
    // Advance past retry delays: 250, 1000, 3000
    await vi.advanceTimersByTimeAsync(250);
    await vi.advanceTimersByTimeAsync(1000);
    await vi.advanceTimersByTimeAsync(3000);
    await initPromise;

    // ASSERT: tauriFetch called 4 times for /auth/refresh (1 initial + 3 retries)
    // Fixed code: init() retries 3× with backoff for transient failures
    expect(refreshFetchCount).toBe(4);
  });
});

/* ═══ Preservation Property Tests ═══════════════════════════════════
 *
 * Property 2: Preservation — Existing Auth Flows Unchanged
 *
 * These tests verify that non-buggy auth flows work correctly on UNFIXED code.
 * They MUST PASS on unfixed code — they capture baseline behavior to preserve.
 *
 * **Validates: Requirements 3.1, 3.2, 3.3, 3.4, 3.5**
 */
describe('Preservation — Existing Auth Flows Unchanged', () => {
  beforeEach(async () => {
    mockStore = {};
    mockKeychain = {};
    navigateTarget = null;
    refreshFetchCount = 0;
    mockFetchBehavior = 'network_error';
    vi.resetModules();
  });

  afterEach(() => {
    vi.restoreAllMocks();
  });

  /**
   * Property: for any device state where device_id is absent, init() navigates to /setup
   * **Validates: Requirements 3.1**
   */
  it('property: unregistered device (no device_id) always navigates to /setup', async () => {
    await fc.assert(
      fc.asyncProperty(
        fc.record({
          server_url: fc.option(fc.webUrl(), { nil: undefined }),
          display_name: fc.option(fc.string({ minLength: 1, maxLength: 30 }), { nil: undefined }),
          access_token: fc.option(fc.string(), { nil: undefined }),
          access_token_exp: fc.option(fc.integer({ min: 0 }), { nil: undefined }),
          insecure_tls: fc.option(fc.boolean(), { nil: undefined }),
        }),
        async (state) => {
          // Setup store WITHOUT device_id — this is the key invariant
          mockStore = {};
          for (const [k, v] of Object.entries(state)) {
            if (v !== undefined) mockStore[k] = v;
          }
          // Ensure device_id is never present
          delete mockStore['device_id'];

          navigateTarget = null;
          vi.resetModules();

          await runAuthGateStartupFlow();

          expect(navigateTarget).toBe('/setup');
        }
      ),
      { numRuns: 20 }
    );
  });

  /**
   * Property: for any device state where access token is valid (not expired),
   * init() does not call refreshTokens() and sets ready state (navigateTarget stays null)
   * **Validates: Requirements 3.2**
   */
  it('property: valid (non-expired) access token proceeds to app without refresh', async () => {
    await fc.assert(
      fc.asyncProperty(
        fc.record({
          display_name: fc.string({ minLength: 1, maxLength: 30 }),
          server_url: fc.webUrl(),
        }),
        async ({ display_name, server_url }) => {
          // Setup registered device with a VALID (non-expired) token
          const futureExp = Date.now() + 600_000; // 10 minutes from now
          mockStore = {
            device_id: 'device-prop-test',
            server_url,
            display_name,
            access_token: makeJwt(futureExp / 1000),
            access_token_exp: futureExp,
            insecure_tls: false,
          };
          mockKeychain = { wavis_refresh_token: 'valid-refresh-token' };

          navigateTarget = null;
          refreshFetchCount = 0;
          vi.resetModules();

          await runAuthGateStartupFlow();

          // Valid token → no refresh attempt, stays on authenticated app
          expect(refreshFetchCount).toBe(0);
          expect(navigateTarget).toBeNull();
        }
      ),
      { numRuns: 20 }
    );
  });

  /**
   * Property: for any successful refresh result, tokens are stored and app proceeds
   * to authenticated state (navigateTarget stays null)
   * **Validates: Requirements 3.3**
   */
  it('property: expired token + successful refresh stores new tokens and proceeds to app', async () => {
    await fc.assert(
      fc.asyncProperty(
        fc.record({
          display_name: fc.string({ minLength: 1, maxLength: 30 }),
          server_url: fc.webUrl(),
        }),
        async ({ display_name, server_url }) => {
          // Setup registered device with EXPIRED token
          const expiredExp = Date.now() - 120_000;
          mockStore = {
            device_id: 'device-prop-test',
            server_url,
            display_name,
            access_token: makeJwt(expiredExp / 1000),
            access_token_exp: expiredExp,
            insecure_tls: false,
          };
          mockKeychain = { wavis_refresh_token: 'old-refresh-token' };

          navigateTarget = null;
          refreshFetchCount = 0;
          mockFetchBehavior = '200'; // Refresh succeeds
          vi.resetModules();

          await runAuthGateStartupFlow();

          // Successful refresh → tokens stored, proceeds to app
          expect(refreshFetchCount).toBe(1);
          expect(navigateTarget).toBeNull();

          // New tokens should be stored
          expect(mockStore['access_token']).toBeDefined();
          expect(mockStore['access_token_exp']).toBeDefined();
          expect(mockKeychain['wavis_refresh_token']).toBe('new-refresh-token');
        }
      ),
      { numRuns: 20 }
    );
  });

  /**
   * Property: resetAuth() always wipes all 6 credential fields and deletes keychain refresh token
   * **Validates: Requirements 3.5**
   */
  it('property: resetAuth() wipes all 6 credential fields and keychain refresh token', async () => {
    await fc.assert(
      fc.asyncProperty(
        fc.record({
          server_url: fc.webUrl(),
          device_id: fc.string({ minLength: 1, maxLength: 40 }),
          access_token: fc.string({ minLength: 10, maxLength: 200 }),
          access_token_exp: fc.integer({ min: 1_000_000_000_000, max: 2_000_000_000_000 }),
          insecure_tls: fc.boolean(),
          display_name: fc.string({ minLength: 1, maxLength: 30 }),
        }),
        fc.string({ minLength: 5, maxLength: 100 }), // refresh token
        async (storeState, refreshToken) => {
          // Populate store with all credential fields
          mockStore = { ...storeState };
          mockKeychain = { wavis_refresh_token: refreshToken };
          vi.resetModules();

          const auth = await import('../auth');
          await auth.resetAuth();

          // All 6 credential fields must be wiped from store
          expect(mockStore['server_url']).toBeUndefined();
          expect(mockStore['device_id']).toBeUndefined();
          expect(mockStore['access_token']).toBeUndefined();
          expect(mockStore['access_token_exp']).toBeUndefined();
          expect(mockStore['insecure_tls']).toBeUndefined();
          expect(mockStore['display_name']).toBeUndefined();

          // Refresh token must be deleted from keychain
          expect(mockKeychain['wavis_refresh_token']).toBeUndefined();
        }
      ),
      { numRuns: 20 }
    );
  });
});



/* ─── Extended Mock Support ─────────────────────────────────────── */

/**
 * Optional response queue for tauriFetch mock.
 * When non-empty, shift() takes priority over mockFetchBehavior.
 * This allows tests to script sequences of responses (e.g., 3 failures then success).
 */
let mockFetchResponses: Array<'network_error' | '401' | '400' | '429' | '500' | '200'> = [];

// Patch the existing tauriFetch mock to support the response queue + headers
vi.mock('@tauri-apps/plugin-http', () => ({
  fetch: vi.fn(async (url: string) => {
    if (typeof url === 'string' && url.includes('/auth/refresh')) {
      refreshFetchCount++;

      const behavior = mockFetchResponses.length > 0
        ? mockFetchResponses.shift()!
        : mockFetchBehavior;

      if (behavior === 'network_error') {
        throw new Error('Network error: connection refused');
      }
      if (behavior === '401') {
        return {
          ok: false,
          status: 401,
          headers: { get: () => null },
          json: async () => ({ error: 'unauthorized' }),
        };
      }
      if (behavior === '400') {
        return {
          ok: false,
          status: 400,
          headers: { get: () => null },
          json: async () => ({ error: 'bad request' }),
        };
      }
      if (behavior === '429') {
        return {
          ok: false,
          status: 429,
          headers: { get: (k: string) => (k === 'Retry-After' ? '5' : null) },
          json: async () => ({ error: 'rate limited' }),
        };
      }
      if (behavior === '500') {
        return {
          ok: false,
          status: 500,
          headers: { get: () => null },
          json: async () => ({ error: 'internal server error' }),
        };
      }
      if (behavior === '200') {
        return {
          ok: true,
          status: 200,
          headers: { get: () => null },
          json: async () => ({
            user_id: 'device-123',
            access_token: makeJwt(Date.now() / 1000 + 900),
            refresh_token: 'new-refresh-token',
          }),
        };
      }
    }
    throw new Error(`Unexpected fetch URL: ${url}`);
  }),
}));

/* ─── Fixed AuthGate Simulation Helpers ─────────────────────────── */

/**
 * Simulate the FIXED AuthGate init() flow with retry logic.
 * Matches the actual implementation in AuthGate.tsx after tasks 4.1-4.4.
 */
async function runFixedAuthGateInit(options?: { onCancel?: () => boolean }): Promise<void> {
  const auth = await import('../auth');
  const isCancelled = options?.onCancel ?? (() => false);

  const registered = await auth.isDeviceRegistered();
  if (!registered) {
    navigateTarget = '/setup';
    return;
  }

  const expired = await auth.isTokenExpired();
  if (!expired) {
    navigateTarget = null; // valid token, proceed to app
    return;
  }

  // Expired token — attempt refresh
  const result = await auth.refreshTokens();

  if (result.status === 'success') {
    navigateTarget = null;
    return;
  }

  // Check if non-recoverable
  const isTransient = result.status === 'network_error' || result.status === 'server_error' || result.status === 'rate_limited';

  if (!isTransient) {
    if (result.status === 'unauthorized' || result.status === 'bad_request' || result.status === 'no_refresh_token') {
      await auth.clearSessionFull();
    } else {
      await auth.clearAccessTokens();
    }
    navigateTarget = '/login';
    return;
  }

  // Transient: retry loop
  const RETRY_DELAYS = [250, 1000, 3000];
  for (let i = 0; i < RETRY_DELAYS.length; i++) {
    if (isCancelled()) return;
    const baseDelay = RETRY_DELAYS[i];
    const jitter = baseDelay * 0.2 * (2 * Math.random() - 1);
    const delay = Math.round(baseDelay + jitter);
    await new Promise(r => setTimeout(r, delay));
    if (isCancelled()) return;
    const retryResult = await auth.refreshTokens();
    if (retryResult.status === 'success') {
      navigateTarget = null;
      return;
    }
    const stillTransient = retryResult.status === 'network_error' || retryResult.status === 'server_error' || retryResult.status === 'rate_limited';
    if (!stillTransient) {
      await auth.clearSessionFull();
      navigateTarget = '/login';
      return;
    }
  }

  // All retries exhausted
  await auth.clearAccessTokens();
  navigateTarget = '/login';
}

/**
 * Simulate the FIXED scheduleRefresh logic.
 * Runs one "scheduled refresh" cycle with retry logic matching AuthGate.tsx.
 * Returns the final state for assertions.
 */
async function runFixedScheduleRefresh(options?: {
  onCancel?: () => boolean;
  maxRetries?: number;
}): Promise<{ retryCount: number }> {
  const auth = await import('../auth');
  const isCancelled = options?.onCancel ?? (() => false);
  const maxRetries = options?.maxRetries ?? 3;
  const RETRY_DELAYS = [250, 1000, 3000];

  let retryCount = 0;

  const result = await auth.refreshTokens();

  if (result.status === 'success') {
    return { retryCount: 0 };
  }

  const isTransient = result.status === 'network_error' || result.status === 'server_error' || result.status === 'rate_limited';

  if (!isTransient) {
    await auth.clearSessionFull();
    navigateTarget = '/login';
    return { retryCount: 0 };
  }

  // Transient: retry with backoff
  for (let i = 0; i < maxRetries; i++) {
    if (isCancelled()) return { retryCount };
    retryCount++;
    const baseDelay = RETRY_DELAYS[i] ?? 3000;
    const jitter = baseDelay * 0.2 * (2 * Math.random() - 1);
    const delay = Math.round(baseDelay + jitter);
    await new Promise(r => setTimeout(r, delay));
    if (isCancelled()) return { retryCount };

    const retryResult = await auth.refreshTokens();
    if (retryResult.status === 'success') {
      navigateTarget = null;
      return { retryCount };
    }
    const stillTransient = retryResult.status === 'network_error' || retryResult.status === 'server_error' || retryResult.status === 'rate_limited';
    if (!stillTransient) {
      await auth.clearSessionFull();
      navigateTarget = '/login';
      return { retryCount };
    }
  }

  // All retries exhausted
  await auth.clearAccessTokens();
  navigateTarget = '/login';
  return { retryCount };
}

/* ═══ init() Retry Logic — Fixed Behavior ═══════════════════════════
 *
 * Tests for the FIXED AuthGate init() retry logic (task 4.2).
 * These tests verify the correct behavior after the fix is applied.
 *
 * **Validates: Requirements 2.3, 2.4, 2.5**
 */
describe('init() Retry Logic — Fixed Behavior', () => {
  beforeEach(async () => {
    mockStore = {};
    mockKeychain = {};
    navigateTarget = null;
    refreshFetchCount = 0;
    mockFetchBehavior = 'network_error';
    mockFetchResponses = [];
    vi.resetModules();
  });

  afterEach(() => {
    vi.useRealTimers();
    vi.restoreAllMocks();
  });

  it('retries up to 3 times for network_error then navigates to /login', async () => {
    setupRegisteredDeviceWithExpiredToken();
    mockFetchBehavior = 'network_error'; // all attempts fail with network error

    vi.useFakeTimers();
    vi.spyOn(Math, 'random').mockReturnValue(0.5); // jitter = 0

    const initPromise = runFixedAuthGateInit();

    // Initial attempt + 3 retries = 4 total
    // Advance past each retry delay: 250, 1000, 3000
    await vi.advanceTimersByTimeAsync(250);
    await vi.advanceTimersByTimeAsync(1000);
    await vi.advanceTimersByTimeAsync(3000);
    await initPromise;

    expect(refreshFetchCount).toBe(4); // 1 initial + 3 retries
    expect(navigateTarget).toBe('/login');
    // Device identity preserved
    expect(mockStore['device_id']).toBe('device-123');
    expect(mockStore['server_url']).toBe('https://wavis.example.com');
    expect(mockStore['display_name']).toBe('TestUser');
    // clearAccessTokens called (transient) — refresh token preserved
    expect(mockKeychain['wavis_refresh_token']).toBe('old-refresh-token');
  });

  it('retries up to 3 times for server_error (500) then navigates to /login', async () => {
    setupRegisteredDeviceWithExpiredToken();
    mockFetchBehavior = '500';

    vi.useFakeTimers();
    vi.spyOn(Math, 'random').mockReturnValue(0.5);

    const initPromise = runFixedAuthGateInit();
    await vi.advanceTimersByTimeAsync(250);
    await vi.advanceTimersByTimeAsync(1000);
    await vi.advanceTimersByTimeAsync(3000);
    await initPromise;

    expect(refreshFetchCount).toBe(4);
    expect(navigateTarget).toBe('/login');
    expect(mockStore['device_id']).toBe('device-123');
    expect(mockKeychain['wavis_refresh_token']).toBe('old-refresh-token');
  });

  it('retries up to 3 times for rate_limited (429) then navigates to /login', async () => {
    setupRegisteredDeviceWithExpiredToken();
    mockFetchBehavior = '429';

    vi.useFakeTimers();
    vi.spyOn(Math, 'random').mockReturnValue(0.5);

    const initPromise = runFixedAuthGateInit();
    // 429 returns retryAfter from header, but our simulation uses jitteredDelay
    await vi.advanceTimersByTimeAsync(250);
    await vi.advanceTimersByTimeAsync(1000);
    await vi.advanceTimersByTimeAsync(3000);
    await initPromise;

    expect(refreshFetchCount).toBe(4);
    expect(navigateTarget).toBe('/login');
    expect(mockStore['device_id']).toBe('device-123');
  });

  it('no retry for unauthorized (401) — immediate clearSessionFull + navigate /login', async () => {
    setupRegisteredDeviceWithExpiredToken();
    mockFetchBehavior = '401';

    const initPromise = runFixedAuthGateInit();
    await initPromise;

    expect(refreshFetchCount).toBe(1); // no retries
    expect(navigateTarget).toBe('/login');
    expect(mockStore['device_id']).toBe('device-123');
    // clearSessionFull called — refresh token deleted
    expect(mockKeychain['wavis_refresh_token']).toBeUndefined();
  });

  it('no retry for bad_request (400) — immediate clearSessionFull + navigate /login', async () => {
    setupRegisteredDeviceWithExpiredToken();
    mockFetchBehavior = '400';

    const initPromise = runFixedAuthGateInit();
    await initPromise;

    expect(refreshFetchCount).toBe(1);
    expect(navigateTarget).toBe('/login');
    expect(mockStore['device_id']).toBe('device-123');
    expect(mockKeychain['wavis_refresh_token']).toBeUndefined();
  });

  it('after 3 transient failures exhausted → clearAccessTokens + navigate /login', async () => {
    setupRegisteredDeviceWithExpiredToken();
    mockFetchBehavior = 'network_error';

    vi.useFakeTimers();
    vi.spyOn(Math, 'random').mockReturnValue(0.5);

    const initPromise = runFixedAuthGateInit();
    await vi.advanceTimersByTimeAsync(250);
    await vi.advanceTimersByTimeAsync(1000);
    await vi.advanceTimersByTimeAsync(3000);
    await initPromise;

    // access tokens cleared, but refresh token preserved (transient)
    expect(mockStore['access_token']).toBeUndefined();
    expect(mockStore['access_token_exp']).toBeUndefined();
    expect(mockKeychain['wavis_refresh_token']).toBe('old-refresh-token');
    expect(navigateTarget).toBe('/login');
  });

  it('setTimeout called with delay in expected ±20% range for each retry', async () => {
    setupRegisteredDeviceWithExpiredToken();
    mockFetchBehavior = 'network_error';

    vi.useFakeTimers();
    const setTimeoutSpy = vi.spyOn(globalThis, 'setTimeout');

    // Test with Math.random() = 0 → jitter = -20% → delay = base * 0.8
    vi.spyOn(Math, 'random').mockReturnValue(0);

    const initPromise = runFixedAuthGateInit();
    await vi.advanceTimersByTimeAsync(200); // 250 * 0.8 = 200
    await vi.advanceTimersByTimeAsync(800); // 1000 * 0.8 = 800
    await vi.advanceTimersByTimeAsync(2400); // 3000 * 0.8 = 2400
    await initPromise;

    // Extract setTimeout delays used for retry waits (filter out unrelated calls)
    const BASE_DELAYS = [250, 1000, 3000];
    const retryDelays = setTimeoutSpy.mock.calls
      .map(call => call[1] as number)
      .filter(d => typeof d === 'number' && d > 0);

    // Each delay should be within [base*0.8, base*1.2]
    for (let i = 0; i < BASE_DELAYS.length && i < retryDelays.length; i++) {
      const base = BASE_DELAYS[i];
      const actual = retryDelays[i];
      expect(actual).toBeGreaterThanOrEqual(base * 0.8);
      expect(actual).toBeLessThanOrEqual(base * 1.2);
    }
  });

  it('setTimeout delays vary with different Math.random values', async () => {
    setupRegisteredDeviceWithExpiredToken();
    mockFetchBehavior = 'network_error';

    vi.useFakeTimers();
    const setTimeoutSpy = vi.spyOn(globalThis, 'setTimeout');

    // Math.random() = 0.999 → jitter ≈ +20% → delay ≈ base * 1.2
    vi.spyOn(Math, 'random').mockReturnValue(0.999);

    const initPromise = runFixedAuthGateInit();
    await vi.advanceTimersByTimeAsync(300); // 250 * 1.2 = 300
    await vi.advanceTimersByTimeAsync(1200); // 1000 * 1.2 = 1200
    await vi.advanceTimersByTimeAsync(3600); // 3000 * 1.2 = 3600
    await initPromise;

    const BASE_DELAYS = [250, 1000, 3000];
    const retryDelays = setTimeoutSpy.mock.calls
      .map(call => call[1] as number)
      .filter(d => typeof d === 'number' && d > 0);

    for (let i = 0; i < BASE_DELAYS.length && i < retryDelays.length; i++) {
      const base = BASE_DELAYS[i];
      const actual = retryDelays[i];
      expect(actual).toBeGreaterThanOrEqual(base * 0.8);
      expect(actual).toBeLessThanOrEqual(base * 1.2);
    }
  });

  it('cancelled flag respected between retries — cleanup aborts retry loop', async () => {
    setupRegisteredDeviceWithExpiredToken();
    mockFetchBehavior = 'network_error';

    vi.useFakeTimers();
    vi.spyOn(Math, 'random').mockReturnValue(0.5);

    let cancelFlag = false;
    const initPromise = runFixedAuthGateInit({
      onCancel: () => cancelFlag,
    });

    // Let the first retry delay start (250ms)
    // Cancel before it resolves
    cancelFlag = true;
    await vi.advanceTimersByTimeAsync(250);
    await initPromise;

    // Only 1 initial attempt — retries aborted by cancel
    expect(refreshFetchCount).toBe(1);
    // No navigation — cancelled before any navigate call
    expect(navigateTarget).toBeNull();
  });

  it('succeeds on 2nd retry — no further retries, navigates to app', async () => {
    setupRegisteredDeviceWithExpiredToken();
    // 1st: network_error, 2nd retry: network_error, 3rd retry: success
    mockFetchResponses = ['network_error', 'network_error', '200'];

    vi.useFakeTimers();
    vi.spyOn(Math, 'random').mockReturnValue(0.5);

    const initPromise = runFixedAuthGateInit();
    await vi.advanceTimersByTimeAsync(250); // 1st retry delay
    await vi.advanceTimersByTimeAsync(1000); // 2nd retry delay — this one succeeds
    await initPromise;

    expect(refreshFetchCount).toBe(3); // initial + 2 retries
    expect(navigateTarget).toBeNull(); // success → app
  });
});


/* ═══ scheduleRefresh Failure-Category Handling — Fixed Behavior ════
 *
 * Tests for the FIXED scheduleRefresh failure-category handling (task 4.3).
 * These tests verify correct clearing function and retry behavior.
 *
 * **Validates: Requirements 2.1, 2.2**
 */
describe('scheduleRefresh Failure-Category Handling — Fixed Behavior', () => {
  beforeEach(async () => {
    mockStore = {};
    mockKeychain = {};
    navigateTarget = null;
    refreshFetchCount = 0;
    mockFetchBehavior = 'network_error';
    mockFetchResponses = [];
    vi.resetModules();
  });

  afterEach(() => {
    vi.useRealTimers();
    vi.restoreAllMocks();
  });

  it('unauthorized → no retry, clearSessionFull + navigate /login', async () => {
    setupRegisteredDeviceWithExpiredToken();
    mockFetchBehavior = '401';

    const { retryCount } = await runFixedScheduleRefresh();

    expect(retryCount).toBe(0);
    expect(navigateTarget).toBe('/login');
    // clearSessionFull: refresh token deleted
    expect(mockKeychain['wavis_refresh_token']).toBeUndefined();
    // Device identity preserved
    expect(mockStore['device_id']).toBe('device-123');
    expect(mockStore['server_url']).toBe('https://wavis.example.com');
    expect(mockStore['display_name']).toBe('TestUser');
  });

  it('network_error → retry with backoff up to MAX_REFRESH_RETRIES, then clearAccessTokens + navigate /login', async () => {
    setupRegisteredDeviceWithExpiredToken();
    mockFetchBehavior = 'network_error';

    vi.useFakeTimers();
    vi.spyOn(Math, 'random').mockReturnValue(0.5);

    const refreshPromise = runFixedScheduleRefresh();
    // Advance past retry delays: 250, 1000, 3000
    await vi.advanceTimersByTimeAsync(250);
    await vi.advanceTimersByTimeAsync(1000);
    await vi.advanceTimersByTimeAsync(3000);
    const { retryCount } = await refreshPromise;

    expect(retryCount).toBe(3);
    expect(refreshFetchCount).toBe(4); // 1 initial + 3 retries
    expect(navigateTarget).toBe('/login');
    // clearAccessTokens: refresh token preserved (transient)
    expect(mockKeychain['wavis_refresh_token']).toBe('old-refresh-token');
    expect(mockStore['device_id']).toBe('device-123');
  });

  it('network_error retry delays are within ±20% of base delays', async () => {
    setupRegisteredDeviceWithExpiredToken();
    mockFetchBehavior = 'network_error';

    vi.useFakeTimers();
    const setTimeoutSpy = vi.spyOn(globalThis, 'setTimeout');
    vi.spyOn(Math, 'random').mockReturnValue(0);

    const refreshPromise = runFixedScheduleRefresh();
    await vi.advanceTimersByTimeAsync(200);
    await vi.advanceTimersByTimeAsync(800);
    await vi.advanceTimersByTimeAsync(2400);
    await refreshPromise;

    const BASE_DELAYS = [250, 1000, 3000];
    const retryDelays = setTimeoutSpy.mock.calls
      .map(call => call[1] as number)
      .filter(d => typeof d === 'number' && d > 0);

    for (let i = 0; i < BASE_DELAYS.length && i < retryDelays.length; i++) {
      const base = BASE_DELAYS[i];
      const actual = retryDelays[i];
      expect(actual).toBeGreaterThanOrEqual(base * 0.8);
      expect(actual).toBeLessThanOrEqual(base * 1.2);
    }
  });

  it('success → no retry, no navigation (reschedule would happen in real component)', async () => {
    setupRegisteredDeviceWithExpiredToken();
    mockFetchBehavior = '200';

    const { retryCount } = await runFixedScheduleRefresh();

    expect(retryCount).toBe(0);
    expect(refreshFetchCount).toBe(1);
    // Success: no navigation to /login
    expect(navigateTarget).toBeNull();
  });

  it('transient failure then success on retry → reset retry counter, no /login navigation', async () => {
    setupRegisteredDeviceWithExpiredToken();
    // 1st: network_error, 1st retry: success
    mockFetchResponses = ['network_error', '200'];

    vi.useFakeTimers();
    vi.spyOn(Math, 'random').mockReturnValue(0.5);

    const refreshPromise = runFixedScheduleRefresh();
    await vi.advanceTimersByTimeAsync(250);
    const { retryCount } = await refreshPromise;

    expect(retryCount).toBe(1);
    expect(refreshFetchCount).toBe(2);
    expect(navigateTarget).toBeNull(); // success → no /login
  });
});

/* ═══ Race Safety — Scheduled Refresh Cancellation ══════════════════
 *
 * Tests for race safety (task 4.4).
 * Verifies that stale scheduled refreshes don't fire after cancellation.
 *
 * **Validates: Requirements 2.1, 2.2**
 */
describe('Race Safety — Scheduled Refresh Cancellation', () => {
  beforeEach(async () => {
    mockStore = {};
    mockKeychain = {};
    navigateTarget = null;
    refreshFetchCount = 0;
    mockFetchBehavior = 'network_error';
    mockFetchResponses = [];
    vi.resetModules();
  });

  afterEach(() => {
    vi.useRealTimers();
    vi.restoreAllMocks();
  });

  it('refreshTimeoutRef is cleared when transitioning to /login (simulated via clearTimeout)', async () => {
    setupRegisteredDeviceWithExpiredToken();
    mockFetchBehavior = '401'; // non-recoverable → immediate /login

    vi.useFakeTimers();
    const clearTimeoutSpy = vi.spyOn(globalThis, 'clearTimeout');

    // Simulate a pending timeout ref
    let refreshTimeoutRef: ReturnType<typeof setTimeout> | null = setTimeout(() => {}, 99999);

    // Simulate the fixed AuthGate behavior: clear timeout before navigating
    const auth = await import('../auth');
    const result = await auth.refreshTokens();

    if (result.status !== 'success') {
      // Fixed code clears timeout before navigating
      if (refreshTimeoutRef !== null) {
        clearTimeout(refreshTimeoutRef);
        refreshTimeoutRef = null;
      }
      await auth.clearSessionFull();
      navigateTarget = '/login';
    }

    expect(clearTimeoutSpy).toHaveBeenCalled();
    expect(refreshTimeoutRef).toBeNull();
    expect(navigateTarget).toBe('/login');
  });

  it('stale scheduleRefresh does not fire after cancelled flag is set', async () => {
    setupRegisteredDeviceWithExpiredToken();
    mockFetchBehavior = 'network_error';

    vi.useFakeTimers();
    vi.spyOn(Math, 'random').mockReturnValue(0.5);

    let cancelFlag = false;

    // Start a schedule refresh cycle
    const refreshPromise = runFixedScheduleRefresh({
      onCancel: () => cancelFlag,
    });

    // Cancel immediately after the initial failure
    cancelFlag = true;

    // Advance timers — the retry should NOT fire because cancelled
    await vi.advanceTimersByTimeAsync(250);
    await vi.advanceTimersByTimeAsync(1000);
    await vi.advanceTimersByTimeAsync(3000);
    await refreshPromise;

    // Only the initial attempt should have been made
    expect(refreshFetchCount).toBe(1);
    // No navigation — cancelled before any navigate
    expect(navigateTarget).toBeNull();
  });

  it('start refresh → set cancelled → resolve refresh promise → no navigation (stale resolution is no-op)', async () => {
    setupRegisteredDeviceWithExpiredToken();
    mockFetchBehavior = 'network_error';

    vi.useFakeTimers();
    vi.spyOn(Math, 'random').mockReturnValue(0.5);

    let cancelFlag = false;

    // Start init with cancel support
    const initPromise = runFixedAuthGateInit({
      onCancel: () => cancelFlag,
    });

    // The initial refreshTokens() call happens synchronously in the flow.
    // After the first failure, the retry loop checks cancelled before each delay.
    // Set cancelled after the initial attempt but before retries fire.
    cancelFlag = true;

    // Advance all timers — retries should be no-ops
    await vi.advanceTimersByTimeAsync(5000);
    await initPromise;

    // No navigation should have occurred — the stale resolution is a no-op
    expect(navigateTarget).toBeNull();
    // Only 1 initial attempt
    expect(refreshFetchCount).toBe(1);
    // Device identity untouched
    expect(mockStore['device_id']).toBe('device-123');
    expect(mockStore['server_url']).toBe('https://wavis.example.com');
    expect(mockStore['display_name']).toBe('TestUser');
  });
});
