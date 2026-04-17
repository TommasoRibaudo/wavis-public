import { useState, useRef, useEffect, useCallback } from 'react';
import { useNavigate } from 'react-router';
import { startPairing, approvePairing, finishPairing, setDisplayName } from './auth';
import { useCopyToClipboardFeedback } from '@shared/hooks/useCopyToClipboardFeedback';
import { AuthFieldRow } from './AuthFieldRow';
import { AuthShell } from './AuthShell';

const MAX_DEVICE_NAME_LENGTH = 32;

function ModeToggle({ mode, onToggle }: { mode: 'new' | 'approve'; onToggle: (m: 'new' | 'approve') => void }) {
  return (
    <div className="flex gap-2">
      <button
        onClick={() => onToggle('new')}
        className={`border px-4 py-1 transition-colors ${
          mode === 'new'
            ? 'border-wavis-accent text-wavis-accent'
            : 'border-wavis-text-secondary text-wavis-text-secondary hover:text-wavis-text'
        }`}
      >
        /new-device
      </button>
      <button
        onClick={() => onToggle('approve')}
        className={`border px-4 py-1 transition-colors ${
          mode === 'approve'
            ? 'border-wavis-accent text-wavis-accent'
            : 'border-wavis-text-secondary text-wavis-text-secondary hover:text-wavis-text'
        }`}
      >
        /approve-device
      </button>
    </div>
  );
}

