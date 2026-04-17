/**
 * Wavis Tray Bridge
 *
 * Bridges frontend ↔ Rust tray events via Tauri's event system.
 * Rust emits `tray-event` when menu items are clicked; the frontend
 * listens and dispatches to voice-room actions. The frontend emits
 * `tray-state-update` back to Rust so the tray can enable/disable
 * menu items and update the mute label.
 */

import { emit, listen } from '@tauri-apps/api/event';

// ─── Types ─────────────────────────────────────────────────────────

/** Actions the Rust tray menu can send to the frontend. */
export type TrayAction = 'toggle-mute' | 'leave' | 'show';

/** State the frontend sends to Rust to update tray menu items. */
export interface TrayStateUpdate {
  inVoiceSession: boolean;
  isMuted: boolean;
}

/** Enable/disable state for tray menu items. */
export interface TrayMenuState {
  muteEnabled: boolean;
  leaveEnabled: boolean;
}

// ─── Constants ─────────────────────────────────────────────────────

const LOG = '[wavis:tray]';
const TRAY_EVENT = 'tray-event';
const TRAY_STATE_UPDATE_EVENT = 'tray-state-update';

// ─── Pure Helpers (exported for testing) ───────────────────────────

/**
 * Compute which tray menu items should be enabled based on voice state.
 * Both mute and leave are disabled when no voice session is active.
 */
export function computeTrayMenuState(update: TrayStateUpdate): TrayMenuState {
  return {
    muteEnabled: update.inVoiceSession,
    leaveEnabled: update.inVoiceSession,
  };
}

/**
 * Return the label for the mute menu item based on current mute state.
 * "Unmute" when muted, "Mute" when not muted.
 */
export function muteMenuLabel(isMuted: boolean): string {
  return isMuted ? 'Unmute' : 'Mute';
}

/**
 * Determine whether the window should be hidden (not destroyed) on close.
 * Returns true when minimize-to-tray is enabled.
 */
export function shouldHideOnClose(minimizeToTray: boolean): boolean {
  return minimizeToTray;
}

// ─── Event Bridge (exported) ───────────────────────────────────────

/**
 * Listen for tray menu click events from Rust.
 * Returns a cleanup function that unsubscribes the listener.
 */
export function listenTrayEvents(handler: (action: TrayAction) => void): () => void {
  let unlisten: (() => void) | null = null;

  listen<{ action: TrayAction }>(TRAY_EVENT, (event) => {
    console.log(LOG, 'received tray action:', event.payload.action);
    handler(event.payload.action);
  })
    .then((fn) => {
      unlisten = fn;
    })
    .catch((err) => {
      console.error(LOG, 'failed to listen for tray events:', err);
    });

  return () => {
    unlisten?.();
  };
}

/**
 * Send current voice/mute state to Rust so the tray menu can
 * enable/disable items and update the mute label.
 */
export function updateTrayState(update: TrayStateUpdate): void {
  emit(TRAY_STATE_UPDATE_EVENT, update).catch((err) => {
    console.error(LOG, 'failed to emit tray state update:', err);
  });
}
