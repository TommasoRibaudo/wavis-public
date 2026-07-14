/**
 * Global room hotkeys, registered only while media is connected:
 * - Watch All toggle (user-configurable, from settings-store)
 * - Focus Main window (user-configurable, from settings-store)
 *
 * Registration goes through @shared/hotkey-bridge; the toggle callback is
 * ref-mirrored so the registered hotkey never captures a stale closure.
 */

import { useEffect, useRef } from 'react';
import {
  registerWatchAllHotkey,
  unregisterWatchAllHotkey,
  registerFocusMainHotkey,
  unregisterFocusMainHotkey,
} from '@shared/hotkey-bridge';
import { getWatchAllHotkey, getFocusMainHotkey } from '@features/settings/settings-store';
import { showMainWindow } from '@shared/window-bridge';

export function useRoomHotkeys(options: {
  mediaConnected: boolean;
  onToggleWatchAll: () => void;
}): void {
  const toggleWatchAllRef = useRef(options.onToggleWatchAll);
  toggleWatchAllRef.current = options.onToggleWatchAll;
  const { mediaConnected } = options;

  // Register Watch All hotkey when media connects
  useEffect(() => {
    if (!mediaConnected) return;

    let cancelled = false;
    let registeredHotkey: string | null = null;
    void getWatchAllHotkey().then((hotkey) => {
      if (cancelled) return;
      registeredHotkey = hotkey;
      registerWatchAllHotkey(hotkey, () => toggleWatchAllRef.current());
    });

    return () => {
      cancelled = true;
      if (registeredHotkey) {
        unregisterWatchAllHotkey(registeredHotkey);
        registeredHotkey = null;
      }
    };
  }, [mediaConnected]);

  // Register Focus Main hotkey when media connects
  useEffect(() => {
    if (!mediaConnected) return;

    let cancelled = false;
    let registeredHotkey: string | null = null;
    void getFocusMainHotkey().then((hotkey) => {
      if (cancelled) return;
      registeredHotkey = hotkey;
      registerFocusMainHotkey(hotkey, () => {
        console.log('[wavis:focus-main] hotkey fired');
        void showMainWindow().catch((e: unknown) =>
          console.error('[wavis:focus-main] hotkey error:', e),
        );
      });
    });

    return () => {
      cancelled = true;
      if (registeredHotkey) {
        unregisterFocusMainHotkey(registeredHotkey);
        registeredHotkey = null;
      }
    };
  }, [mediaConnected]);
}
