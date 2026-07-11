/**
 * Wavis Shell Bridge
 *
 * Opens external URLs in the user's default browser via the Tauri shell
 * plugin. UI components must use this module instead of importing
 * @tauri-apps/plugin-shell directly (see eslint no-restricted-imports fence).
 */

import { open } from '@tauri-apps/plugin-shell';

const LOG = '[wavis:shell-bridge]';

/** Open an external URL in the system default browser. */
export function openExternalUrl(url: string): void {
  open(url).catch((err) => {
    console.error(LOG, 'failed to open external url:', err);
  });
}
