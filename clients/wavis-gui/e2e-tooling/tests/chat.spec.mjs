// Live-backend, multi-participant: the GUI (real Playwright-driven window)
// and a second signaling participant (ws-sfu-test, see spawnPeer in
// live-backend-helpers.mjs) exchange chat messages in the same channel's
// voice room. This verifies the GUI's own ChatPanel rendering reacts
// correctly to a second real participant — it does NOT exercise a second
// real GUI instance (that's an unproven, unbuilt spike — see README).
import { test, expect } from './fixtures.mjs';
import {
  SERVER_URL,
  waitForBackendHealth,
  seedChannelWithInvite,
  registerAndLoginViaUi,
  joinChannelViaUi,
  enterChannelRoom,
  leaveRoomIfActive,
  sendChatMessage,
  chatMessageReceivedPattern,
  visibleText,
  spawnPeer,
  joinVoiceAsPeer,
} from './live-backend-helpers.mjs';

test('chat messages are exchanged between the GUI and a second participant', async ({ app }) => {
  await waitForBackendHealth();

  const suffix = Date.now().toString(36);
  const channelName = `e2e-chat-${suffix}`;
  const { owner, channel, invite } = await seedChannelWithInvite(channelName);

  const main = app.page();
  await leaveRoomIfActive(main);
  const pathname = new URL(main.url()).pathname;

  if (pathname.startsWith('/login') || pathname.startsWith('/setup')) {
    await registerAndLoginViaUi(main, { serverUrl: SERVER_URL });
  }

  await joinChannelViaUi(main, invite.code);
  await enterChannelRoom(main, channelName);

  const peer = spawnPeer();
  try {
    await joinVoiceAsPeer(peer, {
      accessToken: owner.access_token,
      channelId: channel.channel_id,
      displayName: 'PeerBot',
    });

    // GUI -> peer.
    const guiMessage = `hello from gui ${suffix}`;
    await sendChatMessage(main, guiMessage);

    await peer.waitForOutput(chatMessageReceivedPattern(guiMessage));

    // Peer -> GUI.
    const peerMessage = `hello from peer ${suffix}`;
    peer.send({ type: 'chat_send', text: peerMessage });

    await expect(visibleText(main, peerMessage)).toBeVisible({ timeout: 10_000 });
  } finally {
    await peer.close();
  }
});
