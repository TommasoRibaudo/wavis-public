interface Feature {
  marker: string;
  title: string;
  body: string;
  accent: string;
}

const FEATURES: Feature[] = [
  {
    marker: "#",
    title: "Invite-only rooms",
    body: "Rooms are private and capped at six people. You share a code; nobody wanders in.",
    accent: "var(--color-accent)",
  },
  {
    marker: "▸",
    title: "Native desktop app",
    body: "A real app for macOS, Windows, and Linux. No tab, no browser, lower latency.",
    accent: "var(--color-blue)",
  },
  {
    marker: "◉",
    title: "Voice, camera, and screen sharing",
    body: "Talk, turn on your camera, or share a screen. Watch one stream or all of them at once.",
    accent: "var(--color-purple)",
  },
  {
    marker: "/",
    title: "Controls that match what you do",
    body: "Mute, share, join, leave. Every control maps to one action and says what it does.",
    accent: "var(--color-warn)",
  },
];

export function Features() {
  return (
    <section className="mx-auto w-full max-w-5xl px-6 py-20 sm:py-24">
      <div className="mb-10">
        <p className="mb-2 text-xs uppercase tracking-widest text-muted">
          what it is
        </p>
        <h2 className="text-2xl font-bold sm:text-3xl">
          Small rooms, explicit controls
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
