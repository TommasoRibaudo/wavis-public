/* Shared quick action buttons (mute/deafen) used by StreamHoverBar and WatchAllPage. */
import { Mic, MicOff, Headphones, HeadphoneOff } from 'lucide-react';
import { useHotkeys } from '@shared/useHotkeys';

const SEPARATOR = '\u2502';

export { SEPARATOR };

export interface QuickActionState {
  isMuted: boolean;
  isDeafened: boolean;
}

export function quickActionMuteTitle(isMuted: boolean): '/mute' | '/unmute' {
  return isMuted ? '/unmute' : '/mute';
}

export function quickActionMuteAriaLabel(isMuted: boolean): 'Mute yourself' | 'Unmute yourself' {
  return isMuted ? 'Unmute yourself' : 'Mute yourself';
}

interface QuickActionButtonsProps extends QuickActionState {
  onToggleMute: () => void;
  onToggleDeafen: () => void;
}

export default function QuickActionButtons({
  isMuted,
  isDeafened,
  onToggleMute,
  onToggleDeafen,
}: QuickActionButtonsProps) {
  const hotkeys = useHotkeys();
  return (
    <div className="flex items-center leading-none shrink-0">
      <button
        onClick={(e) => {
          e.stopPropagation();
          onToggleMute();
        }}
        className="px-1.5 flex items-center justify-center hover:opacity-70 transition-opacity"
        style={{ color: isMuted ? 'var(--wavis-danger)' : 'var(--wavis-text-secondary)' }}
        title={`${quickActionMuteTitle(isMuted)} (${hotkeys.mute})`}
        aria-label={quickActionMuteAriaLabel(isMuted)}
      >
        {isMuted ? (
          <MicOff size={14} strokeWidth={1.8} aria-hidden="true" />
        ) : (
          <Mic size={14} strokeWidth={1.8} aria-hidden="true" />
        )}
      </button>
      <span className="text-wavis-text-secondary opacity-30 select-none leading-none">
        {SEPARATOR}
      </span>
      <button
        onClick={(e) => {
          e.stopPropagation();
          onToggleDeafen();
        }}
        className="px-1.5 flex items-center justify-center hover:opacity-70 transition-opacity"
        style={{ color: isDeafened ? 'var(--wavis-danger)' : 'var(--wavis-text-secondary)' }}
        title={isDeafened ? '/undeafen' : '/deafen'}
        aria-label={isDeafened ? 'Undeafen yourself' : 'Deafen yourself'}
      >
        {isDeafened ? (
          <HeadphoneOff size={14} strokeWidth={1.8} aria-hidden="true" />
        ) : (
          <Headphones size={14} strokeWidth={1.8} aria-hidden="true" />
        )}
      </button>
    </div>
  );
}
