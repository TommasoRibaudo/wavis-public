/**
 * Wavis Native Media Transport
 *
 * Bridges to the Rust-side RealLiveKitConnection via Tauri IPC commands
 * and events. Used on platforms where the webview lacks WebRTC support
 * (Linux/WebKitGTK). Implements the same callback interface as
 * LiveKitModule so voice-room.ts can swap transparently.
 */

import { invoke } from '@tauri-apps/api/core';
import { listen, type UnlistenFn } from '@tauri-apps/api/event';
import type { MediaCallbacks } from './livekit-media';
import { startSending } from '@features/screen-share/screen-share-viewer';
import { getDenoiseEnabled, inputVolumeToGain } from '@features/settings/settings-store';

const LOG = '[wavis:native-media]';

/* ─── OffscreenCanvas.captureStream() runtime detection ─────────── */

/**
 * Detect whether OffscreenCanvas + captureStream() are available.
 * Requires WebKitGTK ≥ 2.44 (GNOME 46 / Ubuntu 24.04+).
 */
const hasOffscreenCaptureStream: boolean = (() => {
  try {
    const test = new OffscreenCanvas(1, 1);
    return typeof (test as unknown as { captureStream: unknown }).captureStream === 'function';
  } catch {
    return false;
  }
})();

/* ─── Tauri Event Payload Types ─────────────────────────────────── */

interface ScreenShareFramePayload {
  identity: string;
  frame: string; // base64-encoded JPEG
}

interface ScreenShareEndedPayload {
  identity: string;
}

/* ─── Active Share Tracking ─────────────────────────────────────── */

interface ActiveShareEntry {
  stream: MediaStream | null;
  canvas: HTMLCanvasElement | null;
  offscreenCanvas?: OffscreenCanvas;
  ctx?: OffscreenCanvasRenderingContext2D | CanvasRenderingContext2D | null;
  startedAtMs: number;
}

export type ActiveShareInfo = {
  identity: string;
  stream: MediaStream | null;
  canvas: HTMLCanvasElement | null;
  startedAtMs: number;
};

/* ─── Event Types (must match Rust MediaEvent serde tags) ───────── */

interface MediaEventConnected { type: 'connected' }
interface MediaEventFailed { type: 'failed'; reason: string }
interface MediaEventDisconnected { type: 'disconnected' }
interface MediaEventAudioLevels {
  type: 'audio_levels';
  levels: Array<{ identity: string; rms_level: number; is_speaking: boolean }>;
}
interface MediaEventLocalAudioLevel {
  type: 'local_audio_level';
  rms_level: number;
  is_speaking: boolean;
}
interface MediaEventStats {
  type: 'stats';
  rtt_ms: number;
  packet_loss_percent: number;
  jitter_ms: number;
}

interface MediaEventScreenShareStats {
  type: 'screen_share_stats';
  bitrate_kbps: number;
  fps: number;
  quality_limitation_reason: string;
  packet_loss_percent: number;
  frame_width: number;
  frame_height: number;
  pli_count: number;
  nack_count: number;
  available_bandwidth_kbps: number;
}

type MediaEventPayload =
  | MediaEventConnected
  | MediaEventFailed
  | MediaEventDisconnected
  | MediaEventAudioLevels
  | MediaEventLocalAudioLevel
  | MediaEventStats
  | MediaEventScreenShareStats;

interface NativeAudioDevice {
  id: string;
  name: string;
  kind: 'input' | 'output';
  is_default: boolean;
}

/* ═══ NativeMediaModule ═════════════════════════════════════════════ */

export class NativeMediaModule {
  private callbacks: MediaCallbacks;
  private unlistenMedia: UnlistenFn | null = null;
  private unlistenFrame: UnlistenFn | null = null;
  private unlistenEnded: UnlistenFn | null = null;
  private disposed = false;
  private activeShares = new Map<string, ActiveShareEntry>();

  constructor(callbacks: MediaCallbacks) {
    this.callbacks = callbacks;
    console.log(LOG, 'created (native Rust path)',
      hasOffscreenCaptureStream ? '(OffscreenCanvas+captureStream available)' : '(canvas fallback)');
  }

