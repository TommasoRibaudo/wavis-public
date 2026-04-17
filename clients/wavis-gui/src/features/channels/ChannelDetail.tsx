import { useState, useEffect, useCallback, useRef } from 'react';
import { useParams, useNavigate } from 'react-router';
import { writeText } from '@tauri-apps/plugin-clipboard-manager';
import { CmdButton } from '@shared/CmdButton';
import { ConfirmTextGate } from '@shared/ConfirmTextGate';
import { ErrorPanel } from '@shared/ErrorPanel';
import { EmptyState } from '@shared/EmptyState';
import { LoadingBlock } from '@shared/LoadingBlock';
import { usePolling } from '@shared/hooks/usePolling';
import { ApiError } from '@shared/api';
import { getDeviceId } from '@features/auth/auth';
import type { ChannelRole } from './channels';
import {
  type ChannelDetailData,
  type ChannelMember,
  type VoiceStatus,
  type ChannelInvite,
  type BannedMember,
  fetchChannelDetail,
  fetchVoiceStatus,
  fetchInvites,
  createInvite,
  revokeInvite,
  fetchBannedMembers,
  banMember,
  unbanMember,
  changeMemberRole,
  deleteChannel,
  leaveChannel,
} from './channel-detail';
import {
  channelDetailErrorMessage as errorMessage,
  sortMembers,
  getCommands,
} from '@shared/helpers';
import { ChannelRoleBadge } from './ChannelRoleBadge';

/* ─── Constants ─────────────────────────────────────────────────── */
const DETAIL_POLL_MS = 15_000;
const VOICE_POLL_MS = 5_000;
const SUCCESS_DISPLAY_MS = 2_000;
const DIVIDER = '─'.repeat(48);

/* ─── Sub-components ────────────────────────────────────────────── */

function MemberRow({
  member,
  isMe,
  myRole,
  onBan,
  onRole,
  roleTarget,
  setRoleTarget,
  submitting,
  onRoleChange,
}: {
  member: ChannelMember;
  isMe: boolean;
  myRole: ChannelRole;
  onBan?: (userId: string) => void;
  onRole?: (userId: string) => void;
  roleTarget: string | null;
  setRoleTarget: (id: string | null) => void;
  submitting: boolean;
  onRoleChange: (userId: string, role: 'admin' | 'member') => void;
}) {
  const showRoleBtn = myRole === 'owner' && member.role !== 'owner';
  const isRoleTarget = roleTarget === member.userId;

  return (
    <div className={`flex items-center gap-3 px-3 py-2 ${isMe ? 'text-wavis-accent' : ''}`}>
      <span className="min-w-0 truncate flex-1 text-sm">
        {member.displayName || member.userId}
        {isMe && <span className="text-xs ml-1">(you)</span>}
      </span>
      <ChannelRoleBadge role={member.role} className="shrink-0" />
      {showRoleBtn && !isRoleTarget && (
        <button
          onClick={() => onRole?.(member.userId)}
          disabled={submitting}
          className="text-xs text-wavis-text-secondary disabled:opacity-40 disabled:cursor-not-allowed shrink-0 border border-wavis-text-secondary py-0.5 px-1 text-center transition-colors hover:bg-wavis-text-secondary hover:text-wavis-text-contrast"
        >
          /role
        </button>
      )}
      {isRoleTarget && (
        <div className="flex items-center gap-1 shrink-0">
          <button
            onClick={() => { onRoleChange(member.userId, 'admin'); setRoleTarget(null); }}
            disabled={submitting || member.role === 'admin'}
            className="text-xs border border-wavis-warn text-wavis-warn hover:bg-wavis-warn hover:text-wavis-bg transition-colors px-1 py-0.5 disabled:opacity-40 disabled:cursor-not-allowed"
          >
            ADMIN
          </button>
          <button
            onClick={() => { onRoleChange(member.userId, 'member'); setRoleTarget(null); }}
            disabled={submitting || member.role === 'member'}
            className="text-xs border border-wavis-text-secondary text-wavis-text-secondary hover:bg-wavis-text-secondary hover:text-wavis-bg transition-colors px-1 py-0.5 disabled:opacity-40 disabled:cursor-not-allowed"
          >
            MEMBER
          </button>
          <button
            onClick={() => setRoleTarget(null)}
            className="text-xs text-wavis-text-secondary hover:text-wavis-text ml-1"
          >
            ✕
          </button>
        </div>
      )}
      {onBan && !isMe && member.role !== 'owner' && (myRole === 'owner' || (myRole === 'admin' && member.role !== 'admin')) && (
        <button
          onClick={() => onBan(member.userId)}
          disabled={submitting}
          className="text-xs text-wavis-danger disabled:opacity-40 disabled:cursor-not-allowed shrink-0 border border-wavis-danger py-0.5 px-1 text-center transition-colors hover:bg-wavis-danger hover:text-wavis-bg"
        >
          /ban
        </button>
      )}
      <span className="text-xs text-wavis-text-secondary shrink-0 ml-auto">
        {new Date(member.joinedAt).toLocaleDateString()}
      </span>
    </div>
  );
}

