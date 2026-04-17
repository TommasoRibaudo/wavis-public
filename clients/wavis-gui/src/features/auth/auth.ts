/**
 * Wavis Auth Service (Tauri)
 *
 * User registration, account recovery, device pairing, token lifecycle, keychain IPC.
 * Access token -> Tauri store (fast access, short-lived).
 * Refresh token -> OS keychain via Rust IPC (long-lived, secure).
 */

import { invoke } from '@tauri-apps/api/core';
import { load } from '@tauri-apps/plugin-store';
import { fetch as tauriFetch } from '@tauri-apps/plugin-http';

// --- Constants ---
const STORE_NAME = 'wavis-auth.json';
/** Fallback TTL if JWT exp parsing fails */
const ACCESS_TOKEN_TTL_SECS = 900;
const LOG_PREFIX = '[wavis:auth]';

/**
 * Whether the insecure TLS option is available at all.
 * Controlled by VITE_ALLOW_INSECURE_TLS env var. Defaults to false.
 * When false, http:// URLs are always rejected and dangerouslyIgnoreCertificateErrors is never set.
 */
export const INSECURE_TLS_ALLOWED =
  import.meta.env.VITE_ALLOW_INSECURE_TLS === 'true';

// --- Types ---
export type AuthLogEntry = {
  time: string;
  message: string;
  type: 'info' | 'success' | 'warning' | 'error';
};

export type RefreshResult =
  | { status: 'success' }
  | { status: 'network_error'; message: string }
  | { status: 'server_error'; httpStatus: number }
  | { status: 'unauthorized' }
  | { status: 'bad_request' }
  | { status: 'rate_limited'; retryAfter?: number }
  | { status: 'no_refresh_token' }
  | { status: 'no_server_url' };

export interface DeviceInfo {
  device_id: string;
  device_name: string;
  created_at: string;
  revoked_at: string | null;
}

// --- Session State ---
let storeInstance: Awaited<ReturnType<typeof load>> | null = null;
let inflightRefresh: Promise<RefreshResult> | null = null;
let tokenRefreshedCallbacks: Array<() => void> = [];

// --- Helpers (private) ---

async function getStore() {
  if (!storeInstance) {
    storeInstance = await load(STORE_NAME, { defaults: {}, autoSave: true });
  }
  return storeInstance;
}

function makeLogEntry(
  message: string,
  type: AuthLogEntry['type'],
): AuthLogEntry {
  return {
    time: new Date().toLocaleTimeString('en-US', { hour12: false }),
    message,
    type,
  };
}

function notifyTokenRefreshedCallbacks(): void {
  for (const cb of [...tokenRefreshedCallbacks]) {
    try {
      cb();
    } catch (err) {
      console.error(LOG_PREFIX, 'tokenRefreshed callback threw:', err);
    }
  }
}

// --- Pure Functions (exported) ---

export function parseJwtExpiry(jwt: string): number | null {
  try {
    const parts = jwt.split('.');
    if (parts.length !== 3) return null;
    const b64 = parts[1].replace(/-/g, '+').replace(/_/g, '/');
    const padded = b64 + '='.repeat((4 - (b64.length % 4)) % 4);
    const payload = JSON.parse(atob(padded));
    if (typeof payload.exp !== 'number') return null;
    return payload.exp * 1000;
  } catch {
    return null;
  }
}

export function redactToken(token: string): string {
  if (token.length >= 16) return token.slice(0, 16) + '...';
  return '***';
}

export function validateServerUrl(
  url: string,
  insecureTls: boolean,
): { valid: boolean; reason?: string } {
  if (!url || url.trim().length === 0) {
    return { valid: false, reason: 'Server URL cannot be empty' };
  }
  const effectiveInsecure = INSECURE_TLS_ALLOWED && insecureTls;
  try {
    const parsed = new URL(url);
    if (parsed.protocol === 'https:') return { valid: true };
    if (parsed.protocol === 'http:') {
      if (effectiveInsecure) return { valid: true };
      if (!INSECURE_TLS_ALLOWED) {
        return { valid: false, reason: 'http:// is not allowed — use https://' };
      }
      return { valid: false, reason: 'http:// requires "Allow insecure TLS" to be enabled' };
    }
    return { valid: false, reason: 'Unsupported protocol "' + parsed.protocol + '" -- use https://' };
  } catch {
    return { valid: false, reason: 'Malformed URL' };
  }
}

