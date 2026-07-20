/**
 * AuthGate view model
 *
 * Pure control-flow logic for AuthGate.tsx's two refresh paths: the startup
 * init() sequence (bounded 3-retry loop, deps injected so it's testable
 * without real timers or IO) and the scheduleRefresh() background sweep
 * (a synchronous reducer mirroring the recursive-setTimeout retry counter
 * in the component — no async, no IO, trivially exhaustive to test).
 *
 * Both paths always clear only the access token on failure (clearAccessTokens),
 * never the refresh token or device identity — AuthGate never calls
 * clearSessionFull/resetAuth. This preserves device_id/server_url/display_name
 * so the Login page can offer "Reconnect" instead of forcing re-pairing.
 */
import type { RefreshResult } from './auth';

/* ─── Constants ─────────────────────────────────────────────────── */
export const MAX_REFRESH_RETRIES = 3;
export const STARTUP_RETRY_DELAYS = [250, 1000, 3000];

/* ─── Helpers ───────────────────────────────────────────────────── */

export function jitteredDelay(baseMs: number): number {
  const jitter = baseMs * 0.2 * (2 * Math.random() - 1); // +-20%
  return Math.round(baseMs + jitter);
}

export function isTransientFailure(result: RefreshResult): boolean {
  return (
    result.status === 'network_error' ||
    result.status === 'server_error' ||
    result.status === 'rate_limited'
  );
}

/* ─── init() — startup refresh + bounded retry ─────────────────── */

export interface AuthGateDeps {
  isDeviceRegistered: () => Promise<boolean>;
  getUsername: () => Promise<string | null>;
  isTokenExpired: () => Promise<boolean>;
  refreshTokens: () => Promise<RefreshResult>;
  clearAccessTokens: () => Promise<void>;
  fetchMyUsername: () => Promise<string | null>;
  setUsername: (name: string) => Promise<void>;
  /** Injected so tests don't need real timers or fake-timer gymnastics. */
  sleep: (ms: number) => Promise<void>;
}

export interface RunAuthGateInitOptions {
  /** Checked between retry steps; an early-abort, not a return-value guard. */
  isCancelled?: () => boolean;
}

export type AuthGateInitOutcome = 'setup' | 'login' | 'ready' | 'cancelled';

export interface RunAuthGateInitResult {
  outcome: AuthGateInitOutcome;
  /** Username read from the local store before the token check. */
  localUsername: string | null;
  /** Username synced from the server this run (issue #197), if any. */
  syncedUsername: string | null;
}

/** Attempt refreshTokens(), retrying transient failures up to MAX_REFRESH_RETRIES times. */
async function refreshWithRetries(
  deps: AuthGateDeps,
  firstResult: RefreshResult,
  isCancelled: () => boolean,
): Promise<'success' | 'login' | 'cancelled'> {
  if (firstResult.status === 'success') return 'success';

  if (!isTransientFailure(firstResult)) {
    // Non-recoverable on startup — no grace retry (unlike scheduleRefresh's
    // background sweep, which allows one to absorb a token-rotation race).
    await deps.clearAccessTokens();
    return 'login';
  }

  let lastResult: RefreshResult = firstResult;
  for (let i = 0; i < STARTUP_RETRY_DELAYS.length; i++) {
    if (isCancelled()) return 'cancelled';
    const baseDelay = STARTUP_RETRY_DELAYS[i];
    const delay =
      lastResult.status === 'rate_limited' && lastResult.retryAfter
        ? lastResult.retryAfter
        : jitteredDelay(baseDelay);
    await deps.sleep(delay);
    if (isCancelled()) return 'cancelled';

    lastResult = await deps.refreshTokens();
    if (lastResult.status === 'success') return 'success';
    if (!isTransientFailure(lastResult)) {
      await deps.clearAccessTokens();
      return 'login';
    }
  }

  if (isCancelled()) return 'cancelled';
  await deps.clearAccessTokens();
  return 'login';
}

export async function runAuthGateInit(
  deps: AuthGateDeps,
  options: RunAuthGateInitOptions = {},
): Promise<RunAuthGateInitResult> {
  const isCancelled = options.isCancelled ?? (() => false);

  const registered = await deps.isDeviceRegistered();
  if (!registered) {
    return { outcome: 'setup', localUsername: null, syncedUsername: null };
  }

  const localUsername = await deps.getUsername();

  const expired = await deps.isTokenExpired();
  if (expired) {
    const result = await deps.refreshTokens();
    const refreshOutcome = await refreshWithRetries(deps, result, isCancelled);
    if (refreshOutcome === 'cancelled') {
      return { outcome: 'cancelled', localUsername, syncedUsername: null };
    }
    if (refreshOutcome === 'login') {
      return { outcome: 'login', localUsername, syncedUsername: null };
    }
  }

  // Username sync: recovers accounts where username was never persisted
  // locally (e.g. best-effort sync failed after pairing, or store reset).
  let syncedUsername: string | null = null;
  if (!localUsername) {
    try {
      const serverName = await deps.fetchMyUsername();
      if (!isCancelled() && serverName) {
        await deps.setUsername(serverName);
        syncedUsername = serverName;
      }
    } catch {
      // Best-effort — missing username is cosmetic, proceed without blocking
    }
  }

  return { outcome: 'ready', localUsername, syncedUsername };
}

