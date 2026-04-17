import { beforeEach, describe, expect, it, vi } from 'vitest';
import { ApiError, apiFetch, apiPublicFetch, classifyError } from '../api';

const {
  tauriFetchMock,
  getServerUrlMock,
  getAccessTokenMock,
  isTokenExpiredMock,
  getInsecureTlsMock,
  refreshTokensMock,
} = vi.hoisted(() => ({
  tauriFetchMock: vi.fn(),
  getServerUrlMock: vi.fn(),
  getAccessTokenMock: vi.fn(),
  isTokenExpiredMock: vi.fn(),
  getInsecureTlsMock: vi.fn(),
  refreshTokensMock: vi.fn(),
}));

vi.mock('@tauri-apps/plugin-http', () => ({
  fetch: tauriFetchMock,
}));

vi.mock('@features/auth/auth', () => ({
  getServerUrl: getServerUrlMock,
  getAccessToken: getAccessTokenMock,
  isTokenExpired: isTokenExpiredMock,
  refreshTokens: refreshTokensMock,
  getInsecureTls: getInsecureTlsMock,
}));

// ─── classifyError ──────────────────────────────────────────────────

describe('classifyError', () => {
  it('maps 429 to RateLimited', () => {
    expect(classifyError(429, '')).toBe('RateLimited');
  });

  it('maps 401 to Unauthorized', () => {
    expect(classifyError(401, '')).toBe('Unauthorized');
  });

  it('maps 403 to Forbidden', () => {
    expect(classifyError(403, '')).toBe('Forbidden');
  });

  it('maps 404 to NotFound', () => {
    expect(classifyError(404, '')).toBe('NotFound');
  });

  it('maps 400 with "invalid invite" body to InvalidInvite', () => {
    expect(classifyError(400, 'Invalid invite code')).toBe('InvalidInvite');
  });

  it('maps 409 with "already banned" body to AlreadyBanned', () => {
    expect(classifyError(409, 'User is already banned')).toBe('AlreadyBanned');
  });

  it('maps 409 with "already a member" body to AlreadyMember', () => {
    expect(classifyError(409, 'Already a member of this channel')).toBe('AlreadyMember');
  });

  it('maps status 0 to Network', () => {
    expect(classifyError(0, '')).toBe('Network');
  });

  it('maps unknown status to Unknown', () => {
    expect(classifyError(500, 'internal error')).toBe('Unknown');
    expect(classifyError(502, '')).toBe('Unknown');
  });

  it('is case-insensitive for body matching', () => {
    expect(classifyError(400, 'INVALID INVITE')).toBe('InvalidInvite');
    expect(classifyError(409, 'ALREADY BANNED')).toBe('AlreadyBanned');
  });
});

// ─── Server error message surfacing ─────────────────────────────────

function mockOkResponse(body: string) {
  return {
    status: 200,
    ok: true,
    text: () => Promise.resolve(body),
  };
}

function mockErrorResponse(status: number, body: string) {
  return {
    status,
    ok: false,
    text: () => Promise.resolve(body),
  };
}

