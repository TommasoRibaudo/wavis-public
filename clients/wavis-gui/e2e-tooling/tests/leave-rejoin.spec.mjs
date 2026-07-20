// Live-backend: reproduces GH issue #192 ("Joined wavis room, left, now
// cannot reconnect"). Joins a channel's voice room, leaves it via the real
// /leave command, then rejoins the SAME channel a second time — asserting
// the second join completes cleanly (own participant row visible, no stuck
// "reconnecting"/"Connection lost" state) and that no console error of the
// null-localParticipant class from the original bug report fires during
// leave/rejoin.
import { test, expect } from './fixtures.mjs';
import {
  SERVER_URL,
  waitForBackendHealth,
  seedChannelWithInvite,
  registerAndLoginViaUi,
  joinChannelViaUi,
  enterChannelRoom,
  leaveRoomIfActive,
  joinDefaultSubRoomViaUi,
  visibleText,
} from './live-backend-helpers.mjs';

test(
  'leaving a room and rejoining the same channel succeeds',
  async ({ app }) => {
    await waitForBackendHealth();

    const suffix = Date.now().toString(36);
    const channelName = `e2e-leave-rejoin-${suffix}`;
    const { invite } = await seedChannelWithInvite(channelName);

    const main = await app.page();
    await leaveRoomIfActive(main);
    const pathname = new URL(await main.url()).pathname;

    if (pathname.startsWith('/login') || pathname.startsWith('/setup')) {
      await registerAndLoginViaUi(main, { serverUrl: SERVER_URL });
    }

    await main.startCapturingConsoleErrors();

    await joinChannelViaUi(main, invite.code);
    await enterChannelRoom(main, channelName);
    await joinDefaultSubRoomViaUi(main);

    await expect(visibleText(main, /ROOM \d+\s*\(1\)/)).toBeVisible({ timeout: 10_000 });

    // Explicit leave (the exact user action issue #192's title describes).
    await leaveRoomIfActive(main);
    expect(new URL(await main.url()).pathname.startsWith('/room')).toBe(false);

    // Rejoin the SAME channel a second time. Channel membership persists
    // across a room leave (only the voice call ended, not the channel
    // membership), so re-using the invite code here would 409 "already a
    // member" — just re-enter the channel row directly, like a real user
    // clicking it again. Unlike the first entry (enterChannelRoom, above),
    // having visited this channel's room once already means its name now
    // also appears in a persistent recents/switcher element outside the
    // channels-list row, so an exact-text locator resolves to multiple
    // elements here — `.first()` picks the real channels-list row (still
    // first in DOM order).
    await main.getByText(channelName, { exact: true }).first().click();
    await main.waitForURL(/\/room/, { timeout: 10_000 });
    await joinDefaultSubRoomViaUi(main);

    // The bug: after leaving, a second join never recovers — machineState gets
    // stuck in 'reconnecting'/'idle' with an "error" event instead of 'active'.
    // Assert the room actually came back up: own row visible, no stuck
    // "Connection lost"/"reconnecting" system event in the log.
    await expect(visibleText(main, /ROOM \d+\s*\(1\)/)).toBeVisible({ timeout: 10_000 });
    await expect(visibleText(main, 'Connection lost')).toHaveCount(0);
    await expect(visibleText(main, 'signaling reconnect failed')).toHaveCount(0);

    await leaveRoomIfActive(main);

    const consoleErrors = await main.consoleErrors();
    const nullLocalParticipantErrors = consoleErrors.filter((text) =>
      text.includes("Cannot read properties of null (reading 'localParticipant')"),
    );
    expect(nullLocalParticipantErrors).toEqual([]);
  },
  // Double the usual single-join flow (join, leave, rejoin) plus an extra
  // sub-room join — the suite's default 30s budget (sized for one join) is
  // too tight for this spec specifically.
  { timeoutMs: 90_000 },
);