function VoiceStatusPanel({ voice }: { voice: VoiceStatus | null }) {
  if (!voice) return null;
  return (
    <div className="mt-2">
      {voice.active ? (
        <div>
          <span className="text-wavis-accent text-sm">● ACTIVE</span>
          <span className="text-wavis-text-secondary text-xs ml-2">
            {voice.participantCount ?? 0} participant{(voice.participantCount ?? 0) !== 1 ? 's' : ''}
          </span>
          {voice.participants && voice.participants.length > 0 && (
            <div className="text-xs text-wavis-text-secondary mt-1 ml-4">
              {voice.participants.map((p, i) => (
                <span key={i}>
                  {i > 0 && ', '}
                  {p.displayName}
                </span>
              ))}
            </div>
          )}
        </div>
      ) : (
        <span className="text-wavis-text-secondary text-sm">No active voice session</span>
      )}
    </div>
  );
}


function InvitePanel({
  channelId,
  submitting,
  onSuccess,
  onError,
}: {
  channelId: string;
  submitting: boolean;
  onSuccess: (msg: string) => void;
  onError: (msg: string) => void;
}) {
  const [expiry, setExpiry] = useState('');
  const [maxUses, setMaxUses] = useState('');
  const [busy, setBusy] = useState(false);
  const [generatedCode, setGeneratedCode] = useState<string | null>(null);

  const handleGenerate = async () => {
    if (busy) return;
    setBusy(true);
    setGeneratedCode(null);
    try {
      const expiryNum = expiry.trim() ? parseInt(expiry.trim(), 10) : undefined;
      const maxUsesNum = maxUses.trim() ? parseInt(maxUses.trim(), 10) : undefined;
      const invite = await createInvite(channelId, expiryNum, maxUsesNum);

      // Tiered clipboard: Tauri plugin → navigator → inline display
      let copied = false;
      try {
        await writeText(invite.code);
        copied = true;
      } catch {
        try {
          await navigator.clipboard.writeText(invite.code);
          copied = true;
        } catch {
          // last resort: display inline
        }
      }

      if (copied) {
        onSuccess(`invite code copied: ${invite.code}`);
      } else {
        setGeneratedCode(invite.code);
        onSuccess('invite created (copy manually below)');
      }
    } catch (err) {
      if (err instanceof ApiError) {
        onError(errorMessage(err.kind));
      } else {
        onError('something went wrong — try again');
      }
    } finally {
      setBusy(false);
    }
  };

  return (
    <div className="mt-2 p-3 bg-wavis-panel border border-wavis-text-secondary">
      <p className="text-sm text-wavis-text-secondary mb-2">generate invite code</p>
      <div className="flex flex-wrap items-center gap-2 mb-2">
        <label className="text-xs text-wavis-text-secondary">
          expiry (s):
          <input
            type="number"
            value={expiry}
            onChange={(e) => setExpiry(e.target.value)}
            disabled={busy || submitting}
            className="ml-1 w-20 bg-transparent border-b border-wavis-text-secondary outline-none px-1 py-0.5 font-mono text-wavis-text text-xs disabled:opacity-40 disabled:cursor-not-allowed"
            aria-label="Expiry in seconds"
          />
        </label>
        <label className="text-xs text-wavis-text-secondary">
          max uses:
          <input
            type="number"
            value={maxUses}
            onChange={(e) => setMaxUses(e.target.value)}
            disabled={busy || submitting}
            className="ml-1 w-20 bg-transparent border-b border-wavis-text-secondary outline-none px-1 py-0.5 font-mono text-wavis-text text-xs disabled:opacity-40 disabled:cursor-not-allowed"
            aria-label="Max uses"
          />
        </label>
      </div>
      <button
        onClick={handleGenerate}
        disabled={busy || submitting}
        className="border border-wavis-accent text-wavis-accent hover:bg-wavis-accent hover:text-wavis-bg transition-colors px-1 py-0.5 text-xs disabled:opacity-40 disabled:cursor-not-allowed"
      >
        {busy ? 'generating...' : '/generate'}
      </button>
      {generatedCode && (
        <div className="mt-2 text-sm">
          <span className="text-wavis-text-secondary">code: </span>
          <span className="text-wavis-accent font-bold" style={{ letterSpacing: '0.15em' }}>
            {generatedCode}
          </span>
        </div>
      )}
    </div>
  );
}