  /** Connect to LiveKit SFU via the Rust-side RealLiveKitConnection. */
  async connect(sfuUrl: string, token: string): Promise<void> {
    // 1. Subscribe to Rust-side media events before connecting
    this.unlistenMedia = await listen<MediaEventPayload>('media-event', (event) => {
      if (this.disposed) return;
      const payload = event.payload;

      switch (payload.type) {
        case 'connected':
          this.callbacks.onMediaConnected();
          break;

        case 'failed':
          this.callbacks.onMediaFailed(payload.reason);
          break;

        case 'disconnected':
          this.callbacks.onMediaDisconnected();
          break;

        case 'audio_levels': {
          const levels = new Map<string, { isSpeaking: boolean; rmsLevel: number }>();
          for (const entry of payload.levels) {
            levels.set(entry.identity, {
              isSpeaking: entry.is_speaking,
              rmsLevel: entry.rms_level,
            });
          }
          this.callbacks.onAudioLevels(levels);
          break;
        }

        case 'local_audio_level':
          this.callbacks.onLocalAudioLevel(payload.rms_level);
          break;

        case 'stats':
          // TODO: jitter buffer, concealment, candidate type, and bandwidth are
          // not available on the native webrtc-rs path — always reported as zero/unknown.
          this.callbacks.onConnectionQuality({
            rttMs: payload.rtt_ms,
            packetLossPercent: payload.packet_loss_percent,
            jitterMs: payload.jitter_ms,
            jitterBufferDelayMs: 0,
            concealmentEventsPerInterval: 0,
            candidateType: 'unknown',
            availableBandwidthKbps: 0,
          });
          break;

        case 'screen_share_stats':
          this.callbacks.onShareStats?.({
            bitrateKbps: payload.bitrate_kbps,
            fps: payload.fps,
            qualityLimitationReason: payload.quality_limitation_reason,
            packetLossPercent: payload.packet_loss_percent,
            frameWidth: payload.frame_width,
            frameHeight: payload.frame_height,
            pliCount: payload.pli_count,
            nackCount: payload.nack_count,
            availableBandwidthKbps: payload.available_bandwidth_kbps,
          });
          break;
      }
    });

    // 2. Subscribe to remote screen share frame events
    this.unlistenFrame = await listen<ScreenShareFramePayload>('screen_share_frame', (event) => {
      if (this.disposed) return;
      this.handleScreenShareFrame(event.payload);
    });

    // 3. Subscribe to remote screen share ended events
    this.unlistenEnded = await listen<ScreenShareEndedPayload>('screen_share_ended', (event) => {
      if (this.disposed) return;
      this.handleScreenShareEnded(event.payload.identity);
    });

    // 4. Tell Rust to connect
    try {
      const denoiseEnabled = await getDenoiseEnabled();
      await invoke('media_connect', { url: sfuUrl, token, denoiseEnabled });
    } catch (err) {
      this.callbacks.onMediaFailed(
        err instanceof Error ? err.message : String(err),
      );
    }
  }

  /** Disconnect from LiveKit SFU. Idempotent. */
  disconnect(): void {
    if (this.disposed) return;
    this.disposed = true;

    // Clean up all event listeners
    if (this.unlistenMedia) { this.unlistenMedia(); this.unlistenMedia = null; }
    if (this.unlistenFrame) { this.unlistenFrame(); this.unlistenFrame = null; }
    if (this.unlistenEnded) { this.unlistenEnded(); this.unlistenEnded = null; }

    // Clean up all active shares
    for (const identity of [...this.activeShares.keys()]) {
      this.disposeShareEntry(identity);
    }

    invoke('media_disconnect').catch((err) => {
      console.warn(LOG, 'disconnect error:', err);
    });

    console.log(LOG, 'disconnected');
  }

  /** Enable/disable local microphone. */
  async setMicEnabled(enabled: boolean): Promise<void> {
    await invoke('media_set_mic_enabled', { enabled });
  }

  /** Set per-participant volume (0–100). */
  setParticipantVolume(participantIdentity: string, volume: number): void {
    invoke('media_set_participant_volume', {
      id: participantIdentity,
      level: Math.max(0, Math.min(100, Math.round(volume))),
    }).catch(() => {});
  }

  /** Set per-participant screen share audio volume (0–100). */
  setScreenShareAudioVolume(participantIdentity: string, volume: number): void {
    invoke('media_set_screen_share_audio_volume', {
      id: participantIdentity,
      level: Math.max(0, Math.min(100, Math.round(volume))),
    }).catch(() => {});
  }

  /** Allow a participant's screen share audio into the native Rust mix. */
  attachScreenShareAudio(participantIdentity: string): void {
    invoke('media_attach_screen_share_audio', { id: participantIdentity }).catch(() => {});
  }

