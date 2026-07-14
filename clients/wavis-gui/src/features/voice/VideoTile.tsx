import { Component, useEffect, useRef } from 'react';
import type { ReactNode, ErrorInfo } from 'react';
import type { VideoTileViewModel } from './camera-types';

/* ─── Error Boundary ─────────────────────────────────────────────── */

interface ErrorBoundaryState {
  hasError: boolean;
}

class TileErrorBoundary extends Component<{ children: ReactNode }, ErrorBoundaryState> {
  constructor(props: { children: ReactNode }) {
    super(props);
    this.state = { hasError: false };
  }

  static getDerivedStateFromError(): ErrorBoundaryState {
    return { hasError: true };
  }

  componentDidCatch(_error: unknown, _info: ErrorInfo): void {
    console.error('[wavis:video-tile] render error caught by boundary');
  }

  render(): ReactNode {
    if (this.state.hasError) {
      return (
        <div
          className="w-full h-full flex items-center justify-center text-xs border"
          style={{ color: 'var(--wavis-danger)', borderColor: 'var(--wavis-danger)' }}
        >
          video error
        </div>
      );
    }
    return this.props.children;
  }
}

/* ─── VideoTile ──────────────────────────────────────────────────── */

interface VideoTileProps {
  tile: VideoTileViewModel;
}

function VideoTileInner({ tile }: VideoTileProps) {
  const videoRef = useRef<HTMLVideoElement>(null);

  useEffect(() => {
    const el = videoRef.current;
    if (!el) return;
    if (tile.track) {
      const stream = new MediaStream([tile.track]);
      el.srcObject = stream;
      return () => {
        el.srcObject = null;
      };
    } else {
      el.srcObject = null;
    }
  }, [tile.track]);

  return (
    <div className="relative w-full h-full overflow-hidden bg-wavis-panel">
      {tile.track && !tile.isMuted ? (
        <video
          ref={videoRef}
          autoPlay
          playsInline
          muted={tile.isSelf}
          className="w-full h-full object-cover"
          style={tile.isSelf ? { transform: 'scaleX(-1)' } : undefined}
        />
      ) : (
        <div className="w-full h-full flex items-center justify-center">
          <span className="text-2xl" style={{ color: tile.color }}>
            {tile.isMuted ? '⊘' : '◻'}
          </span>
        </div>
      )}
      {/* Name overlay */}
      <div
        className="absolute bottom-0 left-0 right-0 px-1 py-0.5 text-xs truncate"
        style={{
          background: 'rgba(0,0,0,0.55)',
          color: tile.color,
        }}
      >
        {tile.displayName || tile.participantId}
      </div>
      {/* Muted indicator */}
      {tile.isMuted && (
        <div
          className="absolute top-1 right-1 text-[10px] px-1 rounded"
          style={{ background: 'rgba(0,0,0,0.65)', color: 'var(--wavis-danger)' }}
        >
          muted
        </div>
      )}
    </div>
  );
}

export function VideoTile(props: VideoTileProps) {
  return (
    <TileErrorBoundary>
      <VideoTileInner {...props} />
    </TileErrorBoundary>
  );
}