/* ─── visibilitychange resume — token re-check + username sync ──── */

export type AuthGateResumeOutcome = 'login' | 'ready' | 'refreshed' | 'skipped';

export interface RunAuthGateResumeResult {
  outcome: AuthGateResumeOutcome;
  /** Username synced from the server this run (issue #197), if any. */
  syncedUsername: string | null;
}

type AuthGateResumeDeps = Pick<
  AuthGateDeps,
  | 'isTokenExpired'
  | 'refreshTokens'
  | 'clearAccessTokens'
  | 'getUsername'
  | 'fetchMyUsername'
  | 'setUsername'
>;

/**
 * Re-evaluate token and username state when the app regains visibility
 * (e.g. resumes from OS sleep). Covers two token cases — expired while
 * backgrounded (refresh now) and still valid (just re-schedule) — plus the
 * same username sync as runAuthGateInit.
 *
 * That sync matters here specifically: runAuthGateInit only runs once per
 * launch, so a long-running app that's suspended and resumed rather than
 * restarted would otherwise never get a second chance to recover a username
 * a best-effort pairing/recovery sync failed to persist (issue #197).
 */
export async function runAuthGateResume(
  deps: AuthGateResumeDeps,
): Promise<RunAuthGateResumeResult> {
  const expired = await deps.isTokenExpired();

  let outcome: AuthGateResumeOutcome;
  if (expired) {
    const result = await deps.refreshTokens();
    if (result.status === 'success') {
      outcome = 'refreshed';
    } else if (!isTransientFailure(result)) {
      await deps.clearAccessTokens();
      return { outcome: 'login', syncedUsername: null };
    } else {
      // Transient — leave it for the next scheduled refresh or visibility event.
      return { outcome: 'skipped', syncedUsername: null };
    }
  } else {
    outcome = 'ready';
  }

  let syncedUsername: string | null = null;
  const localUsername = await deps.getUsername();
  if (!localUsername) {
    try {
      const serverName = await deps.fetchMyUsername();
      if (serverName) {
        await deps.setUsername(serverName);
        syncedUsername = serverName;
      }
    } catch {
      // Best-effort — missing username is cosmetic, proceed without blocking
    }
  }

  return { outcome, syncedUsername };
}

/* ─── scheduleRefresh() — background sweep reducer ─────────────── */

export type ScheduledRefreshAction = 'success' | 'retry' | 'login';

export interface ScheduledRefreshDecision {
  action: ScheduledRefreshAction;
  /** Present when action === 'retry': ms to wait before calling scheduleRefresh() again. */
  retryDelayMs?: number;
  /** Retry counter to persist (component keeps this in refreshRetriesRef). */
  nextRetryCount: number;
}

/**
 * Pure reducer for one scheduled-refresh timer firing. Mirrors the
 * component's refreshRetriesRef-based state machine exactly:
 *  - success: reset counter, caller reschedules from fresh token expiry.
 *  - non-recoverable (401/400/...): one grace retry (~1s) the first time
 *    it's seen (may be a transient race with token rotation), then login.
 *  - transient (network/5xx/429): backs off through STARTUP_RETRY_DELAYS
 *    until MAX_REFRESH_RETRIES is reached, then login.
 * No IO, no timers — trivially exhaustive to test.
 */
export function decideScheduledRefreshAction(
  result: RefreshResult,
  retryCount: number,
): ScheduledRefreshDecision {
  if (result.status === 'success') {
    return { action: 'success', nextRetryCount: 0 };
  }

  if (!isTransientFailure(result)) {
    if (retryCount === 0) {
      return {
        action: 'retry',
        retryDelayMs: jitteredDelay(1000),
        nextRetryCount: MAX_REFRESH_RETRIES - 1,
      };
    }
    return { action: 'login', nextRetryCount: retryCount };
  }

  const nextRetryCount = retryCount + 1;
  if (nextRetryCount < MAX_REFRESH_RETRIES) {
    const retryDelayMs =
      result.status === 'rate_limited' && result.retryAfter
        ? result.retryAfter
        : jitteredDelay(STARTUP_RETRY_DELAYS[nextRetryCount - 1] ?? 3000);
    return { action: 'retry', retryDelayMs, nextRetryCount };
  }

  return { action: 'login', nextRetryCount };
}
