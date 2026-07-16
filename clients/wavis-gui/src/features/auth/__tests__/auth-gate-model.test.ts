/**
 * Tests for auth-gate-model.ts — the REAL AuthGate.tsx orchestration logic,
 * extracted so it can be exercised directly instead of through a hand-written
 * copy of the component (see git history of this file: the previous version
 * asserted against `runFixedAuthGateInit`, a reimplementation that could
 * silently drift from AuthGate.tsx — e.g. it called `clearSessionFull()` for
 * 401/400, which the real component never does; only `clearAccessTokens()`
 * is ever called, so the refresh token and device identity are always
 * preserved).
 *
 * These tests call `runAuthGateInit` / `decideScheduledRefreshAction`
 * directly — production code, not a mirror. Deps are injected fakes, and
 * `sleep` is a no-op spy, so no fake timers are needed anywhere here.
 */
import { describe, it, expect, vi, beforeEach } from 'vitest';
import fc from 'fast-check';
import type { RefreshResult } from '../auth';
import {
  runAuthGateInit,
  decideScheduledRefreshAction,
  jitteredDelay,
  isTransientFailure,
  MAX_REFRESH_RETRIES,
  STARTUP_RETRY_DELAYS,
  type AuthGateDeps,
} from '../auth-gate-model';

/* ─── Fakes ─────────────────────────────────────────────────────── */

function makeDeps(overrides: Partial<AuthGateDeps> = {}): AuthGateDeps {
  return {
    isDeviceRegistered: vi.fn(async () => true),
    getUsername: vi.fn(async () => 'existing-user'),
    isTokenExpired: vi.fn(async () => false),
    refreshTokens: vi.fn(async (): Promise<RefreshResult> => ({ status: 'success' })),
    clearAccessTokens: vi.fn(async () => {}),
    fetchMyUsername: vi.fn(async () => null),
    setUsername: vi.fn(async () => {}),
    sleep: vi.fn(async () => {}),
    ...overrides,
  };
}

/** refreshTokens fake that returns queued results in order, then repeats the last. */
function queuedRefresh(results: RefreshResult[]) {
  let i = 0;
  return vi.fn(async (): Promise<RefreshResult> => {
    const r = results[Math.min(i, results.length - 1)];
    i++;
    return r;
  });
}

beforeEach(() => {
  vi.restoreAllMocks();
});

/* ═══ runAuthGateInit ════════════════════════════════════════════ */

describe('runAuthGateInit — device registration', () => {
  it('unregistered device: outcome "setup", token/refresh path never touched', async () => {
    const deps = makeDeps({ isDeviceRegistered: vi.fn(async () => false) });

    const result = await runAuthGateInit(deps);

    expect(result.outcome).toBe('setup');
    expect(result.localUsername).toBeNull();
    expect(deps.getUsername).not.toHaveBeenCalled();
    expect(deps.isTokenExpired).not.toHaveBeenCalled();
    expect(deps.refreshTokens).not.toHaveBeenCalled();
  });
});

describe('runAuthGateInit — valid token (no refresh needed)', () => {
  it('non-expired token: outcome "ready", refreshTokens never called', async () => {
    const deps = makeDeps({ isTokenExpired: vi.fn(async () => false) });

    const result = await runAuthGateInit(deps);

    expect(result.outcome).toBe('ready');
    expect(deps.refreshTokens).not.toHaveBeenCalled();
    expect(deps.clearAccessTokens).not.toHaveBeenCalled();
  });
});

describe('runAuthGateInit — expired token, refresh succeeds', () => {
  it('single successful refresh: outcome "ready", no retries, no clearing', async () => {
    const deps = makeDeps({
      isTokenExpired: vi.fn(async () => true),
      refreshTokens: vi.fn(async (): Promise<RefreshResult> => ({ status: 'success' })),
    });

    const result = await runAuthGateInit(deps);

    expect(result.outcome).toBe('ready');
    expect(deps.refreshTokens).toHaveBeenCalledTimes(1);
    expect(deps.sleep).not.toHaveBeenCalled();
    expect(deps.clearAccessTokens).not.toHaveBeenCalled();
  });
});

describe('runAuthGateInit — non-recoverable failure (no retry)', () => {
  it.each(['unauthorized', 'bad_request', 'no_refresh_token', 'no_server_url'] as const)(
    '%s: outcome "login", clearAccessTokens called once, no retry sleep',
    async (status) => {
      const deps = makeDeps({
        isTokenExpired: vi.fn(async () => true),
        refreshTokens: vi.fn(async (): Promise<RefreshResult> => ({ status })),
      });

      const result = await runAuthGateInit(deps);

      expect(result.outcome).toBe('login');
      expect(deps.refreshTokens).toHaveBeenCalledTimes(1);
      expect(deps.sleep).not.toHaveBeenCalled();
      expect(deps.clearAccessTokens).toHaveBeenCalledTimes(1);
    },
  );
});

