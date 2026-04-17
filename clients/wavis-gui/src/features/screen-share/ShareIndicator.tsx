import { emit } from '@tauri-apps/api/event';
import { getCurrentWindow } from '@tauri-apps/api/window';
import type { ShareMode } from './share-types';

/* ─── Helpers ───────────────────────────────────────────────────── */

/** Map share mode to display label. Exported for testing. */
export function shareLabel(mode: ShareMode): string {
  switch (mode) {
    case 'screen_audio':
      return '▲ Sharing Screen';
    case 'window':
      return '▲ Sharing Window';
    case 'audio_only':
      return '♪ Sharing Audio';
    default:
      return '▲ Sharing';
  }
}

interface ShareEntry {
  mode: ShareMode;
  sourceName: string;
}

/* ═══ Component ═════════════════════════════════════════════════════ */

export default function ShareIndicator() {
  // Parse params from URL hash — supports both legacy single-share and new multi-share format
  const hash = decodeURIComponent(window.location.hash.slice(1));
  let shares: ShareEntry[] = [];
  try {
    const params = JSON.parse(hash);
    if (Array.isArray(params.shares)) {
      shares = params.shares;
    } else {
      // Legacy single-share format
      shares = [{ mode: params.shareType ?? 'screen_audio', sourceName: params.sourceName ?? '' }];
    }
  } catch {
    shares = [{ mode: 'screen_audio', sourceName: '' }];
  }

  const handleStop = (mode: ShareMode) => {
    const target = mode === 'audio_only' ? 'audio' : 'video';
    emit('share-indicator:stop', { target });
  };

  const handleStopAll = () => {
    emit('share-indicator:stop', { target: 'all' });
  };

  const handleHide = () => {
    getCurrentWindow().hide().catch(() => {});
  };

  return (
    <div
      className="h-full flex flex-col justify-center gap-0.5 px-3 bg-wavis-bg font-mono text-wavis-text text-xs select-none cursor-move"
      onMouseDown={(e) => {
        if (!(e.target as HTMLElement).closest('button')) {
          getCurrentWindow().startDragging().catch(() => {});
        }
      }}
    >
      {shares.map((s, i) => (
        <div key={i} className="flex items-center gap-2">
          <span className="shrink-0">{shareLabel(s.mode)}</span>
          <span className="truncate text-wavis-text-secondary" title={s.sourceName}>
            {s.sourceName}
          </span>
          <span className="flex-1" />
          {shares.length > 1 && (
            <button
              onClick={() => handleStop(s.mode)}
              className="shrink-0 text-wavis-danger hover:underline focus:outline focus:outline-1 focus:outline-wavis-danger"
              aria-label={`Stop ${s.mode === 'audio_only' ? 'audio' : 'video'} share`}
            >
              [stop]
            </button>
          )}
        </div>
      ))}
      <div className="flex items-center gap-2 mt-0.5">
        <span className="flex-1" />
        <button
          onClick={handleStopAll}
          className="shrink-0 text-wavis-danger hover:underline focus:outline focus:outline-1 focus:outline-wavis-danger"
          aria-label="Stop sharing"
        >
          {shares.length > 1 ? '[stop all]' : '[stop]'}
        </button>
        <button
          onClick={handleHide}
          className="shrink-0 text-wavis-text-secondary hover:underline focus:outline focus:outline-1 focus:outline-wavis-text-secondary"
          aria-label="Hide indicator"
        >
          [hide]
        </button>
      </div>
    </div>
  );
}
