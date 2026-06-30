import { HeroCommand } from "./HeroCommand";
import { Waveform } from "./Waveform";

// A functional mock of the Wavis window — the page's main product signal.
// Built as one component with a fixed aspect-ratio frame, so a real screenshot
// <img> can replace it later without shifting the layout.

const PARTICIPANTS = [
  { initial: "T", name: "tom", color: "#a6e3a1", speaking: true },
  { initial: "M", name: "mara", color: "#89b4fa", speaking: false },
  { initial: "K", name: "kai", color: "#cba6f7", speaking: false },
];

const ROOMS = [
  { name: "general", active: true },
  { name: "design", active: false },
  { name: "standup", active: false },
];

const COMMANDS = ["/create", "/join", "/mute", "/share", "/settings"];

export function AppMock() {
  return (
    <div
      className="w-full overflow-hidden rounded-lg border border-border bg-panel shadow-2xl shadow-black/40"
      role="img"
      aria-label="The Wavis app window: a general voice channel with three people connected and a command bar."
    >
      {/* Title bar */}
      <div className="flex items-center justify-between border-b border-border bg-crust px-3 py-2">
        <div className="flex items-center gap-3">
          <span className="flex items-center gap-1.5" aria-hidden="true">
            <span className="h-2.5 w-2.5 rounded-full bg-danger" />
            <span className="h-2.5 w-2.5 rounded-full bg-warn" />
            <span className="h-2.5 w-2.5 rounded-full bg-accent" />
          </span>
          <span className="text-sm text-muted">
            <span className="text-accent">▸</span> wavis
          </span>
        </div>
        <div className="flex items-center gap-2" aria-hidden="true">
          <Waveform bars={5} height={14} />
          <span className="text-xs uppercase tracking-widest text-muted">live</span>
        </div>
      </div>

      <div className="grid grid-cols-[110px_1fr] sm:grid-cols-[130px_1fr]">
        {/* Sidebar */}
        <aside className="border-r border-border bg-panel p-3 text-sm">
          <p className="mb-2 text-[10px] uppercase tracking-widest text-muted">
            rooms
          </p>
          <ul className="space-y-1">
            {ROOMS.map((room) => (
              <li
                key={room.name}
                className={
                  room.active
                    ? "flex items-center gap-1.5 rounded bg-surface px-1.5 py-1 text-text"
                    : "flex items-center gap-1.5 px-1.5 py-1 text-muted"
                }
              >
                <span className={room.active ? "text-accent" : "text-muted"}>
                  #
                </span>
                <span className="truncate">{room.name}</span>
              </li>
            ))}
          </ul>
          <p className="mb-2 mt-4 text-[10px] uppercase tracking-widest text-muted">
            invite
          </p>
          <div className="rounded border border-dashed border-border px-1.5 py-1 text-xs text-muted">
            WV-7F2K
          </div>
        </aside>

        {/* Main */}
        <section className="flex flex-col p-3">
          <div className="mb-3 flex items-center justify-between">
            <span className="text-sm">
              <span className="text-accent">#</span> general
            </span>
            <span className="text-xs text-muted">3 / 6</span>
          </div>

          <ul className="grid grid-cols-3 gap-2">
            {PARTICIPANTS.map((p) => (
              <li
                key={p.name}
                className="flex flex-col items-center gap-1.5 rounded border border-border bg-crust/60 px-2 py-3"
              >
                <span
                  className="flex h-9 w-9 items-center justify-center rounded-full text-sm font-bold"
                  style={{ background: p.color, color: "#1e1e2e" }}
                  aria-hidden="true"
                >
                  {p.initial}
                </span>
                <span className="flex items-center gap-1 text-xs text-muted">
                  <span
                    className="h-1.5 w-1.5 rounded-full"
                    style={{
                      background: p.speaking
                        ? "var(--color-accent)"
                        : "var(--color-muted)",
                      animation: p.speaking
                        ? "wavis-dot 1.2s ease-in-out infinite"
                        : undefined,
                    }}
                    aria-hidden="true"
                  />
                  {p.name}
                </span>
              </li>
            ))}
          </ul>

          <div className="mt-auto pt-3">
            <div className="mb-2 flex flex-wrap gap-1.5" aria-hidden="true">
              {COMMANDS.map((c) => (
                <span
                  key={c}
                  className="rounded border border-border px-2 py-0.5 text-xs text-muted"
                >
                  {c}
                </span>
              ))}
            </div>
            <div className="rounded border border-border bg-crust px-2 py-1.5">
              <HeroCommand />
            </div>
          </div>
        </section>
      </div>
    </div>
  );
}
