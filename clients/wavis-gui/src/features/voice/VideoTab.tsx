import type { VideoTileViewModel } from './camera-types';
import { VideoTile } from './VideoTile';

interface VideoTabProps {
  videoTilesById: Record<string, VideoTileViewModel>;
}

export function VideoTab({ videoTilesById }: VideoTabProps) {
  const tiles = Object.values(videoTilesById);

  if (tiles.length === 0) {
    return (
      <div className="flex-1 flex items-center justify-center text-wavis-text-secondary text-sm">
        No video active
      </div>
    );
  }

  return (
    <div className="flex-1 overflow-y-auto overflow-x-hidden">
      <div className="flex flex-col">
        {tiles.map((tile) => (
          <div
            key={tile.participantId}
            className="w-full shrink-0"
            style={{ aspectRatio: '16 / 9' }}
          >
            <VideoTile tile={tile} />
          </div>
        ))}
      </div>
    </div>
  );
}
