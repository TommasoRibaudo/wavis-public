import { useState, useEffect, useRef, useCallback } from 'react';
import { getCurrentWindow } from '@tauri-apps/api/window';
import { invoke } from '@tauri-apps/api/core';
import { getStoreValue, setStoreValue, STORE_KEYS } from '@features/settings/settings-store';
import BugReportFlow from './BugReportFlow';

/* ─── Constants ─────────────────────────────────────────────────── */

const CHROMELESS_LABELS = ['share-picker', 'share-indicator'];
const BUTTON_WIDTH = 40;
const BUTTON_HEIGHT = 40;
const EXPANDED_WIDTH = 140;
const DRAG_THRESHOLD = 4;
export const TITLE_BAR_HEIGHT = 32; // h-8, keeps button out of the OS drag region

/* ─── Snap Logic (exported for testing) ─────────────────────────── */

export function snapToEdge(
  x: number,
  y: number,
  buttonWidth: number,
  buttonHeight: number,
  windowWidth: number,
  windowHeight: number,
  minY = 0,
): { x: number; y: number } {
  const midX = x + buttonWidth / 2;
  const midY = y + buttonHeight / 2;
  const centerX = windowWidth / 2;
  const centerY = windowHeight / 2;

  // Determine nearest edge
  const distLeft = midX;
  const distRight = windowWidth - midX;
  const distTop = midY;
  const distBottom = windowHeight - midY;
  const minDist = Math.min(distLeft, distRight, distTop, distBottom);

  let snappedX = x;
  let snappedY = y;

  if (minDist === distLeft) {
    snappedX = 0;
  } else if (minDist === distRight) {
    snappedX = windowWidth - buttonWidth;
  } else if (minDist === distTop) {
    snappedY = minY;
  } else {
    snappedY = windowHeight - buttonHeight;
  }

  // Also snap to nearest corner on the perpendicular axis
  if (minDist === distLeft || minDist === distRight) {
    snappedY = midY < centerY ? minY : windowHeight - buttonHeight;
  } else {
    snappedX = midX < centerX ? 0 : windowWidth - buttonWidth;
  }

  // Clamp within bounds
  snappedX = Math.max(0, Math.min(snappedX, windowWidth - buttonWidth));
  snappedY = Math.max(minY, Math.min(snappedY, windowHeight - buttonHeight));

  return { x: snappedX, y: snappedY };
}

export function shouldExpandLeft(
  x: number,
  windowWidth: number,
  expandedWidth = EXPANDED_WIDTH,
): boolean {
  return x + expandedWidth > windowWidth;
}

export function getButtonTransitionClass(isDragging: boolean): string {
  return isDragging
    ? 'transition-none'
    : 'transition-opacity duration-200';
}

export function getHoverLabelPositionClass(expandLeft: boolean): string {
  return expandLeft
    ? 'right-full mr-1'
    : 'left-full ml-1';
}

/* ═══ Component ═════════════════════════════════════════════════════ */

