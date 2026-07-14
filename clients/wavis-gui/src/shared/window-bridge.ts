/**
 * Wavis Window Bridge
 *
 * Bridges frontend ↔ Tauri main-window lifecycle: programmatic show/close
 * (via Rust IPC commands so platform-specific teardown runs), close/focus
 * event subscriptions, and current-window resizing.
 *
 * UI components must use this module instead of importing @tauri-apps/*
 * directly (see eslint no-restricted-imports fence).
 */

import { invoke } from '@tauri-apps/api/core';
import { listen } from '@tauri-apps/api/event';
import { getCurrentWindow } from '@tauri-apps/api/window';
import { LogicalSize } from '@tauri-apps/api/dpi';

const LOG = '[wavis:window-bridge]';

/** Show + focus the main window (used by tray/hotkey flows). */
export async function showMainWindow(): Promise<void> {
  await invoke('show_main_window');
}

/** Ask Rust to actually close the main window after frontend teardown. */
export async function closeMainWindow(): Promise<void> {
  await invoke('close_main_window');
}

/** Resize the current window to a logical size. */
export async function setCurrentWindowSize(width: number, height: number): Promise<void> {
  await getCurrentWindow().setSize(new LogicalSize(width, height));
}

/**
 * Subscribe to the Rust-emitted "main window is closing" event so the room
 * can tear down (leave voice, close child windows) before the close proceeds.
 * Returns a cleanup function that unsubscribes the listener.
 */
export function onMainWindowClosing(handler: () => void): () => void {
  let unlisten: (() => void) | null = null;
  listen('main-window-closing', () => handler())
    .then((fn) => {
      unlisten = fn;
    })
    .catch((err) => {
      console.error(LOG, 'failed to listen for main-window-closing:', err);
    });
  return () => {
    unlisten?.();
  };
}

/**
 * Subscribe to the "focus the main window" request event (emitted by child
 * windows / global hotkeys). Returns a cleanup function.
 */
export function onFocusMainWindow(handler: () => void): () => void {
  let unlisten: (() => void) | null = null;
  listen('focus-main-window', () => handler())
    .then((fn) => {
      unlisten = fn;
    })
    .catch((err) => {
      console.error(LOG, 'failed to listen for focus-main-window:', err);
    });
  return () => {
    unlisten?.();
  };
}
