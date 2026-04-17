interface ShareSwitchingOverlayProps {
  compact?: boolean;
  displayName?: string;
}

export default function ShareSwitchingOverlay({
  compact = false,
  displayName,
}: ShareSwitchingOverlayProps) {
  const title = compact ? 'stream interrupted...' : 'stream interrupted';
  const subtitle = compact
    ? null
    : `${displayName ?? 'The stream'} was briefly interrupted. Playback resumes automatically.`;

  return (
    <div
      className="absolute inset-0 pointer-events-none flex items-center justify-center"
      style={{
        background:
          'radial-gradient(circle at 50% 18%, rgba(198,120,221,0.18), rgba(13,17,23,0.94) 62%)',
      }}
    >
      <div
        className="border border-wavis-text-secondary/60 bg-wavis-panel/90 text-wavis-text shadow-[0_0_24px_rgba(0,0,0,0.45)]"
        style={{
          width: compact ? 'min(88%, 15rem)' : 'min(88%, 24rem)',
          padding: compact ? '0.75rem 0.875rem' : '1rem 1.125rem',
        }}
      >
        <div className="flex items-center gap-2 text-[0.625rem] uppercase tracking-[0.22em] text-wavis-warn">
          <div className="flex items-center gap-1.5">
            {[0, 1, 2].map((bar) => (
              <span
                key={bar}
                className="inline-block w-1 bg-wavis-purple"
                style={{
                  height: compact ? '0.55rem' : '0.7rem',
                  animation: 'pulse 1.2s ease-in-out infinite',
                  animationDelay: `${bar * 0.16}s`,
                }}
              />
            ))}
          </div>
          <span>[reconnecting]</span>
        </div>
        <div className="mt-2 text-sm text-wavis-text">{title}</div>
        {subtitle && (
          <div className="mt-1 text-xs leading-5 text-wavis-text-secondary">
            {subtitle}
          </div>
        )}
      </div>
    </div>
  );
}
