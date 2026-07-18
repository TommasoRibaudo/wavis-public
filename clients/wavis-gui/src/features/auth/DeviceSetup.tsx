import { useState, useEffect, useRef, useCallback } from 'react';
import { useNavigate } from 'react-router';
import {
  type AuthLogEntry,
  validateServerUrl,
  registerUser,
  setUsername,
  INSECURE_TLS_ALLOWED,
  deleteStoredRecoveryId,
} from './auth';
import { useCopyToClipboardFeedback } from '@shared/hooks/useCopyToClipboardFeedback';
import { setLaunchOnStartupEnabled } from '@shared/autostart-bridge';
import { AuthFieldRow } from './AuthFieldRow';
import { AuthShell } from './AuthShell';
import { AuthLogPanel } from './AuthLogPanel';

const MIN_PHRASE_LENGTH = 4;
const MAX_USERNAME_LENGTH = 64;

type SetupStep = 1 | 2 | 3 | 4;

export function validateStep1(
  username: string,
  phrase: string,
  confirmPhrase: string,
): {
  valid: boolean;
  errors: {
    username?: string;
    phrase?: string;
    confirmPhrase?: string;
  };
} {
  const errors: {
    username?: string;
    phrase?: string;
    confirmPhrase?: string;
  } = {};
  const trimmedName = username.trim();

  if (!trimmedName) {
    errors.username = 'username is required';
  } else if (trimmedName.length > MAX_USERNAME_LENGTH) {
    errors.username = 'username must be 64 characters or less';
  }

  if (phrase.length < MIN_PHRASE_LENGTH) {
    errors.phrase = `Password must be at least ${MIN_PHRASE_LENGTH} characters`;
  }

  if (confirmPhrase !== phrase) {
    errors.confirmPhrase = 'Passwords must match';
  }

  return { valid: Object.keys(errors).length === 0, errors };
}

function InsecureTlsToggle({
  insecureTls,
  setInsecureTls,
  disabled,
}: {
  insecureTls: boolean;
  setInsecureTls: (value: boolean) => void;
  disabled: boolean;
}) {
  if (!INSECURE_TLS_ALLOWED) return null;

  return (
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
          disabled={disabled}
          className="sr-only"
        />
      </label>
      {insecureTls && (
        <p className="text-wavis-warn text-xs mt-2 pl-6">
          TLS certificate verification disabled. Do not use in production.
        </p>
      )}
    </div>
  );
}

function LaunchOnStartupStep({ onChoice }: { onChoice: (wantEnabled: boolean) => Promise<void> }) {
  const [pending, setPending] = useState(false);

  const choose = useCallback(
    (wantEnabled: boolean) => {
      if (pending) return;
      setPending(true);
      void onChoice(wantEnabled).finally(() => setPending(false));
    },
    [pending, onChoice],
  );

  return (
    <div className="px-4 sm:px-6 py-4">
      <div className="mb-6 border border-wavis-text-secondary bg-wavis-bg p-3">
        <div className="mb-2 text-sm font-bold">LAUNCH WAVIS ON STARTUP</div>
        <p className="text-sm text-wavis-text-secondary">
          Start Wavis automatically when you log into this computer, so you never have to open it
          manually. Off by default — you can change this anytime in Settings.
        </p>
      </div>

      <div className="flex items-center gap-3">
        <button
          onClick={() => choose(false)}
          disabled={pending}
          className="border border-wavis-text-secondary text-wavis-text-secondary hover:border-wavis-accent hover:text-wavis-accent transition-colors px-6 py-2 disabled:opacity-40 disabled:cursor-not-allowed"
        >
          /skip
        </button>
        <button
          onClick={() => choose(true)}
          disabled={pending}
          className="border border-wavis-accent text-wavis-accent hover:bg-wavis-accent hover:text-wavis-bg transition-colors px-6 py-2 disabled:opacity-40 disabled:cursor-not-allowed"
        >
          {pending ? 'saving...' : '/enable'}
        </button>
      </div>
    </div>
  );
}

