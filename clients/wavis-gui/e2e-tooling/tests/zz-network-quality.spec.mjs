// Live-backend, real media: proves media stays smooth under good network
// conditions, degrades observably (but keeps flowing) under a simulated bad
// network, and recovers once the bad network clears. audio-received.spec.mjs
// and screen-share-audio-not-heard-until-opened.spec.mjs already prove media
// arrives at all; this is the smoothness/resilience proof neither covers, and
// closes screen-share.spec.mjs's "real watched pixels" scope-limit comment by
// using a real video-publishing peer for the first time.
//
// Named zz- so it runs last in the suite's alphabetical single-worker order:
// if netem teardown ever fails, a shaped container can't contaminate any
// other spec's assertions.
//
// Network simulation mechanism: CDP/Playwright network throttling does not
// touch WebRTC UDP traffic at all (confirmed against the driver's CDP
// session), so the only real lever is shaping traffic at the LiveKit
// container itself — docker-compose.yml gives the `livekit` service
// NET_ADMIN and installs iproute2 (tc) at container start. netem on the
// container's eth0 shapes LiveKit's egress, which is exactly the GUI's
// incoming-media leg (the direction these specs want to degrade). See
// live-backend-helpers.mjs's netem section and README.md's "Media quality +
// network simulation" section for the full mechanism writeup.
//
// Video auto-subscribe: unlike screen-share audio-only (deliberately gated
// behind explicit viewer intent — see issue #174's spec), ScreenShare video
// publications are subscribed unconditionally in TrackPublished
// (livekit-media.ts ~2740-2765: setSubscribed(true)/setEnabled(true) with no
// gate), and the stats-interval's video receiver polling only needs
// `publication.track` to be present (~3319-3330) — which subscription alone
// provides. So this spec does not need to open Watch All or a share tile
// first; the video peer's track decodes and reports fps as soon as it's
// published, the same way plain voice audio does in audio-received.spec.mjs.
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
  spawnPeer,
  joinVoiceAsPeer,
  waitForMediaToken,
  hasNetemSupport,
  setNetworkConditions,
  clearNetworkConditions,
} from './live-backend-helpers.mjs';
import { connectTonePeer } from './livekit-tone-peer.mjs';

// Same band as audio-received.spec.mjs — see that file's header comment for
// why this range can only be reached by genuine decoded-audio analyser RMS.
const DECODED_AUDIO_RMS_MIN = 0.2;
const DECODED_AUDIO_RMS_MAX = 0.95;

// DiagnosticsPage.tsx's own "this is bad" thresholds (concealment >10
// events/interval, packet loss >5%) — reused here as the good/bad line so
// this spec's notion of "degraded" matches what a real user would see
// flagged in the diagnostics window.
const GOOD_CONCEALMENT_MAX = 10;
const GOOD_LOSS_MAX_PERCENT = 5;

const STATS_POLL_INTERVAL_MS = 2_000;

/** Reads the full exposed voice-stats snapshot, plus the peer's (non-self) participant entry. */
async function readStats(page) {
  return page.evaluate(() => {
    const stats = window.__wavisVoiceStats;
    if (!stats) return null;
    const peer = stats.participants.find((p) => p.id !== stats.selfParticipantId) ?? null;
    return {
      networkStats: stats.networkStats,
      videoReceiveStats: stats.videoReceiveStats,
      peerRmsLevel: peer?.rmsLevel ?? null,
    };
  });
}

