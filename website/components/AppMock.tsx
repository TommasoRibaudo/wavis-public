"use client";

import {
  useEffect,
  useRef,
  useState,
  type CSSProperties,
  type KeyboardEvent,
  type PointerEvent as ReactPointerEvent,
  type RefObject,
} from "react";

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

type ActivePanel = "main" | "watchAll";
type DragPosition = { x: number; y: number };

function isDesktopDragPointer() {
  return (
    window.matchMedia("(pointer: fine)").matches &&
    window.matchMedia("(min-width: 1024px)").matches
  );
}

function clamp(value: number, min: number, max: number) {
  return Math.min(Math.max(value, min), max);
}

function handlePanelKeyDown(
  event: KeyboardEvent<HTMLDivElement>,
  focusPanel: () => void,
) {
  if (event.key !== "Enter" && event.key !== " ") return;
  event.preventDefault();
  focusPanel();
}

function useDesktopPanelDrag(
  containerRef: RefObject<HTMLDivElement | null>,
  focusPanel: () => void,
) {
  const panelRef = useRef<HTMLDivElement>(null);
  const positionRef = useRef<DragPosition>({ x: 0, y: 0 });
  const cleanupDragRef = useRef<(() => void) | null>(null);
  const rafRef = useRef<number | null>(null);
  const [position, setPosition] = useState<DragPosition>({ x: 0, y: 0 });
  const [isDragging, setIsDragging] = useState(false);

  useEffect(() => {
    return () => {
      cleanupDragRef.current?.();
      if (rafRef.current !== null) window.cancelAnimationFrame(rafRef.current);
    };
  }, []);

  function applyDragPosition(next: DragPosition) {
    positionRef.current = next;
    if (rafRef.current !== null) return;

    rafRef.current = window.requestAnimationFrame(() => {
      rafRef.current = null;
      panelRef.current?.style.setProperty("--drag-x", `${positionRef.current.x}px`);
      panelRef.current?.style.setProperty("--drag-y", `${positionRef.current.y}px`);
    });
  }

  function onPointerDown(event: ReactPointerEvent<HTMLDivElement>) {
    if (event.button !== 0 || !isDesktopDragPointer()) return;

    const container = containerRef.current;
    const panel = panelRef.current;
    if (!container || !panel) return;

    focusPanel();
    event.preventDefault();
    panel.setPointerCapture?.(event.pointerId);

    const start = positionRef.current;
    const startPointer = { x: event.clientX, y: event.clientY };
    const boundsElement = container.closest("header") ?? container;
    const containerRect = boundsElement.getBoundingClientRect();
    const panelRect = panel.getBoundingClientRect();
    const bounds = {
      minX: start.x + containerRect.left - panelRect.left,
      maxX: start.x + containerRect.right - panelRect.right,
      minY: start.y + containerRect.top - panelRect.top,
      maxY: start.y + containerRect.bottom - panelRect.bottom,
    };
    const previousCursor = document.documentElement.style.cursor;
    const previousUserSelect = document.body.style.userSelect;

    document.documentElement.style.cursor = "grabbing";
    document.body.style.userSelect = "none";
    setIsDragging(true);

    function onPointerMove(moveEvent: PointerEvent) {
      applyDragPosition({
        x: clamp(start.x + moveEvent.clientX - startPointer.x, bounds.minX, bounds.maxX),
        y: clamp(start.y + moveEvent.clientY - startPointer.y, bounds.minY, bounds.maxY),
      });
    }

    function stopDragging() {
      cleanupDragRef.current?.();
      cleanupDragRef.current = null;
      document.documentElement.style.cursor = previousCursor;
      document.body.style.userSelect = previousUserSelect;
      setIsDragging(false);
      setPosition(positionRef.current);
    }

    cleanupDragRef.current = () => {
      window.removeEventListener("pointermove", onPointerMove);
      window.removeEventListener("pointerup", stopDragging);
      window.removeEventListener("pointercancel", stopDragging);
    };

    window.addEventListener("pointermove", onPointerMove);
    window.addEventListener("pointerup", stopDragging);
    window.addEventListener("pointercancel", stopDragging);
  }

  return {
    isDragging,
    onPointerDown,
    panelRef,
    panelStyle: {
      "--drag-x": `${position.x}px`,
      "--drag-y": `${position.y}px`,
    } as CSSProperties,
  };
}

export function AppMock() {
  const [activePanel, setActivePanel] = useState<ActivePanel>("watchAll");
  const containerRef = useRef<HTMLDivElement>(null);
  const mainDrag = useDesktopPanelDrag(containerRef, () => setActivePanel("main"));
  const watchAllDrag = useDesktopPanelDrag(containerRef, () => setActivePanel("watchAll"));

  return (
    <div
      ref={containerRef}
      className="relative isolate mx-auto w-full max-w-[840px] overflow-visible pb-[clamp(44px,7vw,76px)]"
      role="group"
      aria-label="The Wavis app preview: a private room window in front of a Watch All screen sharing panel."
    >
      <WatchAllMock
        isActive={activePanel === "watchAll"}
        onFocus={() => setActivePanel("watchAll")}
        drag={watchAllDrag}
      />
      <MainAppWindow
        isActive={activePanel === "main"}
        onFocus={() => setActivePanel("main")}
        drag={mainDrag}
      />
    </div>
  );
}

