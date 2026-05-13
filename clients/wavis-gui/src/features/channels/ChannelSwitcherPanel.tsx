import { useState, useEffect, useRef, useCallback } from 'react';
import {
  type Channel,
  fetchChannels,
  createChannel,
  joinChannelByInvite,
  normalizeChannelName,
} from './channels';
import { fetchVoiceStatusWithFallback } from './channel-detail';
import { ChannelRoleBadge } from './ChannelRoleBadge';
import { usePolling } from '@shared/hooks/usePolling';
import { ApiError } from '@shared/api';

const POLL_MS = 15_000;

interface ChannelSwitcherPanelProps {
  onChannelSelect: (ch: Channel) => void;
  onClose: () => void;
  currentChannelId?: string;
}

export function ChannelSwitcherPanel({ onChannelSelect, onClose, currentChannelId }: ChannelSwitcherPanelProps) {
  const [channels, setChannels] = useState<Channel[]>([]);
  const [loading, setLoading] = useState(true);
  const [voiceStatus, setVoiceStatus] = useState<Map<string, { active: boolean; participantCount: number }>>(new Map());
  const requestInFlightRef = useRef(false);

  const [activeForm, setActiveForm] = useState<'none' | 'create' | 'join'>('none');

  // Create form
  const [createName, setCreateName] = useState('');
  const [createError, setCreateError] = useState<string | null>(null);
  const [createBusy, setCreateBusy] = useState(false);

  // Join form
  const [joinCode, setJoinCode] = useState('');
  const [joinError, setJoinError] = useState<string | null>(null);
  const [joinBusy, setJoinBusy] = useState(false);
  const [joinSuccess, setJoinSuccess] = useState(false);

  const createInputRef = useRef<HTMLInputElement>(null);
  const joinInputRef = useRef<HTMLInputElement>(null);

  const loadChannels = useCallback(async (showLoading: boolean) => {
    if (showLoading) setLoading(true);
    requestInFlightRef.current = true;
    try {
      const result = await fetchChannels();
      setChannels(result);
      const ids = result.map((ch) => ch.id);
      fetchVoiceStatusWithFallback(ids).then((status) => {
        setVoiceStatus(status);
      }).catch(() => { /* silent */ });
    } catch {
      // silent — switcher is non-critical
    } finally {
      requestInFlightRef.current = false;
      if (showLoading) setLoading(false);
    }
  }, []);

  useEffect(() => {
    loadChannels(true);
  }, [loadChannels]);

  usePolling(() => {
    if (requestInFlightRef.current) return;
    loadChannels(false);
  }, POLL_MS);

  useEffect(() => {
    if (activeForm === 'create') createInputRef.current?.focus();
    else if (activeForm === 'join') joinInputRef.current?.focus();
  }, [activeForm]);

  const closeForm = useCallback(() => {
    setActiveForm('none');
    setCreateName(''); setCreateError(null);
    setJoinCode(''); setJoinError(null); setJoinSuccess(false);
  }, []);

  const toggleForm = useCallback((form: 'create' | 'join') => {
    setActiveForm((prev) => (prev === form ? 'none' : form));
    setCreateName(''); setCreateError(null);
    setJoinCode(''); setJoinError(null); setJoinSuccess(false);
  }, []);

  const handleCreate = useCallback(async () => {
    if (createBusy) return;
    const normalized = normalizeChannelName(createName);
    if (normalized.length < 1 || normalized.length > 100) {
      setCreateError('name must be 1–100 characters');
      return;
    }
    setCreateError(null);
    setCreateBusy(true);
    requestInFlightRef.current = true;
    try {
      const ch = await createChannel(createName);
      setChannels((prev) => [...prev, ch]);
      closeForm();
      onChannelSelect(ch);
    } catch (err) {
      setCreateError(err instanceof ApiError ? err.message : 'something went wrong — try again');
    } finally {
      setCreateBusy(false);
      requestInFlightRef.current = false;
    }
  }, [createName, createBusy, closeForm, onChannelSelect]);

  const handleJoin = useCallback(async () => {
    if (joinBusy) return;
    const code = joinCode.trim();
    if (!code) { setJoinError('enter an invite code'); return; }
    setJoinError(null);
    setJoinBusy(true);
    requestInFlightRef.current = true;
    try {
      const ch = await joinChannelByInvite(code);
      setChannels((prev) => prev.some((c) => c.id === ch.id) ? prev : [...prev, ch]);
      setJoinSuccess(true);
      setTimeout(() => { closeForm(); onChannelSelect(ch); }, 1200);
    } catch (err) {
      setJoinError(err instanceof ApiError ? err.message : 'something went wrong — try again');
    } finally {
      setJoinBusy(false);
      requestInFlightRef.current = false;
    }
  }, [joinCode, joinBusy, closeForm, onChannelSelect]);

  useEffect(() => {
    const handler = (e: KeyboardEvent) => {
      if (e.key === 'Escape' && activeForm !== 'none') closeForm();
    };
    document.addEventListener('keydown', handler);
    return () => document.removeEventListener('keydown', handler);
  }, [activeForm, closeForm]);

  const createValid = normalizeChannelName(createName).length >= 1;

  return (
    <div className="flex-1 flex flex-col min-h-0">
      <div className="px-3 py-3 border-b border-wavis-text-secondary h-[4.5rem] flex items-center justify-between">
        <div className="font-bold text-sm">CHANNELS</div>
        <button
          onClick={onClose}
          className="text-wavis-text-secondary hover:text-wavis-text transition-colors text-xs px-1"
          aria-label="Close channel switcher"
        >[x]</button>
      </div>

      <div className="flex-1 overflow-y-auto p-3 space-y-1">
        {loading && (
          <div className="text-wavis-text-secondary text-xs">loading...</div>
        )}
        {!loading && channels.length === 0 && (
          <div className="text-wavis-text-secondary text-xs">no channels</div>
        )}
        {!loading && channels.map((ch) => {
          const isCurrent = ch.id === currentChannelId;
          return (
            <div
              key={ch.id}
              onClick={() => { if (!isCurrent) onChannelSelect(ch); }}
              className={`flex items-center justify-between gap-3 px-3 py-2 border transition-colors cursor-pointer ${
                isCurrent
                  ? 'border-wavis-accent bg-wavis-accent/8 cursor-default'
                  : 'border-wavis-text-secondary hover:border-wavis-accent'
              }`}
            >
              <span className="min-w-0 truncate text-sm">{ch.name}</span>
              <div className="flex items-center gap-2 shrink-0">
                {voiceStatus.get(ch.id)?.active && (
                  <span className="text-wavis-accent text-xs flex items-center gap-1">
                    <span>●</span>
                    <span>{voiceStatus.get(ch.id)?.participantCount}</span>
                  </span>
                )}
                <ChannelRoleBadge role={ch.role} variant="list" />
                {isCurrent && (
                  <span className="text-wavis-accent text-[0.625rem]">current</span>
                )}
              </div>
            </div>
          );
        })}
      </div>

      {/* Inline forms */}
      {activeForm === 'create' && (
        <div className="border-t border-wavis-text-secondary px-3 py-3">
          <p className="text-xs text-wavis-text-secondary mb-2">create channel</p>
          <div className="flex items-center gap-2">
            <span className="text-wavis-accent shrink-0 text-xs">&gt;</span>
            <input
              ref={createInputRef}
              type="text"
              placeholder="channel name"
              value={createName}
              onChange={(e) => { setCreateName(e.target.value); setCreateError(null); }}
              onKeyDown={(e) => { if (e.key === 'Enter') void handleCreate(); }}
              disabled={createBusy}
              maxLength={100}
              className="flex-1 min-w-0 bg-transparent border-b border-wavis-text-secondary outline-none px-1 py-0.5 font-mono text-xs text-wavis-text disabled:opacity-40"
            />
            <button
              onClick={() => void handleCreate()}
              disabled={createBusy || !createValid}
              className="shrink-0 border border-wavis-accent text-wavis-accent hover:bg-wavis-accent hover:text-wavis-bg transition-colors px-1 py-0.5 text-[0.625rem] disabled:opacity-40 disabled:cursor-not-allowed"
            >/create</button>
          </div>
          {createError && <p className="text-wavis-danger text-xs mt-1 ml-3">{createError}</p>}
        </div>
      )}

      {activeForm === 'join' && (
        <div className="border-t border-wavis-text-secondary px-3 py-3">
          <p className="text-xs text-wavis-text-secondary mb-2">join by invite code</p>
          <div className="flex items-center gap-2">
            <span className="text-wavis-accent shrink-0 text-xs">&gt;</span>
            <input
              ref={joinInputRef}
              type="text"
              placeholder="INVITE CODE"
              value={joinCode}
              onChange={(e) => { setJoinCode(e.target.value); setJoinError(null); setJoinSuccess(false); }}
              onKeyDown={(e) => { if (e.key === 'Enter') void handleJoin(); }}
              disabled={joinBusy}
              className="flex-1 min-w-0 bg-transparent border-b border-wavis-text-secondary outline-none px-1 py-0.5 font-mono text-xs text-wavis-text disabled:opacity-40"
              style={{ letterSpacing: '0.15em' }}
            />
            <button
              onClick={() => void handleJoin()}
              disabled={joinBusy || !joinCode.trim()}
              className="shrink-0 border border-wavis-accent text-wavis-accent hover:bg-wavis-accent hover:text-wavis-bg transition-colors px-1 py-0.5 text-[0.625rem] disabled:opacity-40 disabled:cursor-not-allowed"
            >/join</button>
          </div>
          {joinSuccess && <p className="text-wavis-accent text-xs mt-1 ml-3">joined!</p>}
          {joinError && <p className="text-wavis-danger text-xs mt-1 ml-3">{joinError}</p>}
        </div>
      )}

      {/* Bottom command bar */}
      <div className="border-t border-wavis-text-secondary p-4">
        <div className="flex gap-4 border-b border-transparent w-full max-w-[1000px] mx-auto">
        <button
          onClick={() => toggleForm('create')}
          className={`flex-1 py-[7px] px-1 text-xs text-center transition-colors border ${activeForm === 'create' ? 'border-wavis-accent text-wavis-accent bg-wavis-accent/8 hover:bg-wavis-accent hover:text-wavis-bg' : 'border-wavis-text-secondary text-wavis-text hover:bg-wavis-text-secondary hover:text-wavis-text-contrast'}`}
        >/create</button>
        <button
          onClick={() => toggleForm('join')}
          className={`flex-1 py-[7px] px-1 text-xs text-center transition-colors border ${activeForm === 'join' ? 'border-wavis-accent text-wavis-accent bg-wavis-accent/8 hover:bg-wavis-accent hover:text-wavis-bg' : 'border-wavis-text-secondary text-wavis-text hover:bg-wavis-text-secondary hover:text-wavis-text-contrast'}`}
        >/join</button>
        </div>
      </div>
    </div>
  );
}
