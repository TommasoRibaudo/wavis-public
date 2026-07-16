// Live-backend: joining a channel by invite code via the real ChannelsList
// UI. The channel owner is a REST-only identity (registerDevice) that never
// touches the GUI — only the "joiner" side (this spec's Playwright-driven
// window) is real UI.
import { test, expect } from './fixtures.mjs';
import {
  SERVER_URL,
  waitForBackendHealth,
  seedChannelWithInvite,
  registerAndLoginViaUi,
  joinChannelViaUi,
  leaveRoomIfActive,
} from './live-backend-helpers.mjs';

test('joining a channel by invite code shows it in the channel list', async ({ app }) => {
  await waitForBackendHealth();

  const suffix = Date.now().toString(36);
  const channelName = `e2e-room-join-${suffix}`;
  const { invite } = await seedChannelWithInvite(channelName);

  const main = app.page();
  await leaveRoomIfActive(main);
  const pathname = new URL(main.url()).pathname;

  if (pathname.startsWith('/login') || pathname.startsWith('/setup')) {
    await registerAndLoginViaUi(main, { serverUrl: SERVER_URL });
  }

  await joinChannelViaUi(main, invite.code);

  await expect(main.getByText(channelName, { exact: true })).toBeVisible();
});