function RevokePanel({
  channelId,
  submitting,
  onMutation,
}: {
  channelId: string;
  submitting: boolean;
  onMutation: (fn: () => Promise<void>, msg: string) => Promise<void>;
}) {
  const [invites, setInvites] = useState<ChannelInvite[] | null>(null);
  const [loadError, setLoadError] = useState<string | null>(null);

  useEffect(() => {
    let cancelled = false;
    (async () => {
      try {
        const list = await fetchInvites(channelId);
        if (!cancelled) setInvites(list);
      } catch (err) {
        if (!cancelled) {
          setLoadError(err instanceof ApiError ? errorMessage(err.kind) : 'failed to load invites');
        }
      }
    })();
    return () => { cancelled = true; };
  }, [channelId]);

  if (loadError) {
    return (
      <div className="mt-2 p-3 bg-wavis-panel border border-wavis-text-secondary">
        <p className="text-wavis-danger text-sm">{loadError}</p>
      </div>
    );
  }

  if (!invites) {
    return (
      <LoadingBlock
        message="loading invites..."
        className="mt-2 p-3 bg-wavis-panel border border-wavis-text-secondary text-sm"
      />
    );
  }

  if (invites.length === 0) {
    return <EmptyState message="no active invites" className="mt-2 p-3" />;
  }

  return (
    <div className="mt-2 p-3 bg-wavis-panel border border-wavis-text-secondary">
      <p className="text-sm text-wavis-text-secondary mb-2">active invites</p>
      {invites.map((inv) => (
        <div key={inv.code} className="flex items-center gap-3 py-1">
          <span className="text-sm text-wavis-text font-mono" style={{ letterSpacing: '0.1em' }}>
            {inv.code}
          </span>
          <span className="text-xs text-wavis-text-secondary">
            {inv.uses}/{inv.maxUses ?? '∞'} uses
          </span>
          <button
            onClick={() =>
              onMutation(
                async () => {
                  await revokeInvite(channelId, inv.code);
                  setInvites((prev) => prev?.filter((i) => i.code !== inv.code) ?? null);
                },
                'invite revoked',
              )
            }
            disabled={submitting}
            className="text-xs text-wavis-danger disabled:opacity-40 disabled:cursor-not-allowed border border-wavis-danger py-0.5 px-1 text-center transition-colors hover:bg-wavis-danger hover:text-wavis-bg"
          >
            /revoke
          </button>
        </div>
      ))}
    </div>
  );
}

function BanPanel({
  members,
  myRole,
  myUserId,
  submitting,
  onBan,
}: {
  members: ChannelMember[];
  myRole: ChannelRole;
  myUserId: string;
  submitting: boolean;
  onBan: (userId: string) => void;
}) {
  const [localMembers, setLocalMembers] = useState(members);

  useEffect(() => {
    setLocalMembers(members);
  }, [members]);

  const bannable = localMembers.filter((m) => {
    if (m.userId === myUserId) return false;
    if (m.role === 'owner') return false;
    if (myRole === 'admin' && m.role === 'admin') return false;
    return true;
  });

  if (bannable.length === 0) {
    return <EmptyState message="no bannable members" className="mt-2 p-3" />;
  }

  return (
    <div className="mt-2 p-3 bg-wavis-panel border border-wavis-text-secondary">
      <p className="text-sm text-wavis-text-secondary mb-2">ban member</p>
      {bannable.map((m) => {
        return (
          <div key={m.userId} className="flex items-center gap-3 py-1">
            <span className="text-sm text-wavis-text truncate flex-1">{m.userId}</span>
            <ChannelRoleBadge role={m.role} className="shrink-0" />
            <button
              onClick={() => {
                onBan(m.userId);
                setLocalMembers((prev) => prev.filter((x) => x.userId !== m.userId));
              }}
              disabled={submitting}
              className="text-xs text-wavis-danger disabled:opacity-40 disabled:cursor-not-allowed shrink-0 border border-wavis-danger py-0.5 px-1 text-center transition-colors hover:bg-wavis-danger hover:text-wavis-bg"
            >
              /ban
            </button>
          </div>
        );
      })}
    </div>
  );
}

