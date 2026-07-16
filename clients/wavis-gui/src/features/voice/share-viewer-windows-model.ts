/**
 * Pure decision points extracted from useShareViewerWindows.ts.
 *
 * The hook's window bookkeeping (shareWindowsRef, nativeShareViewersRef,
 * watchAllWindowRef) stays as native Map/Set refs holding real Tauri window
 * handles — that IPC-backed state isn't touched here. What's extracted is
 * the *branching logic* around it: whether an open() call needs to tear
 * down an existing window first (never two live windows for one sharer
 * slot), whether a Watch-All open/toggle creates a new window or reuses
 * the existing one, and which pop-out windows survive a sub-room switch.
 *
 * Zero IO — no Tauri, no MediaStream, no React. Directly unit-testable.
 */
import type { ShareViewerScope } from './useShareViewerWindows';

/* ─── Opening a per-sharer viewer (native or webview pop-out) ──────── */

export interface OpenShareWindowPlan {
  /** A native (Rust-side) viewer for this participant must be closed first. */
  closeExistingNative: boolean;
  /** A webview pop-out for this participant must be closed first (and its
   *  destruction awaited before the same window label can be reused). */
  closeExistingPopout: boolean;
}

/**
 * Decide what must be torn down before opening a viewer for `participantId`.
 * Enforces "one live window per sharer slot": whichever kind is already
 * open for this participant is always closed before a new one is created,
 * so re-requesting the same slot can never leave two windows referencing it.
 */
export function planOpenShareWindow(
  registry: { natives: ReadonlyMap<string, unknown>; popouts: ReadonlyMap<string, unknown> },
  participantId: string,
): OpenShareWindowPlan {
  return {
    closeExistingNative: registry.natives.has(participantId),
    closeExistingPopout: registry.popouts.has(participantId),
  };
}

/* ─── Watch All: open / toggle / reuse ─────────────────────────────── */

export type WatchAllOpenAction = 'create' | 'focus-existing';

/** Opening Watch All while it's already open reuses (focuses) that window instead of recreating it. */
export function planOpenWatchAllWindow(alreadyOpen: boolean): WatchAllOpenAction {
  return alreadyOpen ? 'focus-existing' : 'create';
}

export type WatchAllOpenWhenClosedAction = 'open' | 'noop';

/** The share:toggle-watch-all hotkey when no Watch All window exists yet. */
export function decideWatchAllOpenWhenClosed(
  hasRemoteSharers: boolean,
): WatchAllOpenWhenClosedAction {
  return hasRemoteSharers ? 'open' : 'noop';
}

export type WatchAllToggleWhenOpenAction = 'unminimize' | 'close';

/** The share:toggle-watch-all hotkey when a Watch All window already exists. */
export function decideWatchAllToggleWhenOpen(minimized: boolean): WatchAllToggleWhenOpenAction {
  return minimized ? 'unminimize' : 'close';
}

/* ─── Sub-room switch: which pop-outs survive ──────────────────────── */

/**
 * Whether a tracked pop-out should be closed when the joined sub-room
 * changes. Only windows opened *from Watch All* for a participant who fell
 * out of the new scope are closed — a directly-opened pop-out (scope
 * 'direct') always survives, and a watch-all-opened pop-out for a
 * participant still in scope survives too. This is what keeps switching
 * sub-rooms from closing an unrelated participant's window.
 */
export function shouldCloseOnSubRoomChange(
  windowScope: ShareViewerScope,
  participantId: string,
  newScopeParticipantIds: ReadonlySet<string>,
): boolean {
  if (windowScope !== 'watch-all') return false;
  return !newScopeParticipantIds.has(participantId);
}
