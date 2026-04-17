import { useState, useEffect, useRef, useCallback } from 'react';

/* ─── Types (exported for testing) ──────────────────────────────── */

export interface BrushStroke {
  points: { x: number; y: number }[];
}

/* ─── Props ─────────────────────────────────────────────────────── */

interface ScreenshotRedactorProps {
  screenshotData: Uint8Array;
  onConfirm: (pngBlob: Blob) => void;
  onSkip: () => void;
}

/* ─── Constants ─────────────────────────────────────────────────── */

const BRUSH_SIZE = 20;

/* ─── Pure Functions (exported for testing) ──────────────────────── */

export function applyStrokesToCanvas(
  ctx: CanvasRenderingContext2D,
  strokes: BrushStroke[],
  brushSize: number,
): void {
  ctx.save();
  ctx.strokeStyle = '#000000';
  ctx.fillStyle = '#000000';
  ctx.lineCap = 'round';
  ctx.lineJoin = 'round';
  ctx.lineWidth = brushSize;
  ctx.globalCompositeOperation = 'source-over';

  for (const stroke of strokes) {
    if (!stroke || stroke.points.length === 0) continue;

    if (stroke.points.length === 1) {
      const p = stroke.points[0];
      ctx.beginPath();
      ctx.arc(p.x, p.y, brushSize / 2, 0, Math.PI * 2);
      ctx.fill();
      continue;
    }

    ctx.beginPath();
    ctx.moveTo(stroke.points[0].x, stroke.points[0].y);
    for (let i = 1; i < stroke.points.length; i++) {
      ctx.lineTo(stroke.points[i].x, stroke.points[i].y);
    }
    ctx.stroke();
  }

  ctx.restore();
}

export function undoStroke(strokes: BrushStroke[]): BrushStroke[] {
  if (strokes.length === 0) return strokes;
  return strokes.slice(0, -1);
}

/* ─── Helpers ───────────────────────────────────────────────────── */

function redrawCanvas(
  ctx: CanvasRenderingContext2D,
  image: HTMLImageElement,
  strokes: BrushStroke[],
): void {
  ctx.clearRect(0, 0, ctx.canvas.width, ctx.canvas.height);
  ctx.drawImage(image, 0, 0, ctx.canvas.width, ctx.canvas.height);
  applyStrokesToCanvas(ctx, strokes, BRUSH_SIZE);
}

function getCanvasPoint(
  e: React.PointerEvent<HTMLCanvasElement>,
  canvas: HTMLCanvasElement,
): { x: number; y: number } {
  const rect = canvas.getBoundingClientRect();
  const scaleX = canvas.width / rect.width;
  const scaleY = canvas.height / rect.height;
  return {
    x: (e.clientX - rect.left) * scaleX,
    y: (e.clientY - rect.top) * scaleY,
  };
}

/* ═══ Component ═════════════════════════════════════════════════════ */

