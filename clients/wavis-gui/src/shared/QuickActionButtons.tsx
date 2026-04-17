/* Shared quick action buttons (mute/deafen) used by StreamHoverBar and WatchAllPage. */

const SELF_MUTE_ICON = '\u25cb';
const DEAFEN_ICON = '\u00a4';
const SEPARATOR = '\u2502';

export { SEPARATOR };

export interface QuickActionState {
  isMuted: boolean;
  isDeafened: boolean;
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
  return (
    <div className="flex items-center leading-none shrink-0">
      <button
        onClick={(e) => {
          e.stopPropagation();
          onToggleMute();
        }}
        className="px-1.5 flex items-center justify-center hover:opacity-70 transition-opacity"
        style={{ color: isMuted ? 'var(--wavis-danger)' : 'var(--wavis-text-secondary)' }}
        title={isMuted ? '/unmute' : '/mute'}
        aria-label={isMuted ? 'Unmute yourself' : 'Mute yourself'}
      >
        {SELF_MUTE_ICON}
      </button>
      <span className="text-wavis-text-secondary opacity-30 select-none leading-none">{SEPARATOR}</span>
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
        <span style={{ display: 'inline-block', transform: 'scale(1.25) translateY(8%)' }}>
          {DEAFEN_ICON}
        </span>
      </button>
    </div>
  );
}