export default function BugReportButton() {
  const [position, setPosition] = useState<{ x: number; y: number }>({ x: -1, y: -1 });
  const [isDragging, setIsDragging] = useState(false);
  const [isHovered, setIsHovered] = useState(false);
  const [showFlow, setShowFlow] = useState(false);
  const [isCapturing, setIsCapturing] = useState(false);
  const [preScreenshot, setPreScreenshot] = useState<Uint8Array | null>(null);
  const [isChromeless, setIsChromeless] = useState(false);
  const [loaded, setLoaded] = useState(false);

  const dragRef = useRef<{
    startX: number;
    startY: number;
    offsetX: number;
    offsetY: number;
    moved: boolean;
  } | null>(null);

  // Check if this is a chromeless window at mount
  useEffect(() => {
    const win = getCurrentWindow();
    if (CHROMELESS_LABELS.some((l) => win.label.startsWith(l))) {
      setIsChromeless(true);
      return;
    }

    // Load persisted position
    getStoreValue<{ x: number; y: number }>(
      STORE_KEYS.bugReportButtonPos,
      { x: -1, y: -1 },
    ).then((pos) => {
      if (pos.x === -1 && pos.y === -1) {
        // Default to bottom-right
        setPosition({
          x: window.innerWidth - BUTTON_WIDTH - 16,
          y: window.innerHeight - BUTTON_HEIGHT - 16,
        });
      } else {
        // Clamp both axes to keep the button visible within the current viewport
        const clampedX = Math.max(0, Math.min(pos.x, window.innerWidth - BUTTON_WIDTH));
        const clampedY = Math.max(TITLE_BAR_HEIGHT, Math.min(pos.y, window.innerHeight - BUTTON_HEIGHT));
        setPosition({ x: clampedX, y: clampedY });
      }
      setLoaded(true);
    });
  }, []);

  // Re-clamp position when the window is resized
  useEffect(() => {
    if (!loaded || isChromeless) return;
    const onResize = () => {
      setPosition((prev) => ({
        x: Math.max(0, Math.min(prev.x, window.innerWidth - BUTTON_WIDTH)),
        y: Math.max(TITLE_BAR_HEIGHT, Math.min(prev.y, window.innerHeight - BUTTON_HEIGHT)),
      }));
    };
    window.addEventListener('resize', onResize);
    return () => window.removeEventListener('resize', onResize);
  }, [loaded, isChromeless]);

  const handlePointerDown = useCallback((e: React.PointerEvent) => {
    e.currentTarget.setPointerCapture(e.pointerId);
    dragRef.current = {
      startX: e.clientX,
      startY: e.clientY,
      offsetX: e.clientX - position.x,
      offsetY: e.clientY - position.y,
      moved: false,
    };
  }, [position]);

  const handlePointerMove = useCallback((e: React.PointerEvent) => {
    if (!dragRef.current) return;

    const dx = e.clientX - dragRef.current.startX;
    const dy = e.clientY - dragRef.current.startY;

    if (!dragRef.current.moved && Math.abs(dx) + Math.abs(dy) < DRAG_THRESHOLD) {
      return;
    }

    dragRef.current.moved = true;
    if (!isDragging) setIsDragging(true);

    setPosition({
      x: e.clientX - dragRef.current.offsetX,
      y: Math.max(TITLE_BAR_HEIGHT, e.clientY - dragRef.current.offsetY),
    });
  }, [isDragging]);

  const handlePointerUp = useCallback(() => {
    const wasDragging = dragRef.current?.moved ?? false;
    dragRef.current = null;

    if (wasDragging) {
      // Snap to edge and persist
      const snapped = snapToEdge(
        position.x,
        position.y,
        BUTTON_WIDTH,
        BUTTON_HEIGHT,
        window.innerWidth,
        window.innerHeight,
        TITLE_BAR_HEIGHT,
      );
      setPosition(snapped);
      setStoreValue(STORE_KEYS.bugReportButtonPos, snapped);
      // Delay clearing isDragging so the click handler can check it
      requestAnimationFrame(() => setIsDragging(false));
    } else {
      setIsDragging(false);
    }
  }, [position]);

  const handleClick = useCallback(async () => {
    if (isDragging || isCapturing) return;

    // If the flow is already open, dismiss it so the user can re-click to
    // start a fresh report with a new screenshot.
    if (showFlow) {
      setShowFlow(false);
      setPreScreenshot(null);
      return;
    }

    // Capture the screenshot NOW, before the panel renders, so the panel
    // itself doesn't appear in the screenshot.
    setIsCapturing(true);
    let screenshot: Uint8Array | null = null;
    try {
      const bytes = await invoke<number[]>('capture_window_screenshot');
      screenshot = new Uint8Array(bytes);
    } catch {
      // Non-fatal — flow continues without a screenshot.
    }
    setPreScreenshot(screenshot);
    setIsCapturing(false);
    setShowFlow(true);
  }, [isDragging, isCapturing, showFlow]);

  // Don't render in chromeless windows
  if (isChromeless) return null;
  // Don't render until position is loaded
  if (!loaded) return null;

  const expandLeft = shouldExpandLeft(position.x, window.innerWidth);

  return (
    <>
      <button
        aria-label="Report Bug"
        className={`fixed z-40 flex items-center justify-center font-mono text-xs
          bg-wavis-accent text-wavis-bg rounded overflow-visible
          ${getButtonTransitionClass(isDragging)} cursor-grab select-none
          ${isHovered ? 'opacity-90' : 'opacity-60'}
          ${isDragging ? 'cursor-grabbing' : ''}`}
        style={{
          left: position.x,
          top: position.y,
          width: BUTTON_WIDTH,
          height: BUTTON_HEIGHT,
          appRegion: 'no-drag',
        } as React.CSSProperties}
        onPointerDown={handlePointerDown}
        onPointerMove={handlePointerMove}
        onPointerUp={handlePointerUp}
        onMouseEnter={() => setIsHovered(true)}
        onMouseLeave={() => setIsHovered(false)}
        onClick={handleClick}
      >
        <span>[!]</span>
        {isHovered && (
          <span
            className={`pointer-events-none absolute top-1/2 -translate-y-1/2 whitespace-nowrap
              rounded bg-wavis-accent px-2 py-1.5 text-wavis-bg
              ${getHoverLabelPositionClass(expandLeft)}`}
          >
            {isCapturing ? 'Capturing...' : 'Report Bug'}
          </span>
        )}
      </button>

      {showFlow && (
        <BugReportFlow
          preScreenshot={preScreenshot}
          onClose={() => { setShowFlow(false); setPreScreenshot(null); }}
        />
      )}
    </>
  );
}