export function onTokensRefreshed(cb: () => void): () => void {
  tokenRefreshedCallbacks.push(cb);
  return () => {
    tokenRefreshedCallbacks = tokenRefreshedCallbacks.filter((c) => c !== cb);
  };
}

// --- Store Accessors (exported) ---

export async function getServerUrl(): Promise<string | null> {
  const store = await getStore();
  return (await store.get<string>('server_url')) ?? null;
}

export async function setServerUrl(url: string): Promise<void> {
  const store = await getStore();
  await store.set('server_url', url);
}

export async function getAccessToken(): Promise<string | null> {
  const store = await getStore();
  return (await store.get<string>('access_token')) ?? null;
}

export async function setAccessToken(token: string): Promise<void> {
  const store = await getStore();
  const expMs = parseJwtExpiry(token) ?? Date.now() + ACCESS_TOKEN_TTL_SECS * 1000;
  await store.set('access_token', token);
  await store.set('access_token_exp', expMs);
}

export async function isTokenExpired(): Promise<boolean> {
  const store = await getStore();
  const exp = await store.get<number>('access_token_exp');
  if (!exp) return true;
  return Date.now() >= exp - 60_000;
}

export async function getTokenExpiryMs(): Promise<number> {
  const store = await getStore();
  const exp = await store.get<number>('access_token_exp');
  if (!exp) return 0;
  return Math.max(0, exp - Date.now());
}

export async function getDeviceId(): Promise<string | null> {
  const store = await getStore();
  return (await store.get<string>('device_id')) ?? null;
}

export async function isDeviceRegistered(): Promise<boolean> {
  const deviceId = await getDeviceId();
  return deviceId !== null;
}

export async function storeRefreshToken(token: string): Promise<void> {
  await invoke('store_token', { key: 'wavis_refresh_token', value: token });
}

export async function getRefreshToken(): Promise<string | null> {
  return invoke<string | null>('get_token', { key: 'wavis_refresh_token' });
}

export async function setInsecureTls(value: boolean): Promise<void> {
  const store = await getStore();
  await store.set('insecure_tls', INSECURE_TLS_ALLOWED && value);
}

export async function getInsecureTls(): Promise<boolean> {
  if (!INSECURE_TLS_ALLOWED) return false;
  const store = await getStore();
  return (await store.get<boolean>('insecure_tls')) ?? false;
}

// --- Registration (exported) ---