describe('runAuthGateInit — transient failure retry loop', () => {
  it('network_error on every attempt: 1 initial + 3 retries, then login + clearAccessTokens once', async () => {
    const deps = makeDeps({
      isTokenExpired: vi.fn(async () => true),
      refreshTokens: vi.fn(async (): Promise<RefreshResult> => ({
        status: 'network_error',
        message: 'down',
      })),
    });

    const result = await runAuthGateInit(deps);

    expect(result.outcome).toBe('login');
    expect(deps.refreshTokens).toHaveBeenCalledTimes(1 + STARTUP_RETRY_DELAYS.length);
    expect(deps.sleep).toHaveBeenCalledTimes(STARTUP_RETRY_DELAYS.length);
    expect(deps.clearAccessTokens).toHaveBeenCalledTimes(1);
  });

  it('sleep delays are within +-20% of STARTUP_RETRY_DELAYS, in order', async () => {
    const deps = makeDeps({
      isTokenExpired: vi.fn(async () => true),
      refreshTokens: vi.fn(async (): Promise<RefreshResult> => ({
        status: 'server_error',
        httpStatus: 503,
      })),
    });

    await runAuthGateInit(deps);

    const sleepCalls = (deps.sleep as ReturnType<typeof vi.fn>).mock.calls.map(
      (c) => c[0] as number,
    );
    expect(sleepCalls).toHaveLength(STARTUP_RETRY_DELAYS.length);
    sleepCalls.forEach((actual, i) => {
      const base = STARTUP_RETRY_DELAYS[i];
      expect(actual).toBeGreaterThanOrEqual(Math.round(base * 0.8));
      expect(actual).toBeLessThanOrEqual(Math.round(base * 1.2));
    });
  });

  it('rate_limited with retryAfter uses the server-provided delay verbatim (no jitter)', async () => {
    const deps = makeDeps({
      isTokenExpired: vi.fn(async () => true),
      refreshTokens: vi.fn(async (): Promise<RefreshResult> => ({
        status: 'rate_limited',
        retryAfter: 5000,
      })),
    });

    await runAuthGateInit(deps);

    const sleepCalls = (deps.sleep as ReturnType<typeof vi.fn>).mock.calls.map((c) => c[0]);
    expect(sleepCalls[0]).toBe(5000);
  });

  it('succeeds on the 2nd retry: stops retrying, outcome "ready", no clearing', async () => {
    const deps = makeDeps({
      isTokenExpired: vi.fn(async () => true),
      refreshTokens: queuedRefresh([
        { status: 'network_error', message: 'a' },
        { status: 'network_error', message: 'b' },
        { status: 'success' },
      ]),
    });

    const result = await runAuthGateInit(deps);

    expect(result.outcome).toBe('ready');
    expect(deps.refreshTokens).toHaveBeenCalledTimes(3);
    expect(deps.sleep).toHaveBeenCalledTimes(2);
    expect(deps.clearAccessTokens).not.toHaveBeenCalled();
  });

  it('becomes non-recoverable mid-retry: stops immediately, does not exhaust remaining retries', async () => {
    const deps = makeDeps({
      isTokenExpired: vi.fn(async () => true),
      refreshTokens: queuedRefresh([
        { status: 'network_error', message: 'a' },
        { status: 'unauthorized' },
      ]),
    });

    const result = await runAuthGateInit(deps);

    expect(result.outcome).toBe('login');
    expect(deps.refreshTokens).toHaveBeenCalledTimes(2);
    expect(deps.sleep).toHaveBeenCalledTimes(1);
    expect(deps.clearAccessTokens).toHaveBeenCalledTimes(1);
  });
});

describe('runAuthGateInit — cancellation aborts the retry loop', () => {
  it('already cancelled by the time the retry loop starts: only the initial attempt runs, no clearing, outcome "cancelled"', async () => {
    const deps = makeDeps({
      isTokenExpired: vi.fn(async () => true),
      refreshTokens: vi.fn(async (): Promise<RefreshResult> => ({
        status: 'network_error',
        message: 'down',
      })),
    });

    const result = await runAuthGateInit(deps, { isCancelled: () => true });

    expect(result.outcome).toBe('cancelled');
    expect(deps.refreshTokens).toHaveBeenCalledTimes(1); // only the initial attempt
    expect(deps.sleep).not.toHaveBeenCalled();
    expect(deps.clearAccessTokens).not.toHaveBeenCalled();
  });

  it('cancelled between the sleep and the retry refresh call: aborts before the 2nd refreshTokens', async () => {
    let cancelled = false;
    const deps = makeDeps({
      isTokenExpired: vi.fn(async () => true),
      refreshTokens: vi.fn(async (): Promise<RefreshResult> => ({
        status: 'network_error',
        message: 'down',
      })),
      sleep: vi.fn(async () => {
        cancelled = true; // simulate unmount happening during the retry delay
      }),
    });

    const result = await runAuthGateInit(deps, { isCancelled: () => cancelled });

    expect(result.outcome).toBe('cancelled');
    expect(deps.refreshTokens).toHaveBeenCalledTimes(1); // 2nd call never made
    expect(deps.sleep).toHaveBeenCalledTimes(1);
    expect(deps.clearAccessTokens).not.toHaveBeenCalled();
  });
});

