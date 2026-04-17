import { useState, useRef, useEffect, useCallback } from 'react';
import { useNavigate } from 'react-router';
import {
  type AuthLogEntry,
  validateServerUrl,
  recoverAccount,
  setDisplayName,
  INSECURE_TLS_ALLOWED,
} from './auth';
import { logEntryColor } from './authLog';
import { AuthFieldRow } from './AuthFieldRow';
import { AuthShell } from './AuthShell';

const MIN_PHRASE_LENGTH = 4;
const MAX_DEVICE_NAME_LENGTH = 32;

export default function RecoverAccount() {
  const navigate = useNavigate();
  const inputRef = useRef<HTMLInputElement>(null);

  const [serverUrl, setServerUrl] = useState('');
  const [recoveryId, setRecoveryId] = useState('');
  const [phrase, setPhrase] = useState('');
  const [deviceName, setDeviceName] = useState('');
  const [insecureTls, setInsecureTls] = useState(false);
  const [recovering, setRecovering] = useState(false);
  const [logs, setLogs] = useState<AuthLogEntry[]>([]);
  const [urlError, setUrlError] = useState<string | null>(null);
  const [recoveryIdError, setRecoveryIdError] = useState<string | null>(null);
  const [phraseError, setPhraseError] = useState<string | null>(null);
  const [nameError, setNameError] = useState<string | null>(null);
  const [recoverError, setRecoverError] = useState<string | null>(null);
  const [showRetry, setShowRetry] = useState(false);

  useEffect(() => {
    inputRef.current?.focus();
  }, []);

  const handleSubmit = useCallback(async () => {
    if (recovering) return;

    setUrlError(null);
    setRecoveryIdError(null);
    setPhraseError(null);
    setNameError(null);
    setRecoverError(null);
    setShowRetry(false);

    const trimmedRecoveryId = recoveryId.trim();
    if (!trimmedRecoveryId) {
      setRecoveryIdError('recovery ID is required');
      return;
    }

    if (phrase.length < MIN_PHRASE_LENGTH) {
      setPhraseError(`Password must be at least ${MIN_PHRASE_LENGTH} characters`);
      return;
    }

    const trimmedName = deviceName.trim();
    if (!trimmedName) {
      setNameError('device name is required');
      return;
    }
    if (trimmedName.length > MAX_DEVICE_NAME_LENGTH) {
      setNameError(`device name must be ${MAX_DEVICE_NAME_LENGTH} characters or less`);
      return;
    }

    const validation = validateServerUrl(serverUrl, insecureTls);
    if (!validation.valid) {
      setUrlError(validation.reason ?? 'Invalid server URL');
      return;
    }

    setRecovering(true);
    setLogs([]);

    const result = await recoverAccount(serverUrl, trimmedRecoveryId, phrase, trimmedName, insecureTls, (entry) => {
      setLogs((prev) => [...prev, entry]);
    });

    setRecovering(false);
    setPhrase('');

    if (result.success) {
      await setDisplayName(trimmedName);
      navigate('/', { replace: true });
      return;
    }

    const err = result.error ?? '';
    if (err.includes('too many requests')) {
      setRecoverError('too many requests — try again later');
    } else if (err.includes('Recovery failed')) {
      setRecoverError('Recovery failed — check your Recovery ID and phrase');
    } else {
      setRecoverError('Recovery failed — please try again');
      setShowRetry(true);
    }
  }, [serverUrl, recoveryId, phrase, deviceName, insecureTls, recovering, navigate]);

  const handleKeyDown = useCallback(
    (e: React.KeyboardEvent<HTMLInputElement>) => {
      if (e.key === 'Enter') {
        void handleSubmit();
      } else if (e.key === 'Escape') {
        setRecoverError(null);
        setShowRetry(false);
      }
    },
    [handleSubmit],
  );

  return (
    <AuthShell
      subtitle={<div className="mt-2 text-wavis-text-secondary text-sm">Account recovery</div>}
    >
      <div className="px-4 sm:px-6 py-4">
        <AuthFieldRow
          label="Recovery ID:"
          placeholder="wvs-XXXX-XXXX"
          value={recoveryId}
          onChange={(value) => {
            setRecoveryId(value);
            setRecoveryIdError(null);
          }}
          onKeyDown={handleKeyDown}
          error={recoveryIdError}
          disabled={recovering}
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
          disabled={recovering}
        />

        <AuthFieldRow
          label="Device name:"
          placeholder="my new device"
          value={deviceName}
          onChange={(value) => {
            setDeviceName(value);
            setNameError(null);
          }}
          onKeyDown={handleKeyDown}
          error={nameError}
          disabled={recovering}
          maxLength={MAX_DEVICE_NAME_LENGTH}
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
          disabled={recovering}
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
                disabled={recovering}
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
          disabled={recovering}
          className="border border-wavis-accent text-wavis-accent hover:bg-wavis-accent hover:text-wavis-bg transition-colors px-6 py-2 disabled:opacity-40 disabled:cursor-not-allowed"
        >
          {recovering ? 'recovering...' : '/recover'}
        </button>

        {recoverError && (
          <div className="mt-4">
            <p className="text-wavis-danger text-sm">{recoverError}</p>
            {showRetry && (
              <button
                onClick={() => { void handleSubmit(); }}
                disabled={recovering}
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
          <div className="mb-2 font-bold text-sm">RECOVERY LOG</div>
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
        <span>Recover your account on a new device</span>
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
