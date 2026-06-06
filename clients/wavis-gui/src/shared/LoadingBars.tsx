interface LoadingBarsProps {
  label?: string;
  ariaLabel?: string;
  className?: string;
  barClassName?: string;
  size?: 'sm' | 'md';
}

export function LoadingBars({
  label,
  ariaLabel,
  className = 'inline-flex items-center gap-2',
  barClassName,
  size = 'md',
}: LoadingBarsProps) {
  const barHeight = size === 'sm' ? '0.42rem' : '0.55rem';
  const defaultBarClassName = size === 'sm' ? 'w-0.5 bg-wavis-purple' : 'w-1 bg-wavis-purple';

  return (
    <div className={className} aria-label={ariaLabel}>
      <div className="flex items-center gap-1">
        {[0, 1, 2].map((bar) => (
          <span
            key={bar}
            className={`inline-block ${barClassName ?? defaultBarClassName}`}
            style={{
              height: barHeight,
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