function UnbanPanel({
  channelId,
  submitting,
  onMutation,
}: {
  channelId: string;
  submitting: boolean;
  onMutation: (fn: () => Promise<void>, msg: string) => Promise<void>;
}) {
  const [banned, setBanned] = useState<BannedMember[] | null>(null);
  const [loadError, setLoadError] = useState<string | null>(null);

  useEffect(() => {
    let cancelled = false;
    (async () => {
      try {
        const list = await fetchBannedMembers(channelId);
        if (!cancelled) setBanned(list);
      } catch (err) {
        if (!cancelled) {
          setLoadError(err instanceof ApiError ? errorMessage(err.kind) : 'failed to load bans');
        }
      }
    })();
    return () => { cancelled = true; };
  }, [channelId]);

  if (loadError) {
    return (
      <div className="mt-2 p-3 bg-wavis-panel border border-wavis-text-secondary">
        <p className="text-wavis-danger text-sm">{loadError}</p>
      </div>
    );
  }

  if (!banned) {
    return (
      <LoadingBlock
        message="loading banned members..."
        className="mt-2 p-3 bg-wavis-panel border border-wavis-text-secondary text-sm"
      />
    );
  }

  if (banned.length === 0) {
    return <EmptyState message="no banned members" className="mt-2 p-3" />;
  }

  return (
    <div className="mt-2 p-3 bg-wavis-panel border border-wavis-text-secondary">
      <p className="text-sm text-wavis-text-secondary mb-2">banned members</p>
      {banned.map((b) => (
        <div key={b.userId} className="flex items-center gap-3 py-1">
          <span className="text-sm text-wavis-text truncate flex-1">{b.userId}</span>
          <span className="text-xs text-wavis-text-secondary shrink-0">
            {new Date(b.bannedAt).toLocaleDateString()}
          </span>
          <button
            onClick={() =>
              onMutation(
                async () => {
                  await unbanMember(channelId, b.userId);
                  setBanned((prev) => prev?.filter((x) => x.userId !== b.userId) ?? null);
                },
                'member unbanned',
              )
            }
            disabled={submitting}
            className="text-xs text-wavis-accent disabled:opacity-40 disabled:cursor-not-allowed shrink-0 border border-wavis-accent py-0.5 px-1 text-center transition-colors hover:bg-wavis-accent hover:text-wavis-bg"
          >
            /unban
          </button>
        </div>
      ))}
    </div>
  );
}

/* ═══ Component ═════════════════════════════════════════════════════ */
interface ChannelDetailProps {
  channelIdProp?: string;
  hideJoinVoice?: boolean;
  hideBackButton?: boolean;
}

