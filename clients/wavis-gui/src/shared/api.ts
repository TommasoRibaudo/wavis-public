/**
 * Wavis API Client (Tauri)
 *
 * Authenticated fetch wrapper using tauri-plugin-http.
 * Auto-attaches Bearer token, handles 401 → silent refresh → retry.
 * Classifies errors into typed ApiErrorKind for component-level handling.
 */

import { fetch as tauriFetch } from '@tauri-apps/plugin-http';
import {
  getAccessToken,
  getInsecureTls,
  getServerUrl,
  isTokenExpired,
  refreshTokens,
} from '@features/auth/auth';

// ─── Error Classification ──────────────────────────────────────────

export type ApiErrorKind =
  | 'RateLimited'
  | 'Unauthorized'
  | 'Forbidden'
  | 'NotFound'
  | 'InvalidInvite'
  | 'AlreadyMember'
  | 'AlreadyBanned'
  | 'Network'
  | 'Unknown';

export class ApiError extends Error {
  constructor(
    public readonly status: number,
    message: string,
    public readonly kind: ApiErrorKind,
  ) {
    super(message);
    this.name = 'ApiError';
  }
}

// ─── Helpers (private) ─────────────────────────────────────────────

export function classifyError(status: number, body: string): ApiErrorKind {
  if (status === 429) return 'RateLimited';
  if (status === 401) return 'Unauthorized';
  if (status === 403) return 'Forbidden';
  if (status === 404) return 'NotFound';
  const bodyLower = body.toLowerCase();
  if (status === 400 && bodyLower.includes('invalid invite')) return 'InvalidInvite';
  if (status === 409 && bodyLower.includes('already banned')) return 'AlreadyBanned';
  if (status === 409 && bodyLower.includes('already a member')) return 'AlreadyMember';
  if (status === 0) return 'Network';
  return 'Unknown';
}

async function doFetch(
  endpoint: string,
  init: RequestInit,
  insecure: boolean,
): Promise<Response> {
  const fetchOpts: RequestInit & { dangerouslyIgnoreCertificateErrors?: boolean } = {
    ...init,
  };
  // Encode string bodies to UTF-8 bytes. Tauri's IPC truncates large string
  // payloads (~213 KB), but routes Uint8Array bodies through a binary channel
  // that has no such limit. This also ensures Content-Length reflects actual
  // byte count rather than char count (they differ for multi-byte Unicode).
  if (typeof fetchOpts.body === 'string') {
    fetchOpts.body = new TextEncoder().encode(fetchOpts.body) as unknown as BodyInit;
  }
  if (insecure) {
    fetchOpts.dangerouslyIgnoreCertificateErrors = true;
  }
  return tauriFetch(endpoint, fetchOpts);
}

// ─── API Functions (exported) ──────────────────────────────────────

/**
 * Authenticated fetch wrapper.
 * - Attaches Authorization: Bearer + Content-Type: application/json headers
 * - Pre-request: if token expired, refreshTokens() first
 * - On 401: single retry after refreshTokens()
 * - Classifies all errors into ApiErrorKind
 * - Token values never included in error messages
 */
