import { useState, useEffect } from 'react';
import {
  getMuteHotkey,
  getWatchAllHotkey,
  getFocusMainHotkey,
  DEFAULT_MUTE_HOTKEY,
  DEFAULT_WATCH_ALL_HOTKEY,
  DEFAULT_FOCUS_MAIN_HOTKEY,
} from '@features/settings/settings-store';

function formatForDisplay(hotkey: string): string {
  const isMac = typeof navigator !== 'undefined' && /Mac/i.test(navigator.platform);
  let result = hotkey.replace('CmdOrCtrl', isMac ? 'Cmd' : 'Ctrl');
  if (isMac) result = result.replace(/\bMeta\b/, 'Cmd');
  return result;
}

export interface HotkeyDisplays {
  mute: string;
  watchAll: string;
  focusMain: string;
}

/** Reads all three configured hotkeys from the settings store and formats them for display. */
export function useHotkeys(): HotkeyDisplays {
  const [hotkeys, setHotkeys] = useState<HotkeyDisplays>({
    mute: formatForDisplay(DEFAULT_MUTE_HOTKEY),
    watchAll: formatForDisplay(DEFAULT_WATCH_ALL_HOTKEY),
    focusMain: formatForDisplay(DEFAULT_FOCUS_MAIN_HOTKEY),
  });

  useEffect(() => {
    void Promise.all([getMuteHotkey(), getWatchAllHotkey(), getFocusMainHotkey()]).then(
      ([mute, watchAll, focusMain]) => {
        setHotkeys({
          mute: formatForDisplay(mute),
          watchAll: formatForDisplay(watchAll),
          focusMain: formatForDisplay(focusMain),
        });
      },
    );
  }, []);

  return hotkeys;
}
