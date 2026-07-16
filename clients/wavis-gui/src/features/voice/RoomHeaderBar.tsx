import { connectionModeBadgeText } from '@shared/helpers';
import { Tooltip, TooltipContent, TooltipTrigger } from '../../components/ui/tooltip';
import type { MediaState } from './livekit-media';
import {
  reconnectMedia,
  resetMediaReconnectFailures,
  type VoiceRoomMachineState,
} from './voice-room';

export function rttColor(rttMs: number): string {
  if (rttMs < 100) return 'var(--wavis-accent)';
  if (rttMs <= 300) return 'var(--wavis-warn)';
  return 'var(--wavis-danger)';
}

export function signalingIndicator(
  state: VoiceRoomMachineState,
  lastRateLimitError: string | null,
): { color: string; label: string } {
  switch (state) {
    case 'active':
      return { color: 'var(--wavis-accent)', label: 'Signaling: connected' };
    case 'connecting':
    case 'authenticated':
    case 'joining':
      return { color: 'var(--wavis-warn)', label: 'Signaling: connecting...' };
    case 'reconnecting':
      return {
        color: 'var(--wavis-warn)',
        label: lastRateLimitError
          ? 'Signaling: reconnecting after rate limit...'
          : 'Signaling: reconnecting...',
      };
    case 'idle':
    default:
      return { color: 'var(--wavis-text-secondary)', label: 'Signaling: disconnected' };
  }
}

export function mediaIndicator(
  state: MediaState,
  error: string | null,
): { color: string; label: string } {
  switch (state) {
    case 'connected':
      return { color: 'var(--wavis-accent)', label: 'Media: connected' };
    case 'connecting':
      return { color: 'var(--wavis-warn)', label: 'Media: connecting...' };
    case 'reconnecting':
      return { color: 'var(--wavis-warn)', label: 'Media: reconnecting...' };
    case 'failed':
      return { color: 'var(--wavis-danger)', label: `Media: failed${error ? ` — ${error}` : ''}` };
    case 'disconnected':
    default:
      return { color: 'var(--wavis-text-secondary)', label: 'Media: disconnected' };
  }
}

function combinedStatusBadge(
  machine: VoiceRoomMachineState,
  media: MediaState,
): { text: string; color: string } {
  // Failed media takes priority
  if (media === 'failed') return { text: 'FAILED', color: 'var(--wavis-danger)' };
  // Media reconnecting takes priority over live/connected state
  if (media === 'reconnecting') return { text: 'RECONNECTING', color: 'var(--wavis-warn)' };
  // Both fully connected = live
  if (machine === 'active' && media === 'connected')
    return { text: 'LIVE', color: 'var(--wavis-accent)' };
  // Reconnecting signaling
  if (machine === 'reconnecting') return { text: 'RECONNECTING', color: 'var(--wavis-warn)' };
  // Any connecting state
  if (
    machine === 'connecting' ||
    machine === 'authenticated' ||
    machine === 'joining' ||
    media === 'connecting'
  )
    return { text: 'CONNECTING', color: 'var(--wavis-warn)' };
  // Idle / disconnected
  return { text: 'OFFLINE', color: 'var(--wavis-text-secondary)' };
}

export function StatusDot({ color, label }: { color: string; label: string }) {
  const isAnimating = label.includes('connecting') || label.includes('reconnecting');
  return (
    <Tooltip>
      <TooltipTrigger asChild>
        <span
          className="inline-block w-2 h-2 rounded-full cursor-default"
          style={{
            backgroundColor: color,
            boxShadow: color === 'var(--wavis-accent)' ? `0 0 6px ${color}` : undefined,
            animation: isAnimating ? 'pulse 3s ease-in-out infinite' : undefined,
          }}
          aria-label={label}
        />
      </TooltipTrigger>
      <TooltipContent
        side="bottom"
        className="bg-wavis-panel text-wavis-text border border-wavis-text-secondary font-mono text-xs"
      >
        {label}
      </TooltipContent>
    </Tooltip>
  );
}

export interface MediaStatusBannersProps {
  machineState: VoiceRoomMachineState;
  mediaState: MediaState;
  mediaReconnectFailures: number;
  lastRateLimitError: string | null;
}

/** The three status banners — reused verbatim by the mobile, intermediate, and
 * desktop layouts (each has its own header, but shares this banner stack). */
