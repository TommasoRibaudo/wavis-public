/**
 * Wavis Autostart Bridge
 *
 * Bridges frontend ↔ the OS-level "launch Wavis at login" registration
 * (Windows registry Run key / macOS LaunchAgent / Linux desktop autostart
 * file) via the Tauri autostart plugin. The plugin call itself is the
 * source of truth — there is no separate persisted setting, so the toggle
 * always reflects what the OS actually has registered.
 *
 * UI components must use this module instead of importing
 * @tauri-apps/plugin-autostart directly (see eslint no-restricted-imports
 * fence).
 */

import { enable, disable, isEnabled } from '@tauri-apps/plugin-autostart';

const LOG = '[wavis:autostart-bridge]';

/** Whether Wavis is currently registered to launch at OS login. */
export async function isLaunchOnStartupEnabled(): Promise<boolean> {
  try {
    return await isEnabled();
  } catch (err) {
    console.error(LOG, 'failed to read autostart state:', err);
    return false;
  }
}

/** Register (or unregister) Wavis to launch at OS login. */
export async function setLaunchOnStartupEnabled(wantEnabled: boolean): Promise<void> {
  try {
    if (wantEnabled) {
      await enable();
    } else {
      await disable();
    }
  } catch (err) {
    console.error(LOG, 'failed to set autostart state:', err);
  }
}