describe('runAuthGateInit — username sync (issue #197)', () => {
  it('no local username, server has one: synced and returned', async () => {
    const deps = makeDeps({
      getUsername: vi.fn(async () => null),
      fetchMyUsername: vi.fn(async () => 'diego'),
    });

    const result = await runAuthGateInit(deps);

    expect(result.outcome).toBe('ready');
    expect(deps.setUsername).toHaveBeenCalledWith('diego');
    expect(result.syncedUsername).toBe('diego');
  });

  it('local username already present: /me is never called', async () => {
    const deps = makeDeps({ getUsername: vi.fn(async () => 'existing-user') });

    const result = await runAuthGateInit(deps);

    expect(deps.fetchMyUsername).not.toHaveBeenCalled();
    expect(result.syncedUsername).toBeNull();
  });

  it('fetchMyUsername throws: best-effort, init still completes as "ready"', async () => {
    const deps = makeDeps({
      getUsername: vi.fn(async () => null),
      fetchMyUsername: vi.fn(async () => {
        throw new Error('network down');
      }),
    });

    const result = await runAuthGateInit(deps);

    expect(result.outcome).toBe('ready');
    expect(deps.setUsername).not.toHaveBeenCalled();
    expect(result.syncedUsername).toBeNull();
  });

  it('fetchMyUsername resolves null: store left untouched', async () => {
    const deps = makeDeps({
      getUsername: vi.fn(async () => null),
      fetchMyUsername: vi.fn(async () => null),
    });

    const result = await runAuthGateInit(deps);

    expect(deps.setUsername).not.toHaveBeenCalled();
    expect(result.syncedUsername).toBeNull();
  });

  it('token refresh failure: username sync never reached', async () => {
    const deps = makeDeps({
      getUsername: vi.fn(async () => null),
      isTokenExpired: vi.fn(async () => true),
      refreshTokens: vi.fn(async (): Promise<RefreshResult> => ({ status: 'unauthorized' })),
      fetchMyUsername: vi.fn(async () => 'diego'),
    });

    const result = await runAuthGateInit(deps);

    expect(result.outcome).toBe('login');
    expect(deps.fetchMyUsername).not.toHaveBeenCalled();
  });
});

/* ═══ Property tests — device identity is structurally preserved ═══
 *
 * AuthGateDeps exposes only clearAccessTokens (never clearSessionFull or
 * resetAuth), so the model cannot wipe device_id/server_url/display_name
 * even in principle. These properties pin the *shape* of that guarantee:
 * clearAccessTokens is the only clearing call, and it fires exactly once
 * per failed init — never zero (silently swallowed) and never more than
 * once (double-clear/double-navigate).
 */
describe('runAuthGateInit — properties', () => {
  const nonSuccessStatus = fc.constantFrom<RefreshResult['status']>(
    'network_error',
    'server_error',
    'unauthorized',
    'bad_request',
    'rate_limited',
    'no_refresh_token',
    'no_server_url',
  );

  function buildResult(status: RefreshResult['status']): RefreshResult {
    switch (status) {
      case 'network_error':
        return { status, message: 'x' };
      case 'server_error':
        return { status, httpStatus: 500 };
      case 'rate_limited':
        return { status };
      default:
        return { status };
    }
  }

  it('property: every failing refresh path ends in exactly one clearAccessTokens call and outcome "login"', async () => {
    await fc.assert(
      fc.asyncProperty(nonSuccessStatus, async (status) => {
        const deps = makeDeps({
          isTokenExpired: vi.fn(async () => true),
          refreshTokens: vi.fn(async () => buildResult(status)),
        });

        const result = await runAuthGateInit(deps);

        expect(result.outcome).toBe('login');
        expect(deps.clearAccessTokens).toHaveBeenCalledTimes(1);
      }),
      { numRuns: 20 },
    );
  });

  it('property: jitteredDelay stays within +-20% of the base and is a non-negative integer', () => {
    fc.assert(
      fc.property(fc.integer({ min: 1, max: 60_000 }), (base) => {
        const delay = jitteredDelay(base);
        expect(Number.isInteger(delay)).toBe(true);
        expect(delay).toBeGreaterThanOrEqual(Math.round(base * 0.8));
        expect(delay).toBeLessThanOrEqual(Math.round(base * 1.2));
      }),
      { numRuns: 50 },
    );
  });
});