export async function registerUser(
  serverUrl: string,
  phrase: string,
  deviceName: string,
  insecureTls: boolean,
  onLog: (entry: AuthLogEntry) => void,
): Promise<{ success: true; recovery_id: string } | { success: false; error?: string }> {
  onLog(makeLogEntry('Validating server URL: ' + serverUrl, 'info'));
  const validation = validateServerUrl(serverUrl, insecureTls);
  if (!validation.valid) {
    onLog(makeLogEntry('URL validation failed: ' + validation.reason, 'error'));
    return { success: false, error: validation.reason };
  }
  onLog(makeLogEntry('URL valid', 'success'));

  await setInsecureTls(insecureTls);

  onLog(makeLogEntry('Sending registration request...', 'info'));
  let res: Response;
  try {
    const url = serverUrl.replace(/\/+$/, '') + '/auth/register';
    res = await tauriFetch(url, {
      method: 'POST',
      headers: { 'Content-Type': 'application/json' },
      body: JSON.stringify({ phrase, device_name: deviceName }),
      ...(INSECURE_TLS_ALLOWED && insecureTls ? { dangerouslyIgnoreCertificateErrors: true } : {}),
    });
  } catch (err) {
    const msg = 'Network error -- could not reach server';
    onLog(makeLogEntry(msg, 'error'));
    console.error(LOG_PREFIX, 'register network error:', err);
    return { success: false, error: msg };
  }

  if (res.status === 429) {
    const msg = 'too many requests -- try again later';
    onLog(makeLogEntry(msg, 'warning'));
    return { success: false, error: msg };
  }

  if (!res.ok) {
    const msg = 'Registration failed -- please try again';
    onLog(makeLogEntry('Server returned ' + res.status, 'error'));
    return { success: false, error: msg };
  }

  const body = (await res.json()) as {
    user_id: string;
    device_id: string;
    recovery_id: string;
    access_token: string;
    refresh_token: string;
  };
  onLog(makeLogEntry('Registration successful', 'success'));

  await setServerUrl(serverUrl);
  onLog(makeLogEntry('Server URL stored', 'info'));

  console.log(LOG_PREFIX, 'Storing access token:', redactToken(body.access_token));
  await setAccessToken(body.access_token);
  onLog(makeLogEntry('Access token stored', 'info'));

  console.log(LOG_PREFIX, 'Storing refresh token:', redactToken(body.refresh_token));
  await storeRefreshToken(body.refresh_token);
  onLog(makeLogEntry('Refresh token stored in keychain', 'info'));

  const store = await getStore();
  await store.set('device_id', body.device_id);
  onLog(makeLogEntry('Device registered: ' + body.device_id, 'success'));

  await invoke('store_token', { key: 'wavis_recovery_id', value: body.recovery_id });
  onLog(makeLogEntry('Recovery ID stored in keychain', 'info'));

  notifyTokenRefreshedCallbacks();
  return { success: true, recovery_id: body.recovery_id };
}

// --- Account Recovery (exported) ---

export async function recoverAccount(
  serverUrl: string,
  recoveryId: string,
  phrase: string,
  deviceName: string,
  insecureTls: boolean,
  onLog: (entry: AuthLogEntry) => void,
): Promise<{ success: boolean; error?: string }> {
  onLog(makeLogEntry('Validating server URL: ' + serverUrl, 'info'));
  const validation = validateServerUrl(serverUrl, insecureTls);
  if (!validation.valid) {
    onLog(makeLogEntry('URL validation failed: ' + validation.reason, 'error'));
    return { success: false, error: validation.reason };
  }
  onLog(makeLogEntry('URL valid', 'success'));

  await setInsecureTls(insecureTls);

  onLog(makeLogEntry('Sending recovery request...', 'info'));
  let res: Response;
  try {
    const url = serverUrl.replace(/\/+$/, '') + '/auth/recover';
    res = await tauriFetch(url, {
      method: 'POST',
      headers: { 'Content-Type': 'application/json' },
      body: JSON.stringify({ recovery_id: recoveryId, phrase, device_name: deviceName }),
      ...(INSECURE_TLS_ALLOWED && insecureTls ? { dangerouslyIgnoreCertificateErrors: true } : {}),
    });
  } catch (err) {
    const msg = 'Network error -- could not reach server';
    onLog(makeLogEntry(msg, 'error'));
    console.error(LOG_PREFIX, 'recover network error:', err);
    return { success: false, error: msg };
  }

  if (res.status === 401) {
    const msg = 'Recovery failed';
    onLog(makeLogEntry(msg, 'error'));
    return { success: false, error: msg };
  }

  if (res.status === 429) {
    const msg = 'too many requests -- try again later';
    onLog(makeLogEntry(msg, 'warning'));
    return { success: false, error: msg };
  }

  if (!res.ok) {
    const msg = 'Recovery failed -- please try again';
    onLog(makeLogEntry('Server returned ' + res.status, 'error'));
    return { success: false, error: msg };
  }

  const body = (await res.json()) as {
    user_id: string;
    device_id: string;
    access_token: string;
    refresh_token: string;
  };
  onLog(makeLogEntry('Recovery successful', 'success'));

  await setServerUrl(serverUrl);
  onLog(makeLogEntry('Server URL stored', 'info'));

  console.log(LOG_PREFIX, 'Storing access token:', redactToken(body.access_token));
  await setAccessToken(body.access_token);
  onLog(makeLogEntry('Access token stored', 'info'));

  console.log(LOG_PREFIX, 'Storing refresh token:', redactToken(body.refresh_token));
  await storeRefreshToken(body.refresh_token);
  onLog(makeLogEntry('Refresh token stored in keychain', 'info'));

  const store = await getStore();
  await store.set('device_id', body.device_id);
  onLog(makeLogEntry('Device recovered: ' + body.device_id, 'success'));

  notifyTokenRefreshedCallbacks();
  return { success: true };
}

