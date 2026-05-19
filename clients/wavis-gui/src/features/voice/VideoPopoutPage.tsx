import { useEffect, useMemo, useRef, useState } from 'react';
import { emit, listen } from '@tauri-apps/api/event';
import { getCurrentWindow } from '@tauri-apps/api/window';
import { computeGridLayout } from '@features/screen-share/watch-all-grid';
import { StreamReceiver } from '@features/screen-share/screen-share-viewer';
import type { VideoTileSnapshot } from './camera-types';

interface VideoPopoutParams {
  channelName: string;
}

const TITLE_BAR_HEIGHT = 32;

function parseHashParams(): VideoPopoutParams | null {
  try {
    const hash = window.location.hash.slice(1);
    if (!hash) return null;
    return JSON.parse(decodeURIComponent(hash));
  } catch {
    return null;
  }
}

function VideoPopoutTile({ tile }: { tile: VideoTileSnapshot }) {
  const videoRef = useRef<HTMLVideoElement>(null);
  const receiverRef = useRef<StreamReceiver | null>(null);
  const [stream, setStream] = useState<MediaStream | null>(null);
  const [receiverError, setReceiverError] = useState(false);
  const shouldReceive = tile.hasTrack && !tile.isMuted && !tile.hasError;

  useEffect(() => {
    let cancelled = false;

    if (!shouldReceive) {
      receiverRef.current?.stop();
      receiverRef.current = null;
      setStream(null);
      setReceiverError(false);
      return;
    }

    receiverRef.current?.stop();
    const receiver = new StreamReceiver(tile.participantId, 'camera-popout');
    receiverRef.current = receiver;
    setReceiverError(false);

    receiver.start()
      .then((nextStream) => {
        if (cancelled) return;
        setStream(nextStream);
      })
      .catch(() => {
        if (cancelled) return;
        setStream(null);
        setReceiverError(true);
      });

    return () => {
      cancelled = true;
      receiver.stop();
      if (receiverRef.current === receiver) {
        receiverRef.current = null;
      }
    };
  }, [shouldReceive, tile.participantId]);

  useEffect(() => {
    const el = videoRef.current;
    if (!el) return;
    if (!stream) {
      el.srcObject = null;
      return;
    }
    el.srcObject = stream;
    el.play().catch(() => { });
    return () => {
      el.srcObject = null;
    };
  }, [stream]);

  const showVideo = shouldReceive && stream && !receiverError;

  return (
    <div className="relative overflow-hidden bg-wavis-panel" style={{ width: '100%', height: '100%' }}>
      {showVideo ? (
        <video
          ref={videoRef}
          autoPlay
          playsInline
          muted={tile.isSelf}
          style={{
            width: '100%',
            height: '100%',
            objectFit: 'contain',
            ...(tile.isSelf ? { transform: 'scaleX(-1)' } : {}),
          }}
        />
      ) : (
        <div className="w-full h-full flex items-center justify-center">
          <span className="text-2xl" style={{ color: tile.color }}>
            {tile.isMuted ? '⊘' : '◻'}
          </span>
        </div>
      )}
      <div
        className="absolute bottom-0 left-0 right-0 px-1 py-0.5 text-xs truncate"
        style={{
          background: 'rgba(0,0,0,0.55)',
          color: tile.color,
        }}
      >
        {tile.displayName || tile.participantId}
      </div>
      {tile.isMuted && (
        <div
          className="absolute top-1 right-1 text-[10px] px-1 rounded"
          style={{ background: 'rgba(0,0,0,0.65)', color: 'var(--wavis-danger)' }}
        >
          muted
        </div>
      )}
      {(tile.hasError || receiverError) && (
        <div
          className="absolute top-1 left-1 text-[10px] px-1 rounded"
          style={{ background: 'rgba(0,0,0,0.65)', color: 'var(--wavis-danger)' }}
        >
          video_error
        </div>
      )}
    </div>
  );
}

