/**
 * AudioWorklet processor for native mic bridge frames.
 *
 * Receives Float32Array frames from the main thread via MessagePort (decoded
 * from base64 i16 LE PCM emitted by the Rust WASAPI mic capture thread),
 * buffers them in a ring buffer, and outputs 128-sample quanta to the Web
 * Audio graph.
 *
 * Data flow:
 *   Rust WASAPI mic capture (48 kHz mono, DenoiseFilter applied)
 *     → Tauri event "native_mic_frame" (base64 i16 LE)
 *     → NativeMicBridge: decode → Float32Array → postMessage here
 *     → ring buffer → process() output
 *     → MediaStreamDestination → MediaStreamTrack
 *     → LiveKit publishTrack(track, { source: Microphone })
 */

// Ring buffer: ~200 ms at 48 kHz mono.
const RING_SIZE = 48000 * 0.2; // 9600 samples

class NativeMicProcessor extends AudioWorkletProcessor {
  constructor() {
    super();
    this._ring = new Float32Array(RING_SIZE);
    this._writePos = 0;
    this._readPos = 0;
    this._count = 0;
    this._stopped = false;

    this.port.onmessage = (e) => {
      if (e.data === null) {
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
  }

  process(_inputs, outputs) {
    if (this._stopped) return false;

    const output = outputs[0];
    if (!output || output.length === 0) return true;

    const channel = output[0];
    const len = channel.length;

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

registerProcessor('native-mic-processor', NativeMicProcessor);
