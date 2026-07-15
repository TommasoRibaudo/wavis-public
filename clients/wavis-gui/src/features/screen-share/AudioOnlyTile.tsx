import { memo } from 'react';
import { Music } from 'lucide-react';
import { VolumeSlider } from '@shared/VolumeSlider';
import { STREAM_MUTED_ICON, STREAM_UNMUTED_ICON } from './watch-all-constants';

export interface AudioOnlyTileProps {
  participantId: string;
  displayName: string;
  color: string;
  muted: boolean;
  volume: number;
  onToggleMute: (participantId: string) => void;
  onVolumeChange: (participantId: string, volume: number) => void;
}

const AudioOnlyTile = memo(function AudioOnlyTile({
  participantId,
  displayName,
  color,
  muted,
  volume,
  onToggleMute,
  onVolumeChange,
}: AudioOnlyTileProps) {
  return (
    <div className="flex items-center gap-3 px-3 py-2 text-xs font-mono select-none">
      <button
        className="shrink-0 flex items-center justify-center hover:opacity-70 transition-opacity"
        onClick={() => onToggleMute(participantId)}
        aria-label={muted ? `Unmute ${displayName} audio` : `Mute ${displayName} audio`}
        title={muted ? 'click to unmute' : 'click to mute'}
      >
        <Music
          size={14}
          strokeWidth={1.8}
          color="var(--wavis-danger)"
          fill={muted ? 'var(--wavis-danger)' : 'none'}
          style={{ animation: muted ? 'watchPulse 2s ease-in-out infinite' : undefined }}
          aria-hidden="true"
        />
      </button>
      <span className="truncate min-w-0" style={{ color }}>
        {displayName}
      </span>
      <div className="flex items-center gap-2 ml-auto shrink-0">
        <span className="text-wavis-text-secondary whitespace-nowrap hidden sm:block">
          audio vol
        </span>
        <div
          className="w-20"
          onPointerDown={(e) => e.stopPropagation()}
          onClick={(e) => e.stopPropagation()}
        >
          <VolumeSlider
            value={volume}
            onChange={(v) => onVolumeChange(participantId, v)}
            color={color}
          />
        </div>
        <span className="text-wavis-text-secondary w-5 text-right tabular-nums">
          {muted ? 0 : volume}
        </span>
        <button
          className="shrink-0 hover:opacity-70 transition-opacity"
          style={{ color: muted ? 'var(--wavis-text-secondary)' : color }}
          onClick={() => onToggleMute(participantId)}
          aria-label={muted ? `Unmute ${displayName} audio` : `Mute ${displayName} audio`}
          title="Mute stream"
        >
          {muted ? STREAM_MUTED_ICON : STREAM_UNMUTED_ICON}
        </button>
      </div>
    </div>
  );
});

export default AudioOnlyTile;