export async function apiFetch<T = unknown>(
  path: string,
  init: RequestInit = {},
): Promise<T> {
  const serverUrl = await getServerUrl();
  if (!serverUrl) throw new ApiError(0, 'Server URL not configured', 'Network');

  // Pre-request refresh if token expired
  if (await isTokenExpired()) {
    const ok = await refreshTokens();
    if (!ok) throw new ApiError(401, 'Session expired', 'Unauthorized');
  }

  const insecure = await getInsecureTls();
  const endpoint = serverUrl.replace(/\/+$/, '') + path;

  const makeHeaders = async (): Promise<Headers> => {
    const headers = new Headers(init.headers);
    const token = await getAccessToken();
    if (token) headers.set('Authorization', `Bearer ${token}`);
    headers.set('Content-Type', 'application/json');
    return headers;
  };

  let res: Response;
  try {
    const headers = await makeHeaders();
    res = await doFetch(endpoint, { ...init, headers }, insecure);
  } catch {
    throw new ApiError(0, 'Network error — could not reach server', 'Network');
  }

  // 401 retry: single refresh + replay
  if (res.status === 401) {
    const refreshed = await refreshTokens();
    if (!refreshed) throw new ApiError(401, 'Session expired', 'Unauthorized');

    try {
      const headers = await makeHeaders();
      res = await doFetch(endpoint, { ...init, headers }, insecure);
    } catch {
      throw new ApiError(0, 'Network error — could not reach server', 'Network');
    }

    if (res.status === 401) {
      throw new ApiError(401, 'Session expired', 'Unauthorized');
    }
  }

  if (!res.ok) {
    const body = await res.text();
    // Log raw server response for debugging unexpected error formats
    console.error('[wavis:api] Server error response', res.status, JSON.stringify(body).slice(0, 500));
    const kind = classifyError(res.status, body);
    let serverError: string | null = null;
    try {
      // Our handlers return {"error":"..."}, Axum built-in rejections return {"message":"..."}
      const parsed = JSON.parse(body) as { error?: string; message?: string };
      if (typeof parsed.error === 'string') serverError = parsed.error;
      else if (typeof parsed.message === 'string') serverError = parsed.message;
    } catch {
      if (body.trim()) serverError = body.trim();
    }
    const message =
      kind === 'RateLimited'
        ? 'too many requests — try again later'
        : kind === 'Forbidden'
          ? "you don't have permission"
          : kind === 'NotFound'
            ? 'not found'
            : kind === 'InvalidInvite'
              ? 'invalid invite code'
              : kind === 'AlreadyMember'
                ? 'already a member of this channel'
                : kind === 'AlreadyBanned'
                  ? 'user is already banned'
                  : (serverError ?? 'Request failed');
    throw new ApiError(res.status, message, kind);
  }

  // Some endpoints (DELETE, POST 204) return no body — avoid JSON parse errors.
  const text = await res.text();
  if (!text) return undefined as T;
  return JSON.parse(text) as T;
}

/**
 * Auth-optional fetch wrapper for anonymous/public endpoints.
 * - Builds server URL and handles TLS settings (same as apiFetch)
 * - Omits Authorization header entirely
 * - Classifies errors into ApiErrorKind
 * - Used for anonymous bug report submissions from unauthenticated users
 */
export async function apiPublicFetch<T = unknown>(
  path: string,
  init: RequestInit = {},
): Promise<T> {
  const serverUrl = await getServerUrl();
  if (!serverUrl) throw new ApiError(0, 'Server URL not configured', 'Network');

  const insecure = await getInsecureTls();
  const endpoint = serverUrl.replace(/\/+$/, '') + path;

  const headers = new Headers(init.headers);
  headers.set('Content-Type', 'application/json');

  let res: Response;
  try {
    res = await doFetch(endpoint, { ...init, headers }, insecure);
  } catch {
    throw new ApiError(0, 'Network error — could not reach server', 'Network');
  }

  if (!res.ok) {
    const body = await res.text();
    // Log raw server response for debugging unexpected error formats
    console.error('[wavis:api] Server error response', res.status, JSON.stringify(body).slice(0, 500));
    const kind = classifyError(res.status, body);
    let serverError: string | null = null;
    try {
      // Our handlers return {"error":"..."}, Axum built-in rejections return {"message":"..."}
      const parsed = JSON.parse(body) as { error?: string; message?: string };
      if (typeof parsed.error === 'string') serverError = parsed.error;
      else if (typeof parsed.message === 'string') serverError = parsed.message;
    } catch {
      if (body.trim()) serverError = body.trim();
    }
    const message =
      kind === 'RateLimited'
        ? 'too many requests — try again later'
        : kind === 'Forbidden'
          ? "you don't have permission"
          : kind === 'NotFound'
            ? 'not found'
            : (serverError ?? 'Request failed');
    throw new ApiError(res.status, message, kind);
  }

  const text = await res.text();
  if (!text) return undefined as T;
  return JSON.parse(text) as T;
}