export default function PairDevice() {
  const navigate = useNavigate();
  const inputRef = useRef<HTMLInputElement>(null);

  const [mode, setMode] = useState<'new' | 'approve'>('new');

  const [deviceName, setDeviceName] = useState('');
  const [nameError, setNameError] = useState<string | null>(null);
  const [starting, setStarting] = useState(false);
  const [pairingId, setPairingId] = useState<string | null>(null);
  const [pairingCode, setPairingCode] = useState<string | null>(null);
  const [copyCode, codeCopied] = useCopyToClipboardFeedback();
  const [copyId, idCopied] = useCopyToClipboardFeedback();
  const [finishing, setFinishing] = useState(false);
  const [finishError, setFinishError] = useState<string | null>(null);
  const [startError, setStartError] = useState<string | null>(null);

  const [approvePairingId, setApprovePairingId] = useState('');
  const [approveCode, setApproveCode] = useState('');
  const [approving, setApproving] = useState(false);
  const [approveError, setApproveError] = useState<string | null>(null);
  const [approveSuccess, setApproveSuccess] = useState(false);

  useEffect(() => {
    inputRef.current?.focus();
  }, [mode]);

  const handleStartPairing = useCallback(async () => {
    if (starting) return;

    setNameError(null);
    setStartError(null);

    const trimmed = deviceName.trim();
    if (!trimmed) {
      setNameError('device name is required');
      return;
    }
    if (trimmed.length > MAX_DEVICE_NAME_LENGTH) {
      setNameError(`device name must be ${MAX_DEVICE_NAME_LENGTH} characters or less`);
      return;
    }

    setStarting(true);
    try {
      const result = await startPairing(trimmed);
      setPairingId(result.pairing_id);
      setPairingCode(result.code);
    } catch (err) {
      setStartError(err instanceof Error ? err.message : 'Failed to start pairing');
    } finally {
      setStarting(false);
    }
  }, [deviceName, starting]);

  const handleFinishPairing = useCallback(async () => {
    if (finishing || !pairingId || !pairingCode) return;

    setFinishError(null);
    setFinishing(true);
    try {
      await finishPairing(pairingId, pairingCode);
      const trimmed = deviceName.trim();
      if (trimmed) {
        await setDisplayName(trimmed);
      }
      navigate('/', { replace: true });
    } catch (err) {
      const msg = err instanceof Error ? err.message : 'Failed to finish pairing';
      if (msg.includes('not yet approved') || msg.includes('pending')) {
        setFinishError('Pairing not yet approved — ask your trusted device to approve first');
      } else if (msg.includes('expired')) {
        setFinishError('Pairing expired — please start over');
      } else {
        setFinishError(msg);
      }
    } finally {
      setFinishing(false);
    }
  }, [finishing, pairingId, pairingCode, deviceName, navigate]);

  const handleApprove = useCallback(async () => {
    if (approving) return;

    setApproveError(null);
    setApproveSuccess(false);

    if (!approvePairingId.trim()) {
      setApproveError('pairing ID is required');
      return;
    }
    if (!approveCode.trim()) {
      setApproveError('code is required');
      return;
    }

    setApproving(true);
    try {
      await approvePairing(approvePairingId.trim(), approveCode.trim());
      setApproveSuccess(true);
    } catch (err) {
      setApproveError(err instanceof Error ? err.message : 'Failed to approve pairing');
    } finally {
      setApproving(false);
    }
  }, [approvePairingId, approveCode, approving]);

  const handleNewKeyDown = useCallback(
    (e: React.KeyboardEvent<HTMLInputElement>) => {
      if (e.key === 'Enter') void handleStartPairing();
    },
    [handleStartPairing],
  );

  const handleApproveKeyDown = useCallback(
    (e: React.KeyboardEvent<HTMLInputElement>) => {
      if (e.key === 'Enter') void handleApprove();
    },
    [handleApprove],
  );

  const handleModeToggle = useCallback((nextMode: 'new' | 'approve') => {
    setMode(nextMode);
    setDeviceName('');
    setNameError(null);
    setStartError(null);
    setPairingId(null);
    setPairingCode(null);
    setFinishError(null);
    setApprovePairingId('');
    setApproveCode('');
    setApproveError(null);
    setApproveSuccess(false);
  }, []);

  return (
    <AuthShell
      subtitle={<div className="mt-3"><ModeToggle mode={mode} onToggle={handleModeToggle} /></div>}
    >
      {mode === 'new' && !pairingId && (
        <div className="px-4 sm:px-6 py-4">
          <div className="mb-4 text-wavis-text-secondary text-sm">
            Pair this device with an existing account
          </div>

          <AuthFieldRow
            label="Device name:"
            placeholder="my new device"
            value={deviceName}
            onChange={(value) => {
              setDeviceName(value);
              setNameError(null);
            }}
            onKeyDown={handleNewKeyDown}
            error={nameError}
            disabled={starting}
            maxLength={MAX_DEVICE_NAME_LENGTH}
            inputRef={inputRef}
            autoFocus
            className="mb-6"
          />

          <button
            onClick={() => { void handleStartPairing(); }}
            disabled={starting}
            className="border border-wavis-accent text-wavis-accent hover:bg-wavis-accent hover:text-wavis-bg transition-colors px-6 py-2 disabled:opacity-40 disabled:cursor-not-allowed"
          >
            {starting ? 'starting...' : '/start-pairing'}
          </button>

          {startError && (
            <p className="text-wavis-danger text-sm mt-4">{startError}</p>
          )}
        </div>
      )}

      {mode === 'new' && pairingId && pairingCode && (
        <div className="px-4 sm:px-6 py-4">
          <div className="mb-4">
            <div className="mb-2 text-sm font-bold">PAIRING CODE</div>
            <div className="flex items-center gap-2 p-3 border border-wavis-text-secondary bg-wavis-bg">
              <code className="flex-1 text-wavis-accent text-2xl tracking-widest select-all">
                {pairingCode}
              </code>
              <button
                onClick={() => { void copyCode(pairingCode); }}
                className="border border-wavis-text-secondary text-wavis-text-secondary hover:border-wavis-accent hover:text-wavis-accent transition-colors px-3 py-1 text-sm shrink-0"
              >
                {codeCopied ? 'Copied!' : '/copy'}
              </button>
            </div>
          </div>

          <div className="mb-4">
            <div className="mb-2 text-sm text-wavis-text-secondary">PAIRING ID</div>
            <div className="flex items-center gap-2 p-2 border border-wavis-text-secondary bg-wavis-bg">
              <code className="flex-1 text-wavis-text text-sm tracking-wider select-all">
                {pairingId}
              </code>
              <button
                onClick={() => { void copyId(pairingId); }}
                className="border border-wavis-text-secondary text-wavis-text-secondary hover:border-wavis-accent hover:text-wavis-accent transition-colors px-3 py-1 text-sm shrink-0"
              >
                {idCopied ? 'Copied!' : '/copy'}
              </button>
            </div>
          </div>

          <p className="text-wavis-text-secondary text-sm mb-6">
            Enter this code on your trusted device to approve the pairing.
          </p>

          <button
            onClick={() => { void handleFinishPairing(); }}
            disabled={finishing}
            className="border border-wavis-accent text-wavis-accent hover:bg-wavis-accent hover:text-wavis-bg transition-colors px-6 py-2 disabled:opacity-40 disabled:cursor-not-allowed"
          >
            {finishing ? 'completing...' : '/complete-pairing'}
          </button>

          {finishError && (
            <p className="text-wavis-danger text-sm mt-4">{finishError}</p>
          )}
        </div>
      )}

      {mode === 'approve' && (
        <div className="px-4 sm:px-6 py-4">
          <div className="mb-4 text-wavis-text-secondary text-sm">
            Approve a new device from this trusted device
          </div>

          <div className="mb-4">
            <div className="mb-2 text-sm">Pairing ID:</div>
            <div className="flex items-center gap-2">
              <span className="text-wavis-accent shrink-0">&gt;</span>
              <input
                ref={mode === 'approve' ? inputRef : undefined}
                type="text"
                placeholder="pairing ID from new device"
                value={approvePairingId}
                onChange={(e) => {
                  setApprovePairingId(e.target.value);
                  setApproveError(null);
                }}
                onKeyDown={handleApproveKeyDown}
                disabled={approving}
                className={`flex-1 min-w-0 bg-transparent border-b outline-none px-2 py-1 font-mono text-wavis-text ${
                  approveError && !approvePairingId.trim() ? 'border-wavis-danger' : 'border-wavis-text-secondary'
                } disabled:opacity-40 disabled:cursor-not-allowed`}
                aria-label="Pairing ID"
                autoFocus
              />
            </div>
          </div>

          <div className="mb-6">
            <div className="mb-2 text-sm">Code:</div>
            <div className="flex items-center gap-2">
              <span className="text-wavis-accent shrink-0">&gt;</span>
              <input
                type="text"
                placeholder="pairing code from new device"
                value={approveCode}
                onChange={(e) => {
                  setApproveCode(e.target.value);
                  setApproveError(null);
                }}
                onKeyDown={handleApproveKeyDown}
                disabled={approving}
                className={`flex-1 min-w-0 bg-transparent border-b outline-none px-2 py-1 font-mono text-wavis-text ${
                  approveError && !approveCode.trim() ? 'border-wavis-danger' : 'border-wavis-text-secondary'
                } disabled:opacity-40 disabled:cursor-not-allowed`}
                aria-label="Pairing code"
              />
            </div>
          </div>

          <button
            onClick={() => { void handleApprove(); }}
            disabled={approving}
            className="border border-wavis-accent text-wavis-accent hover:bg-wavis-accent hover:text-wavis-bg transition-colors px-6 py-2 disabled:opacity-40 disabled:cursor-not-allowed"
          >
            {approving ? 'approving...' : '/approve'}
          </button>

          {approveError && (
            <p className="text-wavis-danger text-sm mt-4">{approveError}</p>
          )}

          {approveSuccess && (
            <p className="text-wavis-accent text-sm mt-4">Pairing approved — the new device can now complete pairing.</p>
          )}
        </div>
      )}

      <div className="px-4 sm:px-6 py-3 border-t border-wavis-text-secondary text-wavis-text-secondary text-xs flex items-center justify-between">
        <span>Device pairing</span>
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
