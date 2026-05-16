import { useRef, useState, useEffect } from 'react';
import type { VideoTileViewModel } from './camera-types';
import { VideoTile } from './VideoTile';
import { computeGridLayout } from '@features/screen-share/watch-all-grid';

interface VideoTabProps {
  videoTilesById: Record<string, VideoTileViewModel>;
}

export function VideoTab({ videoTilesById }: VideoTabProps) {
  const gridRef = useRef<HTMLDivElement>(null);
  const [gridSize, setGridSize] = useState({ width: 0, height: 0 });

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

  const tiles = Object.values(videoTilesById);

  const layout =
    tiles.length > 0 && gridSize.width > 0 && gridSize.height > 0
      ? computeGridLayout(tiles.length, gridSize.width, gridSize.height)
      : null;

  return (
    <div ref={gridRef} className="flex-1 overflow-hidden relative">
      {tiles.length === 0 ? (
        <div className="h-full flex items-center justify-center text-wavis-text-secondary text-sm">
          No video active
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
            <VideoTile key={tile.participantId} tile={tile} />
          ))}
        </div>
      ) : (
        <div className="flex flex-wrap gap-2 p-2">
          {tiles.map((tile) => (
            <div key={tile.participantId} className="flex-1 min-w-[120px]">
              <VideoTile tile={tile} />
            </div>
          ))}
        </div>
      )}
    </div>
  );
}
