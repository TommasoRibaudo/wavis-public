/**
 * Pure grid layout algorithm for the Watch All Streams feature.
 * Computes the column/row arrangement that maximizes visible 16:9 video area.
 */

export interface GridLayout {
  columns: number;
  rows: number;
  tileWidth: number;
  tileHeight: number;
}

const TARGET_VIDEO_ASPECT_RATIO = 16 / 9;
const LAYOUT_SWITCH_THRESHOLD = 1.1;

interface ScoredGridLayout extends GridLayout {
  videoArea: number;
}

function computeContainedVideoArea(tileWidth: number, tileHeight: number): number {
  if (tileWidth <= 0 || tileHeight <= 0) {
    return 0;
  }

  const videoWidth = Math.min(tileWidth, tileHeight * TARGET_VIDEO_ASPECT_RATIO);
  const videoHeight = Math.min(tileHeight, tileWidth / TARGET_VIDEO_ASPECT_RATIO);
  return videoWidth * videoHeight;
}

function scoreLayout(
  shareCount: number,
  containerWidth: number,
  containerHeight: number,
  columns: number,
): ScoredGridLayout {
  const rows = Math.ceil(shareCount / columns);
  const tileWidth = Math.floor(containerWidth / columns);
  const tileHeight = Math.floor(containerHeight / rows);

  return {
    columns,
    rows,
    tileWidth,
    tileHeight,
    videoArea: computeContainedVideoArea(tileWidth, tileHeight),
  };
}

/**
 * Compute the optimal grid layout that maximizes visible 16:9 video area.
 *
 * Evaluates every candidate column count from 1 to shareCount and picks
 * the one whose resulting tile dimensions yield the largest contained video area.
 * When `currentColumns` is provided, the current layout is kept unless another
 * layout is at least 10% better, which prevents resize flicker around thresholds.
 *
 * Pure function - no DOM or side-effect dependency.
 */
export function computeGridLayout(
  shareCount: number,
  containerWidth: number,
  containerHeight: number,
  currentColumns?: number,
): GridLayout {
  if (shareCount <= 0) {
    return { columns: 0, rows: 0, tileWidth: 0, tileHeight: 0 };
  }

  let bestLayout = scoreLayout(shareCount, containerWidth, containerHeight, 1);

  for (let c = 1; c <= shareCount; c++) {
    const candidateLayout = scoreLayout(shareCount, containerWidth, containerHeight, c);

    if (candidateLayout.videoArea > bestLayout.videoArea) {
      bestLayout = candidateLayout;
    }
  }

  const currentLayout = currentColumns && currentColumns >= 1 && currentColumns <= shareCount
    ? scoreLayout(shareCount, containerWidth, containerHeight, currentColumns)
    : null;

  if (
    currentLayout &&
    bestLayout.videoArea <= currentLayout.videoArea * LAYOUT_SWITCH_THRESHOLD
  ) {
    return currentLayout;
  }

  return bestLayout;
}