test(
  'media quality survives and reports a simulated bad network, then recovers',
  async ({ app, skip }) => {
    await waitForBackendHealth();

    skip(
      !(await hasNetemSupport()),
      'wavis-livekit container has no tc/netem support (offline apk add at start?)',
    );

    const suffix = Date.now().toString(36);
    const channelName = `e2e-netq-${suffix}`;
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
    let tone;
    try {
      await joinVoiceAsPeer(peer, {
        channelId: channel.channel_id,
        displayName: 'NetQualityPeer',
      });
      await joinDefaultSubRoomAsPeer(peer);

      const { token, sfuUrl } = await waitForMediaToken(peer);
      tone = await connectTonePeer({ sfuUrl, token });
      await tone.startVideoPattern();
      // Signaling half of the share — marks the peer as sharing so the GUI's
      // "waiting for stream..." state resolves once the real track arrives.
      peer.send({ type: 'start_share' });

      /* ── Phase 1: baseline (good network) ─────────────────────────── */
      let baseline;
      await expect
        .poll(
          async () => {
            const stats = await readStats(main);
            if (!stats) return false;
            baseline = stats;
            return (
              stats.peerRmsLevel !== null &&
              stats.peerRmsLevel > DECODED_AUDIO_RMS_MIN &&
              stats.peerRmsLevel < DECODED_AUDIO_RMS_MAX &&
              stats.videoReceiveStats !== null &&
              stats.videoReceiveStats.fps > 0 &&
              stats.networkStats.concealmentEventsPerInterval <= GOOD_CONCEALMENT_MAX &&
              stats.networkStats.packetLossPercent < GOOD_LOSS_MAX_PERCENT
            );
          },
          {
            timeout: 60_000,
            intervals: [STATS_POLL_INTERVAL_MS],
            message:
              'media never reached a smooth baseline (decoded audio + decoded video + good network stats)',
          },
        )
        .toBe(true);

      /* ── Phase 2: degraded network ─────────────────────────────────── */
      await setNetworkConditions({ delayMs: 150, jitterMs: 50, lossPct: 8 });

      await expect
        .poll(
          async () => {
            const stats = await readStats(main);
            if (!stats) return false;
            const audioOk =
              stats.peerRmsLevel !== null &&
              stats.peerRmsLevel > DECODED_AUDIO_RMS_MIN &&
              stats.peerRmsLevel < DECODED_AUDIO_RMS_MAX;
            const videoOk = stats.videoReceiveStats !== null && stats.videoReceiveStats.fps > 0;
            // Comparative (>, not ==) against the recorded baseline to avoid
            // flake from any single noisy interval — the impairment just has
            // to be visibly worse than the known-good baseline, not hit an
            // exact number.
            const impaired =
              stats.networkStats.packetLossPercent > baseline.networkStats.packetLossPercent ||
              stats.networkStats.concealmentEventsPerInterval >
                baseline.networkStats.concealmentEventsPerInterval;
            return audioOk && videoOk && impaired;
          },
          {
            timeout: 60_000,
            intervals: [STATS_POLL_INTERVAL_MS],
            message:
              'media did not stay resilient under a degraded network, or the stats pipeline never reflected the impairment',
          },
        )
        .toBe(true);

      /* ── Phase 3: recovery ────────────────────────────────────────── */
      await clearNetworkConditions();

      await expect
        .poll(
          async () => {
            const stats = await readStats(main);
            if (!stats) return false;
            return (
              stats.networkStats.concealmentEventsPerInterval <= GOOD_CONCEALMENT_MAX &&
              stats.networkStats.packetLossPercent < GOOD_LOSS_MAX_PERCENT
            );
          },
          {
            timeout: 60_000,
            intervals: [STATS_POLL_INTERVAL_MS],
            message: 'network stats never returned to a good band after clearing netem',
          },
        )
        .toBe(true);
    } finally {
      // Unconditional and tolerant-if-absent (see clearNetworkConditions'
      // comment) — a crashed prior assertion above must never leave the
      // shared docker-compose LiveKit container shaped for the next spec.
      await clearNetworkConditions();
      if (tone) await tone.stopVideoPattern().catch(() => {});
      if (tone) await tone.close();
      await peer.close();
      await leaveRoomIfActive(main);
    }
  },
  // Stats tick every 10s (voice-room.ts's statsInterval) — each phase needs
  // 2-3 fresh samples, plus peer spawn/join/publish overhead.
  { timeoutMs: 180_000 },
);
