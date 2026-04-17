import { useState, type ReactNode } from 'react';
import { Volume2 } from 'lucide-react';
import { VolumeSlider } from '@shared/VolumeSlider';
import QuickActionButtons, { SEPARATOR } from '@shared/QuickActionButtons';
import ParticipantMixer, { type MixerParticipant } from '@shared/ParticipantMixer';

interface StreamHoverBarProps {
  visible: boolean;
  isMuted: boolean;
  isDeafened: boolean;
  onToggleMute: () => void;
  onToggleDeafen: () => void;
  streamVolume: number;
  streamMuted: boolean;
  streamVolumeColor: string;
  onStreamVolumeChange: (v: number) => void;
  onStreamMuteToggle: () => void;
  voiceParticipants: MixerParticipant[];
  onVoiceVolumeChange: (id: string, vol: number) => void;
  onVoiceMuteToggle: (id: string) => void;
  ownerControls?: ReactNode;
}

const STREAM_MUTED_ICON = '\u25cb';
const STREAM_UNMUTED_ICON = '\u25cf';

export default function StreamHoverBar({
  visible,
  isMuted,
  isDeafened,
  onToggleMute,
  onToggleDeafen,
  streamVolume,
  streamMuted,
  streamVolumeColor,
  onStreamVolumeChange,
  onStreamMuteToggle,
  voiceParticipants,
  onVoiceVolumeChange,
  onVoiceMuteToggle,
  ownerControls,
}: StreamHoverBarProps) {
  const [voiceMixerOpen, setVoiceMixerOpen] = useState(false);

  return (
    <div
      data-no-drag
      className="absolute bottom-0 left-0 right-0 flex items-center px-3 py-1.5 gap-2 text-xs bg-wavis-panel/90 transition-opacity duration-300"
      style={{
        opacity: visible ? 1 : 0,
        pointerEvents: visible ? 'auto' : 'none',
      }}
    >
      <QuickActionButtons
        isMuted={isMuted}
        isDeafened={isDeafened}
        onToggleMute={onToggleMute}
        onToggleDeafen={onToggleDeafen}
      />

      <span className="text-wavis-text-secondary opacity-30 select-none leading-none">{SEPARATOR}</span>

      <div className="flex items-center gap-2 flex-1 min-w-0">
        {ownerControls ?? (
          <>
          <span className="text-wavis-text-secondary shrink-0">vol</span>
          <div
            className="w-24"
            onPointerDown={(e) => e.stopPropagation()}
            onClick={(e) => e.stopPropagation()}
          >
            <VolumeSlider
              value={streamVolume}
              onChange={onStreamVolumeChange}
              color={streamVolumeColor}
            />
          </div>
          <button
            className="shrink-0 hover:opacity-70 transition-opacity"
            style={{ color: streamMuted ? 'var(--wavis-text-secondary)' : streamVolumeColor }}
            onClick={(e) => {
              e.stopPropagation();
              onStreamMuteToggle();
            }}
            aria-label={streamMuted ? 'Unmute stream audio' : 'Mute stream audio'}
            title={streamMuted ? 'unmute stream' : 'mute stream'}
          >
            {streamMuted ? STREAM_MUTED_ICON : STREAM_UNMUTED_ICON}
          </button>
          </>
        )}
      </div>

      <div className="relative shrink-0">
        <button
          onMouseDown={(e) => e.stopPropagation()}
          onClick={(e) => {
            e.stopPropagation();
            setVoiceMixerOpen((open) => !open);
          }}
          className="h-5 w-5 flex items-center justify-center text-wavis-text-secondary hover:opacity-70 transition-opacity"
          aria-label="Open voice volume"
          title="voice volume"
        >
          <Volume2 size={14} strokeWidth={1.8} aria-hidden="true" />
        </button>
        {voiceMixerOpen && (
          <ParticipantMixer
            participants={voiceParticipants}
            onVolumeChange={onVoiceVolumeChange}
            onToggleMute={onVoiceMuteToggle}
            onClose={() => setVoiceMixerOpen(false)}
            title="voice volume"
            emptyMessage="no voice participants"
          />
        )}
      </div>
    </div>
  );
}
