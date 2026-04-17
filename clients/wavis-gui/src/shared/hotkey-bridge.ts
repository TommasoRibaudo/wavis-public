/**
 * Wavis Hotkey Bridge
 *
 * Wraps @tauri-apps/plugin-global-shortcut for mute hotkey lifecycle.
 * The hotkey is registered on voice session start and unregistered on
 * voice session end (R22.7), not always registered.
 */

import { register, unregister, isRegistered } from '@tauri-apps/plugin-global-shortcut';

const LOG = '[wavis:hotkey]';

/* ─── Types ─────────────────────────────────────────────────────── */

/** Canonical modifier order for formatting. */
const MODIFIER_ORDER = ['Ctrl', 'Shift', 'Alt', 'Meta'] as const;

/* ─── Pure Helpers (exported) ───────────────────────────────────── */

/**
 * Format a hotkey combination from a set of modifiers and a main key.
 * Modifiers are joined in canonical order: Ctrl → Shift → Alt → Meta,
 * followed by the main key. Output is deterministic regardless of
 * the order the modifiers are provided.
 */
export function formatHotkeyCombination(modifiers: string[], key: string): string {
  const sorted = MODIFIER_ORDER.filter((m) => modifiers.includes(m));
  return [...sorted, key].join('+');
}

/* ─── Registration API ──────────────────────────────────────────── */

/**
 * Register the mute hotkey — called from connectMedia() success path.
 * Catches registration errors and re-throws with a user-friendly message.
 */
export async function registerMuteHotkey(
  hotkey: string,
  onToggleMute: () => void,
): Promise<void> {
  try {
    await register(hotkey, (event) => {
      // Only fire on key-down, not key-up
      if (event.state === 'Pressed') {
        onToggleMute();
      }
    });
    console.log(LOG, `registered hotkey: ${hotkey}`);
  } catch (err) {
    const message = err instanceof Error ? err.message : String(err);
    console.warn(LOG, `failed to register hotkey "${hotkey}":`, message);
    throw new Error(`Hotkey conflict — choose a different combination`);
  }
}

/**
 * Unregister the mute hotkey — called from leaveRoom(), onKicked(),
 * and disconnect paths.
 */
export async function unregisterMuteHotkey(hotkey: string): Promise<void> {
  try {
    await unregister(hotkey);
    console.log(LOG, `unregistered hotkey: ${hotkey}`);
  } catch (err) {
    console.warn(LOG, `failed to unregister hotkey "${hotkey}":`, err);
  }
}

/* ─── Watch All Hotkey ──────────────────────────────────────────── */

/**
 * Register the Watch All hotkey — called from connectMedia() success path.
 * Unlike registerMuteHotkey, this does NOT throw on failure.
 * Shortcut failure is non-fatal (Req 6.5) — the button/CLI entry points
 * remain available.
 */
export async function registerWatchAllHotkey(
  hotkey: string,
  onToggle: () => void,
): Promise<void> {
  try {
    await register(hotkey, (event) => {
      if (event.state === 'Pressed') {
        onToggle();
      }
    });
    console.log(LOG, `registered watch-all hotkey: ${hotkey}`);
  } catch (err) {
    const message = err instanceof Error ? err.message : String(err);
    console.warn(LOG, `failed to register watch-all hotkey "${hotkey}":`, message);
  }
}

/**
 * Unregister the Watch All hotkey — called from leaveRoom() and
 * disconnect paths.
 */
export async function unregisterWatchAllHotkey(hotkey: string): Promise<void> {
  try {
    await unregister(hotkey);
    console.log(LOG, `unregistered watch-all hotkey: ${hotkey}`);
  } catch (err) {
    console.warn(LOG, `failed to unregister watch-all hotkey "${hotkey}":`, err);
  }
}

/**
 * Check if a hotkey is currently registered (for settings UI re-registration).
 */
export async function isHotkeyRegistered(hotkey: string): Promise<boolean> {
  try {
    return await isRegistered(hotkey);
  } catch {
    return false;
  }
}
