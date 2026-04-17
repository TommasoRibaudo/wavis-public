/**
 * AudioWorklet processor for WASAPI loopback audio frames.
 *
 * Receives base64-encoded PCM frames (i16 LE, 48 kHz mono, 960 samples / 20 ms)
 * from the main thread via the MessagePort, decodes them into a ring buffer,
 * and outputs float32 samples in the standard Web Audio 128-sample render quanta.
 *
 * Data flow:
 *   Rust WASAPI capture thread
 *     → Tauri event "wasapi_audio_frame" (base64 i16 LE)
 *     → main thread decodes + posts Float32Array to this worklet
 *     → worklet ring buffer → process() output
 *     → MediaStreamDestination → LocalTrack → LiveKit publish
 */

// Ring buffer sized for ~200 ms of audio at 48 kHz mono (enough to absorb jitter).
const RING_SIZE = 48000 * 0.2; // 9600 samples

class WasapiAudioProcessor extends AudioWorkletProcessor {
  constructor() {
    super();
    this._ring = new Float32Array(RING_SIZE);
    this._writePos = 0;
    this._readPos = 0;
    this._count = 0; // samples available in ring

    this.port.onmessage = (e) => {
      if (e.data === null) {
        // Poison pill — stop processing.
        this._stopped = true;
        return;
      }
      const samples = e.data; // Float32Array
      const len = samples.length;
      for (let i = 0; i < len; i++) {
        if (this._count >= RING_SIZE) {
          // Overflow — drop oldest sample.
          this._readPos = (this._readPos + 1) % RING_SIZE;
          this._count--;
        }
        this._ring[this._writePos] = samples[i];
        this._writePos = (this._writePos + 1) % RING_SIZE;
        this._count++;
      }
    };

    this._stopped = false;
  }

  process(_inputs, outputs) {
    if (this._stopped) return false;

    const output = outputs[0];
    if (!output || output.length === 0) return true;

    const channel = output[0]; // mono
    const len = channel.length; // typically 128

    for (let i = 0; i < len; i++) {
      if (this._count > 0) {
        channel[i] = this._ring[this._readPos];
        this._readPos = (this._readPos + 1) % RING_SIZE;
        this._count--;
      } else {
        channel[i] = 0; // underrun — output silence
      }
    }

    return true;
  }
}

registerProcessor('wasapi-audio-processor', WasapiAudioProcessor);
