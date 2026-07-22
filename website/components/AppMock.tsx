"use client";

import { Camera, HeadphoneOff, Mic, MicOff, Music, ScreenShare } from "lucide-react";
import {
  useEffect,
  useRef,
  useState,
  type CSSProperties,
  type KeyboardEvent,
  type PointerEvent as ReactPointerEvent,
} from "react";

// A functional mock of the Wavis window — the page's main product signal.
// Built as one component with a fixed aspect-ratio frame, so a real screenshot
// <img> can replace it later without shifting the layout. The room/participant
// state below is a self-contained faux demo, not a real client — it exists so
// visitors can click mute/deafen/camera/share/join and see the UI actually
// respond, instead of staring at a static screenshot.

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
  { time: "09:41:08", text: "alex joined Room 1", tone: "text-purple" },
  { time: "09:41:45", text: "alex started sharing", tone: "text-purple" },
  { time: "09:42:05", text: "sam joined Room 1", tone: "text-warn" },
  { time: "09:42:31", text: "sam started sharing", tone: "text-warn" },
];

type RoomId = "room1" | "room2";
type ActivePanel = "main" | "watchAll";
type ActivityTab = "chat" | "logs" | "videos";
type ShareQuality = "low" | "high" | "max";
type DragPosition = { x: number; y: number };

// Matches the real quality picker in ActiveRoom.tsx (a <select> that appears
// under /stopshare, not an overlay on the Watch All tiles).
const SHARE_QUALITY_LABELS: Record<ShareQuality, string> = {
  low: "Smooth  1080p @ 60fps",
  high: "Sharp   1440p @ 30fps",
  max: "Max     1440p @ 60fps",
};

interface Participant {
  id: string;
  name: string;
  tone: string;
  hasShare?: boolean;
  hasAudioShare?: boolean;
}

interface WatchAllStream {
  id: string;
  name: string;
  tone: string;
}

interface SelfState {
  isMuted: boolean;
  isDeafened: boolean;
  cameraOn: boolean;
  isSharing: boolean;
  shareQuality: ShareQuality;
}

interface SelfActions {
  toggleMute: () => void;
  toggleDeafen: () => void;
  toggleCamera: () => void;
  toggleSharing: () => void;
  setShareQuality: (quality: ShareQuality) => void;
}

interface RoomState {
  currentRoomNumber: number;
  currentRoomCount: number;
  currentRoomMembers: Participant[];
  otherRoomCreated: boolean;
  otherRoomNumber: number;
  otherRoomCount: number;
  youConnected: boolean;
}

interface RoomActions {
  onJoinVoice: () => void;
  onCreateOtherRoom: () => void;
  onSwitchRoom: () => void;
  onWatchAll: () => void;
}

interface ExpandState {
  expandedId: string | null;
  onToggle: (id: string) => void;
  volumeFor: (id: string) => number;
  onVolumeChange: (id: string, value: number) => void;
}

/** Watch All's audio-only shares strip (below the video grid), mirroring
 * AudioOnlyTile.tsx in the real app. */
interface AudioShareState {
  streams: Participant[];
  mutedFor: (id: string) => boolean;
  volumeFor: (id: string) => number;
  onToggleMute: (id: string) => void;
  onVolumeChange: (id: string, value: number) => void;
}

const ROOM_SEEDS: Record<RoomId, Participant[]> = {
  room1: [
    { id: "alex", name: "alex", tone: "text-purple", hasShare: true },
    { id: "sam", name: "sam", tone: "text-warn", hasShare: true, hasAudioShare: true },
  ],
  room2: [],
};

const COMMAND_BUTTON_TONE = {
  neutral: "border-muted text-text",
  danger: "border-danger bg-danger/10 text-danger",
  "danger-glow":
    "border-danger bg-danger/10 text-danger text-[1.08em] font-bold shadow-[0_0_14px_rgba(243,139,168,0.22)]",
  accent: "border-accent bg-accent/10 text-accent",
  purple:
    "border-purple bg-purple/10 text-purple text-[1.08em] font-bold shadow-[0_0_14px_rgba(203,166,247,0.18)]",
  blue: "border-blue/50 text-blue",
} as const;

