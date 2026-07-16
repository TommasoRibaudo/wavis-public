// Live-backend, real media: the belt-and-braces proof for GitHub issue #174
// ("I can hear a new stream even without opening it"). audio-received.spec.mjs
// already proves plain *voice* audio decodes and plays; this proves the
// opposite gate for *screen-share* audio — that a real ScreenShareAudio track
// arriving on the SFU is deliberately NOT played until the local user
// expresses intent, and DOES attach the moment they do.
//
// Why this is real proof, not a mocked unit test: a @livekit/rtc-node peer
// (livekit-tone-peer.mjs) publishes a real 440Hz tone under
// TrackSource.SOURCE_SCREENSHARE_AUDIO — a genuine decodable track over the
// real local LiveKit/docker stack, attributed to the peer's own signaling
// participant row (same media_token path as audio-received.spec.mjs). The
// assertions read the GUI's own unconditional attach log
// (livekit-media.ts:6655, `[mac-share-audio] attached screen share audio …`),
// which fires only from attachDesiredScreenShareAudio — reachable only after
// the public attachScreenShareAudio() adds the participant to
// screenShareAudioViewerOwners, the single gate shouldPlayScreenShareAudio
// keys off. That method is only called from the viewer-ready handshake and
// the explicit unmute path (useShareViewerWindows.ts), never on track arrival.
//
// Scenario chosen — audio-only share + explicit unmute, NOT Watch-All:
// reading the actual code (not the original handoff's summary) showed
// audio-only shares never flow through Watch-All's attach path.
// getWatchAllScope(roomState).streams contains only *video* screenShareStreams,
// so an audio-only sharer is instead force-muted/detached by a dedicated
// effect (useShareViewerWindows.ts:966-1005) — even while Watch-All is open.
// The only attach path for audio-only is the user unmuting the share
// (syncScreenShareMuted(id,false) -> attachScreenShareAudio). That makes this
// a STRONGER demonstration of #174 than "open the viewer": the audio stays
// silent through the whole render, and only an explicit unmute plays it.
//
// The negative assertion is the heart of #174. It is paired with a positive
// "the track really did arrive" check so that "no attach" can't be a false
// pass from the track never reaching the client at all. That check is the
// [diag] TrackPublished log (the peer's ScreenShareAudio *publication* reaching
// the GUI's SFU connection), NOT TrackSubscribed: the GUI deliberately
// setSubscribed(false)s the deferred track, so TrackSubscribed is intentionally
// suppressed pre-intent — confirmed live, the subscription is cancelled and the
// SDK logs a stream of "Tried to add a track for a participant, that's not
// present" until the unmute below re-subscribes it. That suppression IS the fix
// working; the unmute path (attachScreenShareAudio -> setSubscribed(true)) then
// lets TrackSubscribed fire and the audio attach.
import { test, expect } from './fixtures.mjs';
import { TrackSource } from '@livekit/rtc-node';
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
  visibleTitle,
  spawnPeer,
  joinVoiceAsPeer,
  waitForMediaToken,
} from './live-backend-helpers.mjs';
import { connectTonePeer } from './livekit-tone-peer.mjs';

// livekit-media.ts:6655 — unconditional (not behind a debug flag), so absence
// is genuine "never attached", not "debug logging was off". Distinct substring
// from the `attachScreenShareAudio called for …` debug line so the negative
// assertion can't match that instead.
const ATTACH_LOG = /\[mac-share-audio\] attached screen share audio/;
// livekit-media.ts:2700 — the peer's ScreenShareAudio *publication* reached
// the GUI's SFU connection (gated by VITE_DEBUG_SHARE_TRACK_SUBSCRIPTION, baked
// true in .env). This is the right arrival proof: the GUI deliberately
// setSubscribed(false)s the deferred track, so TrackSubscribed is intentionally
// suppressed pre-intent and is NOT a reliable "arrived" signal.
const TRACK_ARRIVED_LOG = /\[diag\] TrackPublished[^\n]*source: screen_share_audio/;

