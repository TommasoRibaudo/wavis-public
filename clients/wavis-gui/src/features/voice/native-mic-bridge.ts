/**
 * NativeMicBridge
 *
 * Bridges the Rust WASAPI mic capture (with DenoiseFilter) to a Web Audio
 * MediaStreamTrack that LiveKit can publish as the local microphone track.
 *
 * Architecture:
 *   Rust native_mic_frame event (base64 i16 LE PCM, 48 kHz mono)
 *     → decode → Float32Array
 *     → AudioWorklet ring buffer (native-mic-processor)
 *     → MediaStreamAudioDestinationNode
 *     → MediaStreamTrack  ← returned by start()
 *
 * This class is instantiated by LiveKitModule.connect() on Windows when
 * denoiseEnabled=true. It manages the full lifecycle: Tauri event listener,
 * AudioContext, worklet, and Rust command invocations.
 */

import { listen } from '@tauri-apps/api/event';
import { invoke } from '@tauri-apps/api/core';

const LOG = '[wavis:native-mic-bridge]';

export class NativeMicBridge {
  private unlisten: (() => void) | null = null;
  private workletNode: AudioWorkletNode | null = null;
  private destNode: MediaStreamAudioDestinationNode | null = null;
  private audioCtx: AudioContext | null = null;

  /**
   * Start the native mic bridge.
   *
   * Creates an AudioContext, loads the native-mic-processor worklet,
   * registers the Tauri event listener, and starts the Rust WASAPI capture.
   * Returns the MediaStreamTrack to publish via LiveKit.
   */
  async start(denoiseEnabled: boolean, deviceId?: string): Promise<MediaStreamTrack> {
    // Create AudioContext at 48 kHz to match the Rust capture format.
    this.audioCtx = new AudioContext({ sampleRate: 48_000 });
    const ctx = this.audioCtx;

    if (ctx.state === 'suspended') {
      await ctx.resume().catch((e) =>
        console.warn(LOG, 'AudioContext resume failed:', e),
      );
    }

    // Load the worklet processor.
    const workletUrl = new URL('./native-mic-worklet.js', import.meta.url).href;
    await ctx.audioWorklet.addModule(workletUrl);

    // Create worklet node (mono output).
    this.workletNode = new AudioWorkletNode(ctx, 'native-mic-processor', {
      outputChannelCount: [1],
    });

    // Route worklet → MediaStream destination.
    this.destNode = ctx.createMediaStreamDestination();
    this.workletNode.connect(this.destNode);

    const track = this.destNode.stream.getAudioTracks()[0];
    if (!track) throw new Error('no audio track from native mic worklet destination');

    // Register Tauri event listener before starting Rust capture to avoid
    // dropping early frames.
    this.unlisten = await listen<string>('native_mic_frame', (event) => {
      if (!this.workletNode) return;
      const float32 = decodeNativeMicFrame(event.payload);
      this.workletNode.port.postMessage(float32);
    });

    // Start the Rust WASAPI capture.
    await invoke('native_mic_start', {
      denoiseEnabled,
      deviceId: deviceId ?? null,
    });

    console.log(LOG, 'started (denoise=%s)', denoiseEnabled);
    return track;
  }

  /** Stop the bridge: unregister Tauri listener, stop Rust capture, close AudioContext. */
  async stop(): Promise<void> {
    // Unregister listener synchronously before any awaits.
    this.unlisten?.();
    this.unlisten = null;

    // Poison-pill the worklet ring buffer so it outputs silence.
    this.workletNode?.port.postMessage(null);
    this.workletNode?.disconnect();
    this.workletNode = null;
    this.destNode = null;

    try {
      await invoke('native_mic_stop');
    } catch (e) {
      console.warn(LOG, 'native_mic_stop invoke failed:', e);
    }

    if (this.audioCtx) {
      this.audioCtx.close().catch(() => {});
      this.audioCtx = null;
    }

    console.log(LOG, 'stopped');
  }

  /** Toggle noise suppression on the running Rust capture session. */
  async setDenoiseEnabled(enabled: boolean): Promise<void> {
    await invoke('native_mic_set_denoise_enabled', { enabled });
  }

  /** Switch to a different input device (triggers a Rust-side restart). */
  async setInputDevice(deviceId: string): Promise<void> {
    await invoke('native_mic_set_input_device', { deviceId });
  }
}

/**
 * Decode a base64-encoded i16 LE PCM payload into a Float32Array.
 * The Rust capture loop emits 960 i16 samples (1920 bytes) per frame.
 */
function decodeNativeMicFrame(b64: string): Float32Array {
  const binary = atob(b64);
  const bytes = new Uint8Array(binary.length);
  for (let i = 0; i < binary.length; i++) {
    bytes[i] = binary.charCodeAt(i);
  }
  const int16 = new Int16Array(bytes.buffer);
  const float32 = new Float32Array(int16.length);
  for (let i = 0; i < int16.length; i++) {
    float32[i] = int16[i] / 32767;
  }
  return float32;
}
