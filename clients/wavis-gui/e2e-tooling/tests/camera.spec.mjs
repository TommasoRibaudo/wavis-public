// Live-backend, real camera publish. GUI-only spec — no peer can observe a
// camera tile (camera has no signaling of its own, and ws-sfu-test isn't a
// LiveKit participant), so this only verifies the local publish/un-publish
// path: self-row indicator + the VIDEOS tab tile.
//
// The camera button is disabled until joinedSubRoomId is set
// (active-room-camera.ts's shouldDisableCameraButton), so this joins the
// default sub-room before touching it, same as the other specs.
//
// RESOLVED (was a spec bug, not a product bug): the camera capture/publish
// pipeline works correctly — confirmed via page console capture, getUserMedia
// resolves against a real device, the track publishes, and the self row's
// "your camera is on" indicator does appear. The earlier "No video active
// never reappears after camera-off" failure was this spec asserting a state
// the app never presents at the e2e window's tier: ActiveRoom.tsx's
// groupedPanelVideoActivityKey effect (~line 239-256) auto-switches the
// grouped panel away from VIDEO the instant videoTilesById empties, so the
// placeholder is unmounted before it could ever be observed there. See the
// aria-selected assertion + explicit re-click below.
import { test, expect } from './fixtures.mjs';
import {
  SERVER_URL,
  waitForBackendHealth,
  seedChannelWithInvite,
  registerViaUi,
  joinChannelViaUi,
  enterChannelRoom,
  leaveRoomIfActive,
  joinDefaultSubRoomViaUi,
  visibleText,
  visibleTitle,
} from './live-backend-helpers.mjs';

/**
 * The camera button renders as one of two mutually-exclusive variants
 * depending on the youBar's expanded/collapsed state (ActiveRoom.tsx):
 * collapsed -> icon-only button with title={cameraLabel} and no visible
 * text; expanded -> a full-width button whose visible text IS cameraLabel
 * with no title. Only one is ever mounted at a time, so this matches
 * whichever is actually present rather than assuming a layout state.
 */
function cameraButton(page, label) {
  return page
    .locator('button:visible', { hasText: label })
    .or(page.locator(`button[title="${label}"]:visible`));
}

test('toggling the camera publishes and un-publishes a real local video tile', async ({ app }) => {
  await waitForBackendHealth();

  const suffix = Date.now().toString(36);
  const channelName = `e2e-camera-${suffix}`;
  const { invite } = await seedChannelWithInvite(channelName);

  const main = app.page();
  await leaveRoomIfActive(main);
  const pathname = new URL(main.url()).pathname;

  if (pathname.startsWith('/login') || pathname.startsWith('/setup')) {
    await registerViaUi(main, {
      username: `e2e-camera-gui-${suffix}`,
      password: `e2e-password-${suffix}`,
      serverUrl: SERVER_URL,
    });
  }

  await joinChannelViaUi(main, invite.code);
  await enterChannelRoom(main, channelName);
  await joinDefaultSubRoomViaUi(main);

  try {
    // Flake note: if another app already holds the webcam, the GUI toasts
    // "camera device is already in use" instead of publishing, and the
    // assertion below times out visibly rather than hanging silently.
    await cameraButton(main, '/camera-on').click();
    await expect(visibleTitle(main, 'your camera is on')).toBeVisible({ timeout: 10_000 });

    await main
      .locator('button:visible', { hasText: /^VIDEOS?\b/ })
      .first()
      .click();
    await expect(visibleText(main, 'No video active')).toHaveCount(0);

    await cameraButton(main, '/camera-off').click();
    await expect(main.getByTitle('your camera is on')).toHaveCount(0);

    // By design (ActiveRoom.tsx's groupedPanelVideoActivityKey effect), the
    // grouped panel auto-switches itself away from VIDEO the instant
    // videoTilesById empties — so "No video active" never becomes visible
    // right here: the tab itself switches away before the placeholder would
    // render. Assert that designed auto-switch instead of a state this tier
    // never reaches on its own.
    const videoTab = main.locator('button:visible', { hasText: /^VIDEOS?\b/ }).first();
    await expect(videoTab).toHaveAttribute('aria-selected', 'false');

    // Re-select VIDEO explicitly — the effect only fires on tile-activity
    // transitions, so a manual re-click sticks — and confirm the
    // placeholder appears once the tab is actually being viewed.
    await videoTab.click();
    await expect(visibleText(main, 'No video active')).toBeVisible({ timeout: 10_000 });
  } finally {
    await leaveRoomIfActive(main);
  }
});
