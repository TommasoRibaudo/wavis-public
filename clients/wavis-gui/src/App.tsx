import { useEffect, useRef } from 'react';
import { RouterProvider } from 'react-router';
import { router } from './routes';
import TitleBar from '@shared/TitleBar';
import { DebugProvider } from '@shared/debug-context';
import { initNotificationBridge } from '@shared/notification-bridge';
import { WebviewWindow } from '@tauri-apps/api/webviewWindow';
import { emit } from '@tauri-apps/api/event';
import { getCurrentWindow } from '@tauri-apps/api/window';
import { getState } from '@features/voice/voice-room';
import AppUpdatePrompt from '@shared/AppUpdatePrompt';
import type { AppDimensions } from '@features/diagnostics/diagnostics';

/* ─── Helpers ───────────────────────────────────────────────────── */

/** Auxiliary windows (screen share, share picker, diagnostics) — no TitleBar needed. */
function isChromelessWindow(): boolean {
  const p = window.location.pathname;
  return (
    p.startsWith('/screen-share') ||
    p.startsWith('/share-picker') ||
    p.startsWith('/share-indicator') ||
    p.startsWith('/watch-all') ||
    p.startsWith('/camera-popout') ||
    p.startsWith('/diagnostics')
  );
}

async function readAppDimensions(): Promise<AppDimensions> {
  const nativeWindow = await getCurrentWindow().outerSize();
  return {
    nativeWindow: {
      width: Math.round(nativeWindow.width),
      height: Math.round(nativeWindow.height),
    },
    viewport: {
      width: Math.round(window.innerWidth),
      height: Math.round(window.innerHeight),
    },
    devicePixelRatio: window.devicePixelRatio,
    capturedAt: Date.now(),
  };
}

async function emitAppDimensions(): Promise<AppDimensions | null> {
  try {
    const appDimensions = await readAppDimensions();
    await emit('diagnostics:app-dimensions', appDimensions);
    return appDimensions;
  } catch (err) {
    console.warn('[wavis:diagnostics] app dimensions unavailable:', err);
    return null;
  }
}

/* ═══ Component ═════════════════════════════════════════════════════ */

export default function App() {
  const latestAppDimensionsRef = useRef<AppDimensions | null>(null);

  // Initialize notification bridge (seeds visibility cache, starts event listener)
  useEffect(() => {
    const cleanup = initNotificationBridge();
    return cleanup;
  }, []);

  // Auto-open diagnostics window when VITE_DIAGNOSTICS=true (baked in at build time).
  // WAVIS_DIAGNOSTICS_WINDOW is not checked here because dotenvy only loads .env in
  // debug builds — the VITE_ flag is the single source of truth for release builds.
  useEffect(() => {
    if (import.meta.env.VITE_DIAGNOSTICS !== 'true') return;
    if (isChromelessWindow()) return;
    const win = new WebviewWindow('diagnostics', {
      url: '/diagnostics',
      title: 'Wavis Diagnostics',
      width: 400,
      height: 700,
      alwaysOnTop: true,
      decorations: true,
    });
    win.once('tauri://error', (e) => {
      console.warn('[wavis:diagnostics] window open error:', e);
    });
  }, []);

  // Diagnostics voice-stats bridge.
  // The diagnostics window runs in a separate webview with its own JS context, so it
  // cannot read voice-room.ts module state directly. This interval pushes a serialisable
  // snapshot of the current voice-room stats to the diagnostics window via Tauri events.
  // Only runs in the main window (not in screen-share/diagnostics pop-outs).
  useEffect(() => {
    if (import.meta.env.VITE_DIAGNOSTICS !== 'true') return;
    if (isChromelessWindow()) return;

    const intervalId = setInterval(() => {
      const {
        networkStats,
        shareStats,
        videoReceiveStats,
        participants,
        selfParticipantId,
        activeVideoShare,
      } = getState();
      const self = participants.find((p) => p.id === selfParticipantId);
      const fallbackShareActive = self?.isSharing === true && self.shareType !== 'audio_only';
      const isSharing = activeVideoShare !== null || fallbackShareActive;
      void emit('diagnostics:voice-stats', {
        networkStats,
        shareStats,
        videoReceiveStats,
        isSharing,
        shareMode: activeVideoShare?.mode ?? (fallbackShareActive ? self?.shareType ?? 'browser' : null),
        shareSourceName: activeVideoShare?.sourceName ?? (fallbackShareActive ? 'Browser screen share' : null),
        // Serialize only what diagnostics needs — avoids MediaStream and other non-serialisable fields.
        participants: participants.map((p) => ({
          id: p.id,
          rmsLevel: p.rmsLevel,
          isSpeaking: p.isSpeaking,
        })),
        selfParticipantId,
        appDimensions: latestAppDimensionsRef.current,
      });
    }, 1000);

    return () => clearInterval(intervalId);
  }, []);

  // Diagnostics app-size bridge. Sends the main Wavis native window and webview
  // viewport dimensions on startup and resize without a polling loop.
  useEffect(() => {
    if (import.meta.env.VITE_DIAGNOSTICS !== 'true') return;
    if (isChromelessWindow()) return;

    let mounted = true;
    let frameId: number | null = null;
    let unlistenResize: (() => void) | null = null;

    const publishDimensions = async () => {
      const appDimensions = await emitAppDimensions();
      if (mounted && appDimensions) {
        latestAppDimensionsRef.current = appDimensions;
      }
    };

    const scheduleEmit = () => {
      if (frameId !== null) return;
      frameId = window.requestAnimationFrame(() => {
        frameId = null;
        if (mounted) {
          void publishDimensions();
        }
      });
    };

    void publishDimensions();
    window.addEventListener('resize', scheduleEmit);
    getCurrentWindow()
      .onResized(scheduleEmit)
      .then((unlisten) => {
        if (!mounted) {
          unlisten();
          return;
        }
        unlistenResize = unlisten;
      })
      .catch((err) => {
        console.warn('[wavis:diagnostics] resize listener unavailable:', err);
      });

    return () => {
      mounted = false;
      if (frameId !== null) {
        window.cancelAnimationFrame(frameId);
      }
      window.removeEventListener('resize', scheduleEmit);
      unlistenResize?.();
    };
  }, []);

  if (isChromelessWindow()) {
    return <RouterProvider router={router} />;
  }

  return (
    <DebugProvider>
      <div className="flex flex-col h-full">
        <TitleBar />
        <AppUpdatePrompt />
        <div className="flex-1 overflow-hidden">
          <RouterProvider router={router} />
        </div>
      </div>
    </DebugProvider>
  );
}
