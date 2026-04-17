import { useState, useRef, useEffect, useCallback } from 'react';
import { useNavigate } from 'react-router';
import { rotatePhrase } from './auth';

/* ─── Constants ─────────────────────────────────────────────────── */
const MIN_PHRASE_LENGTH = 4;
const DIVIDER = '─'.repeat(48);

/* ═══ Component ═════════════════════════════════════════════════════ */
export default function ChangePhrase() {
  const navigate = useNavigate();
  const inputRef = useRef<HTMLInputElement>(null);

  const [currentPhrase, setCurrentPhrase] = useState('');
  const [newPhrase, setNewPhrase] = useState('');
  const [submitting, setSubmitting] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const [success, setSuccess] = useState(false);

  useEffect(() => {
    inputRef.current?.focus();
  }, []);

  const handleSubmit = useCallback(async () => {
    if (submitting) return;
    setError(null);
    setSuccess(false);

    if (currentPhrase.length < MIN_PHRASE_LENGTH) {
      setError(`current phrase must be at least ${MIN_PHRASE_LENGTH} characters`);
      return;
    }
    if (newPhrase.length < MIN_PHRASE_LENGTH) {
      setError(`new phrase must be at least ${MIN_PHRASE_LENGTH} characters`);
      return;
    }

    setSubmitting(true);
    try {
      await rotatePhrase(currentPhrase, newPhrase);
      setCurrentPhrase('');
      setNewPhrase('');
      setSuccess(true);
    } catch (err) {
      setError(err instanceof Error ? err.message : 'Failed to change phrase');
    } finally {
      setSubmitting(false);
    }
  }, [currentPhrase, newPhrase, submitting]);

  const handleKeyDown = useCallback(
    (e: React.KeyboardEvent<HTMLInputElement>) => {
      if (e.key === 'Enter') handleSubmit();
    },
    [handleSubmit],
  );

  return (
    <div className="h-full flex flex-col bg-wavis-bg font-mono text-wavis-text">
      <div className="flex-1 overflow-y-auto">
        <div className="max-w-2xl mx-auto px-3 sm:px-6 py-6">
          <button
            onClick={() => navigate('/settings')}
            className="mb-4 text-xs text-wavis-text-secondary border border-wavis-text-secondary py-0.5 px-1 text-center transition-colors hover:bg-wavis-text-secondary hover:text-wavis-text-contrast"
          >
            ← /settings
          </button>
          <h2>change password</h2>
          <div className="text-wavis-text-secondary my-4 overflow-hidden">{DIVIDER}</div>

          {/* Current phrase */}
          <div className="mb-4">
            <div className="mb-2 text-sm">Current phrase:</div>
            <div className="flex items-center gap-2">
              <span className="text-wavis-accent shrink-0">&gt;</span>
              <input
                ref={inputRef}
                type="password"
                placeholder="current password"
                value={currentPhrase}
                onChange={(e) => { setCurrentPhrase(e.target.value); setError(null); }}
                onKeyDown={handleKeyDown}
                disabled={submitting}
                className="flex-1 min-w-0 bg-transparent border-b border-wavis-text-secondary outline-none px-2 py-1 font-mono text-wavis-text disabled:opacity-40 disabled:cursor-not-allowed"
                aria-label="Current password"
                autoComplete="off"
                autoFocus
              />
            </div>
          </div>

          {/* New phrase */}
          <div className="mb-6">
            <div className="mb-2 text-sm">New phrase:</div>
            <div className="flex items-center gap-2">
              <span className="text-wavis-accent shrink-0">&gt;</span>
              <input
                type="password"
                placeholder="new password (at least 4 characters)"
                value={newPhrase}
                onChange={(e) => { setNewPhrase(e.target.value); setError(null); }}
                onKeyDown={handleKeyDown}
                disabled={submitting}
                className="flex-1 min-w-0 bg-transparent border-b border-wavis-text-secondary outline-none px-2 py-1 font-mono text-wavis-text disabled:opacity-40 disabled:cursor-not-allowed"
                aria-label="New password"
                autoComplete="off"
              />
            </div>
          </div>

          <button
            onClick={handleSubmit}
            disabled={submitting}
            className="border border-wavis-accent text-wavis-accent hover:bg-wavis-accent hover:text-wavis-bg transition-colors px-6 py-2 disabled:opacity-40 disabled:cursor-not-allowed"
          >
            {submitting ? 'changing...' : '/change-phrase'}
          </button>

          {error && (
            <p className="text-wavis-danger text-sm mt-4">{error}</p>
          )}

          {success && (
            <p className="text-wavis-accent text-sm mt-4">Password changed successfully.</p>
          )}
        </div>
      </div>
    </div>
  );
}