function commandButtonClass(tone: keyof typeof COMMAND_BUTTON_TONE) {
  return `border px-1 py-0.5 text-center transition-colors hover:opacity-80 ${COMMAND_BUTTON_TONE[tone]}`;
}

function isDesktopDragPointer() {
  return (
    window.matchMedia("(pointer: fine)").matches &&
    window.matchMedia("(min-width: 1024px)").matches
  );
}

function clamp(value: number, min: number, max: number) {
  return Math.min(Math.max(value, min), max);
}

// Literal strings (not built from interpolation) so Tailwind's scanner picks
// them up regardless of which index is actually used at runtime.
const GRID_COLS_CLASS = ["", "grid-cols-1", "grid-cols-2", "grid-cols-3", "grid-cols-4"];
const GRID_ROWS_CLASS = ["", "grid-rows-1", "grid-rows-2", "grid-rows-3", "grid-rows-4"];

/** Auto-fit gallery layout (Discord/Zoom-style): pick the col/row count that
 * keeps tiles closest to square for however many streams are live, instead
 * of a hardcoded template per count. The panel itself is roughly square, so
 * a plain sqrt(count) split would put 2 tiles side by side and make them
 * tall and narrow — the wrong way round for landscape share/camera content
 * — so 2 is special-cased to stack top/bottom instead. */
function watchAllGridLayout(count: number) {
  let cols: number;
  let rows: number;
  if (count <= 1) {
    cols = 1;
    rows = 1;
  } else if (count === 2) {
    cols = 1;
    rows = 2;
  } else {
    cols = Math.min(Math.ceil(Math.sqrt(count)), 4);
    rows = Math.min(Math.ceil(count / cols), 4);
  }
  const lastRowCount = count - (rows - 1) * cols;
  return {
    gridClass: `${GRID_COLS_CLASS[cols]} ${GRID_ROWS_CLASS[rows]}`,
    isLonelyLastTile: (index: number) => cols > 1 && lastRowCount === 1 && index === count - 1,
  };
}

function handlePanelKeyDown(
  event: KeyboardEvent<HTMLDivElement>,
  focusPanel: () => void,
) {
  if (event.key !== "Enter" && event.key !== " ") return;
  event.preventDefault();
  focusPanel();
}

