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
import type { CameraQuality } from './camera-types';
import {
  getAudioInputDevice,
  getAudioOutputDevice,
  getDenoiseEnabled,
  inputVolumeToGain,
  setStoreValue,
  STORE_KEYS,
} from '@features/settings/settings-store';

const LOG = '[wavis:native-media]';
type RemoteShareType = 'screen_audio' | 'window' | 'audio_only' | 'browser';
type AudioDeviceKind = 'input' | 'output';

/* ─── Tauri Event Payload Types ─────────────────────────────────── */

interface ScreenShareFramePayload {
  identity: string;
}

interface ScreenShareAvailablePayload {
  identity: string;
}

interface ScreenShareEndedPayload {
  identity: string;
}

interface ScreenShareAudioPayload {
  identity: string;
}

interface CameraAvailablePayload {
  identity: string;
}

interface CameraEndedPayload {
  identity: string;
}

interface PolledCameraFrame {
  identity: string;
  frame: string;
  width: number;
  height: number;
  seq: number;
}

/* ─── Active Share Tracking ─────────────────────────────────────── */

interface ActiveShareEntry {
  stream: MediaStream | null;
  canvas: HTMLCanvasElement | null;
  startedAtMs: number;
}

interface ActiveCameraEntry {
  stream: MediaStream;
  canvas: HTMLCanvasElement;
  context: CanvasRenderingContext2D;
  pollInterval: ReturnType<typeof setInterval> | null;
  lastSeq: number | null;
}

export type ActiveShareInfo = {
  identity: string;
  stream: MediaStream | null;
  canvas: HTMLCanvasElement | null;
  startedAtMs: number;
};

/* ─── Event Types (must match Rust MediaEvent serde tags) ───────── */

