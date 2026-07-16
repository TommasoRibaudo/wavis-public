// Media-publishing peer for audio-received.spec.mjs and
// zz-network-quality.spec.mjs. wavis-cli-test cannot fill this role — its
// `start_share` is a bare signaling message with no pixels/audio ever
// attached (clients/cli-test/src/main.rs), and it can't auth/join_voice
// into a GUI channel's voice call at all. This connects directly to
// LiveKit via @livekit/rtc-node, reusing the same media_token the backend
// hands a ws-sfu-test peer on join_voice (see waitForMediaToken in
// live-backend-helpers.mjs), and publishes a real decodable audio track so
// the GUI's analyser-based rmsLevel is genuine decoded-audio evidence, not
// a signaling-path artifact. startVideoPattern()/stopVideoPattern()
// additionally publish a real moving video track under
// TrackSource.SOURCE_SCREENSHARE for specs that need real decoded pixels.
import {
  AudioFrame,
  AudioSource,
  LocalAudioTrack,
  LocalVideoTrack,
  Room,
  TrackPublishOptions,
  TrackSource,
  VideoBufferType,
  VideoFrame,
  VideoSource,
} from '@livekit/rtc-node';

const SAMPLE_RATE = 48_000;
const CHANNELS = 1;
const FRAME_MS = 10;
const SAMPLES_PER_FRAME = (SAMPLE_RATE * FRAME_MS) / 1000;
const TONE_HZ = 440;
const AMPLITUDE = Math.round(0.8 * 32_767);
const PHASE_STEP = (2 * Math.PI * TONE_HZ) / SAMPLE_RATE;
const TWO_PI = 2 * Math.PI;

const VIDEO_WIDTH = 640;
const VIDEO_HEIGHT = 360;
const VIDEO_FPS = 15;
const VIDEO_FRAME_INTERVAL_MS = 1000 / VIDEO_FPS;

/**
 * Connects to LiveKit as a real publishing participant and immediately
 * starts pumping a 440Hz sine tone as 10ms Int16 PCM frames. Returns
 * `{ stopTone(), close() }`. `close()` does full teardown.
 *
 * `source` selects the LiveKit track source the tone publishes under
 * (default `SOURCE_MICROPHONE`, what audio-received.spec.mjs relies on).
 * screen-share-audio-not-heard-until-opened.spec.mjs passes
 * `SOURCE_SCREENSHARE_AUDIO` so the GUI routes the track through its
 * deferred screen-share-audio path instead of the plain-voice path.
 */
export async function connectTonePeer({ sfuUrl, token, source = TrackSource.SOURCE_MICROPHONE }) {
  const room = new Room();
  await room.connect(sfuUrl, token, { autoSubscribe: false, dynacast: false });

  const audioSource = new AudioSource(SAMPLE_RATE, CHANNELS);
  const track = LocalAudioTrack.createAudioTrack('tone', audioSource);
  await room.localParticipant.publishTrack(track, new TrackPublishOptions({ source }));

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
      await audioSource.captureFrame(
        new AudioFrame(data, SAMPLE_RATE, CHANNELS, SAMPLES_PER_FRAME),
      );
    }
  })();

  let videoSource = null;
  let videoTrack = null;
  let videoRunning = false;
  let videoPump = null;

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
    /**
     * Publishes a moving RGBA pattern (640x360 @ ~15fps) under
     * TrackSource.SOURCE_SCREENSHARE — the source the GUI's
     * videoReceiveStats path and share pipeline key on. The pattern
     * actually changes each frame so it encodes as real motion, not a
     * single static frame the encoder could collapse to near-zero bitrate.
     */
    async startVideoPattern() {
      if (videoSource) return;
      videoSource = new VideoSource(VIDEO_WIDTH, VIDEO_HEIGHT);
      videoTrack = LocalVideoTrack.createVideoTrack('pattern', videoSource);
      await room.localParticipant.publishTrack(
        videoTrack,
        new TrackPublishOptions({ source: TrackSource.SOURCE_SCREENSHARE }),
      );

      videoRunning = true;
      let frameIndex = 0;
      const rowBytes = VIDEO_WIDTH * 4;
      const rowTemplate = new Uint8Array(rowBytes);
      videoPump = (async () => {
        while (videoRunning) {
          const splitX = frameIndex % VIDEO_WIDTH;
          for (let x = 0; x < VIDEO_WIDTH; x++) {
            const bright = x < splitX;
            const i = x * 4;
            rowTemplate[i] = bright ? 255 : 0; // R
            rowTemplate[i + 1] = bright ? 0 : 255; // G
            rowTemplate[i + 2] = frameIndex % 256; // B
            rowTemplate[i + 3] = 255; // A
          }
          const data = new Uint8Array(rowBytes * VIDEO_HEIGHT);
          for (let y = 0; y < VIDEO_HEIGHT; y++) {
            data.set(rowTemplate, y * rowBytes);
          }
          videoSource.captureFrame(
            new VideoFrame(data, VIDEO_WIDTH, VIDEO_HEIGHT, VideoBufferType.RGBA),
          );
          frameIndex++;
          await new Promise((resolve) => setTimeout(resolve, VIDEO_FRAME_INTERVAL_MS));
        }
      })();
    },
    async stopVideoPattern() {
      if (!videoRunning) return;
      videoRunning = false;
      await videoPump;
      await videoTrack.close();
      videoSource = null;
      videoTrack = null;
      videoPump = null;
    },
    async close() {
      running = false;
      await pump;
      if (videoRunning) {
        videoRunning = false;
        await videoPump;
      }
      if (videoTrack) await videoTrack.close();
      await audioSource.close();
      await room.disconnect();
    },
  };
}