export default function DeviceSetup() {
  const navigate = useNavigate();
  const inputRef = useRef<HTMLInputElement>(null);

  const [step, setStep] = useState<SetupStep>(1);
  const [serverUrl, setServerUrl] = useState('');
  const [username, setUsernameValue] = useState('');
  const [phrase, setPhrase] = useState('');
  const [confirmPhrase, setConfirmPhrase] = useState('');
  const [insecureTls, setInsecureTls] = useState(false);
  const [registering, setRegistering] = useState(false);
  const [logs, setLogs] = useState<AuthLogEntry[]>([]);
  const [urlError, setUrlError] = useState<string | null>(null);
  const [nameError, setNameError] = useState<string | null>(null);
  const [phraseError, setPhraseError] = useState<string | null>(null);
  const [confirmPhraseError, setConfirmPhraseError] = useState<string | null>(null);
  const [registerError, setRegisterError] = useState<string | null>(null);
  const [showRetry, setShowRetry] = useState(false);

  const [recoveryId, setRecoveryId] = useState<string | null>(null);
  const [copy, copied] = useCopyToClipboardFeedback();
  const [confirmed, setConfirmed] = useState(false);
  const [trustedDevice, setTrustedDevice] = useState(true);

  useEffect(() => {
    inputRef.current?.focus();
  }, [step]);

  const handleStep1Next = useCallback(() => {
    setNameError(null);
    setPhraseError(null);
    setConfirmPhraseError(null);

    const result = validateStep1(username, phrase, confirmPhrase);
    if (!result.valid) {
      setNameError(result.errors.username ?? null);
      setPhraseError(result.errors.phrase ?? null);
      setConfirmPhraseError(result.errors.confirmPhrase ?? null);
      return;
    }

    setStep(2);
  }, [username, phrase, confirmPhrase]);

  const handleContinue = useCallback(async () => {
    const trimmedName = username.trim();
    if (trimmedName) {
      await setUsername(trimmedName);
    }
    if (!trustedDevice) {
      await deleteStoredRecoveryId();
    }
    setStep(4);
  }, [username, trustedDevice]);

  const handleStartupChoice = useCallback(
    async (wantEnabled: boolean) => {
      await setLaunchOnStartupEnabled(wantEnabled);
      navigate('/', { replace: true });
    },
    [navigate],
  );

  const handleRegister = useCallback(async () => {
    if (registering) return;

    setUrlError(null);
    setRegisterError(null);
    setShowRetry(false);

    const validation = validateServerUrl(serverUrl, insecureTls);
    if (!validation.valid) {
      setUrlError(validation.reason ?? 'Invalid server URL');
      return;
    }

    const trimmedName = username.trim();
    setRegistering(true);
    setLogs([]);

    const result = await registerUser(serverUrl, phrase, trimmedName, insecureTls, (entry) => {
      setLogs((prev) => [...prev, entry]);
    });

    setRegistering(false);
    setPhrase('');
    setConfirmPhrase('');

    if (result.success) {
      setRecoveryId(result.recovery_id);
      setStep(3);
      return;
    }

    const err = result.error ?? '';
    if (err.includes('too many requests')) {
      setRegisterError('too many requests - try again later');
    } else {
      setRegisterError('Registration failed - please try again');
      setShowRetry(true);
    }
  }, [serverUrl, username, phrase, insecureTls, registering]);

  const handleKeyDown = useCallback(
    (e: React.KeyboardEvent<HTMLInputElement>) => {
      if (e.key === 'Enter') {
        if (step === 1) {
          handleStep1Next();
        } else if (step === 2) {
          void handleRegister();
        }
      } else if (e.key === 'Escape') {
        setUrlError(null);
        setRegisterError(null);
        setShowRetry(false);
      }
    },
    [step, handleStep1Next, handleRegister],
  );

  return (
    <AuthShell
      subtitle={
        <div className="mt-2 text-wavis-text-secondary text-sm">
          {step === 1 && 'Step 1 of 4 - create credentials'}
          {step === 2 && 'Step 2 of 4 - choose server'}
          {step === 3 && 'Step 3 of 4 - save recovery details'}
          {step === 4 && 'Step 4 of 4 - launch on startup'}
        </div>
      }
    >
      {step === 1 && (
        <div className="px-4 sm:px-6 py-4">
          <AuthFieldRow
            label="Username:"
            placeholder="your name"
            value={username}
            onChange={(value) => {
              setUsernameValue(value);
              setNameError(null);
            }}
            onKeyDown={handleKeyDown}
            error={nameError}
            disabled={registering}
            maxLength={MAX_USERNAME_LENGTH}
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
              if (confirmPhraseError) setConfirmPhraseError(null);
            }}
            onKeyDown={handleKeyDown}
            error={phraseError}
            disabled={registering}
          />

          <AuthFieldRow
            label="Confirm Password:"
            type="password"
            placeholder="repeat your password"
            value={confirmPhrase}
            onChange={(value) => {
              setConfirmPhrase(value);
              setConfirmPhraseError(null);
            }}
            onKeyDown={handleKeyDown}
            error={confirmPhraseError}
            disabled={registering}
          />

          <button
            onClick={handleStep1Next}
            disabled={registering}
            className="border border-wavis-accent text-wavis-accent hover:bg-wavis-accent hover:text-wavis-bg transition-colors px-6 py-2 disabled:opacity-40 disabled:cursor-not-allowed"
          >
            /next
          </button>
        </div>
      )}

      {step === 2 && (
        <>
          <div className="px-4 sm:px-6 py-4">
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
              inputRef={inputRef}
              autoFocus
            />

            <div className="mb-6 border border-wavis-text-secondary bg-wavis-bg p-3 text-sm text-wavis-text-secondary">
              <div className="mb-2 font-bold text-wavis-text">What is the Server URL?</div>
              <p>
                The address of your Wavis server. Your admin will share it with you, or you can get
                a hosted server at wavis.io.
              </p>
              <p className="mt-2 text-wavis-accent">It looks like: https://wavis.yourteam.com</p>
            </div>

            <InsecureTlsToggle
              insecureTls={insecureTls}
              setInsecureTls={setInsecureTls}
              disabled={registering}
            />

            <div className="flex items-center gap-3">
              <button
                onClick={() => setStep(1)}
                disabled={registering}
                className="border border-wavis-text-secondary text-wavis-text-secondary hover:border-wavis-accent hover:text-wavis-accent transition-colors px-6 py-2 disabled:opacity-40 disabled:cursor-not-allowed"
              >
                /back
              </button>
              <button
                onClick={() => {
                  void handleRegister();
                }}
                disabled={registering}
                className="border border-wavis-accent text-wavis-accent hover:bg-wavis-accent hover:text-wavis-bg transition-colors px-6 py-2 disabled:opacity-40 disabled:cursor-not-allowed"
              >
                {registering ? 'registering...' : '/register'}
              </button>
            </div>

            {registerError && (
              <div className="mt-4">
                <p className="text-wavis-danger text-sm">{registerError}</p>
                {showRetry && (
                  <button
                    onClick={() => {
                      void handleRegister();
                    }}
                    disabled={registering}
                    className="border border-wavis-danger text-wavis-danger hover:bg-wavis-danger hover:text-wavis-bg transition-colors px-3 py-0.5 mt-2 text-sm disabled:opacity-40 disabled:cursor-not-allowed"
                  >
                    /retry
                  </button>
                )}
              </div>
            )}
          </div>

          <AuthLogPanel title="REGISTRATION LOG" logs={logs} />
        </>
      )}

      {step === 3 && recoveryId && (
        <div className="px-4 sm:px-6 py-4">
          <div className="mb-5">
            <div className="mb-2 text-sm font-bold">YOUR RECOVERY ID</div>
            <div className="flex items-center gap-2 p-3 border border-wavis-text-secondary bg-wavis-bg">
              <code className="flex-1 min-w-0 text-wavis-accent text-lg tracking-wider select-all break-all">
                {recoveryId}
              </code>
              <button
                onClick={() => {
                  void copy(recoveryId);
                }}
                className="border border-wavis-text-secondary text-wavis-text-secondary hover:border-wavis-accent hover:text-wavis-accent transition-colors px-3 py-1 text-sm shrink-0"
              >
                {copied ? 'Copied!' : '/copy'}
              </button>
            </div>
            <p className="text-wavis-warn text-sm mt-3">
              Save this Recovery ID. You will need it to recover your account on a new device.
            </p>
          </div>

          <label className="flex items-center gap-3 text-sm cursor-pointer select-none mb-6">
            <input
              type="checkbox"
              checked={confirmed}
              onChange={(e) => setConfirmed(e.target.checked)}
              className="peer sr-only"
            />
            <span className="flex size-6 shrink-0 items-center justify-center border-2 border-wavis-text-secondary bg-wavis-bg text-xs leading-none text-transparent transition-colors peer-focus-visible:outline peer-focus-visible:outline-2 peer-focus-visible:outline-offset-2 peer-focus-visible:outline-wavis-accent peer-checked:border-wavis-accent peer-checked:bg-wavis-accent peer-checked:text-wavis-bg">
              OK
            </span>
            <span className={confirmed ? 'text-wavis-text' : 'text-wavis-text-secondary'}>
              I have saved my Recovery ID (required)
            </span>
          </label>

          <div className="mb-6 border border-wavis-text-secondary bg-wavis-bg p-3">
            <div className="mb-2 text-sm font-bold">TRUST THIS DEVICE</div>
            <p className="text-sm text-wavis-text-secondary mb-4">
              Your Wavis ID will be saved securely on this device. You'll only need your password to
              log back in here.
            </p>
            <label className="flex items-center gap-3 text-sm cursor-pointer select-none">
              <input
                type="checkbox"
                checked={trustedDevice}
                onChange={(e) => setTrustedDevice(e.target.checked)}
                className="peer sr-only"
              />
              <span className="flex size-6 shrink-0 items-center justify-center border-2 border-wavis-text-secondary bg-wavis-bg text-xs leading-none text-transparent transition-colors peer-focus-visible:outline peer-focus-visible:outline-2 peer-focus-visible:outline-offset-2 peer-focus-visible:outline-wavis-accent peer-checked:border-wavis-accent peer-checked:bg-wavis-accent peer-checked:text-wavis-bg">
                OK
              </span>
              <span className={trustedDevice ? 'text-wavis-text' : 'text-wavis-text-secondary'}>
                Trust this device (recommended)
              </span>
            </label>
          </div>

          <button
            onClick={() => {
              void handleContinue();
            }}
            disabled={!confirmed}
            className="border border-wavis-accent text-wavis-accent hover:bg-wavis-accent hover:text-wavis-bg disabled:border-wavis-text-secondary disabled:text-wavis-text-secondary disabled:opacity-60 disabled:hover:bg-transparent disabled:hover:text-wavis-text-secondary transition-colors px-6 py-2"
          >
            /continue
          </button>
        </div>
      )}

      {step === 4 && <LaunchOnStartupStep onChoice={handleStartupChoice} />}

      <div className="px-4 sm:px-6 py-3 border-t border-wavis-text-secondary text-wavis-text-secondary text-xs">
        Tokens stored in local keychain - Auto-refresh - Device ID persisted
      </div>

      {step !== 3 && step !== 4 && (
        <div className="px-4 sm:px-6 py-3 border-t border-wavis-text-secondary text-sm">
          <button
            onClick={() => navigate('/recover')}
            disabled={registering}
            className="text-wavis-text-secondary hover:text-wavis-accent transition-colors disabled:opacity-40"
          >
            /recover-account
          </button>
        </div>
      )}
    </AuthShell>
  );
}
