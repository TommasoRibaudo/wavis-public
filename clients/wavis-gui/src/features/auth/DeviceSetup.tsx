import { useState, useEffect, useRef, useCallback } from 'react';
import { useNavigate } from 'react-router';
import {
  type AuthLogEntry,
  validateServerUrl,
  registerUser,
  setDisplayName,
  INSECURE_TLS_ALLOWED,
} from './auth';
import { logEntryColor } from './authLog';
import { useCopyToClipboardFeedback } from '@shared/hooks/useCopyToClipboardFeedback';
import { AuthFieldRow } from './AuthFieldRow';
import { AuthShell } from './AuthShell';

const MIN_PHRASE_LENGTH = 4;

export default function DeviceSetup() {
  const navigate = useNavigate();
  const inputRef = useRef<HTMLInputElement>(null);

  const [serverUrl, setServerUrl] = useState('');
  const [displayName, setDisplayNameValue] = useState('');
  const [phrase, setPhrase] = useState('');
  const [insecureTls, setInsecureTls] = useState(false);
  const [registering, setRegistering] = useState(false);
  const [logs, setLogs] = useState<AuthLogEntry[]>([]);
  const [urlError, setUrlError] = useState<string | null>(null);
  const [nameError, setNameError] = useState<string | null>(null);
  const [phraseError, setPhraseError] = useState<string | null>(null);
  const [registerError, setRegisterError] = useState<string | null>(null);
  const [showRetry, setShowRetry] = useState(false);

  const [recoveryId, setRecoveryId] = useState<string | null>(null);
  const [copy, copied] = useCopyToClipboardFeedback();
  const [confirmed, setConfirmed] = useState(false);

  useEffect(() => {
    inputRef.current?.focus();
  }, []);

  const handleContinue = useCallback(async () => {
    const trimmedName = displayName.trim();
    if (trimmedName) {
      await setDisplayName(trimmedName);
    }
    navigate('/', { replace: true });
  }, [displayName, navigate]);

  const handleSubmit = useCallback(async () => {
    if (registering) return;

    setUrlError(null);
    setNameError(null);
    setPhraseError(null);
    setRegisterError(null);
    setShowRetry(false);

    const trimmedName = displayName.trim();
    if (!trimmedName || trimmedName.length < 1) {
      setNameError('display name is required');
      return;
    }
    if (trimmedName.length > 32) {
      setNameError('display name must be 32 characters or less');
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

    setRegistering(true);
    setLogs([]);

    const result = await registerUser(serverUrl, phrase, trimmedName, insecureTls, (entry) => {
      setLogs((prev) => [...prev, entry]);
    });

    setRegistering(false);
    setPhrase('');

    if (result.success) {
      setRecoveryId(result.recovery_id);
      return;
    }

    const err = result.error ?? '';
    if (err.includes('too many requests')) {
      setRegisterError('too many requests — try again later');
    } else {
      setRegisterError('Registration failed — please try again');
      setShowRetry(true);
    }
  }, [serverUrl, displayName, phrase, insecureTls, registering]);

  const handleKeyDown = useCallback(
    (e: React.KeyboardEvent<HTMLInputElement>) => {
      if (e.key === 'Enter') {
        void handleSubmit();
      } else if (e.key === 'Escape') {
        setServerUrl('');
        setUrlError(null);
        setRegisterError(null);
        setShowRetry(false);
      }
    },
    [handleSubmit],
  );

  if (recoveryId) {
    return (
      <AuthShell
        subtitle={<div className="mt-2 text-wavis-accent text-sm">Registration successful</div>}
      >
        <div className="px-4 sm:px-6 py-4">
          <div className="mb-4">
            <div className="mb-2 text-sm font-bold">YOUR RECOVERY ID</div>
            <div className="flex items-center gap-2 p-3 border border-wavis-text-secondary bg-wavis-bg">
              <code className="flex-1 text-wavis-accent text-lg tracking-wider select-all">
                {recoveryId}
              </code>
              <button
                onClick={() => { void copy(recoveryId); }}
                className="border border-wavis-text-secondary text-wavis-text-secondary hover:border-wavis-accent hover:text-wavis-accent transition-colors px-3 py-1 text-sm shrink-0"
              >
                {copied ? 'Copied!' : '/copy'}
              </button>
            </div>
          </div>

          <div className="mb-6">
            <p className="text-wavis-warn text-sm">
              Save this Recovery ID. You will need it to recover your account on a new device.
            </p>
          </div>

          <label className="flex items-center gap-3 text-sm cursor-pointer select-none mb-6">
            <span
              className="border px-2 py-0.5 text-xs"
              style={{
                borderColor: confirmed ? 'var(--wavis-accent)' : 'var(--wavis-text-secondary)',
                color: confirmed ? 'var(--wavis-accent)' : 'var(--wavis-text-secondary)',
                backgroundColor: confirmed ? 'rgba(46, 160, 67, 0.08)' : 'transparent',
              }}
            >
              {confirmed ? '✓' : ' '}
            </span>
            <span className={confirmed ? 'text-wavis-text' : 'text-wavis-text-secondary'}>
              I have saved my Recovery ID
            </span>
            <input
              type="checkbox"
              checked={confirmed}
              onChange={(e) => setConfirmed(e.target.checked)}
              className="sr-only"
            />
          </label>

          {confirmed && (
            <button
              onClick={() => { void handleContinue(); }}
              className="border border-wavis-accent text-wavis-accent hover:bg-wavis-accent hover:text-wavis-bg transition-colors px-6 py-2"
            >
              /continue
            </button>
          )}
        </div>

        <div className="px-4 sm:px-6 py-3 border-t border-wavis-text-secondary text-wavis-text-secondary text-xs">
          Recovery ID stored in keychain · Keep a backup copy safe
        </div>
      </AuthShell>
    );
  }

  return (
    <AuthShell
      subtitle={<div className="mt-2 text-wavis-text-secondary text-sm">First-launch device registration</div>}
    >
      <div className="px-4 sm:px-6 py-4">
        <AuthFieldRow
          label="Display name:"
          placeholder="your name"
          value={displayName}
          onChange={(value) => {
            setDisplayNameValue(value);
            setNameError(null);
          }}
          onKeyDown={handleKeyDown}
          error={nameError}
          disabled={registering}
          maxLength={32}
          inputRef={inputRef}
          autoFocus
        />

        <AuthFieldRow
          label="Password:"
          type="password"
          placeholder="at least 4 characters"
          value={phrase}
          onChange={(value) => {
            setPhrase(value);
            setPhraseError(null);
          }}
          onKeyDown={handleKeyDown}
          error={phraseError}
          disabled={registering}
        />

        <AuthFieldRow
          label="Server URL:"
          placeholder="https://wavis.example.com"
          value={serverUrl}
          onChange={(value) => {
            setServerUrl(value);
            setUrlError(null);
          }}
          onKeyDown={handleKeyDown}
          error={urlError}
          disabled={registering}
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
                disabled={registering}
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
          disabled={registering}
          className="border border-wavis-accent text-wavis-accent hover:bg-wavis-accent hover:text-wavis-bg transition-colors px-6 py-2 disabled:opacity-40 disabled:cursor-not-allowed"
        >
          {registering ? 'registering...' : '/register'}
        </button>

        {registerError && (
          <div className="mt-4">
            <p className="text-wavis-danger text-sm">{registerError}</p>
            {showRetry && (
              <button
                onClick={() => { void handleSubmit(); }}
                disabled={registering}
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
          <div className="mb-2 font-bold text-sm">REGISTRATION LOG</div>
          <div className="overflow-y-auto space-y-0.5 text-sm" style={{ maxHeight: '240px' }}>
            {logs.map((entry, i) => (
              <div key={i} style={{ color: logEntryColor(entry.type) }}>
                [{entry.time}] {entry.message}
              </div>
            ))}
          </div>
        </div>
      )}

      <div className="px-4 sm:px-6 py-3 border-t border-wavis-text-secondary text-wavis-text-secondary text-xs">
        Tokens stored in local keychain · Auto-refresh · Device ID persisted
      </div>

      <div className="px-4 sm:px-6 py-3 border-t border-wavis-text-secondary text-sm">
        <button
          onClick={() => navigate('/recover')}
          disabled={registering}
          className="text-wavis-text-secondary hover:text-wavis-accent transition-colors disabled:opacity-40"
        >
          /recover-account
        </button>
      </div>
    </AuthShell>
  );
}
