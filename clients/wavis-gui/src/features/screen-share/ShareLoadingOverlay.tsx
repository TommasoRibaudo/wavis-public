interface ShareLoadingOverlayProps {
  compact?: boolean;
}

export default function ShareLoadingOverlay({ compact = false }: ShareLoadingOverlayProps) {
  return (
    <div className="absolute inset-0 pointer-events-none flex items-center justify-center bg-wavis-overlay-base/80">
      <div className="flex items-center gap-1.5" aria-label="Loading screen share">
        {[0, 1, 2].map((bar) => (
          <span
            key={bar}
            className="inline-block w-1 bg-wavis-purple"
            style={{
              height: compact ? '0.7rem' : '0.9rem',
              animation: 'pulse 1.2s ease-in-out infinite',
              animationDelay: `${bar * 0.16}s`,
            }}
          />
        ))}
      </div>
    </div>
  );
}
