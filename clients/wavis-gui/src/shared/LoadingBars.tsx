interface LoadingBarsProps {
  label?: string;
  ariaLabel?: string;
  className?: string;
  barClassName?: string;
}

export function LoadingBars({
  label,
  ariaLabel,
  className = 'inline-flex items-center gap-2',
  barClassName = 'w-1 bg-wavis-purple',
}: LoadingBarsProps) {
  return (
    <div className={className} aria-label={ariaLabel}>
      <div className="flex items-center gap-1">
        {[0, 1, 2].map((bar) => (
          <span
            key={bar}
            className={`inline-block ${barClassName}`}
            style={{
              height: '0.55rem',
              animation: 'pulse 1.2s ease-in-out infinite',
              animationDelay: `${bar * 0.16}s`,
            }}
          />
        ))}
      </div>
      {label && <span className="text-wavis-text-secondary">{label}</span>}
    </div>
  );
}