export default function ChannelDetail({ channelIdProp, hideJoinVoice, hideBackButton }: ChannelDetailProps = {}) {
  const { channelId: channelIdParam } = useParams();
  const channelId = channelIdProp ?? channelIdParam;
  const navigate = useNavigate();

  /* ── State ── */
  const [detail, setDetail] = useState<ChannelDetailData | null>(null);
  const [voice, setVoice] = useState<VoiceStatus | null>(null);
  const [loading, setLoading] = useState(true);
  const [error, setError] = useState<string | null>(null);
  const [activePanel, setActivePanel] = useState<
    'none' | 'invite' | 'revoke' | 'ban' | 'unban' | 'role'
  >('none');
  const [confirmAction, setConfirmAction] = useState<'none' | 'delete' | 'leave'>('none');
  const [submitting, setSubmitting] = useState(false);
  const [successMsg, setSuccessMsg] = useState<string | null>(null);
  const [mutationError, setMutationError] = useState<string | null>(null);
  const [roleTarget, setRoleTarget] = useState<string | null>(null);
  const [myUserId, setMyUserId] = useState<string | null>(null);

  /* ── Refs ── */
  const skipNextRefresh = useRef(false);
  const abortRef = useRef<AbortController | null>(null);
  const hasLoaded = useRef(false);

  /* ── Resolve device ID once ── */
  useEffect(() => {
    getDeviceId().then((id) => setMyUserId(id));
  }, []);

  /* ── loadDetail ── */
  const loadDetail = useCallback(
    async (silent = false) => {
      if (!channelId) return;

      if (silent) {
        if (skipNextRefresh.current) {
          skipNextRefresh.current = false;
          return;
        }
        if (!hasLoaded.current) return;
      }

      // Abort previous in-flight request
      if (abortRef.current) abortRef.current.abort();
      const controller = new AbortController();
      abortRef.current = controller;

      if (!silent) setLoading(true);
      setError(null);

      try {
        const data = await fetchChannelDetail(channelId, controller.signal);
        setDetail(data);
        hasLoaded.current = true;
      } catch (err) {
        if (err instanceof DOMException && err.name === 'AbortError') return;
        if (controller.signal.aborted) return;
        // Channel deleted (by us or someone else) — redirect to list
        if (err instanceof ApiError && err.kind === 'NotFound') {
          navigate('/', { replace: true });
          return;
        }
        if (!silent) {
          if (err instanceof ApiError) {
            setError(errorMessage(err.kind));
          } else {
            setError('something went wrong — try again');
          }
        }
      } finally {
        if (!silent) setLoading(false);
      }
    },
    [channelId],
  );

  /* ── loadVoice ── */
  const loadVoice = useCallback(async () => {
    if (!channelId || !hasLoaded.current) return;
    try {
      const status = await fetchVoiceStatus(channelId);
      setVoice(status);
    } catch (err) {
      console.error('[wavis:channel-detail] voice poll error:', err);
    }
  }, [channelId]);

  /* ── Initial load ── */
  useEffect(() => {
    loadDetail();
  }, [loadDetail]);

  /* ── Detail auto-refresh (15s) ── */
  usePolling(() => loadDetail(true), DETAIL_POLL_MS);

  /* ── Voice status poll (5s) ── */
  usePolling(loadVoice, VOICE_POLL_MS);

  /* ── Success message auto-clear ── */
  useEffect(() => {
    if (!successMsg) return;
    const id = setTimeout(() => setSuccessMsg(null), SUCCESS_DISPLAY_MS);
    return () => clearTimeout(id);
  }, [successMsg]);

  /* ── handleMutation ── */
  const handleMutation = useCallback(
    async (fn: () => Promise<void>, successMessage: string) => {
      if (!channelId) return;
      setSubmitting(true);
      setMutationError(null);
      setSuccessMsg(null);

      try {
        await fn();
        skipNextRefresh.current = true;
        if (abortRef.current) abortRef.current.abort();
        try {
          const data = await fetchChannelDetail(channelId);
          setDetail(data);
        } catch {
          // re-fetch failed, will catch up on next poll
        }
        setSuccessMsg(successMessage);
      } catch (err) {
        if (err instanceof ApiError) {
          if (err.kind === 'NotFound' || err.kind === 'AlreadyBanned') {
            // State has changed — re-fetch and show informational message
            skipNextRefresh.current = true;
            if (abortRef.current) abortRef.current.abort();
            try {
              const data = await fetchChannelDetail(channelId);
              setDetail(data);
            } catch {
              // re-fetch failed
            }
            setSuccessMsg('state has changed — refreshed');
          } else {
            setMutationError(errorMessage(err.kind));
          }
        } else {
          setMutationError('something went wrong — try again');
        }
      } finally {
        setSubmitting(false);
      }
    },
    [channelId],
  );

  /* ── Panel toggle ── */
  const togglePanel = useCallback(
    (panel: 'invite' | 'revoke' | 'ban' | 'unban' | 'role') => {
      setActivePanel((prev) => (prev === panel ? 'none' : panel));
      setRoleTarget(null);
      setMutationError(null);
      setSuccessMsg(null);
      setConfirmAction('none');
    },
    [],
  );

  /* ── Command handlers ── */
  const handleJoinVoice = useCallback(() => {
    if (!channelId || !detail) return;
    navigate('/room', { state: { channelId, channelName: detail.name, channelRole: detail.role } });
  }, [channelId, detail, navigate]);

  const handleDelete = useCallback(async () => {
    if (!channelId) return;
    setSubmitting(true);
    setMutationError(null);
    try {
      await deleteChannel(channelId);
      navigate('/', { replace: true });
    } catch (err) {
      if (err instanceof ApiError) {
        setMutationError(errorMessage(err.kind));
      } else {
        setMutationError('something went wrong — try again');
      }
    } finally {
      setSubmitting(false);
    }
  }, [channelId, navigate]);

  const handleLeave = useCallback(async () => {
    if (!channelId) return;
    setSubmitting(true);
    setMutationError(null);
    try {
      await leaveChannel(channelId);
      navigate('/', { replace: true });
    } catch (err) {
      if (err instanceof ApiError) {
        setMutationError(errorMessage(err.kind));
      } else {
        setMutationError('something went wrong — try again');
      }
    } finally {
      setSubmitting(false);
    }
  }, [channelId, navigate]);

  const handleBan = useCallback(
    (userId: string) => {
      handleMutation(() => banMember(channelId!, userId), 'member banned');
    },
    [channelId, handleMutation],
  );

  const handleRoleChange = useCallback(
    (userId: string, role: 'admin' | 'member') => {
      handleMutation(() => changeMemberRole(channelId!, userId, role), 'role updated');
    },
    [channelId, handleMutation],
  );

  const handleCmdClick = useCallback(
    (cmd: string) => {
      switch (cmd) {
        case '/voice':
          handleJoinVoice();
          break;
        case '/invite':
          togglePanel('invite');
          break;
        case '/revoke':
          togglePanel('revoke');
          break;
        case '/ban':
          togglePanel('ban');
          break;
        case '/unban':
          togglePanel('unban');
          break;
        case '/role':
          togglePanel('role');
          break;
        case '/delete':
          setConfirmAction('delete');
          setActivePanel('none');
          setMutationError(null);
          setSuccessMsg(null);
          break;
        case '/leave':
          setConfirmAction('leave');
          setActivePanel('none');
          setMutationError(null);
          setSuccessMsg(null);
          break;
        case '/back':
          navigate('/');
          break;
      }
    },
    [handleJoinVoice, togglePanel, navigate],
  );

  /* ── Derived ── */
  const role = detail?.role ?? 'member';
  const allCommands = detail ? getCommands(role) : ['/back'];
  const adminCommands = allCommands.filter(
    (c) => c !== '/voice' && c !== '/leave' && c !== '/back',
  );
  const bottomCommands = allCommands.filter(
    (c) =>
      (c === '/voice' || c === '/leave' || c === '/back') &&
      (!hideJoinVoice || c !== '/voice') &&
      (!hideBackButton || c !== '/back'),
  );
  const sorted = detail ? sortMembers(detail.members) : [];

  /* ── Render ── */
  return (
    <div className="h-full flex flex-col bg-wavis-bg font-mono text-wavis-text">
      <div className="flex-1 overflow-y-auto">
        <div className="max-w-2xl mx-auto px-3 sm:px-6 py-6">
          {/* Back button */}
          {!hideBackButton && (
            <button
              onClick={() => navigate('/')}
              className="mb-4 text-xs text-wavis-text-secondary border border-wavis-text-secondary py-0.5 px-1 text-center transition-colors hover:bg-wavis-text-secondary hover:text-wavis-text-contrast"
            >
              ← /channels
            </button>
          )}

          {/* Loading */}
          {loading && <LoadingBlock />}

          {/* Error */}
          {!loading && error && (
            <ErrorPanel error={error} onRetry={() => loadDetail()} />
          )}

          {/* Detail loaded */}
          {!loading && !error && detail && (
            <>
              {/* Channel header */}
              <div className="flex items-center gap-3 mb-2">
                <h2>{detail.name}</h2>
                <ChannelRoleBadge role={role} />
              </div>

              <div className="text-wavis-text-secondary overflow-hidden">{DIVIDER}</div>

              {/* Voice status */}
              {!hideJoinVoice && (
                <>
                  <div className="mt-4">
                    <p className="text-sm text-wavis-text-secondary mb-1">VOICE</p>
                    <VoiceStatusPanel voice={voice} />
                    <button
                      onClick={handleJoinVoice}
                      className="mt-2 border border-wavis-accent text-wavis-accent hover:bg-wavis-accent hover:text-wavis-bg transition-colors px-1 py-0.5 text-xs"
                    >
                      /join voice
                    </button>
                  </div>

                  <div className="text-wavis-text-secondary mt-4 overflow-hidden">{DIVIDER}</div>
                </>
              )}

              {/* Members */}
              <div className="mt-4">
                <p className="text-sm text-wavis-text-secondary mb-2">
                  MEMBERS ({detail.members.length}/6)
                </p>
                <div className="flex flex-col">
                  {sorted.map((m) => (
                    <MemberRow
                      key={m.userId}
                      member={m}
                      isMe={m.userId === myUserId}
                      myRole={role}
                      onBan={activePanel === 'ban' ? undefined : undefined}
                      onRole={role === 'owner' ? () => setRoleTarget(m.userId) : undefined}
                      roleTarget={roleTarget}
                      setRoleTarget={setRoleTarget}
                      submitting={submitting}
                      onRoleChange={handleRoleChange}
                    />
                  ))}
                </div>
              </div>

              {/* Admin actions (inline, below members) */}
              {adminCommands.length > 0 && (
                <>
                  <div className="text-wavis-text-secondary mt-4 overflow-hidden">{DIVIDER}</div>
                  <div className="mt-3 flex flex-wrap items-center gap-x-6 gap-y-2">
                    {adminCommands.map((cmd) => (
                      <CmdButton
                        key={cmd}
                        label={cmd}
                        onClick={() => handleCmdClick(cmd)}
                        active={
                          (cmd === '/invite' && activePanel === 'invite') ||
                          (cmd === '/revoke' && activePanel === 'revoke') ||
                          (cmd === '/ban' && activePanel === 'ban') ||
                          (cmd === '/unban' && activePanel === 'unban') ||
                          (cmd === '/role' && activePanel === 'role')
                        }
                        danger={cmd === '/delete'}
                        disabled={submitting && cmd !== '/back'}
                      />
                    ))}
                  </div>

                  {/* Feedback messages */}
                  {successMsg && (
                    <p className="text-wavis-accent text-sm mt-2">{successMsg}</p>
                  )}
                  {mutationError && (
                    <p className="text-wavis-danger text-sm mt-2">{mutationError}</p>
                  )}

                  {/* Active panel */}
                  {activePanel === 'invite' && (
                    <InvitePanel
                      channelId={channelId!}
                      submitting={submitting}
                      onSuccess={(msg) => setSuccessMsg(msg)}
                      onError={(msg) => setMutationError(msg)}
                    />
                  )}
                  {activePanel === 'revoke' && (
                    <RevokePanel
                      channelId={channelId!}
                      submitting={submitting}
                      onMutation={handleMutation}
                    />
                  )}
                  {activePanel === 'ban' && (
                    <BanPanel
                      members={sorted}
                      myRole={role}
                      myUserId={myUserId ?? ''}
                      submitting={submitting}
                      onBan={handleBan}
                    />
                  )}
                  {activePanel === 'unban' && (
                    <UnbanPanel
                      channelId={channelId!}
                      submitting={submitting}
                      onMutation={handleMutation}
                    />
                  )}

                  {/* Confirmation prompts */}
                  {confirmAction === 'delete' && (
                    <ConfirmTextGate
                      requiredText="YES"
                      message="Delete this channel permanently?"
                      busy={submitting}
                      onConfirm={handleDelete}
                      onCancel={() => setConfirmAction('none')}
                    />
                  )}
                </>
              )}

              {/* Leave confirmation (available to non-owners) */}
              {confirmAction === 'leave' && (
                <ConfirmTextGate
                  requiredText="YES"
                  message="Leave this channel?"
                  busy={submitting}
                  onConfirm={handleLeave}
                  onCancel={() => setConfirmAction('none')}
                />
              )}
            </>
          )}
        </div>
      </div>

      {/* Bottom command bar */}
      <div className="border-t border-wavis-text-secondary px-3 sm:px-6 py-3 flex flex-wrap items-center gap-x-6 gap-y-2">
        {bottomCommands.map((cmd) => (
          <CmdButton
            key={cmd}
            label={cmd === '/leave' ? '/abandon channel' : cmd}
            onClick={() => handleCmdClick(cmd)}
            active={false}
            danger={cmd === '/leave'}
            disabled={submitting && cmd !== '/back'}
          />
        ))}
      </div>
    </div>
  );
}
