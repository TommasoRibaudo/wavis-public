import { useEffect } from 'react';
import { RouterProvider } from 'react-router';
import { router } from './routes';
import TitleBar from '@shared/TitleBar';
import BugReportButton from '@features/diagnostics/BugReportButton';
import { DebugProvider } from '@shared/debug-context';
import { initNotificationBridge } from '@shared/notification-bridge';
import { WebviewWindow } from '@tauri-apps/api/webviewWindow';
import { emit } from '@tauri-apps/api/event';
import { getState } from '@features/voice/voice-room';
import AppUpdatePrompt from '@shared/AppUpdatePrompt';

/* ─── Helpers ───────────────────────────────────────────────────── */

/** Auxiliary windows (screen share, share picker, diagnostics) — no TitleBar needed. */
function isChromelessWindow(): boolean {
  const p = window.location.pathname;
  return (
    p.startsWith('/screen-share') ||
    p.startsWith('/share-picker') ||
    p.startsWith('/share-indicator') ||
    p.startsWith('/watch-all') ||
    p.startsWith('/diagnostics')
  );
}

/* ═══ Component ═════════════════════════════════════════════════════ */

export default function App() {
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
      const { networkStats, shareStats, videoReceiveStats, participants, selfParticipantId } = getState();
      void emit('diagnostics:voice-stats', {
        networkStats,
        shareStats,
        videoReceiveStats,
        // Serialize only what diagnostics needs — avoids MediaStream and other non-serialisable fields.
        participants: participants.map((p) => ({
          id: p.id,
          rmsLevel: p.rmsLevel,
          isSpeaking: p.isSpeaking,
        })),
        selfParticipantId,
      });
    }, 1000);

    return () => clearInterval(intervalId);
  }, []);

  if (isChromelessWindow()) {
    return <RouterProvider router={router} />;
  }

  return (
    <DebugProvider>
      <div className="flex flex-col h-full">
        <TitleBar />
        <BugReportButton />
        <AppUpdatePrompt />
        <div className="flex-1 overflow-hidden">
          <RouterProvider router={router} />
        </div>
      </div>
    </DebugProvider>
  );
}