interface MediaEventConnected {
  type: 'connected';
}
interface MediaEventFailed {
  type: 'failed';
  reason: string;
}
interface MediaEventDisconnected {
  type: 'disconnected';
}
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
  private unlistenAvailable: UnlistenFn | null = null;
  private unlistenEnded: UnlistenFn | null = null;
  private unlistenAudioAvailable: UnlistenFn | null = null;
  private unlistenAudioEnded: UnlistenFn | null = null;
  private unlistenCameraAvailable: UnlistenFn | null = null;
  private unlistenCameraEnded: UnlistenFn | null = null;
  private disposed = false;
  private activeShares = new Map<string, ActiveShareEntry>();
  private activeCameras = new Map<string, ActiveCameraEntry>();
  private localCamera: ActiveCameraEntry | null = null;
  private localCameraTrack: MediaStreamTrack | null = null;
  private currentScreenShareQuality: 'low' | 'high' | 'max' = 'high';
  private remoteShareTypes = new Map<string, RemoteShareType | undefined>();
  private audioOnlySharers = new Set<string>();
  private inferredAudioOnlySharers = new Set<string>();

  constructor(callbacks: MediaCallbacks) {
    this.callbacks = callbacks;
    console.log(LOG, 'created (native Rust path)');
  }

  private async applySavedAudioDevice(
    deviceId: string | null,
    kind: AudioDeviceKind,
  ): Promise<void> {
    if (!deviceId) return;

    try {
      await invoke('set_audio_device', { deviceId, kind });
    } catch (err) {
      const reason = err instanceof Error ? err.message : String(err);
      console.warn(
        LOG,
        `saved ${kind} device unavailable; falling back to system default:`,
        reason,
      );
      this.callbacks.onSystemEvent?.(`${kind} device unavailable, using system default`);
      await setStoreValue(
        kind === 'input' ? STORE_KEYS.audioInputDevice : STORE_KEYS.audioOutputDevice,
        '',
      ).catch(() => {});
    }
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

    // 2. Subscribe to remote screen share frame events for backward compatibility.
    // The Linux native viewer path renders pixels outside WebKit; this event
    // only marks the share as available in the room UI.
    this.unlistenFrame = await listen<ScreenShareFramePayload>('screen_share_frame', (event) => {
      if (this.disposed) return;
      this.handleScreenShareFrame(event.payload);
    });

    // 3. Subscribe to lightweight remote screen share availability events.
    // Linux stores frames in Rust and the native GTK viewer renders them.
    this.unlistenAvailable = await listen<ScreenShareAvailablePayload>(
      'screen_share_available',
      (event) => {
        if (this.disposed) return;
        this.handleScreenShareAvailable(event.payload.identity);
      },
    );

    // 4. Subscribe to remote screen share ended events
    this.unlistenEnded = await listen<ScreenShareEndedPayload>('screen_share_ended', (event) => {
      if (this.disposed) return;
      this.handleScreenShareEnded(event.payload.identity);
    });

    // 5. Subscribe to remote screen-share audio availability events. The Rust
    // native path can receive ScreenshareAudio without any video frames, so
    // this is the Linux/WebKit equivalent of LiveKitModule's audio-only
    // publication inference.
    this.unlistenAudioAvailable = await listen<ScreenShareAudioPayload>(
      'screen_share_audio_available',
      (event) => {
        if (this.disposed) return;
        this.handleScreenShareAudioAvailable(event.payload.identity);
      },
    );

    this.unlistenAudioEnded = await listen<ScreenShareAudioPayload>(
      'screen_share_audio_ended',
      (event) => {
        if (this.disposed) return;
        this.handleScreenShareAudioEnded(event.payload.identity);
      },
    );

    // 6. Subscribe to remote camera availability events. The Rust native
    // LiveKit path decodes camera frames, and this module paints them into a
    // canvas-backed MediaStreamTrack so the existing VideoTile UI can render.
    this.unlistenCameraAvailable = await listen<CameraAvailablePayload>(
      'camera_available',
      (event) => {
        if (this.disposed) return;
        this.handleCameraAvailable(event.payload.identity);
      },
    );

    this.unlistenCameraEnded = await listen<CameraEndedPayload>('camera_ended', (event) => {
      if (this.disposed) return;
      this.handleCameraEnded(event.payload.identity);
    });

    // 7. Tell Rust which persisted devices to open before it starts CPAL streams.
    try {
      const denoiseEnabled = await getDenoiseEnabled();
      const [inputDeviceId, outputDeviceId] = await Promise.all([
        getAudioInputDevice().catch(() => null),
        getAudioOutputDevice().catch(() => null),
      ]);
      await this.applySavedAudioDevice(inputDeviceId, 'input');
      await this.applySavedAudioDevice(outputDeviceId, 'output');
      await invoke('media_connect', { url: sfuUrl, token, denoiseEnabled });
    } catch (err) {
      this.callbacks.onMediaFailed(err instanceof Error ? err.message : String(err));
    }
  }

  /** Disconnect from LiveKit SFU. Idempotent. */
  disconnect(): void {
    if (this.disposed) return;
    this.disposed = true;

    // Clean up all event listeners
    if (this.unlistenMedia) {
      this.unlistenMedia();
      this.unlistenMedia = null;
    }
    if (this.unlistenFrame) {
      this.unlistenFrame();
      this.unlistenFrame = null;
    }
    if (this.unlistenAvailable) {
      this.unlistenAvailable();
      this.unlistenAvailable = null;
    }
    if (this.unlistenEnded) {
      this.unlistenEnded();
      this.unlistenEnded = null;
    }
    if (this.unlistenAudioAvailable) {
      this.unlistenAudioAvailable();
      this.unlistenAudioAvailable = null;
    }
    if (this.unlistenAudioEnded) {
      this.unlistenAudioEnded();
      this.unlistenAudioEnded = null;
    }
    if (this.unlistenCameraAvailable) {
      this.unlistenCameraAvailable();
      this.unlistenCameraAvailable = null;
    }
    if (this.unlistenCameraEnded) {
      this.unlistenCameraEnded();
      this.unlistenCameraEnded = null;
    }

    // Clean up all active shares
    for (const identity of [...this.activeShares.keys()]) {
      this.disposeShareEntry(identity);
    }
    for (const identity of [...this.activeCameras.keys()]) {
      this.disposeCameraEntry(identity);
    }
    this.disposeLocalCameraEntry();

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

  setParticipantPassthrough(participantIdentity: string, enabled: boolean): void {
    invoke('media_set_participant_passthrough', {
      id: participantIdentity,
      enabled,
    }).catch(() => {});
  }

  setPassthroughFilterSettings(settings: { enabled: boolean; strength: number }): void {
    invoke('media_set_passthrough_filter_settings', {
      enabled: Boolean(settings.enabled),
      strength: Math.max(0, Math.min(100, Math.round(settings.strength))),
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

  /** Keep native Linux audio-only shares in sync with signaling metadata. */
  setRemoteShareType(participantIdentity: string, shareType?: RemoteShareType): void {
    this.applyRemoteShareType(participantIdentity, shareType, false);
  }

  private applyRemoteShareType(
    participantIdentity: string,
    shareType: RemoteShareType | undefined,
    inferred: boolean,
  ): void {
    if (inferred) {
      this.inferredAudioOnlySharers.add(participantIdentity);
    } else {
      this.inferredAudioOnlySharers.delete(participantIdentity);
    }
    this.remoteShareTypes.set(participantIdentity, shareType);
    if (shareType === 'audio_only') {
      if (!this.audioOnlySharers.has(participantIdentity)) {
        this.audioOnlySharers.add(participantIdentity);
        this.callbacks.onAudioOnlySharerAdded?.(participantIdentity);
      }
      return;
    }

    if (this.audioOnlySharers.delete(participantIdentity)) {
      this.callbacks.onAudioOnlySharerRemoved?.(participantIdentity);
      this.detachScreenShareAudio(participantIdentity);
    }
  }

  /** Remove stale remote share metadata when signaling says the share ended. */
  clearRemoteShareType(participantIdentity: string): void {
    this.remoteShareTypes.delete(participantIdentity);
    this.inferredAudioOnlySharers.delete(participantIdentity);
    if (this.audioOnlySharers.delete(participantIdentity)) {
      this.callbacks.onAudioOnlySharerRemoved?.(participantIdentity);
      this.detachScreenShareAudio(participantIdentity);
    }
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
      await invoke('media_set_screen_share_quality', { quality: this.currentScreenShareQuality });
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
    this.currentScreenShareQuality = quality;
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
    });

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

  async publishCamera(opts: {
    deviceId: string | null;
    quality: CameraQuality;
  }): Promise<{ trackId: string }> {
    const entry = this.createCameraCanvasEntry('local');
    if (!entry) {
      throw { kind: 'device_unavailable' };
    }

    const track = entry.stream.getVideoTracks()[0] ?? null;
    if (!track) {
      this.disposeCameraCanvasEntry(entry);
      throw { kind: 'device_unavailable' };
    }

    try {
      await invoke('media_camera_start', {
        deviceId: opts.deviceId,
        width: opts.quality.width,
        height: opts.quality.height,
        fps: opts.quality.maxFps,
      });
    } catch (err) {
      this.disposeCameraCanvasEntry(entry);
      throw this.classifyNativeCameraError(err);
    }

    this.localCamera = entry;
    this.localCameraTrack = track;
    entry.pollInterval = setInterval(() => {
      void this.pollLocalCameraFrame();
    }, 66);
    void this.pollLocalCameraFrame();
    return { trackId: 'native-linux-camera' };
  }

  async unpublishCamera(): Promise<void> {
    await invoke('media_camera_stop').catch(() => {});
    this.disposeLocalCameraEntry();
  }

  getLocalCameraTrack(): MediaStreamTrack | null {
    return this.localCameraTrack;
  }

  applyRemoteCameraVisibility(_visibleParticipantIds: ReadonlySet<string>): void {
    // Native Rust LiveKit subscribes to remote tracks itself. Visibility is
    // enforced in voice-room by deciding which canvas-backed tracks are exposed.
  }

  async setCameraQuality(_quality: CameraQuality): Promise<void> {
    // The ffmpeg/v4l2 native path is fixed at publish time for now.
  }

  async replaceCameraDevice(deviceId: string | null): Promise<{ trackId: string }> {
    await this.unpublishCamera();
    return this.publishCamera({
      deviceId,
      quality: {
        tier: 'low',
        width: 320,
        height: 240,
        maxFps: 15,
        maxBitrate: 120_000,
        codec: 'vp8',
      },
    });
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

  private handleScreenShareFrame(payload: ScreenShareFramePayload): void {
    this.handleScreenShareAvailable(payload.identity);
  }

  private handleScreenShareAvailable(identity: string): void {
    if (this.activeShares.has(identity)) return;

    if (this.inferredAudioOnlySharers.has(identity)) {
      this.inferredAudioOnlySharers.delete(identity);
      this.setRemoteShareType(identity, undefined);
    }

    const entry: ActiveShareEntry = {
      stream: null,
      canvas: null,
      startedAtMs: Date.now(),
    };
    this.activeShares.set(identity, entry);

    console.log(LOG, `screen share available (native viewer path): ${identity}`);
    this.callbacks.onScreenShareSubscribed(identity, null as unknown as MediaStream);
  }

  private handleScreenShareAudioAvailable(identity: string): void {
    if (this.activeShares.has(identity)) {
      return;
    }
    if (this.remoteShareTypes.get(identity) !== 'audio_only') {
      this.applyRemoteShareType(identity, 'audio_only', true);
    }
  }

  private handleScreenShareAudioEnded(identity: string): void {
    if (
      this.inferredAudioOnlySharers.has(identity) ||
      this.remoteShareTypes.get(identity) === 'audio_only'
    ) {
      this.clearRemoteShareType(identity);
    }
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

  private handleCameraAvailable(identity: string): void {
    if (this.activeCameras.has(identity)) return;
    const entry = this.createCameraCanvasEntry(identity);
    const track = entry?.stream.getVideoTracks()[0] ?? null;
    if (!entry || !track) {
      console.warn(
        LOG,
        `remote camera unavailable: canvas captureStream unsupported for ${identity}`,
      );
      return;
    }
    entry.pollInterval = setInterval(() => {
      void this.pollCameraFrame(identity);
    }, 66);
    this.activeCameras.set(identity, entry);

    console.log(LOG, `camera available (native viewer path): ${identity}`);
    this.callbacks.onRemoteCameraPublished?.(identity);
    this.callbacks.onRemoteCameraReady?.(identity, track);
    void this.pollCameraFrame(identity);
  }

  private handleCameraEnded(identity: string): void {
    console.log(LOG, `camera ended: ${identity}`);
    this.disposeCameraEntry(identity);
    this.callbacks.onRemoteCameraUnpublished?.(identity);
  }

  private async pollCameraFrame(identity: string): Promise<void> {
    if (this.disposed) return;
    const entry = this.activeCameras.get(identity);
    if (!entry) return;

    let payload: PolledCameraFrame | null = null;
    try {
      payload = await invoke<PolledCameraFrame | null>('media_poll_camera_frame', {
        identity,
        lastSeq: entry.lastSeq,
      });
    } catch (err) {
      console.warn(LOG, `camera frame poll failed for ${identity}:`, err);
      return;
    }
    if (!payload || payload.seq === entry.lastSeq) return;
    this.paintCameraPayload(entry, payload);
  }

  private async pollLocalCameraFrame(): Promise<void> {
    if (this.disposed || !this.localCamera) return;
    const entry = this.localCamera;
    let payload: PolledCameraFrame | null = null;
    try {
      payload = await invoke<PolledCameraFrame | null>('media_poll_local_camera_frame', {
        lastSeq: entry.lastSeq,
      });
    } catch (err) {
      console.warn(LOG, 'local camera frame poll failed:', err);
      return;
    }
    if (!payload || payload.seq === entry.lastSeq) return;
    this.paintCameraPayload(entry, payload);
  }

  private createCameraCanvasEntry(_identity: string): ActiveCameraEntry | null {
    if (typeof document === 'undefined') return null;

    const canvas = document.createElement('canvas');
    canvas.width = 320;
    canvas.height = 240;
    canvas.style.cssText =
      'position:fixed;top:-9999px;left:-9999px;width:1px;height:1px;pointer-events:none;opacity:0;';
    const context = canvas.getContext('2d');
    const stream = typeof canvas.captureStream === 'function' ? canvas.captureStream(15) : null;
    if (!context || !stream || stream.getVideoTracks().length === 0) {
      return null;
    }
    document.body.appendChild(canvas);
    context.fillStyle = '#11111f';
    context.fillRect(0, 0, canvas.width, canvas.height);
    return {
      stream,
      canvas,
      context,
      pollInterval: null,
      lastSeq: null,
    };
  }

  private paintCameraPayload(entry: ActiveCameraEntry, payload: PolledCameraFrame): void {
    entry.lastSeq = payload.seq;
    if (entry.canvas.width !== payload.width || entry.canvas.height !== payload.height) {
      entry.canvas.width = payload.width;
      entry.canvas.height = payload.height;
    }

    const image = new Image();
    image.onload = () => {
      if (entry.lastSeq !== payload.seq) return;
      entry.context.drawImage(image, 0, 0, payload.width, payload.height);
    };
    image.src = `data:image/jpeg;base64,${payload.frame}`;
  }

  private disposeCameraEntry(identity: string): void {
    const entry = this.activeCameras.get(identity);
    if (!entry) return;

    if (entry.pollInterval) clearInterval(entry.pollInterval);
    for (const track of entry.stream.getTracks()) {
      track.stop();
    }
    if (entry.canvas.parentNode) {
      entry.canvas.parentNode.removeChild(entry.canvas);
    }
    this.activeCameras.delete(identity);
  }

  private disposeLocalCameraEntry(): void {
    if (this.localCamera) {
      this.disposeCameraCanvasEntry(this.localCamera);
    }
    this.localCamera = null;
    this.localCameraTrack = null;
  }

  private disposeCameraCanvasEntry(entry: ActiveCameraEntry): void {
    if (entry.pollInterval) clearInterval(entry.pollInterval);
    for (const track of entry.stream.getTracks()) {
      track.stop();
    }
    if (entry.canvas.parentNode) {
      entry.canvas.parentNode.removeChild(entry.canvas);
    }
  }

  private classifyNativeCameraError(err: unknown): { kind: string } {
    // Tauri invoke rejections are strings or structured objects, never
    // reliably Error instances — stringify objects so their fields still
    // participate in the keyword classification below.
    const message = (
      err instanceof Error ? err.message : typeof err === 'string' ? err : JSON.stringify(err ?? '')
    ).toLowerCase();
    if (message.includes('no camera') || message.includes('/dev/video')) {
      return { kind: 'no_camera_configured' };
    }
    if (message.includes('permission') || message.includes('denied')) {
      return { kind: 'permission_denied' };
    }
    if (message.includes('busy') || message.includes('in use')) {
      return { kind: 'device_in_use' };
    }
    return { kind: 'device_unavailable' };
  }
}
