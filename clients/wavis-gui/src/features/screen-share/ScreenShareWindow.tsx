import { useState, useEffect, useRef, useCallback } from 'react';
import { useVideoStallDetector } from './useVideoStallDetector';

/* ─── Types ─────────────────────────────────────────────────────── */

export type ShareQuality = 'low' | 'high' | 'max';

interface ScreenShareWindowProps {
  /** Display name of the sharer */
  username: string;
  /** Color of the sharer (from TERMINAL_COLORS) */
  userColor: string;
  /** Whether the local user is the one sharing */
  isOwner: boolean;
  /** The MediaStream to render */
  stream: MediaStream;
  /** Close the window (stop watching) */
  onClose: () => void;
  /** Volume 0–100 (viewer only) */
  volume: number;
  /** Volume change callback (viewer only) */
  onVolumeChange: (v: number) => void;
  /** Current quality preset (owner only) */
  quality: ShareQuality;
  /** Quality change callback (owner only) */
  onQualityChange: (q: ShareQuality) => void;
  /** Whether screen audio is being shared (owner only) */
  sharingAudio: boolean;
  /** Toggle audio sharing (owner only) */
  onToggleAudio: () => void;
  /** Request to change the shared window/screen (owner only) */
  onChangeSource: () => void;
}

/* ─── Constants ─────────────────────────────────────────────────── */

const MIN_W = 320;
const MIN_H = 200;
const DEFAULT_W = 640;
const DEFAULT_H = 400;
const HEADER_H = 32;
const MIN_ZOOM = 1;
const MAX_ZOOM = 10;
const ZOOM_STEP = 0.15;

/* ─── Helpers ───────────────────────────────────────────────────── */

type ResizeEdge = 'n' | 's' | 'e' | 'w' | 'ne' | 'nw' | 'se' | 'sw' | null;

function isCorner(edge: ResizeEdge): boolean {
  return edge === 'ne' || edge === 'nw' || edge === 'se' || edge === 'sw';
}

function cursorForEdge(edge: ResizeEdge): string {
  switch (edge) {
    case 'n': case 's': return 'ns-resize';
    case 'e': case 'w': return 'ew-resize';
    case 'ne': case 'sw': return 'nesw-resize';
    case 'nw': case 'se': return 'nwse-resize';
    default: return 'default';
  }
}

/* ═══ Component ═════════════════════════════════════════════════════ */

