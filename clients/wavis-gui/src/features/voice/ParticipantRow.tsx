import {
  Mic,
  MicOff,
  HeadphoneOff,
  Camera,
  ScreenShare,
  ScreenShareOff,
  Music,
  type LucideIcon,
} from 'lucide-react';
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
  /**
   * True when this participant has an active video-type share, independent
   * of isAudioOnlySharer — a participant can run a video share and a
   * standalone audio-only share at the same time, so both badges must be
   * able to render together instead of one hiding the other.
   */
  hasVideoShare: boolean;
  hasScreenShareStream: boolean;
}

function voiceIcon(
  p: Pick<ParticipantRowViewModel, 'isMuted' | 'isSpeaking'>,
  isDeafened?: boolean,
): { Icon: LucideIcon; color: string } {
  if (isDeafened) return { Icon: HeadphoneOff, color: 'var(--wavis-danger)' };
  if (p.isMuted) return { Icon: MicOff, color: 'var(--wavis-danger)' };
  if (p.isSpeaking) return { Icon: Mic, color: 'var(--wavis-accent)' };
  return { Icon: Mic, color: 'var(--wavis-text-secondary)' };
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
        <icon.Icon size={14} strokeWidth={1.8} color={icon.color} aria-hidden="true" />
        {nameVisual.showConnecting && (
          <span className="inline-flex h-4 items-center gap-1 text-[0.625rem] leading-none text-wavis-text-secondary opacity-80">
            <LoadingBars size="sm" />
            <span>connecting</span>
          </span>
        )}
        <div className="ml-auto flex items-center gap-1">
          {p.cameraOn && (
            <Camera
              size={14}
              strokeWidth={1.8}
              color="var(--wavis-accent)"
              aria-label={isSelf ? 'your camera is on' : `${p.displayName}'s camera is on`}
            >
              <title>{isSelf ? 'your camera is on' : `${p.displayName}'s camera is on`}</title>
            </Camera>
          )}
          {isSelf && p.isSharing && (
            <>
              {(hasActiveVideoShare || !hasActiveAudioShare) && (
                <ScreenShare
                  size={14}
                  strokeWidth={1.8}
                  style={{
                    color: 'var(--wavis-danger)',
                    animation: 'watchPulse 2s ease-in-out infinite',
                  }}
                >
                  <title>you are sharing</title>
                </ScreenShare>
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
          {!isSelf && p.isSharing && (
            <>
              {p.isAudioOnlySharer && (
                <button
                  className="flex items-center justify-center hover:opacity-70 transition-opacity"
                  onClick={(e) => {
                    e.stopPropagation();
                    onSyncShareMuted(!shareMuted);
                  }}
                  title={shareMuted ? 'unmute audio share' : 'mute audio share'}
                >
                  <Music
                    size={14}
                    strokeWidth={1.8}
                    color="var(--wavis-danger)"
                    fill={shareMuted ? 'var(--wavis-danger)' : 'none'}
                    style={{
                      animation: shareMuted ? 'watchPulse 2s ease-in-out infinite' : undefined,
                    }}
                    aria-hidden="true"
                  />
                </button>
              )}
              {p.hasVideoShare && (
                <button
                  onClick={(e) => {
                    e.stopPropagation();
                    if (!p.hasScreenShareStream) return;
                    onToggleWatchShare();
                  }}
                  className="flex items-center justify-center hover:opacity-70 transition-opacity"
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
                  {isWatchingShare ? (
                    <ScreenShareOff size={14} strokeWidth={1.8} aria-hidden="true" />
                  ) : (
                    <ScreenShare size={14} strokeWidth={1.8} aria-hidden="true" />
                  )}
                </button>
              )}
            </>
          )}
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
