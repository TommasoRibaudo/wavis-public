// Media-publishing peer for audio-received.spec.mjs. wavis-cli-test cannot
// fill this role — its `start_share` is a bare signaling message with no
// pixels/audio ever attached (clients/cli-test/src/main.rs), and it can't
// auth/join_voice into a GUI channel's voice call at all. This connects
// directly to LiveKit via @livekit/rtc-node, reusing the same media_token
// the backend hands a ws-sfu-test peer on join_voice (see
// waitForMediaToken in live-backend-helpers.mjs), and publishes a real
// decodable audio track so the GUI's analyser-based rmsLevel is genuine
// decoded-audio evidence, not a signaling-path artifact.
import {
  AudioFrame,
  AudioSource,
  LocalAudioTrack,
  Room,
  TrackPublishOptions,
  TrackSource,
} from '@livekit/rtc-node';

const SAMPLE_RATE = 48_000;
const CHANNELS = 1;
const FRAME_MS = 10;
const SAMPLES_PER_FRAME = (SAMPLE_RATE * FRAME_MS) / 1000;
const TONE_HZ = 440;
const AMPLITUDE = Math.round(0.8 * 32_767);
const PHASE_STEP = (2 * Math.PI * TONE_HZ) / SAMPLE_RATE;
const TWO_PI = 2 * Math.PI;

/**
 * Connects to LiveKit as a real publishing participant and immediately
 * starts pumping a 440Hz sine tone as 10ms Int16 PCM frames. Returns
 * `{ stopTone(), close() }`. `close()` does full teardown.
 */
export async function connectTonePeer({ sfuUrl, token }) {
  const room = new Room();
  await room.connect(sfuUrl, token, { autoSubscribe: false, dynacast: false });

  const source = new AudioSource(SAMPLE_RATE, CHANNELS);
  const track = LocalAudioTrack.createAudioTrack('tone', source);
  await room.localParticipant.publishTrack(
    track,
    new TrackPublishOptions({ source: TrackSource.SOURCE_MICROPHONE }),
  );

  let phase = 0;
  let running = true;
  // toneActive gates the *content* of each frame (sine vs. silence), not
  // whether frames are sent — see stopTone() below.
  let toneActive = true;
  // Awaiting captureFrame() (rather than a fire-and-forget setInterval) lets
  // AudioSource's own backpressure pace frames to real time. Without this,
  // frames queue up faster than they play out, building an audible backlog
  // (confirmed: with a setInterval + `void captureFrame()` version, rmsLevel
  // was still ~0.4 fifteen seconds after the content should have gone quiet).
  const pump = (async () => {
    while (running) {
      const data = new Int16Array(SAMPLES_PER_FRAME);
      if (toneActive) {
        for (let i = 0; i < SAMPLES_PER_FRAME; i++) {
          data[i] = Math.round(AMPLITUDE * Math.sin(phase));
          phase += PHASE_STEP;
          if (phase > TWO_PI) phase -= TWO_PI;
        }
      }
      // else: data stays zero-filled — genuine silent PCM, not "no frames".
      await source.captureFrame(new AudioFrame(data, SAMPLE_RATE, CHANNELS, SAMPLES_PER_FRAME));
    }
  })();

  return {
    /**
     * Switches to sending real silent (zeroed) PCM frames rather than
     * stopping capture outright. Confirmed necessary: when frames stopped
     * entirely, the receiver's jitter buffer / packet-loss concealment kept
     * synthesizing audio resembling the last known waveform for well past
     * 15s instead of reading true silence — genuine zero-amplitude frames
     * decode as real silence immediately, the same way a real muted mic
     * would behave (still transmitting, just silent).
     */
    stopTone() {
      toneActive = false;
    },
    async close() {
      running = false;
      await pump;
      await source.close();
      await room.disconnect();
    },
  };
}
