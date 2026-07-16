// Live-backend, multi-participant: confirms ParticipantsPanel reacts to a
// second signaling participant (ws-sfu-test, see spawnPeer in
// live-backend-helpers.mjs) joining and leaving the same channel's voice
// room. Same scope limit as chat.spec.mjs: verifies the GUI's own rendering,
// not a second real GUI instance.
//
// Being in the call and being in a sub-room ("synchronized room") are
// distinct (see frontend_voice_room.md) — ParticipantsPanel only renders
// participants inside their joined sub-room, so both the GUI and the peer
// explicitly join the default sub-room below (joinDefaultSubRoomViaUi /
// joinDefaultSubRoomAsPeer) before any count/row assertion.
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
  joinDefaultSubRoomAsPeer,
  visibleText,
  spawnPeer,
  joinVoiceAsPeer,
} from './live-backend-helpers.mjs';

test('the participant count and row update when a second participant joins and leaves', async ({
  app,
}) => {
  await waitForBackendHealth();

  const suffix = Date.now().toString(36);
  const channelName = `e2e-participants-${suffix}`;
  const { owner, channel, invite } = await seedChannelWithInvite(channelName);

  const main = app.page();
  await leaveRoomIfActive(main);
  const pathname = new URL(main.url()).pathname;

  if (pathname.startsWith('/login') || pathname.startsWith('/setup')) {
    await registerAndLoginViaUi(main, { serverUrl: SERVER_URL });
  }

  await joinChannelViaUi(main, invite.code);
  await enterChannelRoom(main, channelName);
  await joinDefaultSubRoomViaUi(main);

  const peer = spawnPeer();
  try {
    await joinVoiceAsPeer(peer, {
      accessToken: owner.access_token,
      channelId: channel.channel_id,
      displayName: 'PeerBot',
    });
    await joinDefaultSubRoomAsPeer(peer);

    await expect(visibleText(main, /ROOM \d+\s*\(2\)/)).toBeVisible({ timeout: 10_000 });
    await expect(visibleText(main, 'PeerBot')).toBeVisible();

    // The backend closes the leaving peer's own socket immediately after
    // processing Leave (ws_dispatch.rs), so the leaver never observes its
    // own participant_left broadcast — that goes to the other participant
    // (the GUI). Assert on the GUI's side, not the peer's.
    peer.send({ type: 'leave' });

    await expect(visibleText(main, /ROOM \d+\s*\(1\)/)).toBeVisible({ timeout: 10_000 });
    await expect(visibleText(main, 'PeerBot')).toHaveCount(0);
  } finally {
    await peer.close();
  }
});
