import { useState, useEffect, useCallback, useRef } from 'react';
import { useNavigate } from 'react-router';
import {
  type Channel,
  fetchChannels,
  createChannel,
  joinChannelByInvite,
  normalizeChannelName,
} from './channels';
import { ApiError } from '@shared/api';
import {
  channelsListErrorMessage as errorMessage,
} from '@shared/helpers';
import { ChannelRoleBadge } from './ChannelRoleBadge';
import { fetchVoiceStatusWithFallback } from './channel-detail';
import { CmdButton } from '@shared/CmdButton';
import { ErrorPanel } from '@shared/ErrorPanel';
import { EmptyState } from '@shared/EmptyState';
import { LoadingBlock } from '@shared/LoadingBlock';
import { usePolling } from '@shared/hooks/usePolling';

/* Constants */
const POLL_MS = 15_000;
const DIVIDER = '─'.repeat(48);

/* Component */
export default function ChannelsList() {
  const navigate = useNavigate();

  /* State */
  const [channels, setChannels] = useState<Channel[]>([]);
  const [loading, setLoading] = useState(true);
  const [error, setError] = useState<string | null>(null);
  const [activeForm, setActiveForm] = useState<'none' | 'create' | 'join'>('none');
  const [voiceStatus, setVoiceStatus] = useState<Map<string, { active: boolean; participantCount: number }>>(new Map());

  // Create form
  const [createName, setCreateName] = useState('');
  const [createError, setCreateError] = useState<string | null>(null);
  const [createBusy, setCreateBusy] = useState(false);

  // Join form
  const [joinCode, setJoinCode] = useState('');
  const [joinError, setJoinError] = useState<string | null>(null);
  const [joinBusy, setJoinBusy] = useState(false);
  const [joinSuccess, setJoinSuccess] = useState(false);

  /* Refs */
  const skipTicksRef = useRef(0);
  const requestInFlightRef = useRef(false);
  const createInputRef = useRef<HTMLInputElement>(null);
  const joinInputRef = useRef<HTMLInputElement>(null);

  /* Fetch channels (visible loading) */
  const loadChannels = useCallback(async (showLoading: boolean) => {
    if (showLoading) setLoading(true);
    setError(null);
    requestInFlightRef.current = true;
    try {
      const result = await fetchChannels();
      setChannels(result);
      // Fetch voice status in background (silent failure)
      const ids = result.map((ch) => ch.id);
      fetchVoiceStatusWithFallback(ids).then((status) => {
        setVoiceStatus(status);
      }).catch(() => { /* silent */ });
    } catch (err) {
      if (err instanceof ApiError) {
        if (err.kind === 'RateLimited') skipTicksRef.current = 2;
        setError(errorMessage(err.kind));
      } else {
        setError('something went wrong — try again');
      }
    } finally {
      requestInFlightRef.current = false;
      if (showLoading) setLoading(false);
    }
  }, []);

  /* Initial fetch on mount */
  useEffect(() => {
    loadChannels(true);
  }, [loadChannels]);

  /* Auto-refresh: 15s interval */
  usePolling(() => {
    if (skipTicksRef.current > 0) { skipTicksRef.current--; return; }
    if (requestInFlightRef.current) return;
    loadChannels(false);
  }, POLL_MS);

  /* Auto-focus when form opens */
  useEffect(() => {
    if (activeForm === 'create') {
      createInputRef.current?.focus();
    } else if (activeForm === 'join') {
      joinInputRef.current?.focus();
    }
  }, [activeForm]);

  /* Form toggles */
  const toggleForm = useCallback(
    (form: 'create' | 'join') => {
      if (activeForm === form) {
        setActiveForm('none');
      } else {
        setActiveForm(form);
      }
      // Reset form state
      setCreateName('');
      setCreateError(null);
      setJoinCode('');
      setJoinError(null);
      setJoinSuccess(false);
    },
    [activeForm],
  );

  const closeForm = useCallback(() => {
    setActiveForm('none');
    setCreateName('');
    setCreateError(null);
    setJoinCode('');
    setJoinError(null);
    setJoinSuccess(false);
  }, []);

  /* Create channel */
  const handleCreate = useCallback(async () => {
    if (createBusy) return;
    const normalized = normalizeChannelName(createName);
    if (normalized.length < 1 || normalized.length > 100) {
      setCreateError('name must be 1–100 characters after normalization');
      return;
    }
    setCreateError(null);
    setCreateBusy(true);
    requestInFlightRef.current = true;
    try {
      const ch = await createChannel(createName);
      setChannels((prev) => [...prev, ch]);
      closeForm();
    } catch (err) {
      if (err instanceof ApiError) {
        if (err.kind === 'RateLimited') skipTicksRef.current = 2;
        setCreateError(errorMessage(err.kind));
      } else {
        setCreateError('something went wrong — try again');
      }
    } finally {
      setCreateBusy(false);
      requestInFlightRef.current = false;
    }
  }, [createName, createBusy, closeForm]);

  /* Join channel */
  const handleJoin = useCallback(async () => {
    if (joinBusy) return;
    const code = joinCode.trim();
    if (!code) {
      setJoinError('enter an invite code');
      return;
    }
    setJoinError(null);
    setJoinBusy(true);
    requestInFlightRef.current = true;
    try {
      const ch = await joinChannelByInvite(code);
      setChannels((prev) => {
        // Avoid duplicates
        if (prev.some((c) => c.id === ch.id)) return prev;
        return [...prev, ch];
      });
      setJoinSuccess(true);
      setTimeout(() => closeForm(), 1500);
    } catch (err) {
      if (err instanceof ApiError) {
        if (err.kind === 'RateLimited') skipTicksRef.current = 2;
        setJoinError(errorMessage(err.kind));
      } else {
        setJoinError('something went wrong — try again');
      }
    } finally {
      setJoinBusy(false);
      requestInFlightRef.current = false;
    }
  }, [joinCode, joinBusy, closeForm]);

  /* Keyboard: Escape closes form */
  useEffect(() => {
    const handler = (e: KeyboardEvent) => {
      if (e.key === 'Escape' && activeForm !== 'none') {
        closeForm();
      }
    };
    document.addEventListener('keydown', handler);
    return () => document.removeEventListener('keydown', handler);
  }, [activeForm, closeForm]);

  /* Derived */
  const normalized = normalizeChannelName(createName);
  const createValid = normalized.length >= 1 && normalized.length <= 100;

  return (
    <div className="h-full flex flex-col bg-wavis-bg font-mono text-wavis-text">
      <div className="flex-1 overflow-y-auto">
        <div className="max-w-2xl mx-auto px-3 sm:px-6 py-6">
          <h2 className="mb-6">channels</h2>

          {/* Loading state */}
          {loading && <LoadingBlock />}

          {/* Error state */}
          {!loading && error && (
            <ErrorPanel error={error} onRetry={() => loadChannels(true)} />
          )}

          {/* Empty state */}
          {!loading && !error && channels.length === 0 && (
            <EmptyState
              message="no channels yet — create one or join by invite code"
              className="p-4 text-center"
            />
          )}

          {/* Channel list */}
          {!loading && !error && channels.length > 0 && (
            <div className="flex flex-col">
              {channels.map((ch) => (
                <div
                  key={ch.id}
                  onClick={() => navigate('/room', { state: { channelId: ch.id, channelName: ch.name, channelRole: ch.role } })}
                  className="flex items-center justify-between gap-4 px-3 sm:px-4 py-3 bg-wavis-panel border border-wavis-text-secondary hover:border-wavis-accent transition-colors text-left mb-1 cursor-pointer"
                >
                  <span className="min-w-0 truncate">{ch.name}</span>
                  <div className="flex items-center gap-2 shrink-0">
                    {voiceStatus.get(ch.id)?.active && (
                      <span className="text-wavis-accent text-xs flex items-center gap-1">
                        <span>●</span>
                        <span>{voiceStatus.get(ch.id)?.participantCount}</span>
                      </span>
                    )}
                    <ChannelRoleBadge role={ch.role} variant="list" />
                    {ch.role === 'owner' && (
                      <span className="text-wavis-accent text-xs">★</span>
                    )}
                    <button
                      onClick={(e) => { e.stopPropagation(); navigate(`/channel/${ch.id}`); }}
                      className="border border-wavis-text-secondary text-wavis-text-secondary hover:bg-wavis-text-secondary hover:text-wavis-text-contrast transition-colors px-1 py-0.5 text-[0.625rem]"
                    >
                      /channel settings
                    </button>
                  </div>
                </div>
              ))}
            </div>
          )}

          {/* Divider before forms */}
          {activeForm !== 'none' && (
            <div className="text-wavis-text-secondary mt-4 mb-4 overflow-hidden">{DIVIDER}</div>
          )}

          {/* Create form */}
          {activeForm === 'create' && (
            <div className="mb-4">
              <p className="text-sm text-wavis-text-secondary mb-2">create channel</p>
              <div className="flex items-center gap-2">
                <span className="text-wavis-accent shrink-0">&gt;</span>
                <input
                  ref={createInputRef}
                  type="text"
                  placeholder="channel name"
                  value={createName}
                  onChange={(e) => {
                    setCreateName(e.target.value);
                    setCreateError(null);
                  }}
                  onKeyDown={(e) => {
                    if (e.key === 'Enter') handleCreate();
                  }}
                  disabled={createBusy}
                  maxLength={100}
                  className="flex-1 min-w-0 bg-transparent border-b border-wavis-text-secondary outline-none px-2 py-1 font-mono text-wavis-text disabled:opacity-40 disabled:cursor-not-allowed"
                  aria-label="Channel name"
                />
                <button
                  onClick={handleCreate}
                  disabled={createBusy || !createValid}
                  className="shrink-0 border border-wavis-accent text-wavis-accent hover:bg-wavis-accent hover:text-wavis-bg transition-colors px-1 py-0.5 text-xs disabled:opacity-40 disabled:cursor-not-allowed"
                >
                  /create
                </button>
              </div>
              {createName && (
                <p className="text-xs text-wavis-text-secondary mt-1 ml-5">
                  → {normalized || '(empty)'}
                </p>
              )}
              {createError && (
                <p className="text-wavis-danger text-sm mt-1 ml-5">{createError}</p>
              )}
            </div>
          )}

          {/* Join form */}
          {activeForm === 'join' && (
            <div className="mb-4">
              <p className="text-sm text-wavis-text-secondary mb-2">join by invite code</p>
              <div className="flex items-center gap-2">
                <span className="text-wavis-accent shrink-0">&gt;</span>
                <input
                  ref={joinInputRef}
                  type="text"
                  placeholder="INVITE CODE"
                  value={joinCode}
                  onChange={(e) => {
                    setJoinCode(e.target.value);
                    setJoinError(null);
                    setJoinSuccess(false);
                  }}
                  onKeyDown={(e) => {
                    if (e.key === 'Enter') handleJoin();
                  }}
                  disabled={joinBusy}
                  className="flex-1 min-w-0 bg-transparent border-b border-wavis-text-secondary outline-none px-2 py-1 font-mono text-wavis-text disabled:opacity-40 disabled:cursor-not-allowed"
                  style={{ letterSpacing: '0.15em' }}
                  aria-label="Invite code"
                />
                <button
                  onClick={handleJoin}
                  disabled={joinBusy || !joinCode.trim()}
                  className="shrink-0 border border-wavis-accent text-wavis-accent hover:bg-wavis-accent hover:text-wavis-bg transition-colors px-1 py-0.5 text-xs disabled:opacity-40 disabled:cursor-not-allowed"
                >
                  /join
                </button>
              </div>
              {joinSuccess && (
                <p className="text-wavis-accent text-sm mt-1 ml-5">joined!</p>
              )}
              {joinError && (
                <p className="text-wavis-danger text-sm mt-1 ml-5">{joinError}</p>
              )}
            </div>
          )}
        </div>
      </div>

      {/* Bottom command bar */}
      <div className="border-t border-wavis-text-secondary px-3 sm:px-6 py-3 flex flex-wrap items-center gap-x-6 gap-y-2">
        <CmdButton
          label="/create"
          onClick={() => toggleForm('create')}
          active={activeForm === 'create'}
        />
        <CmdButton
          label="/join"
          onClick={() => toggleForm('join')}
          active={activeForm === 'join'}
        />
        <CmdButton
          label="/refresh"
          onClick={() => loadChannels(true)}
        />
        <CmdButton
          label="/direct"
          onClick={() => navigate('/legacy')}
        />
        <CmdButton
          label="/profile"
          onClick={() => navigate('/settings')}
        />
      </div>
    </div>
  );
}