export default function VideoPopoutPage() {
  const paramsRef = useRef(parseHashParams());
  const gridRef = useRef<HTMLDivElement>(null);
  const [gridSize, setGridSize] = useState({ width: 0, height: 0 });
  const [tilesById, setTilesById] = useState<Record<string, VideoTileSnapshot>>({});

  useEffect(() => {
    const el = gridRef.current;
    if (!el) return;
    const ro = new ResizeObserver((entries) => {
      const entry = entries[0];
      if (!entry) return;
      setGridSize({ width: entry.contentRect.width, height: entry.contentRect.height });
    });
    ro.observe(el);
    return () => ro.disconnect();
  }, []);

  useEffect(() => {
    const cleanups = [
      listen<VideoTileSnapshot>('camera-popout:tile-added', (event) => {
        setTilesById((prev) => ({ ...prev, [event.payload.participantId]: event.payload }));
      }),
      listen<VideoTileSnapshot>('camera-popout:tile-updated', (event) => {
        setTilesById((prev) => ({ ...prev, [event.payload.participantId]: event.payload }));
      }),
      listen<{ participantId: string }>('camera-popout:tile-removed', (event) => {
        setTilesById((prev) => {
          const next = { ...prev };
          delete next[event.payload.participantId];
          return next;
        });
      }),
      listen('camera-popout:close', () => {
        getCurrentWindow().close().catch(() => { });
      }),
      listen('voice-session:ended', () => {
        getCurrentWindow().close().catch(() => { });
      }),
    ];

    void emit('camera-popout:ready', {});

    return () => {
      for (const cleanup of cleanups) {
        cleanup.then((fn) => fn());
      }
    };
  }, []);

  useEffect(() => {
    const win = getCurrentWindow();
    const unlisten = win.onCloseRequested(async () => {
      await emit('camera-popout:closed', {});
    });
    return () => { unlisten.then((fn) => fn()); };
  }, []);

  const tiles = useMemo(() => Object.values(tilesById), [tilesById]);
  const layout = tiles.length > 0 && gridSize.width > 0 && gridSize.height > 0
    ? computeGridLayout(tiles.length, gridSize.width, gridSize.height)
    : null;
  const handleClose = () => {
    getCurrentWindow().close();
  };

  return (
    <div className="h-screen flex flex-col bg-wavis-overlay-base font-mono text-wavis-text overflow-hidden select-none">
      <div
        data-tauri-drag-region
        className="flex items-center justify-between px-2 border-b border-wavis-text-secondary bg-wavis-panel text-xs shrink-0"
        style={{ height: TITLE_BAR_HEIGHT }}
      >
        <div className="flex items-center gap-2 min-w-0">
          <span style={{ color: 'var(--wavis-purple)' }}>▲</span>
          <span className="truncate text-wavis-text">
            Camera — {paramsRef.current?.channelName ?? 'wavis'}
          </span>
        </div>
        <button
          onClick={handleClose}
          className="inline-flex items-center justify-center w-8 h-8 hover:bg-wavis-danger hover:text-wavis-text-contrast text-wavis-danger shrink-0 transition-colors"
          aria-label="Close camera window"
        >
          [x]
        </button>
      </div>
      <div ref={gridRef} className="flex-1 overflow-hidden relative">
        {tiles.length === 0 ? (
          <div className="h-full flex items-center justify-center text-wavis-text-secondary text-sm">
            {paramsRef.current?.channelName ? `${paramsRef.current.channelName}: no camera active` : 'No camera active'}
          </div>
        ) : layout ? (
          <div
            className="w-full h-full"
            style={{
              display: 'grid',
              gridTemplateColumns: `repeat(${layout.columns}, 1fr)`,
              gridTemplateRows: `repeat(${layout.rows}, 1fr)`,
            }}
          >
            {tiles.map((tile) => (
              <VideoPopoutTile key={tile.participantId} tile={tile} />
            ))}
          </div>
        ) : null}
      </div>
    </div>
  );
}