function MainAppWindow({
  isActive,
  onFocus,
  drag,
}: {
  isActive: boolean;
  onFocus: () => void;
  drag: ReturnType<typeof useDesktopPanelDrag>;
}) {
  return (
    <div
      ref={drag.panelRef}
      style={drag.panelStyle}
      className={`wavis-main-panel relative w-full overflow-hidden rounded-lg border bg-panel shadow-2xl transition-[border-color,box-shadow,transform] duration-200 lg:cursor-grab ${
        isActive
          ? "z-30 border-blue/70 shadow-black/65"
          : "z-20 border-blue/40 shadow-black/45"
      } ${drag.isDragging ? "lg:cursor-grabbing lg:transition-none lg:will-change-transform" : ""}`}
      role="button"
      tabIndex={0}
      aria-pressed={isActive}
      aria-label="Focus the main Wavis app preview"
      onClick={onFocus}
      onKeyDown={(event) => handlePanelKeyDown(event, onFocus)}
      onPointerDown={drag.onPointerDown}
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

      <div className="grid aspect-[1.5/1] min-h-[300px] grid-cols-[40%_1fr] text-[9px] leading-snug sm:aspect-[1.58/1] sm:min-h-[330px] sm:grid-cols-[34%_42%_24%] sm:text-[9px] lg:aspect-[1.62/1] lg:min-h-[360px] lg:text-[10px]">
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
              <span>[-] ROOM 1 (4)</span>
              <span className="border border-border px-1.5 py-0.5">””</span>
            </div>
            <div className="mb-1 flex items-center justify-between text-purple">
              <span>[+] alex ○</span>
              <span className="text-danger">◉</span>
            </div>
            <div className="mb-1 flex items-center justify-between text-blue">
              <span><span className="text-accent">&gt;</span> sam ○</span>
              <span className="text-danger">◉</span>
            </div>
            <div>
              <span className="block border border-blue/60 bg-blue/10 px-2 py-1 text-center text-[1.08em] font-bold text-blue shadow-black/30">
                /watch-all
              </span>
            </div>
          </div>

          <div className="border-b border-border p-2">
            <span className="border border-blue/50 px-1.5 py-0.5 text-blue">/create room</span>
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

function WatchAllMock({
  isActive,
  onFocus,
  drag,
}: {
  isActive: boolean;
  onFocus: () => void;
  drag: ReturnType<typeof useDesktopPanelDrag>;
}) {
  const streams = [
    { name: "alex", tone: "text-purple" },
    { name: "sam", tone: "text-blue" },
  ];

  return (
    <div
      ref={drag.panelRef}
      style={drag.panelStyle}
      className={`wavis-watch-panel absolute bottom-0 -right-3 w-[52%] min-w-[180px] max-w-[330px] overflow-hidden rounded-lg border bg-crust shadow-2xl transition-[border-color,box-shadow,transform] duration-200 sm:-right-4 sm:w-[48%] lg:-right-5 lg:w-[44%] lg:cursor-grab ${
        isActive
          ? "z-40 border-blue/70 shadow-black/65"
          : "z-10 border-blue/40 shadow-black/45"
      } ${drag.isDragging ? "lg:cursor-grabbing lg:transition-none lg:will-change-transform" : ""}`}
      role="button"
      tabIndex={0}
      aria-pressed={isActive}
      aria-label="Focus the Watch All preview panel"
      onClick={onFocus}
      onKeyDown={(event) => handlePanelKeyDown(event, onFocus)}
      onPointerDown={drag.onPointerDown}
    >
      <div className="flex items-center justify-between border-b border-border bg-panel/95 px-2.5 py-1.5 text-[8px] text-muted sm:text-[9px]">
        <span>
          <span className="text-accent">&gt;</span> watch all
        </span>
        <span className="text-blue">2 streams</span>
      </div>

      <div className="grid aspect-[1.05/1] grid-rows-2 gap-1.5 bg-bg p-1.5 sm:p-2">
        {streams.map((stream, index) => (
          <div
            key={stream.name}
            className="grid min-w-0 grid-rows-[1fr_auto] overflow-hidden border border-border bg-panel"
          >
            <div className="wavis-stream-preview min-h-0" />
            <div className="flex items-center justify-between gap-2 border-t border-border px-1.5 py-1 text-[7px] sm:text-[8px]">
              <span className={`truncate ${stream.tone}`}>{stream.name}</span>
              <span className={index === 0 ? "text-accent" : "text-muted"}>
                {index === 0 ? "live" : "view"}
              </span>
            </div>
          </div>
        ))}
      </div>
    </div>
  );
}
