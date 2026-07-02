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
    body: "Fast startup, small footprint, and no browser tab sitting between you and the room.",
    accent: "var(--color-accent)",
  },
  {
    marker: "◉",
    title: "Camera and sharp screen sharing",
    body: "Turn on camera when it helps, or share a high-quality stream with controls that stay obvious.",
    accent: "var(--color-blue)",
  },
  {
    marker: "▮",
    title: "Audio-only sharing",
    body: "Share system audio without showing your screen when the conversation only needs sound.",
    accent: "var(--color-purple)",
  },
  {
    marker: "/",
    title: "Open source and self-deployable",
    body: "Read the code, audit the server, or run your own Wavis stack for private groups.",
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

      <ul className="grid gap-px overflow-hidden rounded-md border border-border bg-border sm:grid-cols-2">
        {FEATURES.map((f) => (
          <li key={f.title} className="bg-bg p-6">
            <span
              aria-hidden="true"
              className="text-lg font-bold"
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
