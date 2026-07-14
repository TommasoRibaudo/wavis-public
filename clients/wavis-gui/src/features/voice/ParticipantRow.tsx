import { LoadingBars } from '@shared/LoadingBars';
import { VolumeSlider } from '@shared/VolumeSlider';
import { participantNameVisualState } from './active-room-participant-row';

/** Lightweight, render-safe snapshot of a participant — built fresh each parent
 * render via `.map()` in ActiveRoom, never the mutable RoomParticipant objects
 * held in voice-room.ts's module state (those are mutated in place, which makes
 * them unsafe to diff directly in a memoized child's props comparator). */
export interface ParticipantRowViewModel {
  id: string;
  userId?: string;
  displayName: string;
  color: string;
  isMuted: boolean;
  isHostMuted: boolean;
  isDeafened: boolean;
  isSpeaking: boolean;
  isSharing: boolean;
  mediaConnected: boolean;
  volume: number;
  cameraOn: boolean;
  isAudioOnlySharer: boolean;
  hasScreenShareStream: boolean;
}

function voiceIcon(
  p: Pick<ParticipantRowViewModel, 'isMuted' | 'isSpeaking'>,
  isDeafened?: boolean,
): { char: string; color: string; strikethrough?: boolean; transform?: string } {
  if (isDeafened)
    return { char: '¤', color: 'var(--wavis-danger)', transform: 'scale(1.25) translateY(8%)' };
  if (p.isMuted) return { char: '○', color: 'var(--wavis-danger)' };
  if (p.isSpeaking) return { char: '●', color: 'var(--wavis-accent)' };
  return { char: '○', color: 'var(--wavis-text-secondary)' };
}

export interface ParticipantRowProps {
  participant: ParticipantRowViewModel;
  isSelf: boolean;
  selfIsDeafened: boolean;
  hasActiveVideoShare: boolean;
  hasActiveAudioShare: boolean;
  isHost: boolean;
  isExpanded: boolean;
  onToggleExpanded: () => void;
  isLocallyMuted: boolean;
  onToggleLocalMicMute: () => void;
  onSetParticipantVolume: (volume: number) => void;
  shareVolume: number;
  shareMuted: boolean;
  onSyncShareVolume: (volume: number) => void;
  onSyncShareMuted: (muted: boolean) => void;
  isWatchingShare: boolean;
  onToggleWatchShare: () => void;
  onKick: () => void;
  onHostMute: () => void;
  onHostUnmute: () => void;
}

