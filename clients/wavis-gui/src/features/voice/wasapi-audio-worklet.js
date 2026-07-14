/**
 * AudioWorklet processor for WASAPI loopback audio frames.
 *
 * Receives base64-encoded PCM frames (i16 LE, 48 kHz mono, 960 samples / 20 ms)
 * from the main thread via the MessagePort, decodes them into a ring buffer,
 * and outputs float32 samples in the standard Web Audio 128-sample render quanta.
 *
 * The ring buffer primes with PRIME_SAMPLES of audio before playback starts,
 * and re-primes after any underrun, so delivery jitter (Windows sleep(20ms)
 * granularity, WebView2 IPC, main-thread jank from screen-share frame decode)
 * is absorbed as added latency instead of surfacing as silence gaps in the
 * published track. Overflow under sustained overrun is shed in one bounded
 * chunk instead of dropped sample-by-sample.
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
// Priming target: buffer this many samples before starting playback, and again
// after any underrun. ~80 ms is enough to absorb observed delivery jitter
// without being perceptible for a screen-share audio track.
const PRIME_SAMPLES = 48000 * 0.08; // 3840 samples
// Emit a stats snapshot roughly every 5s (process() runs once per 128-sample
// quantum at 48 kHz => ~375 calls/s).
const STATS_INTERVAL_QUANTA = 1875;

class WasapiAudioProcessor extends AudioWorkletProcessor {
  constructor() {
    super();
    this._ring = new Float32Array(RING_SIZE);
    this._writePos = 0;
    this._readPos = 0;
    this._count = 0; // samples available in ring
    this._primed = false;
    this._stopped = false;

    this._underruns = 0;
    this._reprimes = 0;
    this._overflowSheds = 0;
    this._framesQueued = 0;
    this._quantaSinceStats = 0;

    this.port.onmessage = (e) => {
      if (e.data === null) {
        // Poison pill — stop processing.
        this._stopped = true;
        return;
      }
      const samples = e.data; // Float32Array
      const len = samples.length;
      this._framesQueued++;

      if (this._count + len > RING_SIZE) {
        // Sustained overrun — shed the oldest samples down to PRIME_SAMPLES in
        // one bounded chunk instead of dropping one sample at a time, which
        // produced continuous crackle under overrun.
        const overflow = this._count + len - RING_SIZE;
        const toDrop = Math.min(this._count, Math.max(overflow, this._count - PRIME_SAMPLES));
        this._readPos = (this._readPos + toDrop) % RING_SIZE;
        this._count -= toDrop;
        this._overflowSheds++;
      }

      for (let i = 0; i < len; i++) {
        if (this._count >= RING_SIZE) break; // safety guard; shed above should prevent this
        this._ring[this._writePos] = samples[i];
        this._writePos = (this._writePos + 1) % RING_SIZE;
        this._count++;
      }
    };
  }

  process(_inputs, outputs) {
    if (this._stopped) return false;

    const output = outputs[0];
    if (!output || output.length === 0) {
      this._emitStatsIfDue();
      return true;
    }

    const channel = output[0]; // mono
    const len = channel.length; // typically 128

    if (!this._primed) {
      if (this._count >= PRIME_SAMPLES) {
        this._primed = true;
      } else {
        channel.fill(0);
        this._emitStatsIfDue();
        return true;
      }
    }

    for (let i = 0; i < len; i++) {
      if (this._count > 0) {
        channel[i] = this._ring[this._readPos];
        this._readPos = (this._readPos + 1) % RING_SIZE;
        this._count--;
      } else {
        // Underrun mid-quantum — fill the remainder with silence and re-prime
        // before resuming, rather than continuing to interleave silence gaps.
        channel[i] = 0;
        if (this._primed) {
          this._primed = false;
          this._underruns++;
          this._reprimes++;
        }
      }
    }

    this._emitStatsIfDue();
    return true;
  }

  _emitStatsIfDue() {
    this._quantaSinceStats++;
    if (this._quantaSinceStats < STATS_INTERVAL_QUANTA) return;
    this._quantaSinceStats = 0;
    this.port.postMessage({
      type: 'stats',
      payload: {
        underruns: this._underruns,
        reprimes: this._reprimes,
        overflowSheds: this._overflowSheds,
        framesQueued: this._framesQueued,
        occupancy: this._count,
      },
    });
  }
}

registerProcessor('wasapi-audio-processor', WasapiAudioProcessor);
