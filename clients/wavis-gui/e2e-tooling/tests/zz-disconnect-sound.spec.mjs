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

test('a real backend outage plays the disconnect sound once the reconnect budget is exhausted', async ({
  app,
}) => {
  test.setTimeout(360_000);

  await waitForBackendHealth();

  const suffix = Date.now().toString(36);
  const channelName = `e2e-disconnect-sound-${suffix}`;
  const { invite } = await seedChannelWithInvite(channelName);

  const main = app.page();
  await leaveRoomIfActive(main);
  const pathname = new URL(main.url()).pathname;
  if (pathname.startsWith('/login') || pathname.startsWith('/setup')) {
    await registerAndLoginViaUi(main, { serverUrl: SERVER_URL });
  }

  const soundRequests = [];
  main.on('request', (req) => {
    const url = req.url();
    if (url.includes('/sounds/')) soundRequests.push(url);
  });

  await joinChannelViaUi(main, invite.code);
  await enterChannelRoom(main, channelName);
  await joinDefaultSubRoomViaUi(main);

  await expect(visibleText(main, 'LIVE')).toBeVisible();

  try {
    await stopBackendContainer();

    // The disconnect sound is a real Web Audio fetch()+decodeAudioData()
    // call (see notification-sounds.ts) — there is no product-side test
    // hook for "a sound played" (unlike __wavisVoiceStats), so this
    // observes the same /sounds/leave.mp3 fetch the manual live-proof run
    // did, via Playwright's own request log.
    await expect
      .poll(() => soundRequests.some((url) => url.includes('leave')), {
        timeout: 330_000,
        intervals: [5_000],
        message:
          'expected /sounds/leave.mp3 to be fetched once the WS reconnect-give-up path fires',
      })
      .toBe(true);

    await expect(visibleText(main, 'Connection lost')).toBeVisible();

    // Exactly one disconnect sound — not one per failed reconnect attempt.
    expect(soundRequests.filter((url) => url.includes('leave'))).toHaveLength(1);
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
});
