import { useState, useEffect, useRef, useCallback } from 'react';
import { Outlet, useNavigate, useLocation } from 'react-router';
import {
  isDeviceRegistered,
  isTokenExpired,
  refreshTokens,
  clearAccessTokens,
  getTokenExpiryMs,
  getServerUrl,
  getDisplayName,
} from './auth';
import type { RefreshResult } from './auth';
import { parseHostname } from '@shared/helpers';

/* ─── Constants ─────────────────────────────────────────────────── */
const LOG_PREFIX = '[wavis:authgate]';
const MAX_REFRESH_RETRIES = 3;
const STARTUP_RETRY_DELAYS = [250, 1000, 3000];

/* ─── Helpers ───────────────────────────────────────────────────── */

function jitteredDelay(baseMs: number): number {
  const jitter = baseMs * 0.2 * (2 * Math.random() - 1); // ±20%
  return Math.round(baseMs + jitter);
}

function isTransientFailure(result: RefreshResult): boolean {
  return (
    result.status === 'network_error' ||
    result.status === 'server_error' ||
    result.status === 'rate_limited'
  );
}

/* ═══ Component ═════════════════════════════════════════════════════ */
export default function AuthGate() {
  const navigate = useNavigate();
  const location = useLocation();
  const [ready, setReady] = useState(false);
  const [hostname, setHostname] = useState('wavis');
  const [displayName, setDisplayNameVal] = useState<string | null>(null);
  const refreshTimeoutRef = useRef<ReturnType<typeof setTimeout> | null>(null);

  const refreshRetriesRef = useRef(0);

  /** Cancel any pending scheduled refresh — race safety before navigating to /login */
  const cancelScheduledRefresh = useCallback(() => {
    if (refreshTimeoutRef.current !== null) {
      clearTimeout(refreshTimeoutRef.current);
      refreshTimeoutRef.current = null;
    }
  }, []);

  /** Schedule a token refresh timeout at (expiryMs - 60s), clamped to 0 */
  const scheduleRefresh = useCallback(async () => {
    cancelScheduledRefresh();

    const expiryMs = await getTokenExpiryMs();
    const delay = Math.max(0, expiryMs - 120_000);
    console.log(LOG_PREFIX, `Scheduling refresh in ${delay}ms`);

    refreshTimeoutRef.current = setTimeout(async () => {
      console.log(LOG_PREFIX, 'Scheduled refresh firing');
      const result = await refreshTokens();

      if (result.status === 'success') {
        refreshRetriesRef.current = 0;
        scheduleRefresh();
        return;
      }

      // Non-recoverable: retry once more (the first 401 might be a transient
      // race with token rotation), then navigate to /login preserving the
      // refresh token so the Login page can offer "Reconnect".
      if (!isTransientFailure(result)) {
        if (refreshRetriesRef.current === 0) {
          // First non-recoverable failure — retry once after a short delay
          refreshRetriesRef.current = MAX_REFRESH_RETRIES - 1;
          console.warn(LOG_PREFIX, `Scheduled refresh non-recoverable (${result.status}) — retrying once`);
          refreshTimeoutRef.current = setTimeout(() => {
            scheduleRefresh();
          }, jitteredDelay(1000));
          return;
        }
        console.warn(LOG_PREFIX, `Scheduled refresh non-recoverable: ${result.status}`);
        // Preserve refresh token — only clear access tokens so Login shows
        // "Reconnect" instead of forcing a full re-register.
        await clearAccessTokens();
        cancelScheduledRefresh();
        navigate('/login', { replace: true });
        return;
      }

      // Transient: retry with backoff
      refreshRetriesRef.current += 1;
      console.warn(LOG_PREFIX, `Scheduled refresh failed (attempt ${refreshRetriesRef.current}/${MAX_REFRESH_RETRIES}): ${result.status}`);

      if (refreshRetriesRef.current < MAX_REFRESH_RETRIES) {
        const retryDelay =
          result.status === 'rate_limited' && result.retryAfter
            ? result.retryAfter
            : jitteredDelay(STARTUP_RETRY_DELAYS[refreshRetriesRef.current - 1] ?? 3000);
        refreshTimeoutRef.current = setTimeout(() => {
          scheduleRefresh();
        }, retryDelay);
        return;
      }

      // All retries exhausted — transient failure, preserve refresh token
      console.warn(LOG_PREFIX, 'All scheduled refresh retries exhausted');
      await clearAccessTokens();
      cancelScheduledRefresh();
      navigate('/login', { replace: true });
    }, delay);
  }, [navigate, cancelScheduledRefresh]);

  // Re-evaluate token state when app regains focus. Covers two cases:
  // 1. Token expired while backgrounded (setTimeout didn't fire) → refresh now
  // 2. Token still valid but timer was killed → re-schedule so we don't miss the window
  useEffect(() => {
    async function handleVisibility() {
      if (document.visibilityState !== 'visible') return;
      const expired = await isTokenExpired();
      if (expired) {
        console.log(LOG_PREFIX, 'App resumed with expired token — refreshing');
        const result = await refreshTokens();
        if (result.status === 'success') {
          refreshRetriesRef.current = 0;
          scheduleRefresh();
        } else if (!isTransientFailure(result)) {
          await clearAccessTokens();
          cancelScheduledRefresh();
          navigate('/login', { replace: true });
        }
      } else {
        // Token still valid — re-schedule in case the timer was killed while backgrounded
        console.log(LOG_PREFIX, 'App resumed — re-scheduling refresh timer');
        scheduleRefresh();
      }
    }
    document.addEventListener('visibilitychange', handleVisibility);
    return () => document.removeEventListener('visibilitychange', handleVisibility);
  }, [navigate, scheduleRefresh, cancelScheduledRefresh]);

  useEffect(() => {
    let cancelled = false;

    async function init() {
      // 1. Check device registration
      const registered = await isDeviceRegistered();
      if (!registered) {
        if (!cancelled) navigate('/setup', { replace: true });
        return;
      }

      // Load server URL for status bar
      const url = await getServerUrl();
      if (!cancelled && url) {
        setHostname(parseHostname(url));
      }

      // Load display name for status bar
      const name = await getDisplayName();
      if (!cancelled && name) {
        setDisplayNameVal(name);
      }

      // 2. Check token expiry
      const expired = await isTokenExpired();
      if (expired) {
        const result = await refreshTokens();

        if (result.status === 'success') {
          // Refresh succeeded — fall through to schedule
        } else if (!isTransientFailure(result)) {
          // Non-recoverable on startup — preserve refresh token so Login
          // page can offer "Reconnect" instead of forcing re-register.
          if (!cancelled) {
            await clearAccessTokens();
            cancelScheduledRefresh();
            navigate('/login', { replace: true });
          }
          return;
        } else {
          // Transient: retry loop
          let lastResult: RefreshResult = result;
          for (let i = 0; i < STARTUP_RETRY_DELAYS.length; i++) {
            if (cancelled) return;
            const delay =
              lastResult.status === 'rate_limited' && lastResult.retryAfter
                ? lastResult.retryAfter
                : jitteredDelay(STARTUP_RETRY_DELAYS[i]);
            await new Promise((r) => setTimeout(r, delay));
            if (cancelled) return;
            lastResult = await refreshTokens();
            if (lastResult.status === 'success') break;
            if (!isTransientFailure(lastResult)) {
              // Became non-recoverable mid-retry — preserve refresh token
              await clearAccessTokens();
              cancelScheduledRefresh();
              navigate('/login', { replace: true });
              return;
            }
          }
          if (lastResult.status !== 'success') {
            // All retries exhausted — transient, preserve refresh token
            if (!cancelled) {
              await clearAccessTokens();
              cancelScheduledRefresh();
              navigate('/login', { replace: true });
            }
            return;
          }
        }
      }

      // 3. Token is valid (or just refreshed) — schedule background refresh
      if (!cancelled) {
        setReady(true);
        scheduleRefresh();
      }
    }

    init();

    return () => {
      cancelled = true;
      cancelScheduledRefresh();
    };
  }, [navigate, scheduleRefresh, cancelScheduledRefresh]);

  if (!ready) {
    return (
      <div className="h-full flex items-center justify-center text-wavis-text-secondary">
        loading...
      </div>
    );
  }

  const isRoomPage = location.pathname === '/room';

  return (
    <div className="flex flex-col h-full">
      {/* Status bar — hidden on room page (LIVE indicator replaces it) */}
      {!isRoomPage && (
        <div className="shrink-0 flex items-center justify-between px-3 sm:px-4 py-1.5 border-b bg-wavis-panel border-wavis-text-secondary text-xs">
          <div className="flex items-center gap-2 min-w-0">
            <div className="w-1.5 h-1.5 rounded-full bg-wavis-accent shrink-0" />
            <span className="text-wavis-text truncate">{displayName ?? hostname}</span>
            {displayName && <span className="text-wavis-text-secondary truncate">@ {hostname}</span>}
          </div>
          <button
            onClick={() => navigate('/settings')}
            className="text-wavis-text-secondary shrink-0 border border-wavis-text-secondary py-0.5 px-1 text-xs text-center transition-colors hover:bg-wavis-text-secondary hover:text-wavis-text-contrast"
          >
            /settings
          </button>
        </div>
      )}

      <div className="flex-1 min-h-0">
        <Outlet />
      </div>
    </div>
  );
}
