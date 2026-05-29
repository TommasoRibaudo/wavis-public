import { useState, useRef, useEffect, useCallback } from 'react';
import { useNavigate } from 'react-router';
import {
  type AuthLogEntry,
  validateServerUrl,
  recoverAccount,
  setUsername,
  getServerUrl,
  getUsername,
  getInsecureTls,
  getStoredRecoveryId,
  INSECURE_TLS_ALLOWED,
} from './auth';
import { logEntryColor } from './authLog';
import { AuthFieldRow } from './AuthFieldRow';
import { AuthShell } from './AuthShell';

const MIN_PHRASE_LENGTH = 4;

export function deriveLoginMode(storedId: string | null): 'trusted' | 'new-device' {
  return storedId !== null ? 'trusted' : 'new-device';
}

export default function Login() {
  const navigate = useNavigate();
  const inputRef = useRef<HTMLInputElement>(null);

  const [serverUrl, setServerUrl] = useState('');
  const [recoveryId, setRecoveryId] = useState('');
  const [phrase, setPhrase] = useState('');
  const [username, setUsernameValue] = useState('');
  const [insecureTls, setInsecureTls] = useState(false);
  const [logging, setLogging] = useState(false);
  const [logs, setLogs] = useState<AuthLogEntry[]>([]);
  const [urlError, setUrlError] = useState<string | null>(null);
  const [recoveryIdError, setRecoveryIdError] = useState<string | null>(null);
  const [phraseError, setPhraseError] = useState<string | null>(null);
  const [loginError, setLoginError] = useState<string | null>(null);
  const [showRetry, setShowRetry] = useState(false);
  const [loaded, setLoaded] = useState(false);
  const [trustedDevice, setTrustedDevice] = useState(false);

  useEffect(() => {
    let cancelled = false;
    (async () => {
      const [url, name, insecure, storedId] = await Promise.all([
        getServerUrl(),
        getUsername(),
        getInsecureTls(),
        getStoredRecoveryId(),
      ]);
      if (cancelled) return;
      if (url) setServerUrl(url);
      if (name) setUsernameValue(name);
      if (storedId) setRecoveryId(storedId);
      setInsecureTls(insecure);
      setTrustedDevice(deriveLoginMode(storedId) === 'trusted');
      setLoaded(true);
    })();
    return () => {
      cancelled = true;
    };
  }, []);

  useEffect(() => {
    if (loaded) inputRef.current?.focus();
  }, [loaded, trustedDevice]);

  const handleSubmit = useCallback(async () => {
    if (logging) return;

    setUrlError(null);
    setRecoveryIdError(null);
    setPhraseError(null);
    setLoginError(null);
    setShowRetry(false);

    const trimmedRecoveryId = recoveryId.trim();
    if (!trimmedRecoveryId) {
      setRecoveryIdError('Wavis ID is required');
      return;
    }

    if (phrase.length < MIN_PHRASE_LENGTH) {
      setPhraseError(`Password must be at least ${MIN_PHRASE_LENGTH} characters`);
      return;
    }

    const validation = validateServerUrl(serverUrl, insecureTls);
    if (!validation.valid) {
      setUrlError(validation.reason ?? 'Invalid server URL');
      return;
    }

    const nameToUse = username.trim() || 'user';

    setLogging(true);
    setLogs([]);

    const result = await recoverAccount(serverUrl, trimmedRecoveryId, phrase, nameToUse, insecureTls, (entry) => {
      setLogs((prev) => [...prev, entry]);
    });

    setLogging(false);
    setPhrase('');

    if (result.success) {
      await setUsername(nameToUse);
      navigate('/', { replace: true });
      return;
    }

    const err = result.error ?? '';
    if (err.includes('too many requests')) {
      setLoginError('too many requests - try again later');
    } else if (err.includes('Recovery failed')) {
      setLoginError('Login failed - check your Wavis ID and password');
    } else {
      setLoginError('Login failed - please try again');
      setShowRetry(true);
    }
  }, [serverUrl, recoveryId, phrase, username, insecureTls, logging, navigate]);

  const handleKeyDown = useCallback(
    (e: React.KeyboardEvent<HTMLInputElement>) => {
      if (e.key === 'Enter') {
        void handleSubmit();
      } else if (e.key === 'Escape') {
        setLoginError(null);
        setShowRetry(false);
      }
    },
    [handleSubmit],
  );

  if (!loaded) {
    return (
      <div className="h-screen flex items-center justify-center bg-wavis-bg font-mono text-wavis-text-secondary">
        loading...
      </div>
    );
  }

  return (
    <AuthShell
      subtitle={
        <div className="mt-2 text-wavis-text-secondary text-sm">
          {trustedDevice
            ? 'Welcome back - enter your password to continue'
            : 'Log in on a new device'}
        </div>
      }
    >
      <div className="px-4 sm:px-6 py-4">
        {!trustedDevice && (
          <AuthFieldRow
            label="Wavis ID:"
            placeholder="wvs-XXXX-XXXX"
            value={recoveryId}
            onChange={(value) => {
              setRecoveryId(value);
              setRecoveryIdError(null);
            }}
            onKeyDown={handleKeyDown}
            error={recoveryIdError}
            disabled={logging}
            inputRef={inputRef}
            autoFocus
          />
        )}

        <AuthFieldRow
          label="Password:"
          type="password"
          placeholder="your password"
          value={phrase}
          onChange={(value) => {
            setPhrase(value);
            setPhraseError(null);
          }}
          onKeyDown={handleKeyDown}
          error={phraseError}
          disabled={logging}
          inputRef={trustedDevice ? inputRef : undefined}
          autoFocus={trustedDevice}
        />

        {!trustedDevice && (
          <>
            <AuthFieldRow
              label="Server:"
              placeholder="https://wavis.example.com"
              value={serverUrl}
              onChange={(value) => {
                setServerUrl(value);
                setUrlError(null);
              }}
              onKeyDown={handleKeyDown}
              error={urlError}
              disabled={logging}
              ariaLabel="Server URL"
            />

            {INSECURE_TLS_ALLOWED && (
              <div className="mb-6">
                <label className="flex items-center gap-3 text-sm cursor-pointer select-none">
                  <span
                    className="border px-2 py-0.5 text-xs"
                    style={{
                      borderColor: insecureTls ? 'var(--wavis-warn)' : 'var(--wavis-text-secondary)',
                      color: insecureTls ? 'var(--wavis-warn)' : 'var(--wavis-text-secondary)',
                      backgroundColor: insecureTls ? 'rgba(255, 166, 87, 0.08)' : 'transparent',
                    }}
                  >
                    {insecureTls ? 'ON' : 'OFF'}
                  </span>
                  <span className={insecureTls ? 'text-wavis-warn' : 'text-wavis-text-secondary'}>
                    --danger-insecure-tls
                  </span>
                  <input
                    type="checkbox"
                    checked={insecureTls}
                    onChange={(e) => setInsecureTls(e.target.checked)}
                    disabled={logging}
                    className="sr-only"
                  />
                </label>
                {insecureTls && (
                  <p className="text-wavis-warn text-xs mt-2 pl-6">
                    TLS certificate verification disabled. Do not use in production.
                  </p>
                )}
              </div>
            )}
          </>
        )}

        <button
          onClick={() => { void handleSubmit(); }}
          disabled={logging}
          className="border border-wavis-accent text-wavis-accent hover:bg-wavis-accent hover:text-wavis-bg transition-colors px-6 py-2 disabled:opacity-40 disabled:cursor-not-allowed"
        >
          {logging ? 'logging in...' : '/login'}
        </button>

        {loginError && (
          <div className="mt-4">
            <p className="text-wavis-danger text-sm">{loginError}</p>
            {showRetry && (
              <button
                onClick={() => { void handleSubmit(); }}
                disabled={logging}
                className="border border-wavis-danger text-wavis-danger hover:bg-wavis-danger hover:text-wavis-bg transition-colors px-3 py-0.5 mt-2 text-sm disabled:opacity-40 disabled:cursor-not-allowed"
              >
                /retry
              </button>
            )}
          </div>
        )}
      </div>

      {logs.length > 0 && (
        <div className="border-t border-wavis-text-secondary px-4 sm:px-6 py-4">
          <div className="mb-2 font-bold text-sm">LOGIN LOG</div>
          <div className="overflow-y-auto space-y-0.5 text-sm" style={{ maxHeight: '240px' }}>
            {logs.map((entry, i) => (
              <div key={i} style={{ color: logEntryColor(entry.type) }}>
                [{entry.time}] {entry.message}
              </div>
            ))}
          </div>
        </div>
      )}

      <div className="px-4 sm:px-6 py-3 border-t border-wavis-text-secondary text-wavis-text-secondary text-xs flex items-center justify-between gap-3">
        <span>
          {trustedDevice
            ? 'Trusted device login'
            : 'Log in with your Wavis ID and password'}
        </span>
        {trustedDevice ? (
          <button
            onClick={() => {
              setTrustedDevice(false);
              setRecoveryId('');
              setRecoveryIdError(null);
            }}
            className="hover:text-wavis-text transition-colors text-right"
          >
            Not you / log in on a new device
          </button>
        ) : (
          <button
            onClick={() => navigate('/setup')}
            className="hover:text-wavis-text transition-colors"
          >
            /setup
          </button>
        )}
      </div>
    </AuthShell>
  );
}