describe('apiFetch — server error message surfacing', () => {
  beforeEach(() => {
    getServerUrlMock.mockResolvedValue('http://localhost:8080');
    getAccessTokenMock.mockResolvedValue('test-token');
    isTokenExpiredMock.mockResolvedValue(false);
    getInsecureTlsMock.mockResolvedValue(false);
    refreshTokensMock.mockResolvedValue(false);
  });

  it('surfaces the server error field for a 400 Unknown error', async () => {
    tauriFetchMock.mockResolvedValue(
      mockErrorResponse(400, JSON.stringify({ error: 'body too long' })),
    );

    await expect(apiFetch('/bug-report', { method: 'POST', body: '{}' })).rejects.toMatchObject({
      message: 'body too long',
      status: 400,
      kind: 'Unknown',
    });
  });

  it('surfaces the server error field for a 500 Unknown error', async () => {
    tauriFetchMock.mockResolvedValue(
      mockErrorResponse(500, JSON.stringify({ error: 'screenshot upload failed' })),
    );

    await expect(apiFetch('/bug-report', { method: 'POST', body: '{}' })).rejects.toMatchObject({
      message: 'screenshot upload failed',
      status: 500,
      kind: 'Unknown',
    });
  });

  it('falls back to "Request failed" when response body has no error or message field', async () => {
    tauriFetchMock.mockResolvedValue(
      mockErrorResponse(400, JSON.stringify({ detail: 'something else' })),
    );

    await expect(apiFetch('/bug-report', { method: 'POST', body: '{}' })).rejects.toMatchObject({
      message: 'Request failed',
      status: 400,
    });
  });

  it('surfaces plain-text error bodies when response body is not JSON', async () => {
    tauriFetchMock.mockResolvedValue(mockErrorResponse(400, 'plain text error'));

    await expect(apiFetch('/bug-report', { method: 'POST', body: '{}' })).rejects.toMatchObject({
      message: 'plain text error',
      status: 400,
    });
  });

  it('falls back to "Request failed" when response body is empty', async () => {
    tauriFetchMock.mockResolvedValue(mockErrorResponse(400, ''));

    await expect(apiFetch('/bug-report', { method: 'POST', body: '{}' })).rejects.toMatchObject({
      message: 'Request failed',
      status: 400,
    });
  });

  it('surfaces the Axum "message" field (built-in rejection format)', async () => {
    tauriFetchMock.mockResolvedValue(
      mockErrorResponse(400, JSON.stringify({ message: 'Failed to deserialize the JSON body into the target type: missing field `title`' })),
    );

    await expect(apiFetch('/bug-report', { method: 'POST', body: '{}' })).rejects.toMatchObject({
      message: 'Failed to deserialize the JSON body into the target type: missing field `title`',
      status: 400,
      kind: 'Unknown',
    });
  });

  it('prefers "error" field over "message" field when both present', async () => {
    tauriFetchMock.mockResolvedValue(
      mockErrorResponse(400, JSON.stringify({ error: 'body too long', message: 'other message' })),
    );

    await expect(apiFetch('/bug-report', { method: 'POST', body: '{}' })).rejects.toMatchObject({
      message: 'body too long',
      status: 400,
    });
  });

  it('still uses well-known messages for known error kinds (not overridden by server body)', async () => {
    tauriFetchMock.mockResolvedValue(
      mockErrorResponse(429, JSON.stringify({ error: 'custom rate limit message' })),
    );

    await expect(apiFetch('/test', {})).rejects.toMatchObject({
      message: 'too many requests — try again later',
      kind: 'RateLimited',
    });
  });

  it('throws ApiError with correct shape', async () => {
    tauriFetchMock.mockResolvedValue(
      mockErrorResponse(400, JSON.stringify({ error: 'title too long' })),
    );

    let thrown: unknown;
    try {
      await apiFetch('/bug-report', { method: 'POST', body: '{}' });
    } catch (e) {
      thrown = e;
    }

    expect(thrown).toBeInstanceOf(ApiError);
    expect((thrown as ApiError).status).toBe(400);
    expect((thrown as ApiError).kind).toBe('Unknown');
    expect((thrown as ApiError).message).toBe('title too long');
  });

  it('returns parsed JSON on success', async () => {
    tauriFetchMock.mockResolvedValue(
      mockOkResponse(JSON.stringify({ issue_url: 'https://github.com/test/issues/1' })),
    );

    const result = await apiFetch<{ issue_url: string }>('/bug-report', {
      method: 'POST',
      body: '{}',
    });
    expect(result).toEqual({ issue_url: 'https://github.com/test/issues/1' });
  });

  it('sends string request bodies to Tauri as UTF-8 bytes', async () => {
    tauriFetchMock.mockResolvedValue(mockOkResponse(JSON.stringify({ ok: true })));

    const body = JSON.stringify({ screenshot: 'check ✓' });
    await apiFetch<{ ok: boolean }>('/bug-report', {
      method: 'POST',
      body,
    });

    const [, init] = tauriFetchMock.mock.calls[tauriFetchMock.mock.calls.length - 1] as [string, RequestInit];
    expect(init.body).toBeInstanceOf(Uint8Array);
    expect(Array.from(init.body as Uint8Array)).toEqual(Array.from(new TextEncoder().encode(body)));
    expect((init.headers as Headers).get('Authorization')).toBe('Bearer test-token');
    expect((init.headers as Headers).get('Content-Type')).toBe('application/json');
  });
});