// --- Pairing API (exported) ---

export async function startPairing(
  deviceName: string,
): Promise<{ pairing_id: string; code: string }> {
  const serverUrl = await getServerUrl();
  if (!serverUrl) throw new Error('No server URL configured');
  const insecure = await getInsecureTls();

  const url = serverUrl.replace(/\/+$/, '') + '/auth/pair/start';
  const res = await tauriFetch(url, {
    method: 'POST',
    headers: { 'Content-Type': 'application/json' },
    body: JSON.stringify({ device_name: deviceName }),
    ...(insecure ? { dangerouslyIgnoreCertificateErrors: true } : {}),
  });

  if (!res.ok) {
    throw new Error('Failed to start pairing');
  }

  return (await res.json()) as { pairing_id: string; code: string };
}

export async function approvePairing(
  pairingId: string,
  code: string,
): Promise<void> {
  const serverUrl = await getServerUrl();
  if (!serverUrl) throw new Error('No server URL configured');
  const accessToken = await getAccessToken();
  if (!accessToken) throw new Error('Not authenticated');
  const insecure = await getInsecureTls();

  const url = serverUrl.replace(/\/+$/, '') + '/auth/pair/approve';
  const res = await tauriFetch(url, {
    method: 'POST',
    headers: {
      'Content-Type': 'application/json',
      'Authorization': `Bearer ${accessToken}`,
    },
    body: JSON.stringify({ pairing_id: pairingId, code }),
    ...(insecure ? { dangerouslyIgnoreCertificateErrors: true } : {}),
  });

  if (!res.ok) {
    throw new Error('Failed to approve pairing');
  }
}

export async function finishPairing(
  pairingId: string,
  code: string,
): Promise<void> {
  const serverUrl = await getServerUrl();
  if (!serverUrl) throw new Error('No server URL configured');
  const insecure = await getInsecureTls();

  const url = serverUrl.replace(/\/+$/, '') + '/auth/pair/finish';
  const res = await tauriFetch(url, {
    method: 'POST',
    headers: { 'Content-Type': 'application/json' },
    body: JSON.stringify({ pairing_id: pairingId, code }),
    ...(insecure ? { dangerouslyIgnoreCertificateErrors: true } : {}),
  });

  if (!res.ok) {
    throw new Error('Failed to finish pairing');
  }

  const body = (await res.json()) as {
    user_id: string;
    device_id: string;
    access_token: string;
    refresh_token: string;
  };

  console.log(LOG_PREFIX, 'Storing access token:', redactToken(body.access_token));
  await setAccessToken(body.access_token);

  console.log(LOG_PREFIX, 'Storing refresh token:', redactToken(body.refresh_token));
  await storeRefreshToken(body.refresh_token);

  const store = await getStore();
  await store.set('device_id', body.device_id);
  console.log(LOG_PREFIX, 'Pairing complete, device:', body.device_id);

  notifyTokenRefreshedCallbacks();
}

// --- Device Management API (exported) ---

