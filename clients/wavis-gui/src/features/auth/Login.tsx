import { useState, useRef, useEffect, useCallback } from 'react';
import { useNavigate } from 'react-router';
import {
  type AuthLogEntry,
  validateServerUrl,
  recoverAccount,
  setDisplayName,
  getServerUrl,
  getDisplayName,
  getInsecureTls,
  INSECURE_TLS_ALLOWED,
} from './auth';
import { logEntryColor } from './authLog';
import { AuthFieldRow } from './AuthFieldRow';
import { AuthShell } from './AuthShell';

const MIN_PHRASE_LENGTH = 4;

export default function Login() {
  const navigate = useNavigate();
  const inputRef = useRef<HTMLInputElement>(null);

  const [serverUrl, setServerUrl] = useState('');
  const [recoveryId, setRecoveryId] = useState('');
  const [phrase, setPhrase] = useState('');
  const [deviceName, setDeviceName] = useState('');
  const [insecureTls, setInsecureTls] = useState(false);
  const [logging, setLogging] = useState(false);
  const [logs, setLogs] = useState<AuthLogEntry[]>([]);
  const [urlError, setUrlError] = useState<string | null>(null);
  const [recoveryIdError, setRecoveryIdError] = useState<string | null>(null);
  const [phraseError, setPhraseError] = useState<string | null>(null);
  const [loginError, setLoginError] = useState<string | null>(null);
  const [showRetry, setShowRetry] = useState(false);
  const [loaded, setLoaded] = useState(false);

  useEffect(() => {
    let cancelled = false;
    (async () => {
      const [url, name, insecure] = await Promise.all([
        getServerUrl(),
        getDisplayName(),
        getInsecureTls(),
      ]);
      if (cancelled) return;
      if (url) setServerUrl(url);
      if (name) setDeviceName(name);
      setInsecureTls(insecure);
      setLoaded(true);
    })();
    return () => {
      cancelled = true;
    };
  }, []);

  useEffect(() => {
    if (loaded) inputRef.current?.focus();
  }, [loaded]);

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

    const nameToUse = deviceName.trim() || 'device';

    setLogging(true);
    setLogs([]);

    const result = await recoverAccount(serverUrl, trimmedRecoveryId, phrase, nameToUse, insecureTls, (entry) => {
      setLogs((prev) => [...prev, entry]);
    });

    setLogging(false);
    setPhrase('');

    if (result.success) {
      await setDisplayName(nameToUse);
      navigate('/', { replace: true });
      return;
    }

    const err = result.error ?? '';
    if (err.includes('too many requests')) {
      setLoginError('too many requests — try again later');
    } else if (err.includes('Recovery failed')) {
      setLoginError('Login failed — check your Wavis ID and password');
    } else {
      setLoginError('Login failed — please try again');
      setShowRetry(true);
    }
  }, [serverUrl, recoveryId, phrase, deviceName, insecureTls, logging, navigate]);

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
      subtitle={<div className="mt-2 text-wavis-text-secondary text-sm">Log in to your account</div>}
    >
      <div className="px-4 sm:px-6 py-4">
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
        />

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
                TLS certificate verification disabled. Do NOT use in production.
              </p>
            )}
          </div>
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

      <div className="px-4 sm:px-6 py-3 border-t border-wavis-text-secondary text-wavis-text-secondary text-xs flex items-center justify-between">
        <span>Log in with your Wavis ID and password</span>
        <button
          onClick={() => navigate('/setup')}
          className="hover:text-wavis-text transition-colors"
        >
          ← /setup
        </button>
      </div>
    </AuthShell>
  );
}
