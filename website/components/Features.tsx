interface Feature {
  marker: string;
  title: string;
  body: string;
  accent: string;
}

const FEATURES: Feature[] = [
  {
    marker: "#",
    title: "Lightweight native app",
    body: "Fast startup, fast performance.",
    accent: "var(--color-accent)",
  },
  {
    marker: "◉",
    title: "Camera and sharp screen sharing",
    body: "Share a high-quality, up to 1440p@60fps, stream.",
    accent: "var(--color-blue)",
  },
  {
    marker: "▮",
    title: "Audio-only sharing",
    body: "Stream audio to your friends.",
    accent: "var(--color-purple)",
  },
  {
    marker: "/",
    title: "Open source and self-deployable",
    body: "We have published everything as an open-source project, including this website and the aws infrastructure setup.",
    accent: "var(--color-warn)",
  },
];

export function Features() {
  return (
    <section className="mx-auto w-full max-w-5xl px-6 pb-20 pt-8 sm:pb-24 sm:pt-10">
      <div className="mb-10">
        <p className="mb-2 text-xs uppercase tracking-widest text-muted">
          what it is
        </p>
        <h2 className="text-2xl font-bold sm:text-3xl">
          Lightweight, native, and yours to run
        </h2>
      </div>

      <ul className="grid gap-px overflow-hidden rounded-md border border-muted/40 bg-border shadow-[0_0_0_1px_rgba(205,214,244,0.05),0_16px_48px_rgba(205,214,244,0.06)] sm:grid-cols-2">
        {FEATURES.map((f) => (
          <li
            key={f.title}
            className="relative bg-panel p-6 pl-7 transition-colors hover:bg-surface/40"
          >
            <span
              aria-hidden="true"
              className="absolute inset-x-0 top-0 h-0.5"
              style={{ background: f.accent }}
            />
            <span
              aria-hidden="true"
              className="absolute bottom-0 left-0 top-0 w-0.5"
              style={{ background: f.accent }}
            />
            <span
              aria-hidden="true"
              className="inline-flex h-8 min-w-8 items-center justify-center rounded-sm border bg-panel px-2 text-lg font-bold shadow-[0_0_18px_rgba(0,0,0,0.25)]"
              style={{ color: f.accent }}
            >
              {f.marker}
            </span>
            <h3 className="mt-3 text-base font-bold">{f.title}</h3>
            <p className="mt-2 text-sm text-muted">{f.body}</p>
          </li>
        ))}
      </ul>
    </section>
  );
}