export async function listDevices(): Promise<{ devices: DeviceInfo[]; current_device_id: string }> {
  const serverUrl = await getServerUrl();
  if (!serverUrl) throw new Error('No server URL configured');
  const accessToken = await getAccessToken();
  if (!accessToken) throw new Error('Not authenticated');
  const insecure = await getInsecureTls();

  const url = serverUrl.replace(/\/+$/, '') + '/auth/devices';
  const res = await tauriFetch(url, {
    method: 'GET',
    headers: { 'Authorization': `Bearer ${accessToken}` },
    ...(insecure ? { dangerouslyIgnoreCertificateErrors: true } : {}),
  });

  if (!res.ok) {
    throw new Error('Failed to list devices');
  }

  return (await res.json()) as { devices: DeviceInfo[]; current_device_id: string };
}

export async function revokeDevice(deviceId: string): Promise<void> {
  const serverUrl = await getServerUrl();
  if (!serverUrl) throw new Error('No server URL configured');
  const accessToken = await getAccessToken();
  if (!accessToken) throw new Error('Not authenticated');
  const insecure = await getInsecureTls();

  const url = serverUrl.replace(/\/+$/, '') + `/auth/devices/${deviceId}/revoke`;
  const res = await tauriFetch(url, {
    method: 'POST',
    headers: { 'Authorization': `Bearer ${accessToken}` },
    ...(insecure ? { dangerouslyIgnoreCertificateErrors: true } : {}),
  });

  if (!res.ok) {
    throw new Error('Failed to revoke device');
  }
}

export async function logoutAll(): Promise<void> {
  const serverUrl = await getServerUrl();
  if (!serverUrl) throw new Error('No server URL configured');
  const accessToken = await getAccessToken();
  if (!accessToken) throw new Error('Not authenticated');
  const insecure = await getInsecureTls();

  const url = serverUrl.replace(/\/+$/, '') + '/auth/logout_all';
  const res = await tauriFetch(url, {
    method: 'POST',
    headers: { 'Authorization': `Bearer ${accessToken}` },
    ...(insecure ? { dangerouslyIgnoreCertificateErrors: true } : {}),
  });

  if (!res.ok) {
    throw new Error('Failed to logout all devices');
  }

  await resetAuth();
}

export async function rotatePhrase(
  currentPhrase: string,
  newPhrase: string,
): Promise<void> {
  const serverUrl = await getServerUrl();
  if (!serverUrl) throw new Error('No server URL configured');
  const accessToken = await getAccessToken();
  if (!accessToken) throw new Error('Not authenticated');
  const insecure = await getInsecureTls();

  const url = serverUrl.replace(/\/+$/, '') + '/auth/phrase/rotate';
  const res = await tauriFetch(url, {
    method: 'POST',
    headers: {
      'Content-Type': 'application/json',
      'Authorization': `Bearer ${accessToken}`,
    },
    body: JSON.stringify({ current_phrase: currentPhrase, new_phrase: newPhrase }),
    ...(insecure ? { dangerouslyIgnoreCertificateErrors: true } : {}),
  });

  if (!res.ok) {
    throw new Error('Failed to rotate phrase');
  }
}

// --- Token Refresh (exported) ---