describe('apiPublicFetch — server error message surfacing', () => {
  beforeEach(() => {
    getServerUrlMock.mockResolvedValue('http://localhost:8080');
    getInsecureTlsMock.mockResolvedValue(false);
  });

  it('surfaces the server error field for a 400 Unknown error', async () => {
    tauriFetchMock.mockResolvedValue(
      mockErrorResponse(400, JSON.stringify({ error: 'invalid screenshot encoding' })),
    );

    await expect(
      apiPublicFetch('/bug-report', { method: 'POST', body: '{}' }),
    ).rejects.toMatchObject({
      message: 'invalid screenshot encoding',
      status: 400,
      kind: 'Unknown',
    });
  });

  it('surfaces the Axum "message" field (built-in rejection format)', async () => {
    tauriFetchMock.mockResolvedValue(
      mockErrorResponse(400, JSON.stringify({ message: 'Failed to deserialize the JSON body into the target type: missing field `title`' })),
    );

    await expect(
      apiPublicFetch('/bug-report', { method: 'POST', body: '{}' }),
    ).rejects.toMatchObject({
      message: 'Failed to deserialize the JSON body into the target type: missing field `title`',
      status: 400,
      kind: 'Unknown',
    });
  });

  it('falls back to "Request failed" when body has no error or message field', async () => {
    tauriFetchMock.mockResolvedValue(
      mockErrorResponse(400, JSON.stringify({ detail: 'something' })),
    );

    await expect(
      apiPublicFetch('/bug-report', { method: 'POST', body: '{}' }),
    ).rejects.toMatchObject({
      message: 'Request failed',
      status: 400,
    });
  });

  it('surfaces plain-text error bodies when body is not JSON', async () => {
    tauriFetchMock.mockResolvedValue(mockErrorResponse(400, 'bad request'));

    await expect(
      apiPublicFetch('/bug-report', { method: 'POST', body: '{}' }),
    ).rejects.toMatchObject({
      message: 'bad request',
      status: 400,
    });
  });

  it('still uses well-known messages for known error kinds', async () => {
    tauriFetchMock.mockResolvedValue(
      mockErrorResponse(404, JSON.stringify({ error: 'resource not found' })),
    );

    await expect(apiPublicFetch('/test', {})).rejects.toMatchObject({
      message: 'not found',
      kind: 'NotFound',
    });
  });

  it('sends public string request bodies to Tauri as UTF-8 bytes', async () => {
    tauriFetchMock.mockResolvedValue(mockOkResponse(JSON.stringify({ ok: true })));

    const body = JSON.stringify({ screenshot: 'public ✓' });
    await apiPublicFetch<{ ok: boolean }>('/bug-report', {
      method: 'POST',
      body,
    });

    const [, init] = tauriFetchMock.mock.calls[tauriFetchMock.mock.calls.length - 1] as [string, RequestInit];
    expect(init.body).toBeInstanceOf(Uint8Array);
    expect(Array.from(init.body as Uint8Array)).toEqual(Array.from(new TextEncoder().encode(body)));
    expect((init.headers as Headers).get('Authorization')).toBeNull();
    expect((init.headers as Headers).get('Content-Type')).toBe('application/json');
  });
});
