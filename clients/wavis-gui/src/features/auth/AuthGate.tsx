import { useState, useEffect, useRef, useCallback } from 'react';
import { Outlet, useNavigate, useLocation } from 'react-router';
import {
  isDeviceRegistered,
  isTokenExpired,
  refreshTokens,
  clearAccessTokens,
  getTokenExpiryMs,
  getServerUrl,
  getUsername,
  fetchMyUsername,
  setUsername,
} from './auth';
import {
  runAuthGateInit,
  decideScheduledRefreshAction,
  isTransientFailure,
} from './auth-gate-model';
import { parseHostname } from '@shared/helpers';

/* ─── Constants ─────────────────────────────────────────────────── */
const LOG_PREFIX = '[wavis:authgate]';

function sleep(ms: number): Promise<void> {
  return new Promise((resolve) => setTimeout(resolve, ms));
}

/* ═══ Component ═════════════════════════════════════════════════════ */
export default function AuthGate() {
  const navigate = useNavigate();
  const location = useLocation();
  const [ready, setReady] = useState(false);
  const [hostname, setHostname] = useState('wavis');
  const [username, setUsernameState] = useState<string | null>(null);
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

    refreshTimeoutRef.current = setTimeout(() => {
      void (async () => {
        console.log(LOG_PREFIX, 'Scheduled refresh firing');
        const result = await refreshTokens();
        const decision = decideScheduledRefreshAction(result, refreshRetriesRef.current);
        refreshRetriesRef.current = decision.nextRetryCount;

        if (decision.action === 'success') {
          void scheduleRefresh();
          return;
        }

        if (decision.action === 'retry') {
          console.warn(LOG_PREFIX, `Scheduled refresh failed (${result.status}) — retrying`);
          refreshTimeoutRef.current = setTimeout(() => {
            void scheduleRefresh();
          }, decision.retryDelayMs);
          return;
        }

        // login — preserve refresh token, only clear access tokens so Login
        // shows "Reconnect" instead of forcing a full re-register.
        console.warn(LOG_PREFIX, `Scheduled refresh non-recoverable: ${result.status}`);
        await clearAccessTokens();
        cancelScheduledRefresh();
        void navigate('/login', { replace: true });
      })();
    }, delay);
  }, [navigate, cancelScheduledRefresh]);

  // Listen for auth-expired signals from apiFetch. This fires when an API call
  // gets a server-side 401 and the token refresh also fails — the session is
  // definitively dead. Navigate to /login immediately rather than waiting for
  // the scheduled refresh timer (which may be many minutes away).
  useEffect(() => {
    async function handleAuthExpired() {
      console.warn(LOG_PREFIX, 'Auth-expired signal from API call — navigating to /login');
      cancelScheduledRefresh();
      await clearAccessTokens();
      void navigate('/login', { replace: true });
    }
    const onAuthExpired = () => {
      void handleAuthExpired();
    };
    window.addEventListener('wavis:auth-expired', onAuthExpired);
    return () => window.removeEventListener('wavis:auth-expired', onAuthExpired);
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
          void scheduleRefresh();
        } else if (!isTransientFailure(result)) {
          await clearAccessTokens();
          cancelScheduledRefresh();
          void navigate('/login', { replace: true });
        }
      } else {
        // Token still valid — re-schedule in case the timer was killed while backgrounded
        console.log(LOG_PREFIX, 'App resumed — re-scheduling refresh timer');
        void scheduleRefresh();
      }
    }
    const onVisibilityChange = () => {
      void handleVisibility();
    };
    document.addEventListener('visibilitychange', onVisibilityChange);
    return () => document.removeEventListener('visibilitychange', onVisibilityChange);
  }, [navigate, scheduleRefresh, cancelScheduledRefresh]);

  useEffect(() => {
    let cancelled = false;

    async function init() {
      const result = await runAuthGateInit(
        {
          isDeviceRegistered,
          getUsername,
          isTokenExpired,
          refreshTokens,
          clearAccessTokens,
          fetchMyUsername,
          setUsername,
          sleep,
        },
        { isCancelled: () => cancelled },
      );

      if (cancelled) return;

      // Load server URL for status bar (independent of the outcome above)
      const url = await getServerUrl();
      if (!cancelled && url) {
        setHostname(parseHostname(url));
      }

      if (result.localUsername) setUsernameState(result.localUsername);
      if (result.syncedUsername) setUsernameState(result.syncedUsername);

      if (result.outcome === 'setup') {
        void navigate('/setup', { replace: true });
        return;
      }
      if (result.outcome === 'login') {
        cancelScheduledRefresh();
        void navigate('/login', { replace: true });
        return;
      }
      if (result.outcome === 'cancelled') {
        return;
      }

      // ready — token is valid (or was just refreshed)
      setReady(true);
      void scheduleRefresh();
    }

    void init();

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
            <span className="text-wavis-text truncate">{username ?? hostname}</span>
            {username && <span className="text-wavis-text-secondary truncate">@ {hostname}</span>}
          </div>
          <button
            onClick={() => {
              void navigate('/settings');
            }}
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
