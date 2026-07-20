// Live-backend, real backend restart mid-call: proves the signaling
// WebSocket's automatic reconnect (websocket.ts's exponential backoff +
// auth resend, voice-room.ts's session-scoped preferred-sub-room restore)
// carries the room/sub-room session back to a healthy state with NO manual
// user interaction, that the reconnecting client's own account is never
// self-evicted (voice_orchestrator.rs's evict_stale_session is keyed by
// user_id — a race between the old and new session for the SAME account
// could in principle trip it), and that a peer's audio is detected again
// afterward — proving the GUI's media/stats pipeline actually works post-
// recovery, not just that the UI "looks" reconnected.
//
// Scope: one real GUI instance + one @livekit/rtc-node tone peer
// (audio-received.spec.mjs's simpler, proven pattern), not two full GUI
// instances (two-instances.spec.mjs) — driving two real windows through a
// live docker restart at once compounds flake risk for no additional
// coverage of the claims actually under test here.
//
// `ws-sfu-test` (spawnPeer's process) is a one-shot `connect_async` with no
// reconnect loop of its own (confirmed via its source) — restarting
// wavis-backend kills its signaling connection for good, so the pre-restart
// peer cannot itself prove "detected again" after recovery. A second,
// fresh peer is spawned after the restart instead, with the SAME display
// name as the first — this also directly tests that the backend actually
// cleaned up the dead first session rather than leaving a stale duplicate
// participant row alongside the new one.
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
  participantRow,
  spawnPeer,
  joinVoiceAsPeer,
  waitForMediaToken,
  restartBackend,
} from './live-backend-helpers.mjs';
import { connectTonePeer } from './livekit-tone-peer.mjs';

// Same band as audio-received.spec.mjs — see that file's header comment for
// why this range can only be reached by genuine decoded-audio analyser RMS.
const DECODED_AUDIO_RMS_MIN = 0.2;
const DECODED_AUDIO_RMS_MAX = 0.95;

const PEER_DISPLAY_NAME = 'ReconnectPeer';

/** Reads the (non-self) peer's rmsLevel from the exposed voice-stats snapshot. */
async function readPeerRms(page) {
  return page.evaluate(() => {
    const stats = window.__wavisVoiceStats;
    if (!stats) return null;
    const entry = stats.participants.find((p) => p.id !== stats.selfParticipantId);
    return entry ? entry.rmsLevel : null;
  });
}

/** Switches ActiveRoom's visible tab to LOG, tolerating tiers where it's already selected. */
async function switchToLogTab(page) {
  const logsButton = page.locator('button:visible', { hasText: /^LOGS?\b/ }).first();
  if (await logsButton.isVisible().catch(() => false)) {
    await logsButton.click();
  }
}

async function connectPeerWithTone(peer, { channelId, displayName }) {
  await joinVoiceAsPeer(peer, { channelId, displayName });
  await joinDefaultSubRoomAsPeer(peer);
  const { token, sfuUrl } = await waitForMediaToken(peer);
  return connectTonePeer({ sfuUrl, token });
}

test(
  'a backend restart mid-call recovers automatically: no manual rejoin, no duplicate session, media works again',
  async ({ app }) => {
    await waitForBackendHealth();

    const suffix = Date.now().toString(36);
    const channelName = `e2e-reconnect-${suffix}`;
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

    let peer1;
    let tone1;
    let peer2;
    let tone2;
    try {
      /* ── Baseline: a peer's tone is detected before any disruption ────── */
      peer1 = await spawnPeer({ accessToken: owner.access_token });
      tone1 = await connectPeerWithTone(peer1, {
        channelId: channel.channel_id,
        displayName: PEER_DISPLAY_NAME,
      });

      await expect(participantRow(main, PEER_DISPLAY_NAME)).toBeVisible({ timeout: 15_000 });
      await expect
        .poll(
          async () => {
            const rms = await readPeerRms(main);
            return rms !== null && rms > DECODED_AUDIO_RMS_MIN && rms < DECODED_AUDIO_RMS_MAX;
          },
          {
            timeout: 20_000,
            message: 'peer rmsLevel never entered the decoded-audio band before the restart',
          },
        )
        .toBe(true);

      /* ── Disrupt: restart the backend container mid-call ──────────────── */
      await restartBackend();

      /* ── Recovery: automatic rejoin, no manual interaction ────────────── */
      // The room/sub-room header reappearing (rather than the app bouncing to
      // /login or getting stuck on a disconnected placeholder) is the
      // observable proof that voice-room.ts's reconnect+rejoin flow ran on
      // its own — this test never clicks anything to make it happen.
      await expect(visibleText(main, /ROOM \d+\s*\(\d+\)/)).toBeVisible({ timeout: 90_000 });
      await expect(main).toHaveURL(/\/room/);

      // The room event log's explicit "reconnected" marker (voice-room.ts's
      // appendEvent on the reconnect path) — corroboration alongside the
      // header check, not the sole proof.
      await switchToLogTab(main);
      await expect(visibleText(main, 'reconnected — back online')).toBeVisible({ timeout: 15_000 });

      // The core "no duplicate session" claim: the reconnecting client's own
      // account must never be told it was displaced by itself.
      await expect(main.getByText('Session taken over by another device')).toHaveCount(0);

      /* ── A fresh peer, same display name, proves both cleanup and media ─
       * are healthy again: exactly one row (the dead pre-restart session was
       * actually cleaned up server-side, not left as a stale duplicate next
       * to the new one) and a fresh tone lands back in the decoded-audio band.
       */
      peer2 = await spawnPeer({ accessToken: owner.access_token });
      tone2 = await connectPeerWithTone(peer2, {
        channelId: channel.channel_id,
        displayName: PEER_DISPLAY_NAME,
      });

      await expect(participantRow(main, PEER_DISPLAY_NAME)).toHaveCount(1);
      await expect
        .poll(
          async () => {
            const rms = await readPeerRms(main);
            return rms !== null && rms > DECODED_AUDIO_RMS_MIN && rms < DECODED_AUDIO_RMS_MAX;
          },
          {
            timeout: 20_000,
            message: 'peer rmsLevel never re-entered the decoded-audio band after the restart',
          },
        )
        .toBe(true);
    } finally {
      if (tone2) await tone2.close().catch(() => {});
      if (peer2) await peer2.close();
      if (tone1) await tone1.close().catch(() => {});
      if (peer1) await peer1.close();
      await leaveRoomIfActive(main);
    }
  },
  // Real container restart + health poll + WS exponential backoff (up to
  // 10 fast attempts, capped at 30s each) + a fresh peer/tone connection —
  // generous headroom over the media-plane specs' own 90-180s budgets.
  { timeoutMs: 240_000 },
);
