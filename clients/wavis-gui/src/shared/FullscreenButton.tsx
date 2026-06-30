import { Maximize2, Minimize2 } from 'lucide-react';

interface FullscreenButtonProps {
  isFullscreen: boolean;
  onToggle: () => void;
}

export default function FullscreenButton({ isFullscreen, onToggle }: FullscreenButtonProps) {
  return (
    <button
      onMouseDown={(e) => e.stopPropagation()}
      onClick={(e) => { e.stopPropagation(); onToggle(); }}
      className="h-5 w-5 flex items-center justify-center text-wavis-text-secondary hover:opacity-70 transition-opacity"
      aria-label={isFullscreen ? 'Exit fullscreen' : 'Enter fullscreen'}
      title={isFullscreen ? 'exit fullscreen' : 'fullscreen'}
    >
      {isFullscreen
        ? <Minimize2 size={14} strokeWidth={1.8} aria-hidden="true" />
        : <Maximize2 size={14} strokeWidth={1.8} aria-hidden="true" />}
    </button>
  );
}