  /** Remove a participant's screen share audio from the native Rust mix. */
  detachScreenShareAudio(participantIdentity: string): void {
    invoke('media_detach_screen_share_audio', { id: participantIdentity }).catch(() => {});
  }

  /** Set master volume (0–100). */
  setMasterVolume(volume: number): void {
    invoke('media_set_master_volume', {
      level: Math.max(0, Math.min(100, Math.round(volume))),
    }).catch(() => {});
  }

  /**
   * Start screen share via Rust-side capture pipeline.
   * Three-way result: true=started, false=user cancelled, reject=failed.
   */
  async startScreenShare(): Promise<boolean> {
    // invoke() resolves with Ok value, rejects on Err
    // Ok(true) → started, Ok(false) → user cancelled, Err(msg) → reject
    console.log(LOG, 'screen_share_start: invoking IPC command');
    try {
      const result = await invoke<boolean>('screen_share_start');
      console.log(LOG, 'screen_share_start: IPC returned', result);
      return result;
    } catch (err) {
      console.error(LOG, 'screen_share_start: IPC error', err);
      throw err;
    }
  }

  /** Stop screen share — stops capture and unpublishes video track. */
  async stopScreenShare(): Promise<void> {
    await invoke('screen_share_stop');
  }

  /**
   * Apply a quality preset to the native Rust capture pipeline.
   * Unlike the JS SDK path, this routes to the Rust-side ScreenShareConfig
   * which adjusts resolution cap, FPS throttle, and JPEG viewer quality
   * at runtime without restarting the capture.
   */
  async setScreenShareQuality(quality: 'low' | 'high' | 'max'): Promise<void> {
    try {
      await invoke('media_set_screen_share_quality', { quality });
      console.log(LOG, `screen share quality set: ${quality}`);
    } catch (err) {
      console.warn(LOG, 'setScreenShareQuality failed:', err);
    }
  }

  /** Device listing delegates to Rust CPAL (not LiveKit JS SDK). */
  async listDevices(): Promise<{ inputs: MediaDeviceInfo[]; outputs: MediaDeviceInfo[] }> {
    const devices = await invoke<NativeAudioDevice[]>('list_audio_devices');

    const toMediaDeviceInfo = (device: NativeAudioDevice): MediaDeviceInfo => ({
      deviceId: device.id,
      groupId: '',
      kind: device.kind === 'input' ? 'audioinput' : 'audiooutput',
      label: device.name,
      toJSON: () => ({
        deviceId: device.id,
        groupId: '',
        kind: device.kind === 'input' ? 'audioinput' : 'audiooutput',
        label: device.name,
      }),
    } as MediaDeviceInfo);

    return {
      inputs: devices.filter((device) => device.kind === 'input').map(toMediaDeviceInfo),
      outputs: devices.filter((device) => device.kind === 'output').map(toMediaDeviceInfo),
    };
  }

  async setInputDevice(deviceId: string): Promise<void> {
    await invoke('set_audio_device', { deviceId, kind: 'input' });
  }

  async setOutputDevice(deviceId: string): Promise<void> {
    await invoke('set_audio_device', { deviceId, kind: 'output' });
  }

  async setInputVolume(volume: number): Promise<void> {
    await invoke('set_input_gain', { gain: inputVolumeToGain(volume) });
  }

  /** Return active remote screen shares. */
  getActiveScreenShares(): ActiveShareInfo[] {
    const result: ActiveShareInfo[] = [];
    for (const [identity, entry] of this.activeShares) {
      result.push({
        identity,
        stream: entry.stream,
        canvas: entry.canvas,
        startedAtMs: entry.startedAtMs,
      });
    }
    return result;
  }

  /** Whether the module is currently connected. */
  get isConnected(): boolean {
    return !this.disposed;
  }

  /* ─── Private: Screen Share Frame Handling ───────────────────── */

  /**
   * Handle an incoming screen_share_frame event from Rust.
   * Primary path (OffscreenCanvas): createImageBitmap → drawImage → requestFrame
   * Fallback path (visible canvas): Image → drawImage
   */
  private handleScreenShareFrame(payload: ScreenShareFramePayload): void {
    const { identity, frame } = payload;

    if (hasOffscreenCaptureStream) {
      this.handleFramePrimary(identity, frame);
    } else {
      this.handleFrameFallback(identity, frame);
    }
  }

