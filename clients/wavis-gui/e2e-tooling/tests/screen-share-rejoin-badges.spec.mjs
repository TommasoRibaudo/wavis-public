// Live-backend regression test: a participant running an independent
// video-type share (no companion audio) AND a standalone audio-only share at
// the same time used to have the second share silently overwrite the first
// in the backend's per-participant share state. Another participant who left
// and rejoined the room would then see only whichever share was started
// last in the ShareState snapshot — not both.
//
// Uses the same raw ws-sfu-test peer pattern as screen-share.spec.mjs
// (Direction B): the peer never publishes real media, so the GUI's video
// badge renders as "waiting for stream..." rather than "watch share" — that
// is expected and does not affect this test, which is only about whether
// the video-share and audio-share badges render independently, both while
// connected and after the observing window rejoins the room.
import { test } from './fixtures.mjs';
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
  waitForVisibleDomElement,
  spawnPeer,
  joinVoiceAsPeer,
} from './live-backend-helpers.mjs';

/** Same "get back to the channels list" fallback joinChannelViaUi uses internally. */
async function goToChannelsList(page) {
  if (
    !(await page
      .getByRole('heading', { name: 'channels' })
      .isVisible()
      .catch(() => false))
  ) {
    const back = page.getByText('← /channels', { exact: true });
    if (await back.isVisible().catch(() => false)) await back.click();
  }
  await page.getByRole('heading', { name: 'channels' }).waitFor({ state: 'visible' });
}

test(
  'rejoining participant sees both video-share and audio-share badges for a concurrent sharer',
  async ({ app }) => {
    await waitForBackendHealth();

    const suffix = Date.now().toString(36);
    const channelName = `e2e-share-rejoin-${suffix}`;
    const { owner, channel, invite } = await seedChannelWithInvite(channelName);

    const main = await app.page();
    await leaveRoomIfActive(main);
    const pathname = new URL(await main.url()).pathname;

    if (pathname.startsWith('/login') || pathname.startsWith('/setup')) {
      await registerAndLoginViaUi(main, { serverUrl: SERVER_URL });
    }

    await joinChannelViaUi(main, invite.code);
    await enterChannelRoom(main, channelName);
    await joinDefaultSubRoomViaUi(main);

    const peer = await spawnPeer({ accessToken: owner.access_token });
    try {
      await joinVoiceAsPeer(peer, {
        channelId: channel.channel_id,
        displayName: 'SharerBot',
      });
      // Participant rows only render for sub-room members.
      await joinDefaultSubRoomAsPeer(peer);
      // Role-based, not visibleText: the room's event log also renders a
      // "SharerBot joined" line, which a plain text match ambiguously matches
      // too. The participant row itself is the only `role="button"` element
      // with this name (see ParticipantRow.tsx).
      await waitForVisibleDomElement(main, {
        role: 'button',
        text: 'SharerBot',
        timeoutMs: 10_000,
      });

      /* ── Peer starts a video-type share (no companion audio) ──────────── */
      peer.send({ type: 'start_share', shareType: 'window' });
      await peer.waitForOutput(/"type":"share_started"/, 15_000);
      await waitForVisibleDomElement(main, { title: 'waiting for stream...', timeoutMs: 10_000 });

      /* ── Peer ALSO starts an independent standalone audio-only share ──── */
      peer.send({ type: 'start_share', shareType: 'audio_only' });
      // Lookaheads, not an ordered pattern: ws-sfu-test's print_text roundtrips
      // through serde_json::Value (no preserve_order feature), which sorts
      // object keys alphabetically — "shareType" ends up before "type" on the
      // wire line, same phenomenon chatMessageReceivedPattern works around above.
      await peer.waitForOutput(
        /(?=.*"type":"share_started")(?=.*"shareType":"audio_only")/,
        15_000,
      );

      // Both badges must render at once: the audio-only start must not hide
      // the still-active video share, and vice versa.
      await waitForVisibleDomElement(main, { title: 'waiting for stream...', timeoutMs: 10_000 });
      await waitForVisibleDomElement(main, { title: 'mute audio share', timeoutMs: 10_000 });

      /* ── Regression: leave and rejoin — only the ShareState snapshot ──── */
      // now describes the sharer's state to the rejoining client. Both slots
      // must still be reflected, not just whichever share started last.
      await leaveRoomIfActive(main);
      await goToChannelsList(main);
      await enterChannelRoom(main, channelName);
      // SharerBot never leaves the sub-room (it shares continuously across
      // main's leave/rejoin — that's the point of this regression test), so
      // main's rejoin brings the sub-room to 2 occupants, not 1.
      await joinDefaultSubRoomViaUi(main, { expectedCount: 2 });
      // Role-based, not visibleText: the room's event log also renders a
      // "SharerBot joined" line, which a plain text match ambiguously matches
      // too. The participant row itself is the only `role="button"` element
      // with this name (see ParticipantRow.tsx).
      await waitForVisibleDomElement(main, {
        role: 'button',
        text: 'SharerBot',
        timeoutMs: 10_000,
      });

      await waitForVisibleDomElement(main, { title: 'waiting for stream...', timeoutMs: 10_000 });
      await waitForVisibleDomElement(main, { title: 'mute audio share', timeoutMs: 10_000 });
    } finally {
      await peer.close();
      await leaveRoomIfActive(main);
    }
    // Default 30s isn't enough headroom: this is the only spec that runs the
    // full join flow (joinChannelViaUi + enterChannelRoom +
    // joinDefaultSubRoomViaUi, up to a 10s poll plus a 15s wait each) twice —
    // once before the leave, once after the rejoin.
  },
  { timeoutMs: 90_000 },
);
