/**
 * Layout algorithms for the Watch All Streams feature.
 *
 * computeGridLayout — uniform equal-cell grid (used for camera tiles in VideoPopoutPage).
 * computeWatchAllLayout — aspect-ratio-aware justified row layout (used in WatchAllPage).
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

/* ── Aspect-ratio-aware justified layout ──────────────────────────── */

export interface StreamInfo {
  id: string;
  /** width / height, must be > 0. */
  aspectRatio: number;
}

export interface TileInRow {
  id: string;
  /** CSS flex-grow value — proportional to the stream's aspect ratio. */
  flexGrow: number;
}

export interface WatchAllRow {
  tiles: TileInRow[];
  /** CSS flex-grow value for the row — proportional to its natural height. */
  flexGrow: number;
}

export interface WatchAllLayout {
  rows: WatchAllRow[];
}

function computeRowAspectSum(streams: StreamInfo[], start: number, endInclusive: number): number {
  let sum = 0;
  for (let i = start; i <= endInclusive; i++) {
    sum += streams[i].aspectRatio;
  }
  return sum;
}

function computeWatchAllOccupiedArea(
  rowAspectSums: number[],
  containerWidth: number,
  containerHeight: number,
): number {
  if (rowAspectSums.length === 0 || containerWidth <= 0 || containerHeight <= 0) {
    return 0;
  }

  const naturalTotalHeight = rowAspectSums.reduce(
    (acc, rowAspectSum) => acc + (containerWidth / rowAspectSum),
    0,
  );
  if (naturalTotalHeight <= 0) {
    return 0;
  }

  const scale = Math.min(1, containerHeight / naturalTotalHeight);
  return rowAspectSums.reduce((acc, rowAspectSum) => {
    const rowHeight = (containerWidth / rowAspectSum) * scale;
    return acc + rowHeight * rowHeight * rowAspectSum;
  }, 0);
}

/**
 * Compute an aspect-ratio-aware layout for the Watch All panel.
 *
 * Sorts streams by aspect ratio and tries every ordered row partition
 * (2^(N-1) candidates, N ≤ 5).  The winning partition minimises
 * |T − H| where T = Σ(W / S_j) is the total natural height when every
 * stream fills its tile at its native aspect ratio.  Closer to zero means
 * less letterboxing or pillarboxing across all tiles.
 *
 * The returned flex values let the caller render directly:
 *   - outer flex-column: each row gets `flex: row.flexGrow`
 *   - inner flex-row: each tile gets `flex: tile.flexGrow`
 *
 * Pure function — no DOM or side-effect dependency.
 */
export function computeWatchAllLayout(
  streams: StreamInfo[],
  containerWidth: number,
  containerHeight: number,
): WatchAllLayout {
  if (streams.length === 0) return { rows: [] };

  // Sort ascending by AR so adjacent groups have similar sums, which
  // tends to produce balanced natural row heights.
  const sorted = [...streams].sort((a, b) => a.aspectRatio - b.aspectRatio);
  const n = sorted.length;

  let bestArea = -1;
  let bestMask = 0; // bit i=1 means "split after stream i"

  // 2^(N-1) ordered partitions, N ≤ 5 → at most 16 iterations.
  for (let mask = 0; mask < (1 << (n - 1)); mask++) {
    const rowAspectSums: number[] = [];
    let rowStart = 0;

    for (let i = 0; i < n; i++) {
      const splitAfter = i === n - 1 || ((mask >> i) & 1) === 1;
      if (splitAfter) {
        rowAspectSums.push(computeRowAspectSum(sorted, rowStart, i));
        rowStart = i + 1;
      }
    }

    const area = computeWatchAllOccupiedArea(rowAspectSums, containerWidth, containerHeight);
    if (area > bestArea) {
      bestArea = area;
      bestMask = mask;
    }
  }

  // Build rows from the winning partition.
  const rows: WatchAllRow[] = [];
  let rowStart = 0;

  for (let i = 0; i < n; i++) {
    const splitAfter = i === n - 1 || ((bestMask >> i) & 1) === 1;
    if (splitAfter) {
      const S = computeRowAspectSum(sorted, rowStart, i);

      rows.push({
        tiles: sorted.slice(rowStart, i + 1).map((s) => ({
          id: s.id,
          flexGrow: s.aspectRatio,
        })),
        // Natural row height = W / S.  Used as a proportional flex value so
        // rows fill the container in the right ratio without computing pixels.
        flexGrow: containerWidth / S,
      });

      rowStart = i + 1;
    }
  }

  return { rows };
}