export async function refreshTokens(): Promise<RefreshResult> {
  if (inflightRefresh) return inflightRefresh;

  inflightRefresh = (async (): Promise<RefreshResult> => {
    try {
      const serverUrl = await getServerUrl();
      if (!serverUrl) return { status: 'no_server_url' };

      const refreshToken = await getRefreshToken();
      if (!refreshToken) return { status: 'no_refresh_token' };

      const insecure = await getInsecureTls();
      const url = serverUrl.replace(/\/+$/, '') + '/auth/refresh';

      const res = await tauriFetch(url, {
        method: 'POST',
        headers: { 'Content-Type': 'application/json' },
        body: JSON.stringify({ refresh_token: refreshToken }),
        ...(insecure ? { dangerouslyIgnoreCertificateErrors: true } : {}),
      });

      if (res.status === 401) {
        console.error(LOG_PREFIX, 'refresh failed: 401 unauthorized');
        return { status: 'unauthorized' };
      }

      if (res.status === 400) {
        console.error(LOG_PREFIX, 'refresh failed: 400 bad request');
        return { status: 'bad_request' };
      }

      if (res.status === 429) {
        console.error(LOG_PREFIX, 'refresh failed: 429 rate limited');
        const retryAfterHeader = res.headers.get('Retry-After');
        let retryAfter: number | undefined;
        if (retryAfterHeader) {
          const secs = parseFloat(retryAfterHeader);
          if (!isNaN(secs)) {
            retryAfter = Math.min(30_000, Math.max(250, secs * 1000));
          }
        }
        return { status: 'rate_limited', retryAfter };
      }

      if (res.status >= 500) {
        console.error(LOG_PREFIX, 'refresh failed: ' + res.status);
        return { status: 'server_error', httpStatus: res.status };
      }

      if (!res.ok) {
        console.error(LOG_PREFIX, 'refresh failed: ' + res.status);
        return { status: 'server_error', httpStatus: res.status };
      }

      const body = (await res.json()) as {
        user_id: string;
        device_id: string;
        access_token: string;
        refresh_token: string;
      };

      console.log(LOG_PREFIX, 'Storing refreshed access token:', redactToken(body.access_token));
      await setAccessToken(body.access_token);

      console.log(LOG_PREFIX, 'Storing refreshed refresh token:', redactToken(body.refresh_token));
      await storeRefreshToken(body.refresh_token);

      const store = await getStore();
      await store.set('device_id', body.device_id);

      notifyTokenRefreshedCallbacks();
      return { status: 'success' };
    } catch (err) {
      console.error(LOG_PREFIX, 'refreshTokens error:', err);
      const message = err instanceof Error ? err.message : String(err);
      return { status: 'network_error', message };
    }
  })();

  try {
    return await inflightRefresh;
  } finally {
    inflightRefresh = null;
  }
}

// --- Display Name (exported) ---

export async function getDisplayName(): Promise<string | null> {
  const store = await getStore();
  return (await store.get<string>('display_name')) ?? null;
}

export async function setDisplayName(name: string): Promise<void> {
  const store = await getStore();
  await store.set('display_name', name);
}

// --- Session Clearing (exported) ---

/**
 * Clear only access tokens from Tauri store.
 * Preserves refresh token in OS keychain and all device identity fields.
 * Used after transient failures (network_error, server_error, rate_limited)
 * so "Reconnect" remains available on /login.
 */
export async function clearAccessTokens(): Promise<void> {
  const store = await getStore();
  await store.delete('access_token');
  await store.delete('access_token_exp');
}

/**
 * Clear access tokens from Tauri store AND refresh token from OS keychain.
 * Preserves device identity fields (device_id, server_url, display_name, insecure_tls).
 * Used after non-recoverable failures (401, 400) or missing/corrupt keychain token.
 */
export async function clearSessionFull(): Promise<void> {
  const store = await getStore();
  await store.delete('access_token');
  await store.delete('access_token_exp');
  try {
    await invoke('delete_token', { key: 'wavis_refresh_token' });
  } catch (err) {
    console.error(LOG_PREFIX, 'Failed to delete refresh token from keychain:', err);
  }
}

// --- Logout (exported) ---

/**
 * Logout: clears tokens and device_id but preserves server_url, display_name,
 * and insecure_tls so the login page can pre-fill them.
 */
export async function logout(): Promise<void> {
  const store = await getStore();
  await store.delete('access_token');
  await store.delete('access_token_exp');
  await store.delete('device_id');
  try {
    await invoke('delete_token', { key: 'wavis_refresh_token' });
  } catch (err) {
    console.error(LOG_PREFIX, 'Failed to delete refresh token from keychain:', err);
  }
}

// --- Reset (exported) ---

export async function resetAuth(): Promise<void> {
  const store = await getStore();
  await store.delete('server_url');
  await store.delete('device_id');
  await store.delete('access_token');
  await store.delete('access_token_exp');
  await store.delete('insecure_tls');
  await store.delete('display_name');
  try {
    await invoke('delete_token', { key: 'wavis_refresh_token' });
  } catch (err) {
    console.error(LOG_PREFIX, 'Failed to delete refresh token from keychain:', err);
  }
}
