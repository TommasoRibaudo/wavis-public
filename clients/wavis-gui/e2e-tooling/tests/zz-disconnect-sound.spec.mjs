// Live-backend proof for issue #200: the local disconnect sound
// ('leave.mp3') must fire for the person actually disconnecting on a real
// "server down" outage, not just on a voluntary leave/kick/session-displace.
//
// Named zz- (same convention as zz-network-quality.spec.mjs) so it runs
// last in the suite's alphabetical single-worker order — it fully stops
// the shared backend container for several minutes, which would break any
// other spec running concurrently or afterward if the restart step failed.
//
// Real timing, not shortened: websocket.ts's reconnect budget is
// maxReconnectAttempt=10 with exponential backoff capped at 30s (~181s
// worst case) before reconnectExhausted flips true, plus one periodic-retry
// attempt (up to 30s more) before voice-room.ts's give-up branch actually
// fires — there is no test-only override for this today (see the fix's
// commit history: an earlier, faster-looking version of this logic was
// itself a bug). This spec waits out the real budget rather than risk
// re-introducing that bug via a shortcut.
import { test, expect } from './fixtures.mjs';
import {
  SERVER_URL,
  waitForBackendHealth,
  seedChannelWithInvite,
  registerAndLoginViaUi,
  joinChannelViaUi,
  enterChannelRoom,
  joinDefaultSubRoomViaUi,
  leaveRoomIfActive,
  stopBackendContainer,
  startBackendContainer,
  visibleText,
} from './live-backend-helpers.mjs';

test(
  'a real backend outage plays the disconnect sound once the reconnect budget is exhausted',
  async ({ app }) => {
    await waitForBackendHealth();

    const suffix = Date.now().toString(36);
    const channelName = `e2e-disconnect-sound-${suffix}`;
    const { invite } = await seedChannelWithInvite(channelName);

    const main = await app.page();
    await leaveRoomIfActive(main);
    const pathname = new URL(await main.url()).pathname;
    if (pathname.startsWith('/login') || pathname.startsWith('/setup')) {
      await registerAndLoginViaUi(main, { serverUrl: SERVER_URL });
    }

    await joinChannelViaUi(main, invite.code);
    await enterChannelRoom(main, channelName);
    await joinDefaultSubRoomViaUi(main);
    // No transient LIVE-badge precondition here: a responsive tier can hide
    // that badge even after the real sub-room join succeeds. The helper's
    // joined-room assertion plus the outage/reconnect behavior below provide
    // the relevant proof that the client was connected before shutdown.

    try {
      const outageStartedAt = Date.now();
      await stopBackendContainer();

      // The disconnect sound is a real Web Audio fetch()+decodeAudioData()
      // call (see notification-sounds.ts). Watching for a fresh
      // fetch('/sounds/leave.mp3') doesn't work reliably: notification-sounds
      // .ts caches the decoded AudioBuffer per sound name for the process's
      // lifetime, so a *second* "leave" play (e.g. this spec's own
      // leaveRoomIfActive() cleanup step above already played one, if the
      // account happened to start mid-room — a normal, expected scenario,
      // it's why that cleanup step exists) never fetches again — the fetch
      // watch would then wait out the full budget and time out even though
      // the sound genuinely played. window.__wavisNotificationSoundLog
      // (VITE_DIAGNOSTICS-only, notification-sounds.ts) fires on every play
      // regardless of cache hit/miss, so it doesn't have this blind spot.
      await expect
        .poll(
          () =>
            main.evaluate(
              (since) =>
                (window.__wavisNotificationSoundLog ?? []).some(
                  (e) => e.name === 'leave' && e.playedAt >= since,
                ),
              outageStartedAt,
            ),
          {
            timeout: 330_000,
            intervals: [5_000],
            message:
              'expected the "leave" disconnect sound to play once the WS reconnect-give-up path fires',
          },
        )
        .toBe(true);

      await expect(visibleText(main, 'Connection lost')).toBeVisible();

      // Exactly one disconnect sound since the outage started — not one per
      // failed reconnect attempt.
      const soundLog = await main.evaluate(() => window.__wavisNotificationSoundLog ?? []);
      expect(
        soundLog.filter((e) => e.name === 'leave' && e.playedAt >= outageStartedAt),
      ).toHaveLength(1);
    } finally {
      await startBackendContainer();
      // The "Connection lost" screen replaces the normal room UI (no CLI
      // input to send /leave through), but it renders its own /leave button —
      // use that instead of leaveRoomIfActive's CLI-command path so the
      // session doesn't restore mid-room for the next spec run.
      const leaveButton = visibleText(main, '/leave');
      if (await leaveButton.isVisible().catch(() => false)) {
        await leaveButton.click();
      }
      await leaveRoomIfActive(main);
    }
  },
  { timeoutMs: 360_000 },
);