export default function ScreenshotRedactor({
  screenshotData,
  onConfirm,
  onSkip,
}: ScreenshotRedactorProps) {
  const canvasRef = useRef<HTMLCanvasElement>(null);
  const [image, setImage] = useState<HTMLImageElement | null>(null);
  const [strokes, setStrokes] = useState<BrushStroke[]>([]);
  const [isDrawing, setIsDrawing] = useState(false);
  const currentStrokeRef = useRef<BrushStroke | null>(null);

  // Load screenshot into an Image element
  useEffect(() => {
    let cancelled = false;

    const ab = new ArrayBuffer(screenshotData.byteLength);
    new Uint8Array(ab).set(screenshotData);
    const blob = new Blob([ab], { type: 'image/png' });
    const url = URL.createObjectURL(blob);

    const img = new Image();
    img.onload = () => {
      // Revoke right after load — browser has decoded and cached the pixel data,
      // so canvas.drawImage(img) will work even after the URL is gone.
      URL.revokeObjectURL(url);
      if (!cancelled) setImage(img);
    };
    img.onerror = () => URL.revokeObjectURL(url);
    img.src = url;

    return () => {
      cancelled = true;
      // Abort the pending load before revoking — prevents ERR_FILE_NOT_FOUND noise
      // in React Strict Mode where cleanup fires before the async load completes.
      img.onload = null;
      img.onerror = null;
      img.src = '';
      URL.revokeObjectURL(url);
    };
  }, [screenshotData]);

  // Draw image + strokes whenever image or strokes change
  useEffect(() => {
    const canvas = canvasRef.current;
    if (!canvas || !image) return;

    canvas.width = image.naturalWidth;
    canvas.height = image.naturalHeight;

    const ctx = canvas.getContext('2d');
    if (!ctx) return;

    redrawCanvas(ctx, image, strokes);
  }, [image, strokes]);

  const handlePointerDown = useCallback(
    (e: React.PointerEvent<HTMLCanvasElement>) => {
      const canvas = canvasRef.current;
      if (!canvas) return;

      e.currentTarget.setPointerCapture(e.pointerId);
      const point = getCanvasPoint(e, canvas);
      currentStrokeRef.current = { points: [point] };
      setIsDrawing(true);

      // Draw the initial dot immediately
      const ctx = canvas.getContext('2d');
      if (ctx && image) {
        redrawCanvas(ctx, image, [...strokes, currentStrokeRef.current]);
      }
    },
    [image, strokes],
  );

  const handlePointerMove = useCallback(
    (e: React.PointerEvent<HTMLCanvasElement>) => {
      if (!isDrawing || !currentStrokeRef.current) return;
      const canvas = canvasRef.current;
      if (!canvas) return;

      const point = getCanvasPoint(e, canvas);
      currentStrokeRef.current.points.push(point);

      const ctx = canvas.getContext('2d');
      if (ctx && image) {
        redrawCanvas(ctx, image, [...strokes, currentStrokeRef.current]);
      }
    },
    [isDrawing, image, strokes],
  );

  const handlePointerUp = useCallback(() => {
    if (!currentStrokeRef.current) return;

    // Capture before clearing — React 18 batches state updates so the functional
    // updater runs after this synchronous block; if we cleared the ref first the
    // updater would close over null and push null into the strokes array.
    const completedStroke = currentStrokeRef.current;
    currentStrokeRef.current = null;
    setIsDrawing(false);
    setStrokes((prev) => [...prev, completedStroke]);
  }, []);

  const handleUndo = useCallback(() => {
    setStrokes((prev) => undoStroke(prev));
  }, []);

  const handleClear = useCallback(() => {
    setStrokes([]);
  }, []);

  const handleConfirm = useCallback(() => {
    const canvas = canvasRef.current;
    if (!canvas) return;

    canvas.toBlob((blob) => {
      if (blob) onConfirm(blob);
    }, 'image/png');
  }, [onConfirm]);

  if (!image) {
    return (
      <div className="flex items-center justify-center p-8 font-mono text-wavis-text-secondary">
        Loading screenshot...
      </div>
    );
  }

  return (
    <div className="flex flex-col gap-3 font-mono">
      <p className="text-wavis-accent text-sm">
        &gt; Paint over sensitive areas to redact them
      </p>

      <div className="relative border border-wavis-text-secondary overflow-auto max-h-[60vh]">
        <canvas
          ref={canvasRef}
          className="cursor-crosshair block max-w-full"
          style={{ touchAction: 'none' }}
          onPointerDown={handlePointerDown}
          onPointerMove={handlePointerMove}
          onPointerUp={handlePointerUp}
        />
      </div>

      <div className="flex items-center gap-3 flex-wrap">
        <button
          className="border border-wavis-accent text-wavis-accent hover:bg-wavis-accent hover:text-wavis-bg transition-colors px-4 py-1 disabled:opacity-40 disabled:cursor-not-allowed"
          onClick={handleUndo}
          disabled={strokes.length === 0}
        >
          Undo
        </button>
        <button
          className="border border-wavis-accent text-wavis-accent hover:bg-wavis-accent hover:text-wavis-bg transition-colors px-4 py-1 disabled:opacity-40 disabled:cursor-not-allowed"
          onClick={handleClear}
          disabled={strokes.length === 0}
        >
          Clear
        </button>

        <div className="flex-1" />

        <button
          className="border border-wavis-text-secondary text-wavis-text-secondary hover:bg-wavis-text-secondary hover:text-wavis-bg transition-colors px-4 py-1"
          onClick={onSkip}
        >
          Skip screenshot
        </button>
        <button
          className="border border-wavis-accent text-wavis-accent hover:bg-wavis-accent hover:text-wavis-bg transition-colors px-4 py-1"
          onClick={handleConfirm}
        >
          Confirm
        </button>
      </div>
    </div>
  );
}
