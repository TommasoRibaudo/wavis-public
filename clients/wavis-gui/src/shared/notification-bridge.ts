/**
 * Wavis Notification Bridge
 *
 * Wraps @tauri-apps/plugin-notification with notification toggle checking
 * and window visibility gating. Notifications are only sent when the window
 * is hidden (minimized to tray) — in-app toasts handle the visible case.
 */

import { invoke } from '@tauri-apps/api/core';
import { listen } from '@tauri-apps/api/event';
import {
  isPermissionGranted,
  requestPermission,
  sendNotification,
} from '@tauri-apps/plugin-notification';
import { isNotificationEnabled } from '@features/settings/settings-store';
import type { NotificationToggles } from '@features/settings/settings-store';

export type NotifiableEvent = keyof NotificationToggles;

const LOG_PREFIX = '[wavis:notification]';

// ─── Visibility Cache ──────────────────────────────────────────────

let windowVisible = true;

// ─── Init / Cleanup ────────────────────────────────────────────────

/**
 * Initialize the notification bridge:
 * 1. Seed windowVisible cache via is_window_visible IPC
 * 2. Listen for window-visibility-changed events
 * Returns a cleanup function to unsubscribe.
 */
export function initNotificationBridge(): () => void {
  // Seed visibility cache
  invoke<boolean>('is_window_visible')
    .then((visible) => {
      windowVisible = visible;
    })
    .catch(() => {
      windowVisible = true; // assume visible on error
    });

  // Listen for visibility changes (primary mechanism)
  let unlisten: (() => void) | null = null;
  listen<{ visible: boolean }>('window-visibility-changed', (event) => {
    windowVisible = event.payload.visible;
  }).then((fn) => {
    unlisten = fn;
  });

  return () => {
    unlisten?.();
  };
}

// ─── Notification Dispatch ─────────────────────────────────────────

/**
 * Send a native OS notification if:
 * 1. Toggle for this event type is enabled
 * 2. Window is hidden (from local cache)
 * 3. OS notification permission is granted
 *
 * On Linux, errors are caught silently (NFR-3, R19.8).
 */
export async function sendWavisNotification(
  event: NotifiableEvent,
  body: string,
): Promise<void> {
  try {
    // Check toggle
    const enabled = await isNotificationEnabled(event);
    if (!enabled) return;

    // Check window visibility — suppress if visible (in-app toasts suffice)
    if (windowVisible) return;

    // Check OS permission
    let permitted = await isPermissionGranted();
    if (!permitted) {
      const result = await requestPermission();
      permitted = result === 'granted';
    }
    if (!permitted) return;

    sendNotification({ title: 'Wavis', body });
  } catch (err) {
    // Silent failure on Linux (NFR-3)
    console.warn(LOG_PREFIX, 'notification error:', err);
  }
}
