/**
 * Main-window lifecycle wiring for the active room.
 *
 * - main-window-closing (Rust on_window_event): run the room teardown
 *   callback, then ask Rust to complete the close. The Rust side uses
 *   prevent_close() until close_main_window is invoked, giving the JS event
 *   loop a tick to flush the LiveKit Leave signal before webview destruction.
 * - focus-main-window (child windows / hotkeys): restore + focus the main
 *   window from the main webview itself — more reliable than calling
 *   unminimize/setFocus on another window from a child webview.
 */

import { useEffect, useRef } from 'react';
import {
  closeMainWindow,
  onFocusMainWindow,
  onMainWindowClosing,
  showMainWindow,
} from '@shared/window-bridge';

export function useMainWindowLifecycle(onClosing: () => void): void {
  const onClosingRef = useRef(onClosing);
  onClosingRef.current = onClosing;

  useEffect(() => {
    return onMainWindowClosing(() => {
      onClosingRef.current();
      void closeMainWindow().catch(() => {});
    });
  }, []);

  useEffect(() => {
    console.log('[wavis:focus-main] registering focus-main-window listener');
    return onFocusMainWindow(() => {
      console.log('[wavis:focus-main] event received — calling show_main_window');
      void showMainWindow()
        .then(() => {
          console.log('[wavis:focus-main] show_main_window done');
        })
        .catch((e: unknown) => {
          console.error('[wavis:focus-main] error:', e);
        });
    });
  }, []);
}