export default function ScreenShareWindow({
  username,
  userColor,
  isOwner,
  stream,
  onClose,
  volume,
  onVolumeChange,
  quality,
  onQualityChange,
  sharingAudio,
  onToggleAudio,
  onChangeSource,
}: ScreenShareWindowProps) {
  const containerRef = useRef<HTMLDivElement>(null);
  const videoRef = useRef<HTMLVideoElement>(null);
  const videoAreaRef = useRef<HTMLDivElement>(null);

  // Position & size
  const [pos, setPos] = useState({ x: 120, y: 80 });
  const [size, setSize] = useState({ w: DEFAULT_W, h: DEFAULT_H });

  // Aspect ratio of the video content (updated on loadedmetadata)
  const [videoAspect, setVideoAspect] = useState(16 / 9);

  // Zoom state: scale + translate offset (px) for panning
  const [zoom, setZoom] = useState(1);
  const [pan, setPan] = useState({ x: 0, y: 0 });
  const panRef = useRef<{ startX: number; startY: number; origPanX: number; origPanY: number } | null>(null);

  // Drag state
  const dragRef = useRef<{ startX: number; startY: number; origX: number; origY: number } | null>(null);

  // Resize state
  const resizeRef = useRef<{
    edge: ResizeEdge;
    startX: number;
    startY: number;
    origW: number;
    origH: number;
    origX: number;
    origY: number;
  } | null>(null);

  // Attach stream to video element
  useEffect(() => {
    const video = videoRef.current;
    if (!video || !stream) return;
    video.srcObject = stream;
    video.play().catch(() => {});

    const onMeta = () => {
      if (video.videoWidth && video.videoHeight) {
        setVideoAspect(video.videoWidth / video.videoHeight);
      }
    };
    video.addEventListener('loadedmetadata', onMeta);
    if (video.videoWidth && video.videoHeight) {
      setVideoAspect(video.videoWidth / video.videoHeight);
    }
    return () => video.removeEventListener('loadedmetadata', onMeta);
  }, [stream]);

  useVideoStallDetector({ videoRef, stream });

  /* ── Drag handlers ── */

  const onDragStart = useCallback((e: React.MouseEvent) => {
    if ((e.target as HTMLElement).closest('[data-no-drag]')) return;
    dragRef.current = { startX: e.clientX, startY: e.clientY, origX: pos.x, origY: pos.y };
    e.preventDefault();
  }, [pos]);

  useEffect(() => {
    const onMove = (e: MouseEvent) => {
      if (dragRef.current) {
        const dx = e.clientX - dragRef.current.startX;
        const dy = e.clientY - dragRef.current.startY;
        setPos({ x: dragRef.current.origX + dx, y: dragRef.current.origY + dy });
      }
    };
    const onUp = () => { dragRef.current = null; };
    window.addEventListener('mousemove', onMove);
    window.addEventListener('mouseup', onUp);
    return () => {
      window.removeEventListener('mousemove', onMove);
      window.removeEventListener('mouseup', onUp);
    };
  }, []);

  /* ── Resize handlers ── */

  const onResizeStart = useCallback((edge: ResizeEdge, e: React.MouseEvent) => {
    resizeRef.current = {
      edge,
      startX: e.clientX,
      startY: e.clientY,
      origW: size.w,
      origH: size.h,
      origX: pos.x,
      origY: pos.y,
    };
    e.preventDefault();
    e.stopPropagation();
  }, [size, pos]);

  useEffect(() => {
    const onMove = (e: MouseEvent) => {
      const r = resizeRef.current;
      if (!r || !r.edge) return;

      const dx = e.clientX - r.startX;
      const dy = e.clientY - r.startY;
      let newW = r.origW;
      let newH = r.origH;
      let newX = r.origX;
      let newY = r.origY;

      // Compute raw size changes based on edge
      if (r.edge.includes('e')) newW = r.origW + dx;
      if (r.edge.includes('w')) { newW = r.origW - dx; newX = r.origX + dx; }
      if (r.edge.includes('s')) newH = r.origH + dy;
      if (r.edge.includes('n')) { newH = r.origH - dy; newY = r.origY + dy; }

      // Corner resize: lock aspect ratio
      if (isCorner(r.edge)) {
        const aspect = videoAspect;
        // Use the larger delta to drive the resize
        const candidateH = newW / aspect;
        const candidateW = newH * aspect;
        if (Math.abs(dx) > Math.abs(dy)) {
          newH = candidateH;
          if (r.edge.includes('n')) newY = r.origY + (r.origH - newH);
        } else {
          newW = candidateW;
          if (r.edge.includes('w')) newX = r.origX + (r.origW - newW);
        }
      }

      // Enforce minimums
      if (newW < MIN_W) { newW = MIN_W; newX = r.edge.includes('w') ? r.origX + r.origW - MIN_W : newX; }
      if (newH < MIN_H) { newH = MIN_H; newY = r.edge.includes('n') ? r.origY + r.origH - MIN_H : newY; }

      setSize({ w: newW, h: newH });
      setPos({ x: newX, y: newY });
    };
    const onUp = () => { resizeRef.current = null; };
    window.addEventListener('mousemove', onMove);
    window.addEventListener('mouseup', onUp);
    return () => {
      window.removeEventListener('mousemove', onMove);
      window.removeEventListener('mouseup', onUp);
    };
  }, [videoAspect]);

  /* ── Zoom (scroll wheel) + Pan (drag when zoomed) ──────────── */

  const resetZoom = useCallback(() => {
    setZoom(1);
    setPan({ x: 0, y: 0 });
  }, []);

  // Clamp pan so the video doesn't drift out of view
  const clampPan = useCallback((px: number, py: number, z: number, w: number, h: number) => {
    if (z <= 1) return { x: 0, y: 0 };
    const maxPanX = (w * (z - 1)) / 2;
    const maxPanY = (h * (z - 1)) / 2;
    return {
      x: Math.max(-maxPanX, Math.min(maxPanX, px)),
      y: Math.max(-maxPanY, Math.min(maxPanY, py)),
    };
  }, []);

  // Scroll-to-zoom toward cursor position
  useEffect(() => {
    const area = videoAreaRef.current;
    if (!area) return;

    const onWheel = (e: WheelEvent) => {
      e.preventDefault();
      const rect = area.getBoundingClientRect();
      // Cursor position relative to the video area center
      const cx = e.clientX - rect.left - rect.width / 2;
      const cy = e.clientY - rect.top - rect.height / 2;

      setZoom((prev) => {
        const direction = e.deltaY < 0 ? 1 : -1;
        const next = Math.max(MIN_ZOOM, Math.min(MAX_ZOOM, prev + direction * ZOOM_STEP * prev));

        // Adjust pan so the point under the cursor stays fixed
        const scaleFactor = next / prev;
        setPan((prevPan) => {
          const newPx = cx - scaleFactor * (cx - prevPan.x);
          const newPy = cy - scaleFactor * (cy - prevPan.y);
          return clampPan(newPx, newPy, next, rect.width, rect.height);
        });

        return next;
      });
    };

    area.addEventListener('wheel', onWheel, { passive: false });
    return () => area.removeEventListener('wheel', onWheel);
  }, [clampPan]);

  // Pan via mouse drag when zoomed in
  useEffect(() => {
    const area = videoAreaRef.current;
    if (!area) return;

    const onDown = (e: MouseEvent) => {
      // Only pan when zoomed in, and only on left-click directly on the video area
      if (zoom <= 1) return;
      if ((e.target as HTMLElement).closest('[data-no-drag]')) return;
      panRef.current = { startX: e.clientX, startY: e.clientY, origPanX: pan.x, origPanY: pan.y };
      e.preventDefault();
    };
    const onMove = (e: MouseEvent) => {
      if (!panRef.current) return;
      const dx = e.clientX - panRef.current.startX;
      const dy = e.clientY - panRef.current.startY;
      const rect = area.getBoundingClientRect();
      setPan(clampPan(panRef.current.origPanX + dx, panRef.current.origPanY + dy, zoom, rect.width, rect.height));
    };
    const onUp = () => { panRef.current = null; };

    area.addEventListener('mousedown', onDown);
    window.addEventListener('mousemove', onMove);
    window.addEventListener('mouseup', onUp);
    return () => {
      area.removeEventListener('mousedown', onDown);
      window.removeEventListener('mousemove', onMove);
      window.removeEventListener('mouseup', onUp);
    };
  }, [zoom, pan, clampPan]);

  // Re-clamp pan when zoom or size changes
  useEffect(() => {
    setPan((prev) => clampPan(prev.x, prev.y, zoom, size.w, size.h));
  }, [zoom, size.w, size.h, clampPan]);

  /* ── Edge hit-test zones (8px grab area) ── */
  const GRAB = 8;
  const edgeStyle = (edge: ResizeEdge): React.CSSProperties => {
    const base: React.CSSProperties = { position: 'absolute', zIndex: 10, cursor: cursorForEdge(edge) };
    switch (edge) {
      case 'n': return { ...base, top: -GRAB / 2, left: GRAB, right: GRAB, height: GRAB };
      case 's': return { ...base, bottom: -GRAB / 2, left: GRAB, right: GRAB, height: GRAB };
      case 'e': return { ...base, right: -GRAB / 2, top: GRAB, bottom: GRAB, width: GRAB };
      case 'w': return { ...base, left: -GRAB / 2, top: GRAB, bottom: GRAB, width: GRAB };
      case 'ne': return { ...base, top: -GRAB / 2, right: -GRAB / 2, width: GRAB * 2, height: GRAB * 2 };
      case 'nw': return { ...base, top: -GRAB / 2, left: -GRAB / 2, width: GRAB * 2, height: GRAB * 2 };
      case 'se': return { ...base, bottom: -GRAB / 2, right: -GRAB / 2, width: GRAB * 2, height: GRAB * 2 };
      case 'sw': return { ...base, bottom: -GRAB / 2, left: -GRAB / 2, width: GRAB * 2, height: GRAB * 2 };
      default: return base;
    }
  };

  const qualityLabel: Record<ShareQuality, string> = {
    low: 'LOW (smooth)',
    high: 'HIGH (sharp)',
    max: 'MAX (all out)',
  };

  return (
    <div
      ref={containerRef}
      style={{
        position: 'fixed',
        left: pos.x,
        top: pos.y,
        width: size.w,
        height: size.h + HEADER_H,
        zIndex: 9999,
      }}
    >
      {/* Resize grab zones */}
      {(['n', 's', 'e', 'w', 'ne', 'nw', 'se', 'sw'] as ResizeEdge[]).map((edge) => (
        <div key={edge!} style={edgeStyle(edge)} onMouseDown={(e) => onResizeStart(edge, e)} />
      ))}

      {/* Header — draggable */}
      <div
        className="flex items-center justify-between px-2 border border-wavis-text-secondary bg-wavis-panel font-mono text-xs select-none"
        style={{ height: HEADER_H, cursor: 'move' }}
        onMouseDown={onDragStart}
      >
        <div className="flex items-center gap-2 min-w-0">
          <span style={{ color: 'var(--wavis-purple)' }}>▲</span>
          <span className="truncate" style={{ color: userColor }}>{username}</span>
          <span className="text-wavis-text-secondary">{isOwner ? '(you)' : 'screen share'}</span>
        </div>
        <button
          data-no-drag
          onClick={onClose}
          className="px-1 hover:opacity-70 text-wavis-danger shrink-0"
          aria-label="Close screen share window"
        >
          [x]
        </button>
      </div>

      {/* Video area — black background, video centered with object-fit contain */}
      <div
        ref={videoAreaRef}
        className="relative border-x border-b border-wavis-text-secondary overflow-hidden"
        style={{
          width: size.w,
          height: size.h,
          backgroundColor: '#000',
          cursor: zoom > 1 ? 'grab' : 'default',
        }}
      >
        <video
          ref={videoRef}
          autoPlay
          playsInline
          muted={!sharingAudio || isOwner}
          style={{
            width: '100%',
            height: '100%',
            objectFit: 'contain',
            transform: `scale(${zoom}) translate(${pan.x / zoom}px, ${pan.y / zoom}px)`,
            transformOrigin: 'center center',
            willChange: zoom > 1 ? 'transform' : undefined,
          }}
        />

        {/* Bottom controls overlay */}
        <div
          data-no-drag
          className="absolute bottom-0 left-0 right-0 flex items-center justify-between px-3 py-1.5 font-mono text-[0.625rem]"
          style={{ backgroundColor: 'rgba(13,17,23,0.85)' }}
        >
          {isOwner ? (
            /* ── Owner controls ── */
            <div className="flex items-center gap-3 flex-wrap">
              <button
                onClick={onChangeSource}
                className="text-wavis-text hover:text-wavis-accent hover:underline"
              >
                /window
              </button>
              <span className="text-wavis-text-secondary">|</span>
              {(['low', 'high', 'max'] as ShareQuality[]).map((q) => (
                <button
                  key={q}
                  onClick={() => onQualityChange(q)}
                  className="hover:underline"
                  style={{ color: quality === q ? 'var(--wavis-accent)' : 'var(--wavis-text-secondary)' }}
                >
                  {qualityLabel[q]}
                </button>
              ))}
              <span className="text-wavis-text-secondary">|</span>
              <button
                onClick={onToggleAudio}
                className="hover:underline"
                style={{ color: sharingAudio ? 'var(--wavis-accent)' : 'var(--wavis-text-secondary)' }}
              >
                {sharingAudio ? '♪ audio on' : '♪ audio off'}
              </button>
              {zoom > 1 && (
                <>
                  <span className="text-wavis-text-secondary">|</span>
                  <span className="text-wavis-text-secondary">{Math.round(zoom * 100)}%</span>
                  <button onClick={resetZoom} className="text-wavis-text hover:text-wavis-accent hover:underline">/reset</button>
                </>
              )}
            </div>
          ) : (
            /* ── Viewer controls ── */
            <div className="flex items-center gap-2 w-full">
              <span className="text-wavis-text-secondary shrink-0">vol</span>
              <input
                type="range"
                min={0}
                max={100}
                value={volume}
                onChange={(e) => onVolumeChange(Number(e.target.value))}
                className="flex-1"
                style={{ accentColor: userColor, height: '2px' }}
              />
              <span className="text-wavis-text-secondary w-6 text-right">{volume}</span>
              {zoom > 1 && (
                <>
                  <span className="text-wavis-text-secondary">|</span>
                  <span className="text-wavis-text-secondary shrink-0">{Math.round(zoom * 100)}%</span>
                  <button onClick={resetZoom} className="text-wavis-text hover:text-wavis-accent hover:underline shrink-0">/reset</button>
                </>
              )}
            </div>
          )}
        </div>
      </div>
    </div>
  );
}
