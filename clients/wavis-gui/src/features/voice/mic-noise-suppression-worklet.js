// Number of consecutive "close" frames required before attenuation begins.
// Mirrors Rust GATE_CLOSE_HOLD_FRAMES=2 at 10 ms/frame (20 ms hangover).
// JS frames are 128 samples @ 48 kHz ≈ 2.67 ms → 8 frames ≈ 21 ms.
const CLOSE_HOLD_FRAMES = 8;

class WavisMicNoiseSuppressionProcessor extends AudioWorkletProcessor {
  constructor() {
    super();
    this.enabled = false;
    this.gateGain = 1;
    this.closeHoldCount = 0; // consecutive non-speech frames; gate only closes after CLOSE_HOLD_FRAMES
    this.noiseFloor = 0.004;
    this.inputEnergy = 0;
    this.outputEnergy = 0;
    this.processedSamples = 0;
    this.noiseFrames = 0;
    this.speechFrames = 0;
    this.attenuatedFrames = 0;
    this.underruns = 0;
    this.lastState = 'idle';

    this.port.onmessage = (event) => {
      const data = event.data;
      if (!data || data.type !== 'config') return;
      if (typeof data.enabled === 'boolean') {
        this.enabled = data.enabled;
      }
    };
  }

  process(inputs, outputs) {
    const inputChannel = inputs[0]?.[0];
    const outputChannel = outputs[0]?.[0];
    if (!outputChannel) return true;

    if (!inputChannel) {
      outputChannel.fill(0);
      this.underruns += 1;
      this.maybeReport(0, 0, false, false);
      return true;
    }

    let sumSq = 0;
    let peak = 0;
    for (let i = 0; i < inputChannel.length; i++) {
      const abs = Math.abs(inputChannel[i]);
      sumSq += inputChannel[i] * inputChannel[i];
      if (abs > peak) peak = abs;
    }

    const inputRms = Math.sqrt(sumSq / inputChannel.length);
    const speechThreshold = Math.max(this.noiseFloor * 2.8, 0.012);
    const peakThreshold = Math.max(this.noiseFloor * 4.5, 0.03);
    const likelySpeech = inputRms > speechThreshold || peak > peakThreshold;

    if (!this.enabled) {
      outputChannel.set(inputChannel);
      this.maybeReport(inputRms, inputRms, false, likelySpeech);
      return true;
    }

    if (!likelySpeech) {
      this.noiseFloor = clamp(this.noiseFloor * 0.98 + inputRms * 0.02, 0.0015, 0.04);
    } else {
      this.noiseFloor = clamp(this.noiseFloor * 0.999 + inputRms * 0.001, 0.0015, 0.04);
    }

    // Close-hold: only start attenuating after CLOSE_HOLD_FRAMES consecutive non-speech frames.
    // Mirrors Rust GATE_CLOSE_HOLD_FRAMES — prevents brief VAD/energy dips from clipping words.
    if (likelySpeech) {
      this.closeHoldCount = 0;
    } else {
      this.closeHoldCount += 1;
    }
    const shouldClose = !likelySpeech && this.closeHoldCount > CLOSE_HOLD_FRAMES;

    // Gate smoothing: open fast (0.15), close slow (0.027).
    // 0.027/frame × 2.67 ms/frame ≈ 100 ms to fully close — mirrors Rust GATE_ATTACK_STEP=0.1/10ms.
    const targetGate = shouldClose ? 0.08 : 1.0;
    const smoothing = shouldClose ? 0.027 : 0.15;

    // Capture gain at frame start, then advance — used to ramp across the frame.
    const startGain = this.gateGain;
    this.gateGain += (targetGate - this.gateGain) * smoothing;
    const endGain = this.gateGain;

    // Apply a linear gain ramp across the 128-sample frame (mirrors Rust apply_gate_gain_ramped).
    // Eliminates gain discontinuities at frame boundaries that cause crackle.
    // No spectral subtraction — pass samples through the gate directly.
    let outputSumSq = 0;
    const len = inputChannel.length;
    const denom = Math.max(len - 1, 1);
    for (let i = 0; i < len; i++) {
      const t = i / denom;
      const gain = startGain + (endGain - startGain) * t;
      const out = inputChannel[i] * gain;
      outputChannel[i] = out;
      outputSumSq += out * out;
    }

    const attenuated = endGain < 0.5;
    const outputRms = Math.sqrt(outputSumSq / len);
    this.maybeReport(inputRms, outputRms, attenuated, likelySpeech);
    return true;
  }

  maybeReport(inputRms, outputRms, attenuated, likelySpeech) {
    this.inputEnergy += inputRms * inputRms;
    this.outputEnergy += outputRms * outputRms;
    this.processedSamples += 128;

    if (likelySpeech) {
      this.speechFrames += 1;
    } else {
      this.noiseFrames += 1;
    }
    if (attenuated) {
      this.attenuatedFrames += 1;
    }

    const nextState = !this.enabled
      ? 'bypass'
      : likelySpeech
        ? 'speech_passed'
        : attenuated
          ? 'noise_attenuated'
          : 'noise_detected';

    if (nextState !== this.lastState) {
      this.lastState = nextState;
      this.port.postMessage({
        type: 'state',
        payload: {
          state: nextState,
          noiseFloor: round3(this.noiseFloor),
          gateGain: round3(this.gateGain),
          inputRms: round3(inputRms),
          outputRms: round3(outputRms),
        },
      });
    }

    if (this.processedSamples < sampleRate) {
      return;
    }

    const avgInputRms = Math.sqrt(this.inputEnergy / Math.max(this.processedSamples / 128, 1));
    const avgOutputRms = Math.sqrt(this.outputEnergy / Math.max(this.processedSamples / 128, 1));
    const attenuationRatio = avgInputRms > 1e-6 ? avgOutputRms / avgInputRms : 1;

    this.port.postMessage({
      type: 'stats',
      payload: {
        enabled: this.enabled,
        inputRms: round3(avgInputRms),
        outputRms: round3(avgOutputRms),
        attenuationRatio: round3(attenuationRatio),
        noiseFrames: this.noiseFrames,
        speechFrames: this.speechFrames,
        attenuatedFrames: this.attenuatedFrames,
        underruns: this.underruns,
        noiseFloor: round3(this.noiseFloor),
        gateGain: round3(this.gateGain),
      },
    });

    this.inputEnergy = 0;
    this.outputEnergy = 0;
    this.processedSamples = 0;
    this.noiseFrames = 0;
    this.speechFrames = 0;
    this.attenuatedFrames = 0;
    this.underruns = 0;
  }
}

function clamp(value, min, max) {
  return Math.min(max, Math.max(min, value));
}

function round3(value) {
  return Math.round(value * 1000) / 1000;
}

registerProcessor('wavis-mic-noise-suppression', WavisMicNoiseSuppressionProcessor);
