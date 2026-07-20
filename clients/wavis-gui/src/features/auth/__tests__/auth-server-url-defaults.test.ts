import { describe, it, expect, vi, afterEach } from 'vitest';

/**
 * DEFAULT_SERVER_URL / SERVER_OVERRIDE_ALLOWED are read from import.meta.env
 * at module load time (same pattern as INSECURE_TLS_ALLOWED), so each case
 * needs vi.resetModules() + vi.stubEnv() + a fresh dynamic import to see a
 * different env combination — see auth.test.ts's "Mocked Integration Tests"
 * header comment for the same pattern applied to store/keychain state.
 */
describe('DEFAULT_SERVER_URL / SERVER_OVERRIDE_ALLOWED', () => {
  afterEach(() => {
    vi.unstubAllEnvs();
  });

  it('DEFAULT_SERVER_URL falls back to empty string when VITE_DEFAULT_SERVER_URL is unset', async () => {
    vi.resetModules();
    vi.stubEnv('VITE_DEFAULT_SERVER_URL', undefined);
    const auth = await import('../auth');

    expect(auth.DEFAULT_SERVER_URL).toBe('');
  });

  it('DEFAULT_SERVER_URL reflects VITE_DEFAULT_SERVER_URL when set', async () => {
    vi.resetModules();
    vi.stubEnv('VITE_DEFAULT_SERVER_URL', 'https://d1d06fp0adg6ml.cloudfront.net');
    const auth = await import('../auth');

    expect(auth.DEFAULT_SERVER_URL).toBe('https://d1d06fp0adg6ml.cloudfront.net');
  });

  it('SERVER_OVERRIDE_ALLOWED defaults to false when VITE_ALLOW_SERVER_OVERRIDE is unset', async () => {
    vi.resetModules();
    vi.stubEnv('VITE_ALLOW_SERVER_OVERRIDE', undefined);
    const auth = await import('../auth');

    expect(auth.SERVER_OVERRIDE_ALLOWED).toBe(false);
  });

  it('SERVER_OVERRIDE_ALLOWED is true only for the exact string "true"', async () => {
    vi.resetModules();
    vi.stubEnv('VITE_ALLOW_SERVER_OVERRIDE', 'yes');
    const authWrongValue = await import('../auth');
    expect(authWrongValue.SERVER_OVERRIDE_ALLOWED).toBe(false);

    vi.resetModules();
    vi.stubEnv('VITE_ALLOW_SERVER_OVERRIDE', 'true');
    const authTrue = await import('../auth');
    expect(authTrue.SERVER_OVERRIDE_ALLOWED).toBe(true);
  });
});