function useDesktopPanelDrag(focusPanel: () => void) {
  const panelRef = useRef<HTMLDivElement>(null);
  const positionRef = useRef<DragPosition>({ x: 0, y: 0 });
  const cleanupDragRef = useRef<(() => void) | null>(null);
  const rafRef = useRef<number | null>(null);
  const lastPointerDownRef = useRef<{ time: number; x: number; y: number } | null>(null);
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

  function resetPosition() {
    cleanupDragRef.current?.();
    cleanupDragRef.current = null;
    if (rafRef.current !== null) {
      window.cancelAnimationFrame(rafRef.current);
      rafRef.current = null;
    }
    positionRef.current = { x: 0, y: 0 };
    panelRef.current?.style.setProperty("--drag-x", "0px");
    panelRef.current?.style.setProperty("--drag-y", "0px");
    setIsDragging(false);
    setPosition({ x: 0, y: 0 });
  }

  function onPointerDown(event: ReactPointerEvent<HTMLDivElement>) {
    if (event.button !== 0 || !isDesktopDragPointer()) return;

    const panel = panelRef.current;
    if (!panel) return;

    focusPanel();
    event.preventDefault();

    // event.preventDefault() above suppresses the browser's own click/dblclick
    // compat events, so double-click-to-reset has to be detected by hand from
    // consecutive pointerdowns rather than an onDoubleClick handler.
    const now = event.timeStamp;
    const last = lastPointerDownRef.current;
    const isDoubleClick =
      last !== null &&
      now - last.time < 600 &&
      Math.abs(event.clientX - last.x) < 10 &&
      Math.abs(event.clientY - last.y) < 10;
    lastPointerDownRef.current = { time: now, x: event.clientX, y: event.clientY };

    if (isDoubleClick) {
      lastPointerDownRef.current = null;
      resetPosition();
      return;
    }

    panel.setPointerCapture?.(event.pointerId);

    const start = positionRef.current;
    const startPointer = { x: event.clientX, y: event.clientY };
    // Bound to the whole document, not just the hero — lets the panel be
    // dragged anywhere on the page instead of being trapped in the header.
    const pageRect = document.documentElement.getBoundingClientRect();
    const panelRect = panel.getBoundingClientRect();
    const bounds = {
      minX: start.x + pageRect.left - panelRect.left,
      maxX: start.x + pageRect.right - panelRect.right,
      minY: start.y + pageRect.top - panelRect.top,
      maxY: start.y + pageRect.bottom - panelRect.bottom,
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
  const mainDrag = useDesktopPanelDrag(() => setActivePanel("main"));
  const watchAllDrag = useDesktopPanelDrag(() => setActivePanel("watchAll"));

  // null = the visitor (a third participant, distinct from the alex/sam NPCs
  // already in the room) hasn't connected yet. /join voice sets this to the
  // room they're joining; connecting and switching rooms are the same action.
  const [connectedRoomId, setConnectedRoomId] = useState<RoomId | null>(null);
  const [otherRoomCreated, setOtherRoomCreated] = useState(false);

  const [self, setSelf] = useState<SelfState>({
    isMuted: true,
    isDeafened: false,
    cameraOn: false,
    isSharing: false,
    shareQuality: "max",
  });
  const [activityTab, setActivityTab] = useState<ActivityTab>("chat");

  const [expandedId, setExpandedId] = useState<string | null>(null);
  const [volumes, setVolumes] = useState<Record<string, number>>({});
  const [audioShareMuted, setAudioShareMuted] = useState<Record<string, boolean>>({});
  const [audioShareVolumes, setAudioShareVolumes] = useState<Record<string, number>>({});

  const youConnected = connectedRoomId !== null;
  const viewedRoomId: RoomId = connectedRoomId ?? "room1";
  const currentRoomMembers = ROOM_SEEDS[viewedRoomId];
  const otherRoomId: RoomId = viewedRoomId === "room1" ? "room2" : "room1";
  const otherRoomMembers = ROOM_SEEDS[otherRoomId];
  const otherRoomExists = otherRoomId === "room1" ? true : otherRoomCreated;

  const room: RoomState = {
    currentRoomNumber: viewedRoomId === "room1" ? 1 : 2,
    currentRoomCount: currentRoomMembers.length + (youConnected ? 1 : 0),
    currentRoomMembers,
    otherRoomCreated: otherRoomExists,
    otherRoomNumber: otherRoomId === "room1" ? 1 : 2,
    otherRoomCount: otherRoomMembers.length,
    youConnected,
  };

  const roomActions: RoomActions = {
    onJoinVoice: () => setConnectedRoomId(viewedRoomId),
    onCreateOtherRoom: () => setOtherRoomCreated(true),
    onSwitchRoom: () => setConnectedRoomId(otherRoomId),
    onWatchAll: () => setActivePanel("watchAll"),
  };

  const selfActions: SelfActions = {
    toggleMute: () => setSelf((s) => ({ ...s, isMuted: !s.isMuted })),
    toggleDeafen: () => setSelf((s) => ({ ...s, isDeafened: !s.isDeafened })),
    toggleCamera: () =>
      setSelf((s) => {
        const cameraOn = !s.cameraOn;
        if (cameraOn) setActivityTab("videos");
        return { ...s, cameraOn };
      }),
    toggleSharing: () =>
      setSelf((s) => {
        const isSharing = !s.isSharing;
        if (isSharing) setActivePanel("watchAll");
        return { ...s, isSharing };
      }),
    setShareQuality: (quality) => setSelf((s) => ({ ...s, shareQuality: quality })),
  };

  const expand: ExpandState = {
    expandedId,
    onToggle: (id) => setExpandedId((cur) => (cur === id ? null : id)),
    volumeFor: (id) => volumes[id] ?? 70,
    onVolumeChange: (id, value) => setVolumes((cur) => ({ ...cur, [id]: value })),
  };

  const sharingMembers = currentRoomMembers.filter((p) => p.hasShare);
  const streams: WatchAllStream[] = [
    ...sharingMembers.map((p) => ({ id: p.id, name: p.name, tone: p.tone })),
    ...(youConnected && self.isSharing ? [{ id: "you", name: "you", tone: "text-blue" }] : []),
  ];

  const audioShares: AudioShareState = {
    streams: currentRoomMembers.filter((p) => p.hasAudioShare),
    mutedFor: (id) => audioShareMuted[id] ?? false,
    volumeFor: (id) => audioShareVolumes[id] ?? 70,
    onToggleMute: (id) => setAudioShareMuted((cur) => ({ ...cur, [id]: !cur[id] })),
    onVolumeChange: (id, value) => {
      setAudioShareVolumes((cur) => ({ ...cur, [id]: value }));
      if (value === 0) setAudioShareMuted((cur) => ({ ...cur, [id]: true }));
    },
  };

  return (
    <div
      className="relative isolate z-40 mx-auto w-full max-w-[840px] overflow-visible pb-[clamp(44px,7vw,76px)]"
      role="group"
      aria-label="The Wavis app preview: a private room window in front of a Watch All screen sharing panel. Drag either panel by its title bar anywhere on the page; double-click the title bar to reset its position."
    >
      <WatchAllMock
        isActive={activePanel === "watchAll"}
        onFocus={() => setActivePanel("watchAll")}
        drag={watchAllDrag}
        streams={streams}
        audioShares={audioShares}
      />
      <MainAppWindow
        isActive={activePanel === "main"}
        onFocus={() => setActivePanel("main")}
        drag={mainDrag}
        self={self}
        selfActions={selfActions}
        room={room}
        roomActions={roomActions}
        activityTab={activityTab}
        onSetActivityTab={setActivityTab}
        expand={expand}
      />
    </div>
  );
}

function MainAppWindow({
  isActive,
  onFocus,
  drag,
  self,
  selfActions,
  room,
  roomActions,
  activityTab,
  onSetActivityTab,
  expand,
}: {
  isActive: boolean;
  onFocus: () => void;
  drag: ReturnType<typeof useDesktopPanelDrag>;
  self: SelfState;
  selfActions: SelfActions;
  room: RoomState;
  roomActions: RoomActions;
  activityTab: ActivityTab;
  onSetActivityTab: (tab: ActivityTab) => void;
  expand: ExpandState;
}) {
  return (
    <div
      ref={drag.panelRef}
      style={drag.panelStyle}
      className={`wavis-main-panel relative w-full overflow-hidden rounded-lg border bg-panel shadow-2xl transition-[border-color,box-shadow,transform] duration-200 ${
        isActive
          ? "z-30 border-blue/70 shadow-black/65"
          : "z-20 border-blue/40 shadow-black/45"
      } ${drag.isDragging ? "lg:transition-none lg:will-change-transform" : ""}`}
      role="button"
      tabIndex={0}
      aria-pressed={isActive}
      aria-label="Focus the main Wavis app preview"
      onClick={onFocus}
      onKeyDown={(event) => handlePanelKeyDown(event, onFocus)}
    >
      {/* Title bar — the only draggable region; double-click resets position */}
      <div
        className={`flex items-center justify-between border-b border-border bg-panel px-3 py-2 lg:cursor-grab ${
          drag.isDragging ? "lg:cursor-grabbing" : ""
        }`}
        onPointerDown={drag.onPointerDown}
      >
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
              <span className="text-muted">{room.currentRoomCount}/6</span>
              <span className="hidden text-accent sm:inline">77ms</span>
            </div>
            <div className="flex items-center justify-between gap-2">
              <span className="truncate font-bold text-text">Design sync</span>
              <span className="border border-border px-1.5 py-0.5 text-muted">&gt;</span>
            </div>
          </div>

          <div className="border-b border-border p-2">
            <div className="mb-1.5 text-muted">
              [-] ROOM {room.currentRoomNumber} ({room.currentRoomCount})
            </div>

            <div className="max-h-[92px] space-y-1 overflow-y-auto pr-0.5 sm:max-h-[104px] lg:max-h-[118px]">
              {room.youConnected && (
                <div className="flex items-center justify-between text-blue">
                  <span className="inline-flex items-center gap-1">
                    <span className="text-accent">&gt;</span> you
                    {self.isDeafened ? (
                      <HeadphoneOff size={9} strokeWidth={2} className="text-danger" aria-hidden="true" />
                    ) : self.isMuted ? (
                      <MicOff size={9} strokeWidth={2} className="text-danger" aria-hidden="true" />
                    ) : (
                      <Mic size={9} strokeWidth={2} className="text-accent" aria-hidden="true" />
                    )}
                  </span>
                  <span className="inline-flex items-center gap-1">
                    {self.cameraOn && (
                      <Camera size={9} strokeWidth={2} className="text-accent" aria-hidden="true" />
                    )}
                    {self.isSharing && (
                      <ScreenShare
                        size={9}
                        strokeWidth={2}
                        className="wavis-share-pulse"
                        aria-hidden="true"
                      />
                    )}
                  </span>
                </div>
              )}

              {room.currentRoomMembers.map((p) => (
                <ParticipantMockRow
                  key={p.id}
                  participant={p}
                  isExpanded={expand.expandedId === p.id}
                  onToggleExpand={() => expand.onToggle(p.id)}
                  volume={expand.volumeFor(p.id)}
                  onVolumeChange={(value) => expand.onVolumeChange(p.id, value)}
                />
              ))}
            </div>

            {!room.youConnected && (
              <button
                type="button"
                onClick={roomActions.onJoinVoice}
                className="mt-1.5 block w-full border border-accent/60 bg-accent/10 px-2 py-1 text-center text-[1.08em] font-bold text-accent shadow-black/30"
              >
                /join voice
              </button>
            )}

            <button
              type="button"
              onClick={roomActions.onWatchAll}
              className="mt-1.5 block w-full border border-blue/60 bg-blue/10 px-2 py-1 text-center text-[1.08em] font-bold text-blue shadow-black/30"
            >
              /watch-all
            </button>
          </div>

          <div className="border-b border-border p-2">
            {room.otherRoomCreated ? (
              <>
                <div className="mb-1.5 flex items-center justify-between text-muted">
                  <span>
                    [+] ROOM {room.otherRoomNumber} ({room.otherRoomCount})
                  </span>
                </div>
                <button
                  type="button"
                  onClick={roomActions.onSwitchRoom}
                  className={`block w-full ${commandButtonClass("blue")}`}
                >
                  /join room {room.otherRoomNumber}
                </button>
              </>
            ) : (
              <button
                type="button"
                onClick={roomActions.onCreateOtherRoom}
                className={`block w-full ${commandButtonClass("blue")}`}
              >
                /create room
              </button>
            )}
          </div>

          {room.youConnected && (
            <div className="space-y-1 p-2">
              <div className="text-blue">[-] you</div>
              <div className="grid grid-cols-2 gap-1">
                <button
                  type="button"
                  onClick={selfActions.toggleMute}
                  className={commandButtonClass(self.isMuted ? "danger" : "neutral")}
                >
                  {self.isMuted ? "/unmute" : "/mute"}
                </button>
                <button
                  type="button"
                  onClick={selfActions.toggleDeafen}
                  className={commandButtonClass(self.isDeafened ? "danger" : "neutral")}
                >
                  {self.isDeafened ? "/undeafen" : "/deafen"}
                </button>
              </div>
              <button
                type="button"
                onClick={selfActions.toggleCamera}
                className={`block w-full ${commandButtonClass(self.cameraOn ? "accent" : "neutral")}`}
              >
                {self.cameraOn ? "/camera-off" : "/camera-on"}
              </button>
              <button
                type="button"
                onClick={selfActions.toggleSharing}
                className={`block w-full ${commandButtonClass(self.isSharing ? "danger-glow" : "purple")}`}
              >
                {self.isSharing ? "/stop-sharing" : "/share-screen"}
              </button>
              {self.isSharing && (
                // Matches ActiveRoom.tsx's real quality <select>, which sits
                // directly under /stopshare rather than on a Watch All tile.
                <select
                  value={self.shareQuality}
                  onChange={(event) =>
                    selfActions.setShareQuality(event.target.value as ShareQuality)
                  }
                  className="w-full cursor-pointer border border-muted bg-panel px-1 py-0.5 text-text"
                  aria-label="Share quality"
                >
                  {(Object.entries(SHARE_QUALITY_LABELS) as [ShareQuality, string][]).map(
                    ([quality, label]) => (
                      <option key={quality} value={quality}>
                        {label}
                      </option>
                    ),
                  )}
                </select>
              )}
              <span className="block border border-muted px-1 py-0.5 text-center text-text">
                /settings
              </span>
            </div>
          )}
        </aside>

        {/* Chat/Logs/Videos — combined into one tabbed pane below sm (matches
            ActiveRoom.tsx's groupedPanel), split into separate columns at sm+ */}
        <section className="grid min-w-0 grid-rows-[auto_1fr_auto] bg-bg sm:hidden">
          <div className="grid grid-cols-3 border-b border-border text-center font-bold">
            <button
              type="button"
              onClick={() => onSetActivityTab("chat")}
              className={`border-r border-border px-1 py-3 transition-colors ${
                activityTab === "chat" ? "bg-teal/10 text-accent" : "text-muted hover:text-text"
              }`}
            >
              CHAT
            </button>
            <button
              type="button"
              onClick={() => onSetActivityTab("logs")}
              className={`border-r border-border px-1 py-3 transition-colors ${
                activityTab === "logs" ? "bg-teal/10 text-accent" : "text-muted hover:text-text"
              }`}
            >
              LOGS
            </button>
            <button
              type="button"
              onClick={() => onSetActivityTab("videos")}
              className={`px-1 py-3 transition-colors ${
                activityTab === "videos" ? "bg-teal/10 text-accent" : "text-muted hover:text-text"
              }`}
            >
              VIDEOS
            </button>
          </div>

          {activityTab === "chat" && <ChatMessages />}
          {activityTab === "logs" && <LogsList />}
          {activityTab === "videos" && <VideosPanel cameraOn={self.cameraOn} />}

          <div className="border-t border-border px-3 py-1.5">
            <span className="mr-3 text-accent">&gt;</span>
            <span className="text-muted">
              {activityTab === "chat" ? "type message..." : "type command..."}
            </span>
          </div>
        </section>

        {/* Chat — desktop column (sm+) */}
        <section className="hidden min-w-0 grid-rows-[auto_1fr_auto] bg-bg sm:grid">
          <div className="border-b border-border px-3 py-3 font-bold text-muted">CHAT</div>
          <ChatMessages />
          <div className="border-t border-border px-3 py-1.5">
            <span className="mr-3 text-accent">&gt;</span>
            <span className="text-muted">type message...</span>
          </div>
        </section>

        {/* Activity — desktop column (sm+) */}
        <aside className="hidden min-w-0 grid-rows-[auto_1fr_auto] border-l border-border bg-panel sm:grid">
          <div className="grid grid-cols-2 border-b border-border text-center font-bold">
            <button
              type="button"
              onClick={() => onSetActivityTab("logs")}
              className={`border-r border-border px-2 py-3 transition-colors ${
                activityTab !== "videos" ? "bg-teal/10 text-accent" : "text-muted hover:text-text"
              }`}
            >
              LOGS
            </button>
            <button
              type="button"
              onClick={() => onSetActivityTab("videos")}
              className={`px-2 py-3 transition-colors ${
                activityTab === "videos" ? "bg-teal/10 text-accent" : "text-muted hover:text-text"
              }`}
            >
              VIDEOS
            </button>
          </div>

          {activityTab === "videos" ? (
            <VideosPanel cameraOn={self.cameraOn} />
          ) : (
            <LogsList />
          )}

          <div className="border-t border-border px-3 py-1.5">
            <span className="mr-3 text-accent">&gt;</span>
            <span className="text-muted">type command...</span>
          </div>
        </aside>
      </div>
    </div>
  );
}

function ChatMessages() {
  return (
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
  );
}

function LogsList() {
  return (
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
  );
}

function VideosPanel({ cameraOn }: { cameraOn: boolean }) {
  return (
    <div className="overflow-hidden p-3">
      {cameraOn ? (
        <div className="grid aspect-[16/10] w-full grid-rows-[1fr_auto] overflow-hidden border border-border bg-panel">
          <div className="wavis-stream-preview min-h-0" />
          <div className="flex items-center justify-between gap-2 border-t border-border px-1.5 py-1 text-[7px]">
            <span className="truncate text-blue">you</span>
            <span className="text-accent">live</span>
          </div>
        </div>
      ) : (
        <p className="text-muted">no active cameras</p>
      )}
    </div>
  );
}

function ParticipantMockRow({
  participant,
  isExpanded,
  onToggleExpand,
  volume,
  onVolumeChange,
}: {
  participant: Participant;
  isExpanded: boolean;
  onToggleExpand: () => void;
  volume: number;
  onVolumeChange: (value: number) => void;
}) {
  return (
    <div>
      <button
        type="button"
        onClick={onToggleExpand}
        className={`flex w-full items-center justify-between text-left hover:opacity-80 ${participant.tone}`}
      >
        <span className="inline-flex items-center gap-1">
          {isExpanded ? "[-]" : "[+]"} {participant.name}
          <Mic size={9} strokeWidth={2} className="text-muted" aria-hidden="true" />
        </span>
        <span className="inline-flex items-center gap-1">
          {participant.hasAudioShare && (
            <Music size={9} strokeWidth={2} className="wavis-share-pulse" aria-hidden="true" />
          )}
          {participant.hasShare && (
            <ScreenShare size={9} strokeWidth={2} className="wavis-share-pulse" aria-hidden="true" />
          )}
        </span>
      </button>
      {isExpanded && (
        <div className="mt-1 flex items-center gap-1.5 pl-3">
          <span className="shrink-0 text-muted">vol</span>
          <input
            type="range"
            min={0}
            max={100}
            value={volume}
            onChange={(event) => onVolumeChange(Number(event.target.value))}
            className="wavis-volume-slider w-full"
            aria-label={`passthrough volume for ${participant.name}`}
          />
          <span className="w-6 shrink-0 text-right text-muted">{volume}</span>
        </div>
      )}
    </div>
  );
}

function WatchAllMock({
  isActive,
  onFocus,
  drag,
  streams,
  audioShares,
}: {
  isActive: boolean;
  onFocus: () => void;
  drag: ReturnType<typeof useDesktopPanelDrag>;
  streams: WatchAllStream[];
  audioShares: AudioShareState;
}) {
  return (
    <div
      ref={drag.panelRef}
      style={drag.panelStyle}
      className={`wavis-watch-panel absolute bottom-0 -right-3 w-[52%] min-w-[180px] max-w-[330px] overflow-hidden rounded-lg border bg-crust shadow-2xl transition-[border-color,box-shadow,transform] duration-200 sm:-right-4 sm:w-[48%] lg:-right-5 lg:w-[44%] ${
        isActive
          ? "z-40 border-blue/70 shadow-black/65"
          : "z-10 border-blue/40 shadow-black/45"
      } ${drag.isDragging ? "lg:transition-none lg:will-change-transform" : ""}`}
      role="button"
      tabIndex={0}
      aria-pressed={isActive}
      aria-label="Focus the Watch All preview panel"
      onClick={onFocus}
      onKeyDown={(event) => handlePanelKeyDown(event, onFocus)}
    >
      <div
        className={`flex items-center justify-between border-b border-border bg-panel/95 px-2.5 py-1.5 text-[8px] text-muted sm:text-[9px] lg:cursor-grab ${
          drag.isDragging ? "lg:cursor-grabbing" : ""
        }`}
        onPointerDown={drag.onPointerDown}
      >
        <span>
          <span className="text-accent">&gt;</span> watch all
        </span>
        <span className="text-blue">
          {streams.length} stream{streams.length === 1 ? "" : "s"}
        </span>
      </div>

      {(() => {
        const { gridClass, isLonelyLastTile } = watchAllGridLayout(streams.length);
        return (
          <div className={`grid aspect-[1.05/1] gap-1.5 bg-bg p-1.5 sm:p-2 ${gridClass}`}>
            {streams.length === 0 ? (
              <p className="flex items-center justify-center text-muted">no active shares</p>
            ) : (
              streams.map((stream, index) => (
                <div
                  key={stream.id}
                  className={`grid min-w-0 grid-rows-[1fr_auto] overflow-hidden border border-border bg-panel ${
                    isLonelyLastTile(index) ? "col-span-full" : ""
                  }`}
                >
                  <div className="wavis-stream-preview min-h-0" />
                  <div className="flex items-center justify-between gap-2 border-t border-border px-1.5 py-1 text-[7px] sm:text-[8px]">
                    <span className={`truncate ${stream.tone}`}>{stream.name}</span>
                    <span className="text-accent">live</span>
                  </div>
                </div>
              ))
            )}
          </div>
        );
      })()}

      {/* Audio-only shares strip — below the video grid, mirrors AudioOnlyTile.tsx */}
      {audioShares.streams.length > 0 && (
        <div className="divide-y divide-border border-t border-border bg-panel/95">
          {audioShares.streams.map((p) => {
            const muted = audioShares.mutedFor(p.id);
            const volume = audioShares.volumeFor(p.id);
            return (
              <div
                key={p.id}
                className="flex items-center gap-1.5 px-2 py-1.5 text-[7px] sm:text-[8px]"
              >
                <button
                  type="button"
                  onClick={(event) => {
                    event.stopPropagation();
                    audioShares.onToggleMute(p.id);
                  }}
                  className="shrink-0"
                  aria-label={muted ? `Unmute ${p.name} audio` : `Mute ${p.name} audio`}
                  title={muted ? "click to unmute" : "click to mute"}
                >
                  <Music
                    size={11}
                    strokeWidth={1.8}
                    fill={muted ? "currentColor" : "none"}
                    className={muted ? "wavis-share-pulse" : "text-danger"}
                    aria-hidden="true"
                  />
                </button>
                <span className={`min-w-0 truncate ${p.tone}`}>{p.name}</span>
                <span className="ml-auto hidden text-muted sm:inline">audio vol</span>
                <input
                  type="range"
                  min={0}
                  max={100}
                  value={volume}
                  onChange={(event) => {
                    event.stopPropagation();
                    audioShares.onVolumeChange(p.id, Number(event.target.value));
                  }}
                  onClick={(event) => event.stopPropagation()}
                  className="wavis-volume-slider w-10 shrink-0 sm:w-14"
                  aria-label={`${p.name} audio volume`}
                />
                <span className="w-5 shrink-0 text-right text-muted">{muted ? 0 : volume}</span>
              </div>
            );
          })}
        </div>
      )}
    </div>
  );
}
