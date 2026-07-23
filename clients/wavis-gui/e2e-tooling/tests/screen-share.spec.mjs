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
  visibleIcon,
  waitForVisibleDomElement,
  switchToParticipantsTab,
  spawnPeer,
  joinVoiceAsPeer,
  sendCliCommand,
} from './live-backend-helpers.mjs';

test(
  'GUI shares its screen and a peer observes start/stop; a peer signaling a share shows as waiting-for-stream',
  async ({ app }) => {
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
      // The harness window is narrow enough to use ActiveRoom's mobile tabbed
      // layout. Open the visible VOICE tab when present; otherwise a text-only
      // check can false-positive on the transient "PeerBot joined" toast while
      // every real participant row remains hidden behind the Chat tab.
      await switchToParticipantsTab(main, { displayName: 'PeerBot' });

      /* ── Direction A: GUI shares, peer observes ───────────────────── */
      await sendCliCommand(main, '/share');
      // Linux/WebKitGTK opens the picker in its own Tauri window because
      // getDisplayMedia() is unavailable there. Other platforms may render
      // the inline picker in the main room, so drive whichever surface the
      // application actually opened.
      const picker = process.platform === 'linux' ? await app.getPageByPath('/share-picker') : main;
      await visibleText(picker, '▲ Share Picker').waitFor({ state: 'visible', timeout: 10_000 });

      const listbox = picker.getByRole('listbox', { name: 'Available sources' });
      await listbox.getByRole('option').first().click({ timeout: 15_000 });
      const shareButton = picker.getByRole('button', { name: 'Share', exact: true });
      if (process.platform === 'linux') {
        // The picker closes itself from this handler. WebKitWebDriver's W3C
        // pointer action otherwise tries to release the pointer in a window
        // that no longer exists. Schedule the same DOM click after execute()
        // has returned to the driver so there is no in-flight action sequence.
        await shareButton.waitFor({ state: 'visible', timeout: 10_000 });
        const element = await shareButton.resolveOne();
        await picker.browser.execute((target) => {
          setTimeout(() => target.click(), 0);
        }, element);
      } else {
        await shareButton.click();
      }

      // Move WebKitWebDriver back to the main window before inspecting capture
      // state. A single DOM query avoids role-locator scans racing React's
      // rapidly updating room rows, while still preserving the native error
      // text that the UI only displays for five seconds.
      await main.url();
      await new Promise((resolve) => setTimeout(resolve, 1_000));
      const captureError = await main.browser.execute(() => {
        const dismiss = document.querySelector('[aria-label="Dismiss screen share error"]');
        return dismiss?.parentElement?.innerText?.trim() ?? null;
      });
      if (captureError) throw new Error(`GUI screen capture failed: ${captureError}`);

      await peer.waitForOutput(/"type":"share_started"/, 15_000);
      await expect(visibleIcon(main, 'you are sharing')).toBeVisible({ timeout: 10_000 });

      await sendCliCommand(main, '/stopshare');
      await peer.waitForOutput(/"type":"share_stopped"/, 15_000);
      await expect(main.getByRole('img', { name: 'you are sharing' })).toHaveCount(0);

      /* ── Direction B: peer signals a share, GUI shows waiting indicator ── */
      // Use the current typed signaling form so the UI can classify this as a
      // video slot even though the script peer intentionally publishes no
      // media track.
      peer.send({ type: 'start_share', shareType: 'window' });
      await peer.waitForOutput(/(?=.*"shareType":"window")(?=.*"type":"share_started")/, 10_000);
      // sendCliCommand('/share' | '/stopshare') activates the mobile Log tab;
      // return to VOICE before asserting a control inside ParticipantRow.
      await switchToParticipantsTab(main, { displayName: 'PeerBot' });
      await waitForVisibleDomElement(main, { title: 'waiting for stream...', timeoutMs: 20_000 });

      // Wire tag is "stop-share" (hyphen), not "stop_share" — StopShare has an
      // explicit #[serde(rename = "stop-share")] overriding the enum's
      // rename_all = "snake_case" default (shared/src/signaling/mod.rs). The
      // wrong tag parses as an unknown message type and gets silently dropped.
      peer.send({ type: 'stop-share' });
      await waitForVisibleDomElement(main, {
        title: 'waiting for stream...',
        visible: false,
        timeoutMs: 10_000,
      });
    } finally {
      await peer.close();
      await leaveRoomIfActive(main);
    }
  },
  { timeoutMs: process.platform === 'linux' ? 90_000 : 60_000 },
);