  /**
   * Primary path — OffscreenCanvas + captureStream (WebKitGTK ≥ 2.44).
   * Creates a synthetic MediaStream and feeds into the loopback bridge.
   */
  private handleFramePrimary(identity: string, frameData: string): void {
    const binary = atob(frameData);
    const bytes = new Uint8Array(binary.length);
    for (let i = 0; i < binary.length; i++) bytes[i] = binary.charCodeAt(i);
    const blob = new Blob([bytes], { type: 'image/jpeg' });

    createImageBitmap(blob).then((bitmap) => {
      let entry = this.activeShares.get(identity);

      if (!entry) {
        // First frame for this identity — create OffscreenCanvas + stream
        const offscreenCanvas = new OffscreenCanvas(bitmap.width, bitmap.height);
        const ctx = offscreenCanvas.getContext('2d');
        const stream = (offscreenCanvas as unknown as { captureStream(fps: number): MediaStream }).captureStream(0);

        entry = {
          stream,
          canvas: null,
          offscreenCanvas,
          ctx,
          startedAtMs: Date.now(),
        };
        this.activeShares.set(identity, entry);

        console.log(LOG, `screen share subscribed (primary path): ${identity}`);
        this.callbacks.onScreenShareSubscribed(identity, stream);

        // Feed into loopback bridge for ScreenShareWindow rendering
        startSending(identity, 'watch-all', stream).catch((err) => {
          console.warn(LOG, `loopback bridge start failed for ${identity}:`, err);
        });
      }

      // Resize canvas if frame dimensions changed
      if (entry.offscreenCanvas &&
          (entry.offscreenCanvas.width !== bitmap.width || entry.offscreenCanvas.height !== bitmap.height)) {
        entry.offscreenCanvas.width = bitmap.width;
        entry.offscreenCanvas.height = bitmap.height;
      }

      // Paint frame and push to stream
      if (entry.ctx) {
        entry.ctx.drawImage(bitmap, 0, 0);
        const tracks = entry.stream?.getVideoTracks();
        if (tracks && tracks.length > 0) {
          (tracks[0] as unknown as { requestFrame(): void }).requestFrame();
        }
      }
      bitmap.close();
    }).catch((err) => {
      console.warn(LOG, `frame decode failed for ${identity}:`, err);
    });
  }

  /**
   * Fallback path — visible <canvas> element (WebKitGTK < 2.44).
   * No MediaStream — canvas is rendered directly in ScreenShareWindow.
   */
  private handleFrameFallback(identity: string, frameData: string): void {
    let entry = this.activeShares.get(identity);

    if (!entry) {
      // First frame — create visible canvas
      const canvas = document.createElement('canvas');
      const ctx = canvas.getContext('2d');

      entry = {
        stream: null,
        canvas,
        ctx,
        startedAtMs: Date.now(),
      };
      this.activeShares.set(identity, entry);

      console.log(LOG, `screen share subscribed (canvas fallback): ${identity}`);
      // Pass null stream — ScreenShareWindow will use the canvas directly
      this.callbacks.onScreenShareSubscribed(identity, null as unknown as MediaStream);
    }

    // Decode base64 JPEG and paint onto canvas
    const img = new Image();
    img.onload = () => {
      if (!entry) return;
      // Resize canvas to match frame dimensions
      if (entry.canvas && (entry.canvas.width !== img.width || entry.canvas.height !== img.height)) {
        entry.canvas.width = img.width;
        entry.canvas.height = img.height;
      }
      if (entry.ctx) {
        (entry.ctx as CanvasRenderingContext2D).drawImage(img, 0, 0);
      }
    };
    img.src = `data:image/jpeg;base64,${frameData}`;
  }

  /** Handle screen_share_ended event — clean up and notify. */
  private handleScreenShareEnded(identity: string): void {
    console.log(LOG, `screen share ended: ${identity}`);
    this.disposeShareEntry(identity);
    this.callbacks.onScreenShareUnsubscribed(identity);
  }

  /** Dispose resources for a single share entry. */
  private disposeShareEntry(identity: string): void {
    const entry = this.activeShares.get(identity);
    if (!entry) return;

    // Stop all tracks on the synthetic stream
    if (entry.stream) {
      for (const track of entry.stream.getTracks()) {
        track.stop();
      }
    }

    // Remove visible canvas from DOM if it was appended
    if (entry.canvas && entry.canvas.parentNode) {
      entry.canvas.parentNode.removeChild(entry.canvas);
    }

    this.activeShares.delete(identity);
  }
}
