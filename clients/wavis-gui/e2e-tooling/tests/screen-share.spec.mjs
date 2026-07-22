// Live-backend, screen-share signaling in both directions.
//
// Direction A (committed, strong): the GUI starts a real native screen
// capture via the inline SharePicker (ActiveRoom.tsx's `/share` CLI command
// -> real list_share_sources IPC, no OS dialog), and a ws-sfu-test peer
// observes the resulting share_started/share_stopped broadcasts.
//
// Direction B (committed, signaling-level only): a ws-sfu-test peer sends
// raw start_share/stop-share signaling with no media ever attached (it can't
// publish real pixels — see livekit-tone-peer.mjs's header comment for why
// no scriptable peer here can), and the GUI shows the "waiting for
// stream..." indicator, which is exactly the correct behavior for a
// signaling-only sharer with no track.
//
// Scope limit: real watched pixels (a peer that actually publishes video)
// are not covered here — that needs a video-publishing peer, which is a
// timeboxed stretch tracked separately (see README.md).
//
// RESOLVED (was a spec bug, not a product bug): Direction B's "stop-share"
// send used the wrong wire tag ("stop_share"). SignalingMessage's enum
// default is rename_all = "snake_case", but StopShare carries an explicit
// #[serde(rename = "stop-share")] override (shared/src/signaling/mod.rs) —
// asymmetric with StartShare, which has no override and does use plain
// snake_case. The wrong tag parsed as an unknown message type and was
// silently dropped server-side, so the indicator never cleared. Both
// directions pass now that the send uses the real tag.
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
  visibleTitle,
  visibleIcon,
  spawnPeer,
  joinVoiceAsPeer,
  sendCliCommand,
} from './live-backend-helpers.mjs';

test('GUI shares its screen and a peer observes start/stop; a peer signaling a share shows as waiting-for-stream', async ({
  app,
}) => {
  await waitForBackendHealth();

  const suffix = Date.now().toString(36);
  const channelName = `e2e-share-${suffix}`;
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
      displayName: 'PeerBot',
    });
    // Participant rows (and hence the "waiting for stream..." indicator in
    // Direction B) only render for sub-room members.
    await joinDefaultSubRoomAsPeer(peer);
    await expect(visibleText(main, 'PeerBot')).toBeVisible({ timeout: 10_000 });

    /* ── Direction A: GUI shares, peer observes ───────────────────── */
    await sendCliCommand(main, '/share');
    // Linux/WebKitGTK opens the picker in its own Tauri window because
    // getDisplayMedia() is unavailable there. Other platforms may render
    // the inline picker in the main room, so drive whichever surface the
    // application actually opened.
    const picker =
      process.platform === 'linux' ? await app.getPageByPath('/share-picker') : main;
    await visibleText(picker, '▲ Share Picker').waitFor({ state: 'visible', timeout: 10_000 });

    const listbox = picker.getByRole('listbox', { name: 'Available sources' });
    await listbox.getByRole('option').first().click({ timeout: 15_000 });
    const shareButton = picker.getByRole('button', { name: 'Share', exact: true });
    if (process.platform === 'linux') {
      // The picker closes itself from this handler. WebKitWebDriver's W3C
      // pointer action otherwise waits to release the pointer in a window
      // that no longer exists and strands the whole session. Schedule the
      // same native button click after execute() has returned to the driver.
      await shareButton.waitFor({ state: 'visible', timeout: 10_000 });
      const element = await shareButton.resolveOne();
      await picker.browser.execute((target) => {
        setTimeout(() => target.click(), 0);
      }, element);
    } else {
      await shareButton.click();
    }

    await peer.waitForOutput(/"type":"share_started"/, 15_000);
    await expect(visibleIcon(main, 'you are sharing')).toBeVisible({ timeout: 10_000 });

    await sendCliCommand(main, '/stopshare');
    await peer.waitForOutput(/"type":"share_stopped"/, 15_000);
    await expect(main.getByRole('img', { name: 'you are sharing' })).toHaveCount(0);

    /* ── Direction B: peer signals a share, GUI shows waiting indicator ── */
    peer.send({ type: 'start_share' });
    await expect(visibleTitle(main, 'waiting for stream...')).toBeVisible({ timeout: 10_000 });

    // Wire tag is "stop-share" (hyphen), not "stop_share" — StopShare has an
    // explicit #[serde(rename = "stop-share")] overriding the enum's
    // rename_all = "snake_case" default (shared/src/signaling/mod.rs). The
    // wrong tag parses as an unknown message type and gets silently dropped.
    peer.send({ type: 'stop-share' });
    await expect(main.getByTitle('waiting for stream...')).toHaveCount(0, { timeout: 10_000 });
  } finally {
    await peer.close();
    await leaveRoomIfActive(main);
  }
});
