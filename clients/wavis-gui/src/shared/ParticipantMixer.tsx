import { useEffect, useRef } from 'react';
import { VolumeSlider } from '@shared/VolumeSlider';

export interface MixerParticipant {
  id: string;
  name: string;
  color: string;
  volume: number;
  muted: boolean;
}

interface ParticipantMixerProps {
  participants: MixerParticipant[];
  onVolumeChange: (id: string, vol: number) => void;
  onToggleMute: (id: string) => void;
  onClose: () => void;
  title?: string;
  emptyMessage?: string;
  positionClassName?: string;
}

const MUTED_ICON = '\u25cb';
const UNMUTED_ICON = '\u25cf';

export default function ParticipantMixer({
  participants,
  onVolumeChange,
  onToggleMute,
  onClose,
  title,
  emptyMessage = 'no active shares',
  positionClassName = 'absolute bottom-full right-0 mb-1',
}: ParticipantMixerProps) {
  const panelRef = useRef<HTMLDivElement>(null);

  useEffect(() => {
    const handleMouseDown = (event: MouseEvent) => {
      const target = event.target;
      if (!(target instanceof Node)) return;
      if (panelRef.current?.contains(target)) return;
      onClose();
    };

    document.addEventListener('mousedown', handleMouseDown);
    return () => document.removeEventListener('mousedown', handleMouseDown);
  }, [onClose]);

  return (
    <div
      ref={panelRef}
      className={`${positionClassName} bg-wavis-panel border border-wavis-text-secondary/30 rounded text-xs p-2 w-[220px] z-10 shadow-lg`}
      onMouseDown={(e) => e.stopPropagation()}
      onClick={(e) => e.stopPropagation()}
    >
      {title && (
        <div className="mb-2 text-wavis-text-secondary uppercase tracking-normal">
          {title}
        </div>
      )}
      {participants.length === 0 ? (
        <div className="text-wavis-text-secondary whitespace-nowrap">{emptyMessage}</div>
      ) : (
        <div className="space-y-2">
          {participants.map((participant) => (
            <div key={participant.id} className="flex items-center gap-2">
              <span
                className="w-2 h-2 rounded-full shrink-0"
                style={{ backgroundColor: participant.color }}
                aria-hidden="true"
              />
              <span
                className="truncate w-20"
                style={{ color: participant.color }}
                title={participant.name}
              >
                {participant.name}
              </span>
              <div className="w-20 shrink-0">
                <VolumeSlider
                  value={participant.volume}
                  onChange={(volume) => onVolumeChange(participant.id, volume)}
                  color={participant.color}
                />
              </div>
              <button
                className="shrink-0 hover:opacity-70 transition-opacity"
                style={{
                  color: participant.muted ? 'var(--wavis-text-secondary)' : participant.color,
                }}
                onClick={() => onToggleMute(participant.id)}
                aria-label={participant.muted ? `Unmute ${participant.name}` : `Mute ${participant.name}`}
                title={participant.muted ? 'unmute' : 'mute'}
              >
                {participant.muted ? MUTED_ICON : UNMUTED_ICON}
              </button>
            </div>
          ))}
        </div>
      )}
    </div>
  );
}
