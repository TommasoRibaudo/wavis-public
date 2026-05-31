import { useCallback, useEffect, useRef, useState } from 'react';
import { getCurrentWindow } from '@tauri-apps/api/window';

export function useFullscreen() {
  const [isFullscreen, setIsFullscreen] = useState(false);
  const isFullscreenRef = useRef(false);

  // Sync actual window state on mount — handles windows that open already
  // fullscreen and avoids a stale-false initial render.
  useEffect(() => {
    void getCurrentWindow().isFullscreen().then((fs) => {
      isFullscreenRef.current = fs;
      setIsFullscreen(fs);
    });
  }, []);

  const exitFullscreen = useCallback(async () => {
    await getCurrentWindow().setFullscreen(false);
    isFullscreenRef.current = false;
    setIsFullscreen(false);
  }, []);

  const enterFullscreen = useCallback(async () => {
    await getCurrentWindow().setFullscreen(true);
    isFullscreenRef.current = true;
    setIsFullscreen(true);
  }, []);

  const toggleFullscreen = useCallback(() => {
    if (isFullscreenRef.current) {
      void exitFullscreen();
    } else {
      void enterFullscreen();
    }
  }, [enterFullscreen, exitFullscreen]);

  // Sync when OS-level fullscreen exit happens (e.g. macOS green button,
  // native keybind, or ESC handled by the WebView). The onResized event fires
  // for all such transitions, so this is the single authoritative sync path —
  // no separate ESC keydown handler needed.
  useEffect(() => {
    const win = getCurrentWindow();
    let unlisten: (() => void) | null = null;
    win.onResized(() => {
      void win.isFullscreen().then((fs) => {
        isFullscreenRef.current = fs;
        setIsFullscreen(fs);
      });
    }).then((fn) => { unlisten = fn; });
    return () => { unlisten?.(); };
  }, []);

  return { isFullscreen, enterFullscreen, exitFullscreen, toggleFullscreen };
}
