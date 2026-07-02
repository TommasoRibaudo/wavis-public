import { Waveform } from "./Waveform";

// A functional mock of the Wavis window — the page's main product signal.
// Built as one component with a fixed aspect-ratio frame, so a real screenshot
// <img> can replace it later without shifting the layout.

const CHAT = [
  { time: "09:42:05", user: "sam", color: "text-warn", text: "starting screen share" },
  {
    time: "09:43:10",
    user: "mira",
    color: "text-purple",
    text: "let's split into breakout rooms",
  },
];

const LOGS = [
  { time: "09:41:08", text: "alex joined Room 1", tone: "text-accent" },
  { time: "09:42:05", text: "sam started sharing", tone: "text-purple" },
  { time: "09:43:10", text: "Room 2 created", tone: "text-warn" },
  { time: "09:45:31", text: "nora joined Room 2", tone: "text-accent" },
];

export function AppMock() {
  return (
    <div
      className="w-full overflow-hidden rounded-lg border border-border bg-panel shadow-2xl shadow-black/40"
      role="img"
      aria-label="The Wavis app window: a private room with chat, logs, breakout room controls, and screen sharing controls."
    >
      {/* Title bar */}
      <div className="flex items-center justify-between border-b border-border bg-panel px-3 py-2">
        <div className="flex items-center gap-3">
          <span className="text-sm text-muted">
            <span className="text-accent">▸</span> wavis
          </span>
        </div>
        <div className="flex items-center gap-5 text-xs text-muted" aria-hidden="true">
          <span>[!]</span>
          <span>−</span>
          <span>□</span>
          <span>×</span>
        </div>
      </div>

      <div className="grid aspect-[1.65/1] min-h-[280px] grid-cols-[40%_1fr] text-[9px] leading-snug sm:grid-cols-[34%_42%_24%] sm:text-[9px] lg:text-[10px]">
        {/* Sidebar */}
        <aside className="grid min-w-0 grid-rows-[auto_auto_1fr_auto] border-r border-border bg-panel">
          <div className="border-b border-border p-2">
            <div className="mb-1 flex items-center gap-1">
              <span className="h-1.5 w-1.5 rounded-full bg-accent shadow-[0_0_8px_#a6e3a1]" />
              <span className="h-1.5 w-1.5 rounded-full bg-accent shadow-[0_0_8px_#a6e3a1]" />
              <span className="text-accent">LIVE</span>
              <span className="text-muted">2/6</span>
              <span className="hidden text-accent sm:inline">77ms</span>
            </div>
            <div className="flex items-center justify-between gap-2">
              <span className="truncate font-bold text-text">Design sync</span>
              <span className="border border-border px-1.5 py-0.5 text-muted">&gt;</span>
            </div>
          </div>

          <div className="border-b border-border p-2">
            <div className="mb-1.5 flex items-center justify-between text-muted">
              <span>[-] ROOM 1 (2)</span>
              <span className="border border-border px-1.5 py-0.5">””</span>
            </div>
            <div className="mb-1 flex items-center justify-between text-purple">
              <span>[+] alex ○</span>
              <span className="text-danger">◉</span>
            </div>
            <div className="mb-2 flex items-center justify-between text-blue">
              <span><span className="text-accent">&gt;</span> sam ○</span>
              <span className="text-danger">◉</span>
            </div>
            <div>
              <span className="block border border-accent bg-accent/10 px-2 py-1 text-center text-[1.08em] font-bold text-accent shadow-[0_0_14px_rgba(166,227,161,0.16)]">
                /watch-all
              </span>
            </div>
          </div>

          <div className="border-b border-border p-2">
            <span className="border border-accent px-1.5 py-0.5 text-accent">/create room</span>
          </div>

          <div className="space-y-1 p-2">
            <div className="text-blue">[-] sam</div>
            <div className="grid grid-cols-2 gap-1">
              <span className="border border-danger bg-danger/10 px-1 py-0.5 text-center text-danger">/unmute</span>
              <span className="border border-muted px-1 py-0.5 text-center text-text">/deafen</span>
            </div>
            <span className="block border border-muted px-1 py-0.5 text-center text-text">/camera-on</span>
            <span className="block border border-purple bg-purple/10 px-2 py-1 text-center text-[1.08em] font-bold text-purple shadow-[0_0_14px_rgba(203,166,247,0.18)]">
              /share-screen
            </span>
            <span className="block border border-muted px-1 py-0.5 text-center text-text">/settings</span>
          </div>
        </aside>

        {/* Chat */}
        <section className="grid min-w-0 grid-rows-[auto_1fr_auto] bg-bg">
          <div className="border-b border-border px-3 py-3 font-bold text-muted">
            CHAT
          </div>

          <div className="space-y-1.5 overflow-hidden p-3">
            {CHAT.map((item) => (
              <p key={`${item.time}-${item.user}`} className="truncate text-text">
                <span className="text-muted">[{item.time}] </span>
                <span className={item.color}>{item.user}</span>
                <span>: </span>
                <span>{item.text}</span>
              </p>
            ))}
          </div>

          <div className="border-t border-border px-3 py-1.5">
            <span className="mr-3 text-accent">&gt;</span>
            <span className="text-muted">type message...</span>
          </div>
        </section>

        {/* Activity */}
        <aside className="hidden min-w-0 grid-rows-[auto_1fr_auto] border-l border-border bg-panel sm:grid">
          <div className="grid grid-cols-2 border-b border-border text-center font-bold">
            <span className="border-r border-border bg-teal/10 px-2 py-3 text-accent">
              LOGS
            </span>
            <span className="px-2 py-3 text-muted">VIDEOS</span>
          </div>

          <div className="space-y-1.5 overflow-hidden p-3">
            <div className="flex items-center gap-2 text-muted">
              <span className="h-px flex-1 bg-muted" />
              <span>today</span>
              <span className="h-px flex-1 bg-muted" />
            </div>
            {LOGS.map((log) => (
              <p key={`${log.time}-${log.text}`} className="text-text">
                <span className="text-muted">[{log.time}] </span>
                <span className={log.tone}>{log.text}</span>
              </p>
            ))}
            <div className="mt-2 border border-border bg-crust p-2">
              <div className="mb-1 flex items-center gap-2 text-accent">
                <Waveform bars={4} height={12} />
                screen share
              </div>
              <div className="h-8 border border-border bg-surface/60" />
            </div>
          </div>

          <div className="border-t border-border px-3 py-1.5">
            <span className="mr-3 text-accent">&gt;</span>
            <span className="text-muted">type command...</span>
          </div>
        </aside>
      </div>
    </div>
  );
}
