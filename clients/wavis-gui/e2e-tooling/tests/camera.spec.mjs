// Live-backend, real camera publish. GUI-only spec — no peer can observe a
// camera tile (camera has no signaling of its own, and ws-sfu-test isn't a
// LiveKit participant), so this verifies the local publish/un-publish path
// through the self-row indicator. That indicator is derived from
// videoTilesById only after publishCamera() resolves, so it proves the same
// published local tile state without breakpoint-specific tab choreography.
//
// The camera button is disabled until joinedSubRoomId is set
// (active-room-camera.ts's shouldDisableCameraButton), so this joins the
// default sub-room before touching it, same as the other specs.
//
import { test, expect } from './fixtures.mjs';
import { readdirSync } from 'node:fs';
import {
  SERVER_URL,
  waitForBackendHealth,
  seedChannelWithInvite,
  registerAndLoginViaUi,
  joinChannelViaUi,
  enterChannelRoom,
  leaveRoomIfActive,
  joinDefaultSubRoomViaUi,
  visibleIcon,
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

async function activateWithoutPointerWait(page, locator) {
  const element = await locator.resolveOne();
  await page.browser.execute((target) => target.click(), element);
}

const CAMERA_SKIP_REASON =
  process.platform === 'linux' && !readdirSync('/dev').some((entry) => /^video\d+$/.test(entry))
    ? 'no Linux V4L2 camera device is available under /dev/video*'
    : false;

if (CAMERA_SKIP_REASON) {
  console.warn(`[e2e:camera] skipped: ${CAMERA_SKIP_REASON}`);
}

test(
  'toggling the camera publishes and un-publishes real local video',
  async ({ app }) => {
    await waitForBackendHealth();

    const suffix = Date.now().toString(36);
    const channelName = `e2e-camera-${suffix}`;
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

    try {
      // Flake note: if another app already holds the webcam, the GUI toasts
      // "camera device is already in use" instead of publishing, and the
      // assertion below times out visibly rather than hanging silently.
      await activateWithoutPointerWait(main, cameraButton(main, '/camera-on'));
      await expect(visibleIcon(main, 'your camera is on')).toBeVisible({ timeout: 10_000 });

      await activateWithoutPointerWait(main, cameraButton(main, '/camera-off'));
      await expect(visibleIcon(main, 'your camera is on')).toHaveCount(0, {
        timeout: 20_000,
      });
    } finally {
      await leaveRoomIfActive(main);
    }
    // Default 30s isn't enough headroom: registerAndLoginViaUi + joinChannelViaUi
    // + enterChannelRoom + joinDefaultSubRoomViaUi (up to a 10s poll plus a 15s
    // wait on their own) already eat most of it before the real camera
    // getUserMedia round-trip even starts.
  },
  { timeoutMs: 90_000, skipReason: CAMERA_SKIP_REASON },
);
