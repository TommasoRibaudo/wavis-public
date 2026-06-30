// The signature element: a monospace VU meter built from CSS bars.
// Decorative everywhere it appears, so it is hidden from assistive tech.
// Animation is transform-only (scaleY) and frozen under prefers-reduced-motion.

interface WaveformProps {
  bars?: number;
  height?: number;
  color?: string;
  className?: string;
}

// A fixed, musical-looking set of phase offsets so the meter never reads as a
// uniform sine wave. Indexed modulo the bar count.
const PHASES = [0, 0.35, 0.7, 0.2, 0.55, 0.9, 0.45, 0.15, 0.8, 0.3, 0.6, 0.05];

export function Waveform({
  bars = 5,
  height = 16,
  color = "var(--color-accent)",
  className,
}: WaveformProps) {
  return (
    <span
      aria-hidden="true"
      className={className}
      style={{
        display: "inline-flex",
        alignItems: "center",
        gap: 2,
        height,
      }}
    >
      {Array.from({ length: bars }).map((_, i) => (
        <span
          key={i}
          style={{
            display: "block",
            width: 2,
            height: "100%",
            background: color,
            transformOrigin: "center",
            animation: `wavis-bar 1.1s ease-in-out ${PHASES[i % PHASES.length]}s infinite`,
          }}
        />
      ))}
    </span>
  );
}

// A full-width section separator: a faint waveform straddling a hairline rule.
export function WaveDivider() {
  return (
    <div
      aria-hidden="true"
      style={{
        display: "flex",
        alignItems: "center",
        gap: 16,
        padding: "0 24px",
        opacity: 0.5,
      }}
    >
      <span style={{ flex: 1, height: 1, background: "var(--color-border)" }} />
      <Waveform bars={9} height={14} color="var(--color-muted)" />
      <span style={{ flex: 1, height: 1, background: "var(--color-border)" }} />
    </div>
  );
}