export function MediaStatusBanners({
  machineState,
  mediaState,
  mediaReconnectFailures,
  lastRateLimitError,
}: MediaStatusBannersProps) {
  return (
    <>
      {mediaState === 'reconnecting' && (
        <div className="px-3 py-2 border-b border-wavis-warn bg-wavis-panel text-xs text-wavis-warn">
          Audio reconnecting... still in room
        </div>
      )}
      {mediaState === 'failed' && mediaReconnectFailures > 0 && (
        <div className="px-3 py-2 border-b border-wavis-danger bg-wavis-panel text-xs flex items-center justify-between gap-2">
          <span className="text-wavis-danger">
            media disconnected — automatic retries exhausted
          </span>
          <button
            className="border border-wavis-accent text-wavis-accent hover:bg-wavis-accent hover:text-wavis-bg transition-colors px-1 py-0.5 text-xs text-center shrink-0"
            onClick={() => {
              resetMediaReconnectFailures();
              void reconnectMedia();
            }}
          >
            /retry
          </button>
        </div>
      )}
      {lastRateLimitError &&
        (machineState === 'reconnecting' ||
          machineState === 'authenticated' ||
          machineState === 'joining') && (
          <div className="px-3 py-2 border-b border-wavis-warn bg-wavis-panel text-xs text-wavis-warn">
            {lastRateLimitError}
          </div>
        )}
    </>
  );
}

export interface RoomHeaderBarProps {
  machineState: VoiceRoomMachineState;
  mediaState: MediaState;
  mediaError: string | null;
  lastRateLimitError: string | null;
  connectionMode: 'livekit' | 'native' | undefined;
  showSecrets: boolean;
  channelName: string;
  participantCount: number;
  rttMs: number;
  packetLossPercent: number;
  channelSwitcherOpen: boolean;
  onToggleChannelSwitcher: () => void;
}

export function ChannelSwitcherToggle({
  channelSwitcherOpen,
  onToggleChannelSwitcher,
}: {
  channelSwitcherOpen: boolean;
  onToggleChannelSwitcher: () => void;
}) {
  return (
    <button
      onClick={onToggleChannelSwitcher}
      className={`shrink-0 border px-2 py-1 text-xs transition-colors ${
        channelSwitcherOpen
          ? 'border-wavis-accent text-wavis-accent hover:bg-wavis-accent hover:text-wavis-bg'
          : 'border-wavis-text-secondary text-wavis-text-secondary hover:border-wavis-accent hover:text-wavis-accent'
      }`}
      title="Change channel"
    >
      {channelSwitcherOpen ? '<' : '>'}
    </button>
  );
}

export function RoomHeaderBar({
  machineState,
  mediaState,
  mediaError,
  lastRateLimitError,
  connectionMode,
  showSecrets,
  channelName,
  participantCount,
  rttMs,
  packetLossPercent,
  channelSwitcherOpen,
  onToggleChannelSwitcher,
}: RoomHeaderBarProps) {
  const sigDot = signalingIndicator(machineState, lastRateLimitError);
  const mediaDot = mediaIndicator(mediaState, mediaError);
  const statusBadge = combinedStatusBadge(machineState, mediaState);
  const badge = connectionModeBadgeText(showSecrets, connectionMode);

  return (
    <div className="px-3 py-3 border-b border-wavis-text-secondary h-[4.5rem] flex items-center gap-3 overflow-hidden">
      <div className="flex-1 flex flex-col justify-center gap-0.5 min-w-0">
        <div className="flex items-center gap-2">
          <StatusDot color={sigDot.color} label={sigDot.label} />
          <StatusDot color={mediaDot.color} label={mediaDot.label} />
          {badge && <span className="text-[0.625rem] text-wavis-purple">[{badge}]</span>}
          <span className="text-sm" style={{ color: statusBadge.color }}>
            {statusBadge.text}
          </span>
          <span className="text-[0.625rem] text-wavis-text-secondary">{participantCount}/6</span>
          <span className="text-[0.625rem]" style={{ color: rttColor(rttMs) }}>
            {rttMs}ms
          </span>
          <span className="text-[0.625rem] text-wavis-text-secondary">
            {packetLossPercent.toFixed(1)}% loss
          </span>
        </div>
        <div
          className={`font-bold truncate min-w-0${channelName.length > 20 ? ' text-xs' : ' text-sm'}`}
          title={channelName}
        >
          {channelName}
        </div>
      </div>
      <ChannelSwitcherToggle
        channelSwitcherOpen={channelSwitcherOpen}
        onToggleChannelSwitcher={onToggleChannelSwitcher}
      />
    </div>
  );
}