export function ParticipantRow({
  participant: p,
  isSelf,
  selfIsDeafened,
  hasActiveVideoShare,
  hasActiveAudioShare,
  isHost,
  isExpanded,
  onToggleExpanded,
  isLocallyMuted,
  onToggleLocalMicMute,
  onSetParticipantVolume,
  shareVolume,
  shareMuted,
  onSyncShareVolume,
  onSyncShareMuted,
  isWatchingShare,
  onToggleWatchShare,
  onKick,
  onHostMute,
  onHostUnmute,
}: ParticipantRowProps) {
  const isDeafened = isSelf ? selfIsDeafened : p.isDeafened;
  const icon = voiceIcon(p, isDeafened);
  const nameVisual = participantNameVisualState(p, isSelf);

  return (
    <div key={p.id} className="pl-2">
      <div
        role="button"
        tabIndex={isSelf ? -1 : 0}
        onClick={() => {
          if (!isSelf) onToggleExpanded();
        }}
        onKeyDown={(e) => {
          if (!isSelf && (e.key === 'Enter' || e.key === ' ')) onToggleExpanded();
        }}
        className="w-full text-left flex items-center gap-2 hover:opacity-80"
        style={{ cursor: isSelf ? 'default' : 'pointer' }}
      >
        {isSelf ? (
          <span className="text-xs text-wavis-accent inline-block w-6 text-center flex-none">
            &gt;
          </span>
        ) : (
          <span className="text-[0.625rem] text-wavis-text-secondary inline-block w-6 text-center flex-none">
            {isExpanded ? '[-]' : '[+]'}
          </span>
        )}
        <span className="inline-flex min-h-5 items-center gap-1.5">
          <span
            style={{
              color: nameVisual.color,
              opacity: nameVisual.opacity,
              animation: nameVisual.animation,
              filter: nameVisual.filter,
            }}
          >
            {p.displayName}
          </span>
        </span>
        <span
          style={{
            color: icon.color,
            textDecoration: icon.strikethrough ? 'line-through' : undefined,
            ...(icon.transform ? { display: 'inline-block', transform: icon.transform } : {}),
          }}
        >
          {icon.char}
        </span>
        {nameVisual.showConnecting && (
          <span className="inline-flex h-4 items-center gap-1 text-[0.625rem] leading-none text-wavis-text-secondary opacity-80">
            <LoadingBars size="sm" />
            <span>connecting</span>
          </span>
        )}
        <div className="ml-auto flex items-center gap-1">
          {p.cameraOn && (
            <span
              aria-label={isSelf ? 'your camera is on' : `${p.displayName}'s camera is on`}
              title={isSelf ? 'your camera is on' : `${p.displayName}'s camera is on`}
              style={{
                display: 'inline-block',
                width: '0.75rem',
                height: '0.75rem',
                backgroundColor: 'var(--wavis-accent)',
                WebkitMaskImage: 'url(/video-camera.png)',
                WebkitMaskSize: 'contain',
                WebkitMaskRepeat: 'no-repeat',
                WebkitMaskPosition: 'center',
                maskImage: 'url(/video-camera.png)',
                maskSize: 'contain',
                maskRepeat: 'no-repeat',
                maskPosition: 'center',
                pointerEvents: 'none',
              }}
            />
          )}
          {isSelf && p.isSharing && (
            <>
              {(hasActiveVideoShare || !hasActiveAudioShare) && (
                <span
                  className="text-sm leading-none"
                  style={{
                    color: 'var(--wavis-danger)',
                    animation: 'watchPulse 2s ease-in-out infinite',
                  }}
                  title="you are sharing"
                >
                  {'◉'}
                </span>
              )}
              {hasActiveAudioShare && (
                <span
                  className="text-sm leading-none"
                  style={{
                    color: 'var(--wavis-danger)',
                    animation: 'watchPulse 2s ease-in-out infinite',
                  }}
                  title="you are sharing audio"
                >
                  {'♪'}
                </span>
              )}
            </>
          )}
          {!isSelf &&
            p.isSharing &&
            (p.isAudioOnlySharer ? (
              <button
                className="text-sm leading-none hover:opacity-70 transition-opacity"
                style={{
                  color: shareMuted ? 'var(--wavis-danger)' : 'transparent',
                  WebkitTextStroke: shareMuted ? undefined : '1px var(--wavis-danger)',
                  animation: shareMuted ? 'watchPulse 2s ease-in-out infinite' : undefined,
                }}
                title={shareMuted ? 'unmute audio share' : 'mute audio share'}
                onClick={(e) => {
                  e.stopPropagation();
                  onSyncShareMuted(!shareMuted);
                }}
              >
                {'♪'}
              </button>
            ) : (
              <button
                onClick={(e) => {
                  e.stopPropagation();
                  if (!p.hasScreenShareStream) return;
                  onToggleWatchShare();
                }}
                className="text-sm leading-none"
                style={
                  isWatchingShare
                    ? { color: 'var(--wavis-danger)' }
                    : p.hasScreenShareStream
                      ? {
                          color: 'var(--wavis-danger)',
                          animation: 'watchPulse 2s ease-in-out infinite',
                        }
                      : { color: 'var(--wavis-text-secondary)', opacity: 0.4 }
                }
                title={
                  isWatchingShare
                    ? 'close share'
                    : p.hasScreenShareStream
                      ? 'watch share'
                      : 'waiting for stream...'
                }
              >
                {isWatchingShare ? '◎' : '◉'}
              </button>
            ))}
        </div>
      </div>
      {isExpanded && !isSelf && (
        <div className="pl-6 py-1 space-y-0.5 text-xs">
          <div className="flex items-center gap-2">
            <span className="text-wavis-text-secondary shrink-0">mic</span>
            <div className="flex-1">
              <VolumeSlider
                value={p.volume}
                onChange={(v) => {
                  if (isLocallyMuted) onToggleLocalMicMute();
                  onSetParticipantVolume(v);
                }}
                color={p.color}
              />
            </div>
            <span className="text-wavis-text-secondary w-6 text-right">
              {isLocallyMuted ? 0 : p.volume}
            </span>
            <button
              onClick={onToggleLocalMicMute}
              className="text-xs border px-1 py-0.5 transition-colors hover:opacity-70 shrink-0"
              style={
                isLocallyMuted
                  ? { color: 'var(--wavis-warn)', borderColor: 'var(--wavis-warn)' }
                  : undefined
              }
              title={isLocallyMuted ? 'unmute mic (local)' : 'mute mic (local)'}
            >
              {isLocallyMuted ? '/unmute' : '/mute'}
            </button>
          </div>
          {p.isSharing && (
            <div className="flex items-center gap-2 mt-1">
              <span className="text-wavis-text-secondary shrink-0">share vol</span>
              <div className="flex-1">
                <VolumeSlider
                  value={shareVolume}
                  onChange={(v) => {
                    onSyncShareVolume(v);
                  }}
                  color={p.color}
                />
              </div>
              <span className="text-wavis-text-secondary w-6 text-right">
                {shareMuted ? 0 : shareVolume}
              </span>
              <button
                onClick={() => onSyncShareMuted(!shareMuted)}
                className="text-xs border px-1 py-0.5 transition-colors hover:opacity-70 shrink-0"
                style={
                  shareMuted
                    ? { color: 'var(--wavis-warn)', borderColor: 'var(--wavis-warn)' }
                    : undefined
                }
                title={shareMuted ? 'unmute sys audio (local)' : 'mute sys audio (local)'}
              >
                {shareMuted ? '/unmute' : '/mute'}
              </button>
            </div>
          )}
          {isHost && (
            <div className="flex flex-wrap items-center justify-center gap-2">
              <button
                onClick={onKick}
                className="text-xs text-center border border-wavis-danger text-wavis-danger px-1 py-0.5 transition-colors hover:opacity-70"
              >
                /kick
              </button>
              {p.isHostMuted ? (
                <button
                  onClick={onHostUnmute}
                  className="text-xs text-center border border-wavis-accent text-wavis-accent px-1 py-0.5 transition-colors hover:opacity-70"
                >
                  /unmute
                </button>
              ) : (
                !p.isMuted && (
                  <button
                    onClick={onHostMute}
                    className="text-xs text-center border px-1 py-0.5 transition-colors hover:opacity-70"
                    style={{ color: 'var(--wavis-warn)', borderColor: 'var(--wavis-warn)' }}
                  >
                    /mute
                  </button>
                )
              )}
            </div>
          )}
        </div>
      )}
    </div>
  );
}
