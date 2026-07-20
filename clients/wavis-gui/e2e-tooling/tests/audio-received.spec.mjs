// Live-backend, real media: proves audio one participant sends is actually
// decoded and heard by another, not just signaled. This is also the spike
// for "does real WebRTC media work at all in the CDP-launched debug exe" —
// if the GUI never connects to LiveKit in this launch context, this fails
// immediately.
//
// Real media over the default local docker-compose stack needs two fixes,
// both required together (see e2e-tooling/README.md's "Known local media
// blocker" for the full diagnosis):
// 1. deploy/livekit.yaml: use_external_ip: false + node_ip: 127.0.0.1 (was
//    advertising an unreachable STUN-discovered public IP as LiveKit's own
//    ICE host candidate).
// 2. driver.mjs launches the debug exe with
//    --disable-features=WebRtcHideLocalIpsWithMdns (WebView2/Chromium
//    otherwise hides its real local IP behind an unresolvable ".local" mDNS
//    hostname in ICE candidates — Docker's bridge network can't resolve
//    those, and a non-browser @livekit/rtc-node peer against this same
//    LiveKit instance was unaffected, confirming this was browser-specific).
//
// The peer here is a @livekit/rtc-node "tone peer" (livekit-tone-peer.mjs),
// not ws-sfu-test/wavis-cli-test — neither of those can publish real media
// (wavis-cli-test's start_share is a bare signaling message with no pixels
// ever attached, and it can't join a GUI channel's voice call at all). The
// tone peer reuses the same media_token the backend hands a ws-sfu-test peer
// on join_voice (sfu_relay.rs), so the published tone is attributed to the
// peer's own signaling participant row in the GUI.
//
// Load-bearing assertion: window.__wavisVoiceStats (App.tsx, VITE_DIAGNOSTICS
// builds only) exposes the peer's live rmsLevel, computed by
// livekit-media.ts's analyser path from decoded remote PCM (attachAudioTrack
// -> Web Audio AnalyserNode). A loud sine's analyser RMS lands well inside
// 0.2-0.95 — outside both signaling-path artifacts that also set rmsLevel:
// the ActiveSpeakersChanged hardcode of 1.0, and the boosted value of
// RMS_START_THRESHOLD + 0.05 = 0.11 (voice-room.ts). isSpeaking/"Remote
// Speaking" are corroboration only, since server active-speaker events can
// set those independent of real audio.
//
// Scope limit: reverse direction (GUI mic -> peer hears) is not covered here
// — GUI mic capture is nondeterministic real hardware. The tone peer could
// gain a subscribe-and-measure mode as a follow-up.
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
  waitForMediaToken,
} from './live-backend-helpers.mjs';
import { connectTonePeer } from './livekit-tone-peer.mjs';

// voice-room.ts: RMS_START_THRESHOLD = 0.06, RMS_STOP_THRESHOLD = 0.03. The
// boosted signaling-path value is RMS_START_THRESHOLD + 0.05 = 0.11 — the
// lower bound here sits well above that so a positive match can only be
// analyser-derived.
const DECODED_AUDIO_RMS_MIN = 0.2;
const DECODED_AUDIO_RMS_MAX = 0.95;
const RMS_STOP_THRESHOLD = 0.03;

/** Reads the peer's (non-self) participant entry from the exposed voice-stats snapshot. */
async function readPeerVoiceStats(page) {
  return page.evaluate(() => {
    const stats = window.__wavisVoiceStats;
    if (!stats) return null;
    const entry = stats.participants.find((p) => p.id !== stats.selfParticipantId);
    return entry ? { rmsLevel: entry.rmsLevel, isSpeaking: entry.isSpeaking } : null;
  });
}

test(
  'audio published by a peer is decoded and reflected in the GUI as real rmsLevel',
  async ({ app }) => {
    await waitForBackendHealth();

    const suffix = Date.now().toString(36);
    const channelName = `e2e-audio-${suffix}`;
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
        displayName: 'TonePeer',
      });
      // Same sub-room as the GUI — audio routing may be sub-room scoped, and
      // this removes any ambiguity about why a track wouldn't arrive.
      await joinDefaultSubRoomAsPeer(peer);

      const { token, sfuUrl } = await waitForMediaToken(peer);
      tone = await connectTonePeer({ sfuUrl, token });

      await expect
        .poll(
          async () => {
            const entry = await readPeerVoiceStats(main);
            if (!entry) return false;
            return (
              entry.rmsLevel > DECODED_AUDIO_RMS_MIN &&
              entry.rmsLevel < DECODED_AUDIO_RMS_MAX &&
              entry.isSpeaking === true
            );
          },
          { timeout: 20_000, message: 'peer rmsLevel never entered the decoded-audio band' },
        )
        .toBe(true);

      // Corroboration only (see header comment) — active-speaker signaling
      // could set this independent of real audio, so it's not the primary check.
      const diagnostics = await app.getPageByPath('/diagnostics');
      // Case-insensitive and tolerant of spacing/colon: the diagnostics window
      // applies CSS text-transform to this label (confirmed via a real DOM
      // snapshot — the accessible text renders as "Remote speaking 1 / 1",
      // lowercase "speaking", no colon, spaced slash), which is a rendering
      // detail unrelated to what this check actually verifies.
      await expect(visibleText(diagnostics, /remote speaking:?\s*1\s*\/\s*1/i)).toBeVisible({
        timeout: 10_000,
      });

      // Negative control: once the tone stops (switches to real silent
      // frames — see livekit-tone-peer.mjs's stopTone() comment for why
      // stopping capture outright isn't equivalent), decoded RMS must decay
      // back toward silence — proves the earlier reading tracked live audio
      // rather than a stuck/cached value.
      //
      // 30s, not 15s: LiveKit's RoomEvent.ActiveSpeakersChanged hardcodes
      // rmsLevel to 1.0 for anyone the server still considers an active
      // speaker (livekit-media.ts), and observed empirically to have a
      // multi-second server-side hangover after real audio stops before that
      // clears — a real product behavior (smooths over brief pauses), not a
      // bug, but it competes with the correct decaying analyser-derived value
      // for the same participant. This is a timeout increase, not a threshold
      // change — RMS_STOP_THRESHOLD stays the same.
      tone.stopTone();
      await expect
        .poll(
          async () => {
            const entry = await readPeerVoiceStats(main);
            return entry?.rmsLevel ?? null;
          },
          { timeout: 30_000, message: 'peer rmsLevel never decayed after stopTone()' },
        )
        .toBeLessThan(RMS_STOP_THRESHOLD);
    } finally {
      if (tone) await tone.close();
      await peer.close();
      await leaveRoomIfActive(main);
    }
  },
  // Default 30s isn't enough headroom: the primary assertion alone can poll
  // up to 20s, and the negative control's ActiveSpeakersChanged-hangover
  // tolerance (see below) up to 30s more.
  { timeoutMs: 90_000 },
);
