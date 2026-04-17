import { useState, useRef, useEffect } from 'react';

export function VolumeSlider({
  value,
  onChange,
  color = 'var(--wavis-accent)',
  disabled = false,
}: {
  value: number;
  onChange: (v: number) => void;
  color?: string;
  disabled?: boolean;
}) {
  const trackRef = useRef<HTMLDivElement>(null);
  const [dragging, setDragging] = useState(false);
  const [localValue, setLocalValue] = useState(value);

  useEffect(() => {
    if (!dragging) setLocalValue(value);
  }, [value, dragging]);

  const calcFromPointer = (clientX: number): number => {
    const track = trackRef.current;
    if (!track) return localValue;
    const rect = track.getBoundingClientRect();
    return Math.round(Math.max(0, Math.min(1, (clientX - rect.left) / rect.width)) * 100);
  };

  const handlePointerDown = (e: React.PointerEvent<HTMLDivElement>) => {
    e.currentTarget.setPointerCapture(e.pointerId);
    setDragging(true);
    const v = calcFromPointer(e.clientX);
    setLocalValue(v);
    onChange(v);
  };

  const handlePointerMove = (e: React.PointerEvent<HTMLDivElement>) => {
    if (!dragging) return;
    const v = calcFromPointer(e.clientX);
    setLocalValue(v);
    onChange(v);
  };

  const handlePointerUp = (e: React.PointerEvent<HTMLDivElement>) => {
    e.currentTarget.releasePointerCapture(e.pointerId);
    setDragging(false);
  };

  const pct = `${localValue}%`;

  return (
    <div
      ref={trackRef}
      className="relative select-none cursor-pointer"
      style={{ height: '22px', ...(disabled ? { pointerEvents: 'none', opacity: 0.4 } : {}) }}
      onPointerDown={handlePointerDown}
      onPointerMove={handlePointerMove}
      onPointerUp={handlePointerUp}
    >
      {/* Track rail */}
      <div
        className="absolute inset-x-0"
        style={{
          top: '50%',
          height: '1px',
          transform: 'translateY(-50%)',
          backgroundColor: 'var(--wavis-text-secondary)',
          opacity: 0.35,
        }}
      />
      {/* Fill bar — lags behind ball via CSS transition */}
      <div
        className="absolute left-0"
        style={{
          top: '50%',
          height: '2px',
          transform: 'translateY(-50%)',
          width: pct,
          backgroundColor: color,
          transition: 'width 1200ms cubic-bezier(0.22, 1, 0.36, 1)',
          opacity: localValue === 0 ? 0.25 : 0.75,
        }}
      />
      {/* Ball — instant position, only size/glow animate */}
      <div
        className="absolute"
        style={{
          top: '50%',
          left: pct,
          transform: 'translate(-50%, -50%)',
          width: dragging ? '13px' : '10px',
          height: dragging ? '13px' : '10px',
          borderRadius: '50%',
          backgroundColor: color,
          boxShadow: dragging
            ? `0 0 0 3px var(--wavis-bg), 0 0 10px ${color}`
            : `0 0 0 1.5px var(--wavis-bg)`,
          transition: 'width 120ms ease, height 120ms ease, box-shadow 120ms ease',
          zIndex: 2,
        }}
      />
    </div>
  );
}