/* ═══ decideScheduledRefreshAction — pure reducer, exhaustive ══════ */

describe('decideScheduledRefreshAction', () => {
  it('success always resets the retry counter, regardless of prior count', () => {
    for (const retryCount of [0, 1, 2, 5]) {
      const decision = decideScheduledRefreshAction({ status: 'success' }, retryCount);
      expect(decision).toEqual({ action: 'success', nextRetryCount: 0 });
    }
  });

  it('non-recoverable, first time seen (retryCount 0): one grace retry, counter jumps to MAX-1', () => {
    const decision = decideScheduledRefreshAction({ status: 'unauthorized' }, 0);
    expect(decision.action).toBe('retry');
    expect(decision.nextRetryCount).toBe(MAX_REFRESH_RETRIES - 1);
    expect(decision.retryDelayMs).toBeGreaterThanOrEqual(800);
    expect(decision.retryDelayMs).toBeLessThanOrEqual(1200);
  });

  it('non-recoverable, seen again after the grace retry: login, no further retry', () => {
    const decision = decideScheduledRefreshAction(
      { status: 'unauthorized' },
      MAX_REFRESH_RETRIES - 1,
    );
    expect(decision).toEqual({ action: 'login', nextRetryCount: MAX_REFRESH_RETRIES - 1 });
  });

  it('transient failures back off through STARTUP_RETRY_DELAYS until exhausted', () => {
    let retryCount = 0;
    for (let i = 0; i < STARTUP_RETRY_DELAYS.length - 1; i++) {
      const decision = decideScheduledRefreshAction(
        { status: 'network_error', message: 'x' },
        retryCount,
      );
      expect(decision.action).toBe('retry');
      expect(decision.nextRetryCount).toBe(retryCount + 1);
      const base = STARTUP_RETRY_DELAYS[decision.nextRetryCount - 1];
      expect(decision.retryDelayMs).toBeGreaterThanOrEqual(Math.round(base * 0.8));
      expect(decision.retryDelayMs).toBeLessThanOrEqual(Math.round(base * 1.2));
      retryCount = decision.nextRetryCount;
    }

    // One more transient failure exhausts the budget
    const final = decideScheduledRefreshAction(
      { status: 'server_error', httpStatus: 500 },
      retryCount,
    );
    expect(final).toEqual({ action: 'login', nextRetryCount: MAX_REFRESH_RETRIES });
  });

  it('rate_limited with retryAfter uses the server-provided delay verbatim', () => {
    const decision = decideScheduledRefreshAction({ status: 'rate_limited', retryAfter: 7000 }, 0);
    expect(decision.action).toBe('retry');
    expect(decision.retryDelayMs).toBe(7000);
  });

  it('property: action is "success" iff the refresh result status is "success"', () => {
    fc.assert(
      fc.property(
        fc.constantFrom<RefreshResult['status']>(
          'network_error',
          'server_error',
          'unauthorized',
          'bad_request',
          'rate_limited',
          'no_refresh_token',
          'no_server_url',
          'success',
        ),
        fc.nat({ max: 5 }),
        (status, retryCount) => {
          const result = buildResult(status);
          const decision = decideScheduledRefreshAction(result, retryCount);
          expect(decision.action === 'success').toBe(status === 'success');
        },
      ),
      { numRuns: 40 },
    );

    function buildResult(status: RefreshResult['status']): RefreshResult {
      switch (status) {
        case 'network_error':
          return { status, message: 'x' };
        case 'server_error':
          return { status, httpStatus: 500 };
        default:
          return { status };
      }
    }
  });
});

describe('isTransientFailure', () => {
  it('classifies network_error, server_error, rate_limited as transient; everything else as not', () => {
    const transient: RefreshResult[] = [
      { status: 'network_error', message: 'x' },
      { status: 'server_error', httpStatus: 500 },
      { status: 'rate_limited' },
    ];
    const notTransient: RefreshResult[] = [
      { status: 'success' },
      { status: 'unauthorized' },
      { status: 'bad_request' },
      { status: 'no_refresh_token' },
      { status: 'no_server_url' },
    ];
    transient.forEach((r) => expect(isTransientFailure(r)).toBe(true));
    notTransient.forEach((r) => expect(isTransientFailure(r)).toBe(false));
  });
});