test("a peer's screen-share audio stays silent until the user unmutes it", async ({ app }) => {
  // Peer spawn (cargo run -p ws-sfu-test) + LiveKit connect + the settle
  // window for the negative assertion push this past the 30s default.
  test.setTimeout(90_000);
  await waitForBackendHealth();

  const suffix = Date.now().toString(36);
  const channelName = `e2e-ssaudio-${suffix}`;
  const { owner, channel, invite } = await seedChannelWithInvite(channelName);

  const main = app.page();
  // Capture console before anything publishes — the whole point is proving a
  // log never fired, so the listener must predate the peer's track.
  const logs = [];
  main.on('console', (msg) => logs.push(msg.text()));

  await leaveRoomIfActive(main);
  const pathname = new URL(main.url()).pathname;
  if (pathname.startsWith('/login') || pathname.startsWith('/setup')) {
    await registerAndLoginViaUi(main, { serverUrl: SERVER_URL });
  }

  await joinChannelViaUi(main, invite.code);
  await enterChannelRoom(main, channelName);
  await joinDefaultSubRoomViaUi(main);

  const peer = spawnPeer();
  let tone;
  try {
    await joinVoiceAsPeer(peer, {
      accessToken: owner.access_token,
      channelId: channel.channel_id,
      displayName: 'AudioSharePeer',
    });
    // ParticipantsPanel only renders participants inside the joined sub-room,
    // so the peer must share the GUI's sub-room for its row (and the unmute
    // button) to appear at all.
    await joinDefaultSubRoomAsPeer(peer);

    const { token, sfuUrl } = await waitForMediaToken(peer);
    // Real ScreenShareAudio track — the GUI routes this through its deferred
    // screen-share-audio path (isDeferredScreenShareAudioTrack returns true
    // for TrackSource.ScreenShareAudio), not the plain-voice attach path.
    tone = await connectTonePeer({ sfuUrl, token, source: TrackSource.SOURCE_SCREENSHARE_AUDIO });
    // The signaling half of a real audio-only share: makes the peer
    // isSharing + a confirmed audioOnlySharer, so ParticipantsPanel renders
    // the row's "unmute audio share" button (ParticipantRow.tsx:185-204).
    peer.send({ type: 'start_share', shareType: 'audio_only' });

    // The button only appears once share_started(audio_only) is processed —
    // its visibility is the GUI's own confirmation it registered the share.
    const unmuteBtn = visibleTitle(main, 'unmute audio share').first();
    await unmuteBtn.waitFor({ state: 'visible', timeout: 15_000 });

    // The track genuinely reached the client (so the negative below isn't a
    // false pass from a track that never arrived).
    await expect
      .poll(() => logs.some((l) => TRACK_ARRIVED_LOG.test(l)), {
        timeout: 15_000,
        message: 'GUI never logged TrackPublished for the peer ScreenShareAudio track',
      })
      .toBe(true);

    // CORE #174 ASSERTION: the track arrived and the row is on screen, yet
    // nothing has attached/played it. Hold a settle window so a late attach
    // would have fired, then assert it did not.
    await new Promise((resolve) => setTimeout(resolve, 4_000));
    expect(
      logs.some((l) => ATTACH_LOG.test(l)),
      'screen-share audio attached before any user intent (issue #174 regression)',
    ).toBe(false);

    // POSITIVE: an explicit unmute is real viewer intent — attach must fire
    // now, and only now (everything before this point already established no
    // attach line existed).
    const marker = logs.length;
    await unmuteBtn.click();
    await expect
      .poll(() => logs.slice(marker).some((l) => ATTACH_LOG.test(l)), {
        timeout: 10_000,
        message: 'screen-share audio never attached after the user unmuted it',
      })
      .toBe(true);
  } finally {
    if (tone) await tone.close();
    await peer.close();
    await leaveRoomIfActive(main);
  }
});
