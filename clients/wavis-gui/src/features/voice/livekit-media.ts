/**
 * Wavis LiveKit Media Transport
 *
 * Wraps the livekit-client JS SDK Room lifecycle, audio track
 * publishing/subscribing, Web Audio volume control, speaking
 * indicators, screen share, and cleanup. One instance per
 * VoiceRoom session — never reused across sessions.
 */ 

import {
  Room, RoomEvent, Track,
  RemoteTrack, RemoteTrackPublication, RemoteParticipant,
  LocalParticipant, LocalTrackPublication, LocalAudioTrack, Participant,
  VideoPreset, TrackPublication,
} from 'livekit-client';
import type { AudioProcessorOptions, TrackProcessor, LocalVideoTrack } from 'livekit-client';
import { invoke } from '@tauri-apps/api/core';
import { listen } from '@tauri-apps/api/event';
import { NativeMicBridge } from './native-mic-bridge';
import { getAudioOutputDevice, getAudioInputDevice, getInputVolume, setInputVolume, inputVolumeToGain, setStoreValue, STORE_KEYS, getDenoiseEnabled } from '@features/settings/settings-store';
import type { AudioShareStartResult } from '@features/screen-share/share-types';
import type {
  NativeShareLeakStage,
  ShareLeakBrowserWebRtcSnapshot,
  ShareLeakCaptureBackend,
  ShareLeakDegradationPreferenceResult,
  ShareLeakMemorySample,
  ShareLeakSenderReuseDiagnostics,
  ShareSessionLeakSummary,
} from './share-leak-diagnostics';

const LOG = '[wavis:livekit-media]';
const NS_LOG = '[wavis:ns]';
const DEBUG_CAPTURE = import.meta.env.VITE_DEBUG_SCREEN_CAPTURE === 'true';
const DEBUG_AUDIO_OUTPUT = import.meta.env.VITE_DEBUG_AUDIO_OUTPUT === 'true';
const DEBUG_WASAPI = import.meta.env.VITE_DEBUG_WASAPI === 'true';
const DEBUG_SHARE_AUDIO = import.meta.env.VITE_DEBUG_SHARE_AUDIO === 'true';
const DEBUG_SHARE_TRACK_SUB = import.meta.env.VITE_DEBUG_SHARE_TRACK_SUBSCRIPTION === 'true';
const DEBUG_MAC_SHARE_AUDIO = import.meta.env.VITE_DEBUG_MAC_SHARE_AUDIO === 'true';
const DEBUG_NOISE_SUPPRESSION = import.meta.env.VITE_DEBUG_NOISE_SUPPRESSION === 'true';

// Capture the real browser enumerateDevices from the prototype BEFORE Tauri's
// native media module patches the instance. The patched instance method returns
// WASAPI IDs (e.g. "output:Altavoces (Razer Barracuda X 2.4)") that setSinkId
// rejects. The prototype method returns real Chromium hex deviceIds that setSinkId
// actually accepts. We bind it to navigator.mediaDevices so `this` is correct.
const _realEnumerateDevices: (() => Promise<MediaDeviceInfo[]>) | null = (() => {
  try {
    const desc = Object.getOwnPropertyDescriptor(MediaDevices.prototype, 'enumerateDevices');
    if (desc?.value) {
      return (desc.value as () => Promise<MediaDeviceInfo[]>).bind(navigator.mediaDevices);
    }
  } catch { /* ignore — unavailable in test environments */ }
  return null;
})();

function getBrowserEnumerateDevices(): (() => Promise<MediaDeviceInfo[]>) | null {
  if (_realEnumerateDevices) return _realEnumerateDevices;
  try {
    const desc = Object.getOwnPropertyDescriptor(MediaDevices.prototype, 'enumerateDevices');
    if (desc?.value) {
      return (desc.value as () => Promise<MediaDeviceInfo[]>).bind(navigator.mediaDevices);
    }
  } catch {
    // ignore — unavailable in test environments
  }
  return null;
}

function audioOutputLabelsMatch(left: string, right: string): boolean {
  const a = left.trim();
  const b = right.trim();
  return a === b || a.startsWith(b) || b.startsWith(a);
}

/** Returns true when running inside a Windows Tauri webview. */
function isWindows(): boolean {
  return /Windows NT/i.test(navigator.userAgent);
}

/** Returns true when running inside a macOS Tauri webview. */
function isMac(): boolean {
  return /Macintosh/i.test(navigator.userAgent);
}

/** Windows and macOS use the native Rust PCM bridge for screen-share audio. */
function usesNativeScreenShareAudio(): boolean {
  return isWindows() || isMac();
}

/* ─── Types ─────────────────────────────────────────────────────── */

export type MediaState = 'disconnected' | 'connecting' | 'connected' | 'failed';

/** Quality tier for screen share — matches the export in voice-room.ts. */
export type ShareQuality = 'low' | 'high' | 'max';

/** Live sender stats from the 5s screen-share polling loop. Used by the diagnostics window. */
export interface ShareStats {
  bitrateKbps: number;
  fps: number;
  qualityLimitationReason: string;
  packetLossPercent: number;
  frameWidth: number;
  frameHeight: number;
  /** Delta PLIs (picture-loss indications) since the last poll — not cumulative. */
  pliCount: number;
  /** Delta NACKs since the last poll — not cumulative. */
  nackCount: number;
  availableBandwidthKbps: number;
}

/**
 * Live receiver stats for an incoming screen share. Polled from the subscriber PC.
 * All delta fields are per-interval (not cumulative) — reset on disconnect.
 */
export interface VideoReceiveStats {
  /** Decoded frames per second. */
  fps: number;
  /** Decoded frame width in pixels. */
  frameWidth: number;
  /** Decoded frame height in pixels. */
  frameHeight: number;
  /** Delta frames dropped since last poll. */
  framesDropped: number;
  /** Inbound packet loss percentage (inbound-rtp packetsReceived + packetsLost). */
  packetLossPercent: number;
  /** Jitter buffer target delay in ms (instantaneous or lifetime-avg fallback). */
  jitterBufferDelayMs: number;
  /** Delta freeze count since last poll. */
  freezeCount: number;
  /** Delta total freeze duration in ms since last poll (float, rounded). */
  freezeDurationMs: number;
  /** Delta PLIs sent by the receiver since last poll. */
  pliCount: number;
  /** Delta NACKs sent by the receiver since last poll. */
  nackCount: number;
  /** Average decode time per frame in ms (totalDecodeTime / framesDecoded * 1000). */
  avgDecodeTimeMs: number;
}

/** Reported from LiveKitModule to VoiceRoom after track is published. */
export interface ShareQualityInfo {
  width: number;
  height: number;
  frameRate: number;
}

/* ─── Capture Profile ───────────────────────────────────────────── */

interface CaptureProfile {
  resolution: { width: number; height: number };
  frameRate: number;
  contentHint: 'detail' | 'motion';
  surfaceSwitching: 'include' | 'exclude';
  selfBrowserSurface: 'include' | 'exclude';
  audio: boolean;
  /** Prevent echo when capturing system audio (Chromium 2025+). */
  suppressLocalAudioPlayback: boolean;
}

export const DEFAULT_CAPTURE_PROFILE: CaptureProfile = {
  resolution: { width: 2560, height: 1440 },
  frameRate: 60,
  contentHint: 'detail',
  surfaceSwitching: 'include',
  selfBrowserSurface: 'exclude',
  audio: false,
  suppressLocalAudioPlayback: true,
};

/* ─── Publish Options ───────────────────────────────────────────── */

interface ScreenSharePublishOptions {
  /** Screen-share-specific encoding (not videoEncoding, which is the camera path). */
  screenShareEncoding: {
    maxBitrate: number;
    maxFramerate: number;
  };
  videoCodec: 'vp9';
  /**
   * LiveKit SDK shape: boolean | { codec: 'vp8' | 'h264'; encoding?: VideoEncoding }.
   * We use the object form to explicitly select VP8 as backup.
   */
  backupCodec: boolean | { codec: 'vp8' | 'h264'; encoding?: { maxBitrate: number; maxFramerate: number } };
  degradationPreference: 'maintain-resolution';
  /**
   * Screen-share simulcast layers (not videoSimulcastLayers, which is the camera path).
   * Note: has no effect when VP9 SVC is active — kept for VP8 backup fallback.
   */
  screenShareSimulcastLayers: Array<{ width: number; height: number }>;
}

const DEFAULT_PUBLISH_OPTIONS: ScreenSharePublishOptions = {
  screenShareEncoding: { maxBitrate: 8_000_000, maxFramerate: 60 },
  videoCodec: 'vp9',
  backupCodec: { codec: 'vp8' },
  degradationPreference: 'maintain-resolution',
  screenShareSimulcastLayers: [
    { width: 640, height: 360 },
    { width: 1280, height: 720 },
  ],
};

/* ─── Adaptive Quality ──────────────────────────────────────────── */

export type AdaptiveTier = 'full' | 'reduced-fps' | 'reduced-resolution';

export interface AdaptiveQualityState {
  currentTier: AdaptiveTier;
  /** Number of consecutive polls where loss exceeded the step-down threshold. */
  consecutiveLossPolls: number;
  /** Number of consecutive polls where loss was below the recovery threshold. */
  consecutiveRecoveryPolls: number;
  /** Number of consecutive polls where qualityLimitationReason was 'bandwidth'. */
  consecutiveBandwidthPolls: number;
  /** The original preset before adaptive reductions. */
  basePreset: ShareQuality;
}

/** Packet loss thresholds for adaptive quality transitions. */
const ADAPTIVE_LOSS_THRESHOLD_MODERATE = 5;   // >5% → reduce FPS
const ADAPTIVE_LOSS_THRESHOLD_SEVERE = 15;    // >15% → reduce resolution (when already in reduced-fps)
const ADAPTIVE_RECOVERY_THRESHOLD = 3;        // <3% → recover
const ADAPTIVE_STEPDOWN_POLLS = 2;            // 2 consecutive polls above threshold (10s at 5s cadence)
const ADAPTIVE_RECOVERY_POLLS = 3;            // 3 consecutive polls below threshold (15s at 5s cadence)
const ADAPTIVE_BANDWIDTH_STEPDOWN_POLLS = 3;  // 3 consecutive polls with bandwidth limitation (15s)

/** Resolution tiers for adaptive step-down (width × height). */
const RESOLUTION_TIERS: Array<{ width: number; height: number }> = [
  { width: 2560, height: 1440 },
  { width: 1920, height: 1080 },
  { width: 1280, height: 720 },
];

/** FPS tiers for adaptive step-down. */
const FPS_TIERS = [60, 30, 15];

/* ─── Quality Presets ───────────────────────────────────────────── */

interface QualityPreset {
  resolution: { width: number; height: number };
  maxFramerate: number;
  maxBitrate: number;
  degradationPreference: 'maintain-resolution' | 'maintain-framerate' | 'balanced';
  contentHint: 'detail' | 'motion';
}

const QUALITY_PRESETS: Record<ShareQuality, QualityPreset> = {
  low: {
    resolution: { width: 1920, height: 1080 },
    maxFramerate: 60,
    maxBitrate: 6_000_000,
    degradationPreference: 'maintain-framerate',
    contentHint: 'motion',
  },
  high: {
    resolution: { width: 2560, height: 1440 },
    maxFramerate: 30,
    maxBitrate: 6_000_000,
    degradationPreference: 'maintain-resolution',
    contentHint: 'detail',
  },
  max: {
    resolution: { width: 2560, height: 1440 },
    maxFramerate: 60,
    maxBitrate: 8_000_000,
    degradationPreference: 'maintain-resolution',
    contentHint: 'detail',
  },
};

export interface MediaCallbacks {
  /** Called when media transitions to connected (mic published or listen-only). */
  onMediaConnected: () => void;
  /** Called when media connection fails. */
  onMediaFailed: (reason: string) => void;
  /** Called when media disconnects (cleanup complete). */
  onMediaDisconnected: () => void;
  /** Called with batched audio level updates (coalesced via rAF). */
  onAudioLevels: (levels: Map<string, { isSpeaking: boolean; rmsLevel: number }>) => void;
  /** Called with the local mic RMS level. */
  onLocalAudioLevel: (level: number) => void;
  /** Called when active speakers change. */
  onActiveSpeakers: (speakerIdentities: string[]) => void;
  /** Called when connection quality metrics are available. */
  onConnectionQuality: (stats: {
    rttMs: number;
    packetLossPercent: number;
    jitterMs: number;
    /** Current jitter buffer target delay in ms (instantaneous or lifetime avg fallback). */
    jitterBufferDelayMs: number;
    /** Concealment events (PLC) since the previous stats poll. */
    concealmentEventsPerInterval: number;
    candidateType: 'host' | 'srflx' | 'relay' | 'unknown';
    /** Estimated available outgoing bandwidth in kbps (0 = unavailable). */
    availableBandwidthKbps: number;
  }) => void;
  /** Called when a remote screen share track is subscribed. Passes MediaStream, NOT a DOM element. */
  onScreenShareSubscribed: (identity: string, stream: MediaStream) => void;
  /** Called when a remote screen share track is unsubscribed. */
  onScreenShareUnsubscribed: (identity: string) => void;
  /** Called when the local screen share track ends (OS dialog stop or unpublish). */
  onLocalScreenShareEnded: () => void;
  /** Called when a remote participant mutes or unmutes their audio track. */
  onParticipantMuteChanged: (identity: string, isMuted: boolean) => void;
  /** Called to append a system event to the event log. */
  onSystemEvent: (message: string) => void;
  /** Called when screen share quality info is available (actual captured settings). */
  onShareQualityInfo?: (info: ShareQualityInfo) => void;
  /** Called every ~5s with live screen-share sender stats. Used by the diagnostics window. */
  onShareStats?: (stats: ShareStats) => void;
  /** Called every ~10s with live screen-share receiver stats (subscriber PC). Used by the diagnostics window. */
  onVideoReceiveStats?: (stats: VideoReceiveStats) => void;
  /** Called when a native video share leak session closes with a structured summary. */
  onShareLeakSummary?: (summary: ShareSessionLeakSummary) => void;
  /** Called when the native mic bridge activates or deactivates (Windows only). */
  onNativeMicBridgeState?: (active: boolean) => void;
  /** Called when the active microphone path has JS noise suppression attached or removed. */
  onNoiseSuppressionState?: (active: boolean) => void;
}

/* ─── Helpers ───────────────────────────────────────────────────── */

/**
 * Perceptual volume curve matching the Rust cubic curve: (vol/100)³ × 3.0.
 * At default 70 → ~1.03× (unity). At 100 → 3.0× (boost). At 50 → 0.375×.
 */
function perceptualGain(volume: number): number {
  const v = Math.max(0, Math.min(100, volume)) / 100;
  return v * v * v * 3.0;
}

/**
 * Extract RTT, packet loss, jitter, jitter buffer delay, concealment events,
 * candidate type, and available bandwidth from one or more RTCStatsReports
 * (publisher + subscriber). RTT comes from the nominated candidate-pair
 * (typically on the publisher PC). Audio stats come from inbound-rtp streams
 * (subscriber PC).
 */
function extractStatsFromReports(reports: RTCStatsReport[]): {
  rttMs: number;
  packetLossPercent: number;
  jitterMs: number;
  jitterBufferDelayMs: number;
  concealmentEventsTotal: number;
  candidateType: 'host' | 'srflx' | 'relay' | 'unknown';
  availableBandwidthKbps: number;
} {
  let rttMs = 0;
  let jitterMs = 0;
  let totalPackets = 0;
  let lostPackets = 0;
  let jitterBufferDelayMs = 0;
  let concealmentEventsTotal = 0;
  let candidateType: 'host' | 'srflx' | 'relay' | 'unknown' = 'unknown';
  let availableBandwidthKbps = 0;

  for (const report of reports) {
    // Build a local id→entry map to resolve candidate-pair → local-candidate references
    const entryById = new Map<string, RTCStats>();
    report.forEach((entry) => { entryById.set(entry.id, entry); });

    report.forEach((entry) => {
      // Nominated ICE candidate pair: RTT, available outgoing bandwidth, local candidate type
      if (entry.type === 'candidate-pair' && entry.nominated) {
        if (rttMs === 0 && typeof entry.currentRoundTripTime === 'number') {
          rttMs = Math.round(entry.currentRoundTripTime * 1000);
        }
        if (availableBandwidthKbps === 0 && typeof entry.availableOutgoingBitrate === 'number') {
          availableBandwidthKbps = Math.round(entry.availableOutgoingBitrate / 1000);
        }
        if (candidateType === 'unknown' && entry.localCandidateId) {
          const local = entryById.get(entry.localCandidateId) as Record<string, unknown> | undefined;
          if (local) {
            const ct = typeof local.candidateType === 'string' ? local.candidateType : undefined;
            if (ct === 'host' || ct === 'srflx' || ct === 'relay') {
              candidateType = ct;
            } else if (ct === 'prflx') {
              candidateType = 'srflx'; // peer-reflexive ≈ srflx for display purposes
            }
          }
        }
      }
      // Inbound audio RTP: packet loss, jitter, jitter buffer delay, concealment events
      if (entry.type === 'inbound-rtp' && entry.kind === 'audio') {
        if (typeof entry.jitter === 'number') {
          jitterMs = Math.round(entry.jitter * 1000);
        }
        if (typeof entry.packetsReceived === 'number' && typeof entry.packetsLost === 'number') {
          totalPackets += entry.packetsReceived + entry.packetsLost;
          lostPackets += entry.packetsLost;
        }
        // Both jitterBufferTargetDelay and jitterBufferDelay are cumulative totals (seconds)
        // across all emitted samples — divide by jitterBufferEmittedCount to get the average.
        // Prefer jitterBufferTargetDelay (target) over jitterBufferDelay (actual) when present.
        if (jitterBufferDelayMs === 0 && typeof entry.jitterBufferTargetDelay === 'number' && typeof entry.jitterBufferEmittedCount === 'number' && entry.jitterBufferEmittedCount > 0) {
          jitterBufferDelayMs = Math.round((entry.jitterBufferTargetDelay / entry.jitterBufferEmittedCount) * 1000);
        }
        if (jitterBufferDelayMs === 0 && typeof entry.jitterBufferDelay === 'number' && typeof entry.jitterBufferEmittedCount === 'number' && entry.jitterBufferEmittedCount > 0) {
          jitterBufferDelayMs = Math.round((entry.jitterBufferDelay / entry.jitterBufferEmittedCount) * 1000);
        }
        if (typeof entry.concealmentEvents === 'number') {
          concealmentEventsTotal += entry.concealmentEvents;
        }
      }
    });
  }

  const packetLossPercent = totalPackets > 0
    ? Math.round((lostPackets / totalPackets) * 1000) / 10 // one decimal
    : 0;

  return { rttMs, packetLossPercent, jitterMs, jitterBufferDelayMs, concealmentEventsTotal, candidateType, availableBandwidthKbps };
}

function getTrackSettingsSafe(track: MediaStreamTrack | undefined): Partial<MediaTrackSettings> {
  if (!track || typeof track.getSettings !== 'function') return {};
  try {
    return track.getSettings();
  } catch {
    return {};
  }
}

function getTrackCapabilitiesSafe(track: MediaStreamTrack | undefined): Record<string, unknown> {
  if (!track || typeof track.getCapabilities !== 'function') return {};
  try {
    return track.getCapabilities() as Record<string, unknown>;
  } catch {
    return {};
  }
}

interface RustDiagnosticsSnapshot {
  rssMb: number;
  childCount: number;
  timestampMs: number;
}

interface RawShareLeakMemorySample {
  capturedAt: string;
  rssMb: number | null;
  childProcessCount: number | null;
  jsHeapUsedMb: number | null;
  jsHeapTotalMb: number | null;
  domNodes: number;
}

interface NativeCaptureLeakSessionState {
  summary: ShareSessionLeakSummary;
  startedAtMs: number;
  stopRequestedAtMs: number | null;
  baselineRawPromise: Promise<RawShareLeakMemorySample>;
  activeRaw: RawShareLeakMemorySample | null;
}

function makeShareLeakSessionId(): string {
  if (typeof crypto !== 'undefined' && typeof crypto.randomUUID === 'function') {
    return crypto.randomUUID();
  }
  return `share-leak-${Date.now()}-${Math.random().toString(36).slice(2, 10)}`;
}

function toShareLeakMemorySample(
  sample: RawShareLeakMemorySample,
  baseline: RawShareLeakMemorySample | null,
): ShareLeakMemorySample {
  const baselineRss = baseline?.rssMb ?? null;
  const baselineJsHeapUsed = baseline?.jsHeapUsedMb ?? null;
  return {
    capturedAt: sample.capturedAt,
    rssMb: sample.rssMb,
    childProcessCount: sample.childProcessCount,
    jsHeapUsedMb: sample.jsHeapUsedMb,
    jsHeapTotalMb: sample.jsHeapTotalMb,
    domNodes: sample.domNodes,
    deltaRssMb:
      baselineRss !== null && sample.rssMb !== null
        ? Math.round((sample.rssMb - baselineRss) * 10) / 10
        : null,
    deltaJsHeapUsedMb:
      baselineJsHeapUsed !== null && sample.jsHeapUsedMb !== null
        ? Math.round((sample.jsHeapUsedMb - baselineJsHeapUsed) * 10) / 10
        : null,
    deltaDomNodes: baseline ? sample.domNodes - baseline.domNodes : null,
  };
}

function leakSessionBackendForStage(stage: NativeShareLeakStage): ShareLeakCaptureBackend {
  return stage === 'native_capture_start' ? 'native-poll' : 'browser-display-media';
}

const publisherPeerConnectionIds = new WeakMap<RTCPeerConnection, string>();
let publisherPeerConnectionIdCounter = 0;

function getPublisherPeerConnectionId(peerConnection: RTCPeerConnection | null): string | null {
  if (!peerConnection) return null;
  const existing = publisherPeerConnectionIds.get(peerConnection);
  if (existing) return existing;
  publisherPeerConnectionIdCounter += 1;
  const nextId = `publisher-pc-${publisherPeerConnectionIdCounter}`;
  publisherPeerConnectionIds.set(peerConnection, nextId);
  return nextId;
}

function hasReusableInactiveVideoTransceiver(snapshot: ShareLeakBrowserWebRtcSnapshot | null): boolean {
  return snapshot?.transceivers.some((transceiver) =>
    transceiver.stopped !== true &&
    transceiver.direction === 'inactive' &&
    transceiver.senderTrackId === null &&
    transceiver.receiverTrackKind === 'video'
  ) ?? false;
}

interface WavisSenderData {
  reused: boolean;
  degradationPreferenceConfigured: boolean;
  attemptedPreferences: string[];
  invalidStateSkipped: boolean;
  lastErrorName: string | null;
  lastErrorMessage: string | null;
}

type WavisSenderDataStoreHost = typeof globalThis & {
  __wavisSenderData?: WeakMap<RTCRtpSender, WavisSenderData>;
};

function getWavisSenderDataStore(): WeakMap<RTCRtpSender, WavisSenderData> | null {
  return (globalThis as WavisSenderDataStoreHost).__wavisSenderData ?? null;
}

function getShareSenderDegradationPreferenceResult(
  sender: RTCRtpSender | null | undefined,
): ShareLeakDegradationPreferenceResult | null {
  if (!sender) return null;
  const senderData = getWavisSenderDataStore()?.get(sender);
  if (!senderData) return null;
  const attemptedPreferences = Array.isArray(senderData.attemptedPreferences)
    ? senderData.attemptedPreferences.filter(
      (value): value is string => typeof value === 'string',
    )
    : [];
  const senderWasReused = senderData.reused === true;
  const invalidStateSkipped = senderData.invalidStateSkipped === true;
  const finalErrorName = typeof senderData.lastErrorName === 'string'
    ? senderData.lastErrorName
    : null;
  const finalErrorMessage = typeof senderData.lastErrorMessage === 'string'
    ? senderData.lastErrorMessage
    : null;

  if (
    !senderWasReused &&
    attemptedPreferences.length === 0 &&
    !invalidStateSkipped &&
    finalErrorName === null &&
    finalErrorMessage === null
  ) {
    return null;
  }

  return {
    senderWasReused,
    attemptedPreferences,
    finalErrorName,
    finalErrorMessage,
    invalidStateSkipped,
  };
}

/* ─── Mic Gain Processor ─────────────────────────────────────────── */

type NoiseSuppressionStats = {
  enabled: boolean;
  inputRms: number;
  outputRms: number;
  attenuationRatio: number;
  noiseFrames: number;
  speechFrames: number;
  attenuatedFrames: number;
  underruns: number;
  noiseFloor: number;
  gateGain: number;
};

type NoiseSuppressionStatePayload = {
  state: 'bypass' | 'speech_passed' | 'noise_attenuated' | 'noise_detected';
  noiseFloor: number;
  gateGain: number;
  inputRms: number;
  outputRms: number;
};

/** Composite track processor: JS noise suppression first, then user input gain. */
class MicAudioProcessor implements TrackProcessor<Track.Kind.Audio, AudioProcessorOptions> {
  name = 'wavis-mic-audio';
  processedTrack?: MediaStreamTrack;
  private source: MediaStreamAudioSourceNode | null = null;
  private denoiseNode: AudioWorkletNode | null = null;
  private gainNode: GainNode | null = null;
  private destination: MediaStreamAudioDestinationNode | null = null;
  /** Stored from init() so restart() can reuse it (LiveKit omits audioContext on restart). */
  private audioCtx: AudioContext | null = null;
  private _gain = 1.0;
  private denoiseEnabled = false;
  private onStats?: (stats: NoiseSuppressionStats) => void;
  private onState?: (state: NoiseSuppressionStatePayload) => void;
  private static moduleLoadedForCtx: AudioContext | null = null;
  private static moduleLoadPromise: Promise<void> | null = null;

  constructor(params?: {
    denoiseEnabled?: boolean;
    onStats?: (stats: NoiseSuppressionStats) => void;
    onState?: (state: NoiseSuppressionStatePayload) => void;
  }) {
    this.denoiseEnabled = params?.denoiseEnabled ?? false;
    this.onStats = params?.onStats;
    this.onState = params?.onState;
  }

  setGain(gain: number): void {
    this._gain = Math.max(0, Math.min(1, gain));
    if (this.gainNode) this.gainNode.gain.value = this._gain;
  }

  setDenoiseEnabled(enabled: boolean): void {
    this.denoiseEnabled = enabled;
    this.denoiseNode?.port.postMessage({
      type: 'config',
      enabled,
    });
  }

  private async ensureWorkletModule(ctx: AudioContext): Promise<void> {
    if (MicAudioProcessor.moduleLoadedForCtx !== ctx) {
      const workletUrl = new URL('./mic-noise-suppression-worklet.js', import.meta.url).href;
      MicAudioProcessor.moduleLoadedForCtx = ctx;
      MicAudioProcessor.moduleLoadPromise = ctx.audioWorklet.addModule(workletUrl);
    }
    await MicAudioProcessor.moduleLoadPromise!;
  }

  async init({ track, audioContext }: AudioProcessorOptions): Promise<void> {
    const ctx = audioContext ?? this.audioCtx;
    if (!ctx) throw new Error('MicAudioProcessor: no AudioContext available');
    this.audioCtx = ctx;
    await this.ensureWorkletModule(ctx);
    this.source = ctx.createMediaStreamSource(new MediaStream([track]));
    this.denoiseNode = new AudioWorkletNode(ctx, 'wavis-mic-noise-suppression', {
      outputChannelCount: [1],
    });
    this.denoiseNode.port.onmessage = (event: MessageEvent<{ type: string; payload: unknown }>) => {
      if (event.data?.type === 'stats') {
        this.onStats?.(event.data.payload as NoiseSuppressionStats);
      } else if (event.data?.type === 'state') {
        this.onState?.(event.data.payload as NoiseSuppressionStatePayload);
      }
    };
    this.denoiseNode.port.postMessage({
      type: 'config',
      enabled: this.denoiseEnabled,
    });
    this.gainNode = ctx.createGain();
    this.gainNode.gain.value = this._gain;
    this.destination = ctx.createMediaStreamDestination();
    this.source.connect(this.denoiseNode);
    this.denoiseNode.connect(this.gainNode);
    this.gainNode.connect(this.destination);
    this.processedTrack = this.destination.stream.getAudioTracks()[0];
  }

  async restart(opts: AudioProcessorOptions): Promise<void> {
    // Keep old chain alive until new one succeeds — if init() throws, the
    // sender still has a working processedTrack instead of a dead one.
    const oldSource = this.source;
    const oldGain = this.gainNode;
    const oldDest = this.destination;
    this.source = null;
    this.gainNode = null;
    this.destination = null;
    await this.init(opts);
    oldSource?.disconnect();
    oldGain?.disconnect();
    oldDest?.disconnect();
  }

  async destroy(): Promise<void> {
    this.source?.disconnect();
    this.denoiseNode?.disconnect();
    this.gainNode?.disconnect();
    this.destination?.disconnect();
    this.source = null;
    this.denoiseNode = null;
    this.gainNode = null;
    this.destination = null;
    this.processedTrack = undefined;
  }
}

/* ─── Class ─────────────────────────────────────────────────────── */

export class LiveKitModule {
  private room: Room | null = null;
  private audioContext: AudioContext | null = null;
  private masterGain: GainNode | null = null;
  private participantGains: Map<string, GainNode> = new Map();
  private desiredParticipantVolumes: Map<string, number> = new Map();
  private audioElementMap: Map<string, HTMLAudioElement> = new Map();
  private screenShareElements: Map<string, { stream: MediaStream; startedAtMs: number; trackSid: string; dummyVideo?: HTMLVideoElement; trackEndedCleanup?: () => void }> = new Map();
  private screenShareAudioTracks: Map<string, { track: RemoteTrack; participant: RemoteParticipant }> = new Map();
  private screenShareAudioPublications: Map<string, RemoteTrackPublication> = new Map();
  /** Participants whose viewer window is open but whose audio track hadn't arrived yet when attachScreenShareAudio was called. */
  private screenShareAudioPending = new Set<string>();
  private pendingLevels: Map<string, { isSpeaking: boolean; rmsLevel: number }> = new Map();
  private rafId: number | null = null;
  private statsInterval: ReturnType<typeof setInterval> | null = null;
  private disposed = false;
  private gestureListener: (() => void) | null = null;
  private listeners: Array<{ event: RoomEvent; handler: (...args: unknown[]) => void }> = [];
  private callbacks: MediaCallbacks;
  private analyserMap: Map<string, AnalyserNode> = new Map();
  private analyserInterval: ReturnType<typeof setInterval> | null = null;
  private localMicAnalyser: AnalyserNode | null = null;
  private localMicSource: MediaStreamAudioSourceNode | null = null;
  private localMicInterval: ReturnType<typeof setInterval> | null = null;
  private micAudioProcessor: MicAudioProcessor | null = null;
  private localMicTrack: LocalAudioTrack | null = null;
  /** Whether the JS-side noise suppression processor should be active on this session's mic. */
  private jsDenoise = false;
  /** True only when the JS noise suppression processor is confirmed attached to the live mic track. */
  private jsDenoiseProcessorActive = false;
  /** Active native mic bridge instance (Windows + denoiseEnabled only). */
  private nativeMicBridge: NativeMicBridge | null = null;

  /* ─── Audio Output Routing ────────────────────────────────────── */
  private deviceChangeListener: (() => void) | null = null;
  private sharePinnedAudioOutputDeviceId: string | null = null;

  /* ─── WASAPI Audio Bridge ────────────────────────────────────── */
  private wasapiAudioCtx: AudioContext | null = null;
  private wasapiWorkletNode: AudioWorkletNode | null = null;
  private wasapiDestNode: MediaStreamAudioDestinationNode | null = null;
  private wasapiAudioPublication: LocalTrackPublication | null = null;
  private wasapiFrameUnlisten: (() => void) | null = null;
  private wasapiStoppedUnlisten: (() => void) | null = null;

  /** Counter for wasapi audio frames received — used for diagnostic logging. */
  private wasapiFrameCount = 0;

  /**
   * masterGain value saved when system audio share starts so it can be
   * restored when sharing stops.  null = no share active.
   */
  private preShareGain: number | null = null;

  /* ─── Screen Share Quality State ──────────────────────────────── */
  private currentCaptureProfile: CaptureProfile = { ...DEFAULT_CAPTURE_PROFILE };
  private currentPublishOptions: ScreenSharePublishOptions = { ...DEFAULT_PUBLISH_OPTIONS };
  private currentQuality: ShareQuality = 'high';
  private postPublishRetryTimeout: ReturnType<typeof setTimeout> | null = null;
  private screenShareStatsInterval: ReturnType<typeof setInterval> | null = null;
  private adaptiveState: AdaptiveQualityState | null = null;
  private nativeCaptureLeakSession: NativeCaptureLeakSessionState | null = null;

  /* ─── Audio/share quality delta tracking (reset on disconnect) ─── */
  /** Cumulative concealment events at last stats poll — used to compute per-interval delta. */
  private prevConcealmentEventsTotal = 0;
  /** Cumulative PLI count at last share stats poll — used to compute per-interval delta. */
  private prevSharePliCount = 0;
  /** Cumulative NACK count at last share stats poll — used to compute per-interval delta. */
  private prevShareNackCount = 0;
  /** Last known jitter buffer delay ms — carried forward when subscriber report not polled. */
  private lastJitterBufferDelayMs = 0;
  /** Last known concealment events per interval — carried forward between polls. */
  private lastConcealmentEventsPerInterval = 0;
  /** Last known ICE candidate type — carried forward once resolved. */
  private lastCandidateType: 'host' | 'srflx' | 'relay' | 'unknown' = 'unknown';
  /** Last known available outgoing bandwidth kbps. */
  private lastAvailableBandwidthKbps = 0;
  /** Last known RTT ms — carried forward on subscriber-only cycles where RTT is 0. */
  private lastRttMs = 0;
  /** Last known packet loss % — carried forward between polls. */
  private lastPacketLossPercent = 0;
  /** Last known jitter ms — carried forward between polls. */
  private lastJitterMs = 0;
  /** Cumulative framesDropped at last subscriber poll — used to compute per-interval delta. */
  private prevVideoRecvFramesDropped = 0;
  /** Cumulative freezeCount at last subscriber poll — used to compute per-interval delta. */
  private prevVideoRecvFreezeCount = 0;
  /** Cumulative totalFreezesDuration (seconds) at last subscriber poll — float delta. */
  private prevVideoRecvTotalFreezesDuration = 0;
  /** Cumulative pliCount (receiver-sent) at last subscriber poll. */
  private prevVideoRecvPliCount = 0;
  /** Cumulative nackCount (receiver-sent) at last subscriber poll. */
  private prevVideoRecvNackCount = 0;

  constructor(callbacks: MediaCallbacks) {
    this.callbacks = callbacks;
    console.log(LOG, 'created');
  }

  private syncParticipantMicMute(
    participant: Participant,
    publication: TrackPublication | RemoteTrackPublication | null | undefined,
  ): void {
    if (!publication) return;
    if (publication.kind !== Track.Kind.Audio) return;
    if (publication.source !== Track.Source.Microphone) return;
    this.callbacks.onParticipantMuteChanged(participant.identity, publication.isMuted);
  }

  private setNoiseSuppressionActive(active: boolean): void {
    if (this.jsDenoiseProcessorActive === active) return;
    this.jsDenoiseProcessorActive = active;
    this.callbacks.onNoiseSuppressionState?.(active);
    console.log(NS_LOG, `processor ${active ? 'active' : 'inactive'}`);
  }

  private handleNoiseSuppressionStats(stats: NoiseSuppressionStats): void {
    if (!DEBUG_NOISE_SUPPRESSION) return;
    console.log(
      NS_LOG,
      `stats enabled=${stats.enabled} in_rms=${stats.inputRms.toFixed(3)} out_rms=${stats.outputRms.toFixed(3)} attenuation=${stats.attenuationRatio.toFixed(3)} noise_frames=${stats.noiseFrames} speech_frames=${stats.speechFrames} attenuated_frames=${stats.attenuatedFrames} underruns=${stats.underruns} noise_floor=${stats.noiseFloor.toFixed(3)} gate=${stats.gateGain.toFixed(3)}`,
    );
  }

  private handleNoiseSuppressionState(state: NoiseSuppressionStatePayload): void {
    if (!DEBUG_NOISE_SUPPRESSION) return;
    console.log(
      NS_LOG,
      `state=${state.state} noise_floor=${state.noiseFloor.toFixed(3)} gate=${state.gateGain.toFixed(3)} in_rms=${state.inputRms.toFixed(3)} out_rms=${state.outputRms.toFixed(3)}`,
    );
  }

  private shouldUseJsNoiseSuppression(denoiseEnabled: boolean): boolean {
    return denoiseEnabled && (isWindows() || isMac());
  }

  private shouldUseNativeMicBridge(_denoiseEnabled: boolean): boolean {
    return false;
  }

  private logNoiseSuppressionCapabilities(track: MediaStreamTrack, context: string): void {
    console.log(
      NS_LOG,
      `${context} capabilities=${JSON.stringify(getTrackCapabilitiesSafe(track))} settings=${JSON.stringify(getTrackSettingsSafe(track))} browser_constraint_is_baseline_only=true`,
    );
  }

  private async syncMicProcessor(reason: string, opts?: { originalTrack?: MediaStreamTrack | null }): Promise<void> {
    const track = this.localMicTrack;
    if (!track) return;

    const volume = await getInputVolume();
    const gain = inputVolumeToGain(volume);
    const shouldEnableDenoise = this.jsDenoise;
    const needsProcessor = shouldEnableDenoise || volume < 100;
    const originalTrack = opts?.originalTrack ?? track.mediaStreamTrack;

    if (!needsProcessor) {
      if (track.getProcessor()) {
        await track.stopProcessor().catch(() => {});
      }
      this.micAudioProcessor?.destroy().catch(() => {});
      this.micAudioProcessor = null;
      this.setNoiseSuppressionActive(false);
      this.stopLocalMicMonitor();
      console.log(NS_LOG, `${reason} processor bypassed gain=${gain.toFixed(3)} denoise=${shouldEnableDenoise}`);
      return;
    }

    const processor = new MicAudioProcessor({
      denoiseEnabled: shouldEnableDenoise,
      onStats: (stats) => this.handleNoiseSuppressionStats(stats),
      onState: (state) => this.handleNoiseSuppressionState(state),
    });
    processor.setGain(gain);

    this.micAudioProcessor?.destroy().catch(() => {});
    this.micAudioProcessor = processor;
    track.setAudioContext(this.ensureAudioContext());
    await track.setProcessor(processor);
    this.setNoiseSuppressionActive(shouldEnableDenoise);
    this.startLocalMicMonitor(originalTrack);

    console.log(
      NS_LOG,
      `${reason} processor attached denoise=${shouldEnableDenoise} gain=${gain.toFixed(3)} original_track_id=${originalTrack?.id ?? 'unknown'} processed_track_id=${processor.processedTrack?.id ?? 'unknown'} publication_track_id=${track.mediaStreamTrack?.id ?? 'unknown'}`,
    );
  }

  private beginShareLeakSession(details: {
    shareSessionId?: string | null;
    mode: 'screen_audio' | 'window';
    sourceId: string;
    sourceName: string;
    startStage: NativeShareLeakStage;
  }): void {
    // Both Windows browser/WebView capture and the native picker path feed the
    // same leak-session summary. Keep instrumentation on both entry points:
    // startScreenShare() for normal Windows/macOS shares, and
    // beginNativeCaptureLeakSession() for the custom Rust capture path.
    const shareSessionId = details.shareSessionId ?? makeShareLeakSessionId();
    const startedAt = new Date().toISOString();
    const captureBackend = leakSessionBackendForStage(details.startStage);
    this.nativeCaptureLeakSession = {
      summary: {
        shareSessionId,
        sourceId: details.sourceId,
        sourceName: details.sourceName,
        mode: details.mode,
        captureBackend,
        startedAt,
        endedAt: startedAt,
        stages: {},
        counters: {
          pollTicks: 0,
          newFrames: 0,
          duplicateFrameSkips: 0,
          decodeFailures: 0,
          earlyFrameBufferPeak: 0,
          firstFrameLatencyMs: null,
          stopCleanupLatencyMs: null,
        },
        cleanupFlags: {
          pollIntervalCleared: null,
          frameHandlerCleared: null,
          earlyFramesCleared: null,
          canvasRemoved: null,
          publicationCleared: null,
          unpublishAttempted: null,
          unpublishSucceeded: null,
          trackStopped: null,
        },
        browserWebRtcBeforeStop: null,
        browserWebRtcAfterStop: null,
        senderReuseDiagnostics: {
          publishWebRtcSnapshot: null,
          reuseExpected: false,
          events: [],
          finalSetParametersError: null,
          degradationPreferenceResult: null,
        },
        baselineMemory: null,
        activeMemory: null,
        cleanupMemory: null,
        error: null,
      },
      startedAtMs: Date.now(),
      stopRequestedAtMs: null,
      baselineRawPromise: this.captureRawShareLeakMemorySnapshot(),
      activeRaw: null,
    };
    this.markNativeCaptureLeakStage(details.startStage);
  }

  beginNativeCaptureLeakSession(details: {
    shareSessionId: string | null;
    mode: 'screen_audio' | 'window';
    sourceId: string;
    sourceName: string;
  }): void {
    if (!details.shareSessionId) return;
    // This is only the custom native-source path used by the share picker.
    // It is not the default Windows `/share` path; that goes through
    // startScreenShare() and must keep its own leak instrumentation.
    this.beginShareLeakSession({
      shareSessionId: details.shareSessionId,
      mode: details.mode,
      sourceId: details.sourceId,
      sourceName: details.sourceName,
      startStage: 'native_capture_start',
    });
  }

  markNativeCaptureFailure(reason: string): void {
    if (!this.nativeCaptureLeakSession) return;
    this.nativeCaptureLeakSession.summary.error = reason;
    console.warn(
      LOG,
      `native capture: session=${this.nativeCaptureLeakSession.summary.shareSessionId} failure=${reason}`,
    );
  }

  private getPublisherPeerConnection(): RTCPeerConnection | null {
    if (!this.room) return null;
    // eslint-disable-next-line @typescript-eslint/no-explicit-any
    const engine = (this.room as any).engine;
    const publisher = engine?.pcManager?.publisher;
    const candidates = [publisher, publisher?.pc, publisher?._pc];
    for (const candidate of candidates) {
      if (
        candidate &&
        typeof candidate.getSenders === 'function' &&
        typeof candidate.getTransceivers === 'function'
      ) {
        return candidate as RTCPeerConnection;
      }
    }
    return null;
  }

  private captureBrowserWebRtcSnapshot(expectedTrackId?: string | null): ShareLeakBrowserWebRtcSnapshot | null {
    if (!this.room) return null;

    const localPublications = Array.from(this.room.localParticipant.trackPublications.values());
    const screenSharePublication = this.room.localParticipant.getTrackPublication(Track.Source.ScreenShare);
    const publicationTrackId = screenSharePublication?.track?.mediaStreamTrack?.id ?? null;
    const resolvedTrackId = expectedTrackId ?? publicationTrackId;
    const peerConnection = this.getPublisherPeerConnection();
    const publisherPeerConnectionId = getPublisherPeerConnectionId(peerConnection);
    const senders = peerConnection ? peerConnection.getSenders() : null;
    const transceivers = peerConnection ? peerConnection.getTransceivers() : null;
    const videoSenders = senders?.filter((sender) => sender.track?.kind === 'video') ?? null;
    const screenShareSenders = videoSenders && resolvedTrackId
      ? videoSenders.filter((sender) => sender.track?.id === resolvedTrackId)
      : null;

    return {
      capturedAt: new Date().toISOString(),
      publisherPeerConnectionId,
      publicationExists: !!screenSharePublication,
      expectedTrackId: resolvedTrackId,
      publicationTrackId,
      localScreenSharePublicationCount: localPublications.filter((pub) => pub.source === Track.Source.ScreenShare).length,
      localVideoPublicationCount: localPublications.filter((pub) => pub.kind === Track.Kind.Video).length,
      senderCount: senders ? senders.length : null,
      videoSenderCount: videoSenders ? videoSenders.length : null,
      transceiverCount: transceivers ? transceivers.length : null,
      screenShareSenderCount: screenShareSenders ? screenShareSenders.length : null,
      liveVideoSenderTrackIds: videoSenders
        ? videoSenders
          .filter((sender) => sender.track?.readyState === 'live')
          .map((sender) => sender.track?.id)
          .filter((id): id is string => typeof id === 'string')
        : [],
      endedVideoSenderTrackIds: videoSenders
        ? videoSenders
          .filter((sender) => sender.track?.readyState === 'ended')
          .map((sender) => sender.track?.id)
          .filter((id): id is string => typeof id === 'string')
        : [],
      transceivers: transceivers
        ? transceivers.map((transceiver, index) => ({
          index,
          mid: transceiver.mid ?? null,
          direction: transceiver.direction ?? null,
          currentDirection: transceiver.currentDirection ?? null,
          stopped: typeof (transceiver as { stopped?: unknown }).stopped === 'boolean'
            ? (transceiver as { stopped?: boolean }).stopped ?? null
            : null,
          senderTrackId: transceiver.sender.track?.id ?? null,
          senderTrackKind: transceiver.sender.track?.kind ?? null,
          senderTrackReadyState: transceiver.sender.track?.readyState ?? null,
          receiverTrackKind: transceiver.receiver.track?.kind ?? null,
        }))
        : [],
    };
  }

  private async captureRawShareLeakMemorySnapshot(): Promise<RawShareLeakMemorySample> {
    let rssMb: number | null = null;
    let childProcessCount: number | null = null;
    try {
      const raw = await invoke<RustDiagnosticsSnapshot>('get_diagnostics_snapshot');
      rssMb = raw.rssMb;
      childProcessCount = raw.childCount;
    } catch {
      // Diagnostics IPC is best-effort for leak triage.
    }

    const perfMemory = (performance as {
      memory?: {
        usedJSHeapSize: number;
        totalJSHeapSize: number;
      };
    }).memory;

    return {
      capturedAt: new Date().toISOString(),
      rssMb,
      childProcessCount,
      jsHeapUsedMb: perfMemory ? perfMemory.usedJSHeapSize / 1024 / 1024 : null,
      jsHeapTotalMb: perfMemory ? perfMemory.totalJSHeapSize / 1024 / 1024 : null,
      domNodes: typeof document.querySelectorAll === 'function' ? document.querySelectorAll('*').length : 0,
    };
  }

  private markNativeCaptureLeakStage(stage: NativeShareLeakStage): void {
    if (!this.nativeCaptureLeakSession) return;
    if (!this.nativeCaptureLeakSession.summary.stages[stage]) {
      this.nativeCaptureLeakSession.summary.stages[stage] = new Date().toISOString();
    }
    if (
      stage === 'first_js_frame_seen' &&
      this.nativeCaptureLeakSession.summary.counters.firstFrameLatencyMs === null
    ) {
      this.nativeCaptureLeakSession.summary.counters.firstFrameLatencyMs =
        Date.now() - this.nativeCaptureLeakSession.startedAtMs;
    }
    console.log(
      LOG,
      `[share-leak] session=${this.nativeCaptureLeakSession.summary.shareSessionId} stage=${stage}`,
    );
  }

  private getShareLeakSenderReuseDiagnostics(): ShareLeakSenderReuseDiagnostics | null {
    return this.nativeCaptureLeakSession?.summary.senderReuseDiagnostics ?? null;
  }

  private recordShareLeakSenderReuseEvent(
    name: 'publish_started' | 'publish_snapshot_captured' | 'reuse_inferred',
    detail: string,
  ): void {
    const diagnostics = this.getShareLeakSenderReuseDiagnostics();
    if (!diagnostics) return;
    diagnostics.events.push({
      capturedAt: new Date().toISOString(),
      name,
      detail,
    });
  }

  private noteShareLeakPublishStart(): void {
    const session = this.nativeCaptureLeakSession;
    const diagnostics = this.getShareLeakSenderReuseDiagnostics();
    if (!session || !diagnostics) return;

    const prePublishSnapshot = this.captureBrowserWebRtcSnapshot();
    diagnostics.reuseExpected = hasReusableInactiveVideoTransceiver(prePublishSnapshot);
    const senderCount = prePublishSnapshot?.senderCount ?? 'n/a';
    const transceiverCount = prePublishSnapshot?.transceiverCount ?? 'n/a';

    this.recordShareLeakSenderReuseEvent(
      'publish_started',
      `sender_count=${senderCount} transceivers=${transceiverCount} reusable_inactive_video_transceiver=${diagnostics.reuseExpected}`,
    );
    console.log(
      LOG,
      `[share-leak] session=${session.summary.shareSessionId} publish_start sender_count=${senderCount} transceivers=${transceiverCount} reusable_inactive_video_transceiver=${diagnostics.reuseExpected}`,
    );
  }

  private captureShareLeakPublishDiagnostics(): void {
    const session = this.nativeCaptureLeakSession;
    const diagnostics = this.getShareLeakSenderReuseDiagnostics();
    if (!session || !diagnostics) return;

    const publishSnapshot = this.captureBrowserWebRtcSnapshot();
    diagnostics.publishWebRtcSnapshot = publishSnapshot;
    const trackId = publishSnapshot?.publicationTrackId ?? null;
    const peerConnection = this.getPublisherPeerConnection();
    const publishSender = trackId && peerConnection
      ? peerConnection.getSenders().find((sender) => sender.track?.id === trackId) ?? null
      : null;
    diagnostics.degradationPreferenceResult = getShareSenderDegradationPreferenceResult(publishSender);
    const publishTransceiver = publishSnapshot?.transceivers.find(
      (transceiver) => transceiver.senderTrackId === trackId,
    );
    const senderCount = publishSnapshot?.senderCount ?? 'n/a';
    const videoSenderCount = publishSnapshot?.videoSenderCount ?? 'n/a';
    const transceiverCount = publishSnapshot?.transceiverCount ?? 'n/a';
    const publishMid = publishTransceiver?.mid ?? 'n/a';

    this.recordShareLeakSenderReuseEvent(
      'publish_snapshot_captured',
      `sender_count=${senderCount} video_senders=${videoSenderCount} transceivers=${transceiverCount} publication_track_id=${trackId ?? 'n/a'} publish_mid=${publishMid}`,
    );
    console.log(
      LOG,
      `[share-leak] session=${session.summary.shareSessionId} publish_snapshot sender_count=${senderCount} video_senders=${videoSenderCount} transceivers=${transceiverCount} publication_track_id=${trackId ?? 'n/a'} publish_mid=${publishMid}`,
    );

    this.recordShareLeakSenderReuseEvent(
      'reuse_inferred',
      `reusable_inactive_video_transceiver=${diagnostics.reuseExpected} publish_mid=${publishMid}`,
    );
    console.log(
      LOG,
      `[share-leak] session=${session.summary.shareSessionId} reuse_inferred reusable_inactive_video_transceiver=${diagnostics.reuseExpected} publish_mid=${publishMid}`,
    );

    if (diagnostics.degradationPreferenceResult) {
      const { senderWasReused, attemptedPreferences, invalidStateSkipped, finalErrorName } =
        diagnostics.degradationPreferenceResult;
      console.log(
        LOG,
        `[share-leak] session=${session.summary.shareSessionId} degradation_preference sender_was_reused=${senderWasReused} attempted_preferences=${attemptedPreferences.join('|') || 'none'} invalid_state_skipped=${invalidStateSkipped} final_error_name=${finalErrorName ?? 'none'}`,
      );
    }
  }

  private async finalizeNativeCaptureLeakSession(): Promise<void> {
    const session = this.nativeCaptureLeakSession;
    if (!session) return;

    const baselineRaw = await session.baselineRawPromise;
    const cleanupRaw = await this.captureRawShareLeakMemorySnapshot();
    const summary = session.summary;
    summary.baselineMemory = toShareLeakMemorySample(baselineRaw, baselineRaw);
    summary.activeMemory = session.activeRaw
      ? toShareLeakMemorySample(session.activeRaw, baselineRaw)
      : null;
    summary.cleanupMemory = toShareLeakMemorySample(cleanupRaw, baselineRaw);
    if (session.stopRequestedAtMs !== null) {
      summary.counters.stopCleanupLatencyMs = Date.now() - session.stopRequestedAtMs;
    }
    summary.endedAt = cleanupRaw.capturedAt;
    if (!summary.stages.session_closed) {
      summary.stages.session_closed = cleanupRaw.capturedAt;
    }
    const cleanupRssDelta = summary.cleanupMemory?.deltaRssMb ?? 'n/a';
    const cleanupHeapDelta = summary.cleanupMemory?.deltaJsHeapUsedMb ?? 'n/a';
    const afterStopSenders = summary.browserWebRtcAfterStop?.screenShareSenderCount ?? 'n/a';
    console.log(
      LOG,
      `[share-leak] session_closed metrics session=${summary.shareSessionId} backend=${summary.captureBackend} cleanup_rss_delta_mb=${cleanupRssDelta} cleanup_heap_delta_mb=${cleanupHeapDelta} after_stop_screen_share_senders=${afterStopSenders}`,
    );
    console.log(LOG, '[share-leak] session_closed summary_json', summary);
    this.callbacks.onShareLeakSummary?.(summary);
    this.nativeCaptureLeakSession = null;
  }

  /* ─── Preset → Capture/Publish Sync ──────────────────────────── */

  /**
   * Derive capture profile and publish options from the active quality preset.
   * Called before every screen share start/restart so the initial getDisplayMedia
   * constraints and LiveKit encoding match the user's selected preset.
   */
  private syncProfileFromPreset(): void {
    const preset = QUALITY_PRESETS[this.currentQuality];
    this.currentCaptureProfile = {
      ...this.currentCaptureProfile,
      resolution: { ...preset.resolution },
      frameRate: preset.maxFramerate,
      contentHint: preset.contentHint,
    };
    this.currentPublishOptions = {
      ...this.currentPublishOptions,
      screenShareEncoding: {
        maxBitrate: preset.maxBitrate,
        maxFramerate: preset.maxFramerate,
      },
      degradationPreference: 'maintain-resolution',
    };
  }

  /* ─── AudioContext ────────────────────────────────────────────── */

  private ensureAudioContext(): AudioContext {
    if (!this.audioContext) {
      this.audioContext = new AudioContext();
      this.masterGain = this.audioContext.createGain();
      this.masterGain.gain.setValueAtTime(perceptualGain(70), this.audioContext.currentTime);

      // Route masterGain directly to AudioContext.destination.
      // AudioContext.setSinkId() is called with a real Chromium hex deviceId (resolved
      // from the WASAPI label via the prototype-captured enumerateDevices) to route all
      // Web Audio output to the user's selected output device.
      this.masterGain.connect(this.audioContext.destination);
      if (DEBUG_AUDIO_OUTPUT || DEBUG_SHARE_AUDIO || DEBUG_SHARE_TRACK_SUB) {
        console.log(LOG, '[audio-output] shared AudioContext created', {
          sampleRate: this.audioContext.sampleRate,
          state: this.audioContext.state,
        });
      }
    }
    if (this.audioContext.state === 'suspended') {
      // Try to resume immediately — in Tauri (WebView2/WebKit) the autoplay
      // policy is often more relaxed than in a browser tab, so this may
      // succeed without a user gesture.
      this.audioContext.resume().then(() => {
        if (this.audioContext?.state === 'running') {
          this.callbacks.onSystemEvent('audio context resumed');
        }
      }).catch(() => { /* ignore — fallback to gesture listener below */ });

      // Also register a gesture listener as fallback in case resume() fails
      if (!this.gestureListener) {
        const resume = () => {
          this.audioContext?.resume().then(() => {
            this.callbacks.onSystemEvent('audio context resumed');
          });
          document.removeEventListener('click', resume);
          document.removeEventListener('keydown', resume);
          this.gestureListener = null;
        };
        document.addEventListener('click', resume, { once: true });
        document.addEventListener('keydown', resume, { once: true });
        this.gestureListener = resume;
        this.callbacks.onSystemEvent('audio context suspended — click or press a key to enable audio');
      }
    }
    return this.audioContext;
  }

  /**
   * Route the Web Audio context (and the LiveKit room) to the saved output
   * device so that Wavis audio never plays through the system default endpoint
   * (which gets captured by screen share loopback).
   *
   * Called at three points so the routing is always active:
   *   1. When the room connects (and on every reconnect).
   *   2. When system audio devices change (devicechange event).
   *   3. When the user manually selects an output device.
   *
   * No-op when no device has been saved yet, or when setSinkId is unavailable.
   */
  private async setAudioContextSinkId(deviceId: string): Promise<boolean> {
    if (!this.audioContext || !('setSinkId' in this.audioContext)) {
      if (DEBUG_AUDIO_OUTPUT) {
        console.warn(LOG, '[audio-output] AudioContext.setSinkId NOT SUPPORTED — Wavis audio cannot be routed away from system default');
      }
      return false;
    }

    try {
      // eslint-disable-next-line @typescript-eslint/no-explicit-any
      await (this.audioContext as any).setSinkId(deviceId);
      if (DEBUG_AUDIO_OUTPUT) {
        const displayId = deviceId === '' ? '(default)' : deviceId.slice(0, 16) + '…';
        console.log(LOG, '[audio-output] audioContext.setSinkId SUCCESS — audio now routed to:', displayId);
      }
      return true;
    } catch (err) {
      console.warn(LOG, '[audio-output] audioContext.setSinkId FAILED:', err instanceof Error ? err.message : String(err));
      return false;
    }
  }

  private async resolveBrowserOutputDeviceIdFromLabel(deviceLabel: string): Promise<string | null> {
    const browserEnumerateDevices = getBrowserEnumerateDevices();
    if (!browserEnumerateDevices) return null;

    try {
      const realDevices = await browserEnumerateDevices();
      if (DEBUG_AUDIO_OUTPUT) {
        const realOutputs = realDevices.filter(d => d.kind === 'audiooutput');
        console.log(LOG, '[audio-output] real browser output devices:', realOutputs.map(d => `"${d.label}" (${d.deviceId.slice(0, 16)}…)`));
      }
      const match = realDevices.find(
        (d) => d.kind === 'audiooutput' && audioOutputLabelsMatch(d.label, deviceLabel),
      );
      return match?.deviceId ?? null;
    } catch (err) {
      if (DEBUG_AUDIO_OUTPUT) {
        console.warn(LOG, '[audio-output] browser enumerateDevices failed:', err instanceof Error ? err.message : String(err));
      }
      return null;
    }
  }

  private async resolveSavedAudioOutputDeviceId(savedId: string): Promise<string> {
    const deviceLabel = savedId.startsWith('output:') ? savedId.slice('output:'.length) : savedId;
    const resolvedId = await this.resolveBrowserOutputDeviceIdFromLabel(deviceLabel);
    if (resolvedId) {
      if (DEBUG_AUDIO_OUTPUT) {
        console.log(LOG, `[audio-output] resolved saved output "${deviceLabel}" → real deviceId: ${resolvedId.slice(0, 16)}…`);
      }
      return resolvedId;
    }

    if (DEBUG_AUDIO_OUTPUT) {
      console.warn(LOG, `[audio-output] no real device matched label "${deviceLabel}" — falling back to savedId`);
    }
    return savedId;
  }

  private async resolveCoreAudioOutputDeviceId(coreAudioUid: string, deviceName?: string | null): Promise<string | null> {
    const patchedDevices = await navigator.mediaDevices.enumerateDevices().catch(() => []);
    const nativeMatch = patchedDevices.find(
      (device) =>
        device.kind === 'audiooutput' &&
        (device.deviceId === coreAudioUid || device.groupId === coreAudioUid),
    );

    if (nativeMatch) {
      return this.resolveBrowserOutputDeviceIdFromLabel(nativeMatch.label);
    }

    // CoreAudio UIDs (e.g. "~:AMS2_StackedOutput:0") don't match browser deviceId/groupId
    // on the bypass path. Fall back to label matching using the device name from Rust.
    if (deviceName) {
      if (DEBUG_AUDIO_OUTPUT) {
        console.log(LOG, '[audio-output] CoreAudio UID not matched in enumerateDevices; trying label fallback for:', deviceName);
      }
      return this.resolveBrowserOutputDeviceIdFromLabel(deviceName);
    }

    return null;
  }

  private async pinShareAudioOutputToRealDevice(coreAudioUid: string, deviceName?: string | null): Promise<void> {
    const resolvedDeviceId = await this.resolveCoreAudioOutputDeviceId(coreAudioUid, deviceName);
    if (!resolvedDeviceId) {
      console.warn(
        LOG,
        '[share-audio] no browser audiooutput matched CoreAudio UID; skipping AudioContext pinning:',
        coreAudioUid,
      );
      return;
    }

    const didPin = await this.setAudioContextSinkId(resolvedDeviceId);
    if (didPin) {
      this.sharePinnedAudioOutputDeviceId = resolvedDeviceId;
      if (DEBUG_SHARE_AUDIO) {
        console.log(LOG, '[share-audio] room audio pinned to real output device:', resolvedDeviceId.slice(0, 16) + '…');
      }
    }
  }

  private async restoreAudioOutputDeviceAfterShare(): Promise<void> {
    const hadPinnedOutput = this.sharePinnedAudioOutputDeviceId !== null;
    this.sharePinnedAudioOutputDeviceId = null;

    if (!hadPinnedOutput) return;

    const savedId = await getAudioOutputDevice();
    if (savedId) {
      await this.applyAudioOutputDevice();
      return;
    }

    await this.setAudioContextSinkId('');
  }

  private async applyAudioOutputDevice(): Promise<void> {
    if (this.sharePinnedAudioOutputDeviceId) {
      if (DEBUG_AUDIO_OUTPUT) {
        console.log(LOG, '[audio-output] share override active — keeping room audio pinned to:', this.sharePinnedAudioOutputDeviceId.slice(0, 16) + '…');
      }
      await this.setAudioContextSinkId(this.sharePinnedAudioOutputDeviceId);
      return;
    }

    const savedId = await getAudioOutputDevice();

    if (DEBUG_AUDIO_OUTPUT) {
      console.log(LOG, '[audio-output] applyAudioOutputDevice called — saved deviceId:', savedId ?? '(none)');
    }

    if (!savedId) {
      if (DEBUG_AUDIO_OUTPUT) {
        console.log(LOG, '[audio-output] no saved output device — using system default');
      }
      return;
    }

    const preferredResolvedId = await this.resolveSavedAudioOutputDeviceId(savedId);
    await this.setAudioContextSinkId(preferredResolvedId);

    if (this.room) {
      try {
        await this.room.switchActiveDevice('audiooutput', savedId);
        if (DEBUG_AUDIO_OUTPUT) {
          console.log(LOG, '[audio-output] room.switchActiveDevice SUCCESS');
        }
      } catch (err) {
        console.warn(LOG, '[audio-output] room.switchActiveDevice FAILED:', err instanceof Error ? err.message : String(err));
      }
    } else if (DEBUG_AUDIO_OUTPUT) {
      console.log(LOG, '[audio-output] room not ready — skipping switchActiveDevice (will retry on connect)');
    }
    return;

    /*
    // Route the Web Audio context to the selected output device.
    // Tauri's native media module patches navigator.mediaDevices.enumerateDevices at the
    // *instance* level to return WASAPI endpoint IDs (e.g. "output:Altavoces (Razer...)").
    // AudioContext.setSinkId() uses the WebView2 audio engine which only accepts real
    // Chromium hex deviceIds. We captured MediaDevices.prototype.enumerateDevices at module
    // load time (before any patching) to resolve the real hex deviceId by matching label.
    // The browser appends a USB vendor:product suffix to the label, so we use startsWith.
    let resolvedId = savedId;
    if (_realEnumerateDevices) {
      try {
        const realDevices = await _realEnumerateDevices();
        if (DEBUG_AUDIO_OUTPUT) {
          const realOutputs = realDevices.filter(d => d.kind === 'audiooutput');
          console.log(LOG, '[audio-output] real browser output devices:', realOutputs.map(d => `"${d.label}" (${d.deviceId.slice(0, 16)}…)`));
        }
        // WASAPI IDs are prefixed with "output:" — strip it to get the base device label.
        // The real browser label may have a USB vendor:product suffix appended (e.g. " (1532:0552)"),
        // so match with startsWith rather than strict equality.
        const wasapiLabel = savedId.startsWith('output:') ? savedId.slice('output:'.length) : savedId;
        const match = realDevices.find(
          d => d.kind === 'audiooutput' && d.label.startsWith(wasapiLabel),
        );
        if (match) {
          resolvedId = match.deviceId;
          if (DEBUG_AUDIO_OUTPUT) {
            console.log(LOG, `[audio-output] resolved WASAPI label "${wasapiLabel}" → real deviceId: ${resolvedId.slice(0, 16)}… (browser label: "${match.label}")`);
          }
        } else {
          if (DEBUG_AUDIO_OUTPUT) {
            console.warn(LOG, `[audio-output] no real device matched label "${wasapiLabel}" — falling back to savedId`);
          }
        }
      } catch (err) {
        if (DEBUG_AUDIO_OUTPUT) {
          console.warn(LOG, '[audio-output] _realEnumerateDevices failed:', err instanceof Error ? err.message : String(err));
        }
      }
    }

    if (this.audioContext && 'setSinkId' in this.audioContext) {
      try {
        // eslint-disable-next-line @typescript-eslint/no-explicit-any
        await (this.audioContext as any).setSinkId(resolvedId);
        if (DEBUG_AUDIO_OUTPUT) {
          console.log(LOG, '[audio-output] audioContext.setSinkId SUCCESS — audio now routed to:', resolvedId.slice(0, 16) + '…');
        }
      } catch (err) {
        console.warn(LOG, '[audio-output] audioContext.setSinkId FAILED:', err instanceof Error ? err.message : String(err));
      }
    } else if (DEBUG_AUDIO_OUTPUT) {
      console.warn(LOG, '[audio-output] AudioContext.setSinkId NOT SUPPORTED — Wavis audio cannot be routed away from system default');
    }

    // Also tell LiveKit so any elements it manages internally follow the same device.
    if (this.room) {
      try {
        await this.room.switchActiveDevice('audiooutput', savedId);
        if (DEBUG_AUDIO_OUTPUT) {
          console.log(LOG, '[audio-output] room.switchActiveDevice SUCCESS');
        }
      } catch (err) {
        console.warn(LOG, '[audio-output] room.switchActiveDevice FAILED:', err instanceof Error ? err.message : String(err));
      }
    } else if (DEBUG_AUDIO_OUTPUT) {
      console.log(LOG, '[audio-output] room not ready — skipping switchActiveDevice (will retry on connect)');
    }
    */
  }
  /**
   * Apply the user's saved audio input device to the active LiveKit room.
   * Mirrors applyAudioOutputDevice() — strips the "input:" WASAPI prefix,
   * resolves the real Chromium deviceId by matching label, then calls
   * room.switchActiveDevice('audioinput', resolvedId).
   * No-op when no device has been saved yet or when the room is not connected.
   */
  private async applyAudioInputDevice(): Promise<void> {
    if (!this.room) return;
    const savedId = await getAudioInputDevice();
    if (!savedId) return;

    let resolvedId = savedId;
    if (_realEnumerateDevices) {
      try {
        const realDevices = await _realEnumerateDevices();
        const wasapiLabel = savedId.startsWith('input:') ? savedId.slice('input:'.length) : savedId;
        const match = realDevices.find(
          (d) => d.kind === 'audioinput' && d.label.startsWith(wasapiLabel),
        );
        if (match) {
          resolvedId = match.deviceId;
        }
      } catch {
        // ignore — fall back to savedId
      }
    }

    try {
      await this.room.switchActiveDevice('audioinput', resolvedId);
    } catch (err) {
      console.warn(LOG, '[audio-input] switchActiveDevice FAILED:', err instanceof Error ? err.message : String(err));
    }
  }

  /**
   * Register a devicechange listener that re-applies the saved output device
   * any time the OS audio device list changes (e.g. headphones plugged in/out).
   * Idempotent — only registers once per session.
   */
  private startDeviceChangeWatcher(): void {
    if (this.deviceChangeListener) return;
    const handler = () => {
      if (DEBUG_AUDIO_OUTPUT) {
        navigator.mediaDevices.enumerateDevices().then((devices) => {
          const outputs = devices.filter(d => d.kind === 'audiooutput');
          console.log(LOG, '[audio-output] devicechange event — output devices now:', outputs.map(d => `"${d.label}" (${d.deviceId.slice(0, 8)}…)`));
        }).catch(() => {});
      }
      this.applyAudioOutputDevice();
    };
    navigator.mediaDevices.addEventListener('devicechange', handler);
    this.deviceChangeListener = handler;
    if (DEBUG_AUDIO_OUTPUT) {
      console.log(LOG, '[audio-output] devicechange watcher registered');
    }
  }

  /** Unregister the devicechange listener (called on disconnect). */
  private stopDeviceChangeWatcher(): void {
    if (!this.deviceChangeListener) return;
    navigator.mediaDevices.removeEventListener('devicechange', this.deviceChangeListener);
    this.deviceChangeListener = null;
  }

  /** Connect to LiveKit SFU. Creates Room, connects, publishes mic. */
  async connect(sfuUrl: string, token: string): Promise<void> {
    try {
      // 0. Read denoise preference for this session.
      const denoiseEnabled = await getDenoiseEnabled();
      const useNativeBridge = this.shouldUseNativeMicBridge(denoiseEnabled);
      this.jsDenoise = this.shouldUseJsNoiseSuppression(denoiseEnabled);
      this.setNoiseSuppressionActive(false);
      console.log(
        NS_LOG,
        `session start platform=${isWindows() ? 'windows' : isMac() ? 'mac' : 'other'} denoise_pref=${denoiseEnabled} native_bridge_bypassed=${!useNativeBridge} js_processor_enabled=${this.jsDenoise}`,
      );

      // 1. Create Room
      this.room = new Room({
        adaptiveStream: true,
        dynacast: false,          // Disable dynacast — with ≤6 participants it aggressively
                                  // downgrades screen share based on transient congestion
                                  // and doesn't recover well. Audio-only rooms don't benefit
                                  // from dynacast anyway (single video = screen share).
      });

      // 2. Eagerly init AudioContext before connecting
      this.ensureAudioContext();

      // 3. Helper to register listeners with cleanup tracking
      // eslint-disable-next-line @typescript-eslint/no-explicit-any
      const addListener = (event: RoomEvent, handler: (...args: any[]) => void) => {
        this.room!.on(event, handler as (...args: unknown[]) => void);
        this.listeners.push({ event, handler: handler as (...args: unknown[]) => void });
      };

      // 4. Connection gating flags
      let lkConnected = false;
      let micReady = false;
      let listenOnly = false;
      let mediaConnectedFired = false;

      const checkReady = () => {
        if (mediaConnectedFired) return;
        if (lkConnected && (micReady || listenOnly)) {
          mediaConnectedFired = true;
          this.callbacks.onMediaConnected();
          // Apply output device here — getUserMedia has already completed at this
          // point so the browser has audio permission and setSinkId will succeed.
          // Calling it earlier (RoomEvent.Connected) always fails because getUserMedia
          // hasn't run yet and setSinkId requires an active audio permission grant.
          if (DEBUG_AUDIO_OUTPUT) console.log(LOG, '[audio-output] checkReady — mic ready, applying output device');
          this.applyAudioOutputDevice();
          this.applyAudioInputDevice();
        }
      };

      // 5. Register all event listeners

      // a. Connected
      addListener(RoomEvent.Connected, () => {
        if (this.disposed) return;
        lkConnected = true;
        // Watch for OS device changes so the routing survives plug/unplug events.
        // Output device routing itself is applied in checkReady() after getUserMedia.
        this.startDeviceChangeWatcher();
        if (useNativeBridge) {
          // Windows + denoiseEnabled: capture mic in Rust through DenoiseFilter,
          // then publish the denoised MediaStreamTrack via LiveKit.
          this.startNativeMicBridge(denoiseEnabled)
            .catch((err: unknown) => {
              // Bridge failed — fall back to browser mic without denoise.
              console.warn(LOG, 'native mic bridge failed, falling back to browser mic:', err);
              this.callbacks.onSystemEvent(
                `native mic bridge failed — falling back to browser mic (no denoise)`,
              );
              return this.room?.localParticipant.setMicrophoneEnabled(true, {
                noiseSuppression: false,
              });
            })
            .catch((err: unknown) => {
              listenOnly = true;
              this.callbacks.onSystemEvent(
                `mic failed: ${err instanceof Error ? err.message : String(err)} — listen-only mode`,
              );
              if (DEBUG_AUDIO_OUTPUT) console.log(LOG, '[audio-output] listen-only mode — applying output device (best-effort)');
              this.applyAudioOutputDevice();
              checkReady();
            });
        } else {
          this.room!.localParticipant.setMicrophoneEnabled(true, {
            noiseSuppression: false,
          }).catch((err: unknown) => {
            listenOnly = true;
            this.callbacks.onSystemEvent(
              `mic permission denied: ${err instanceof Error ? err.message : String(err)} — listen-only mode`,
            );
            if (DEBUG_AUDIO_OUTPUT) console.log(LOG, '[audio-output] listen-only mode — applying output device (best-effort)');
            this.applyAudioOutputDevice();
            checkReady();
          });
        }
      });

      // b. Disconnected
      addListener(RoomEvent.Disconnected, () => {
        if (this.disposed) return;
        this.callbacks.onMediaDisconnected();
      });

      // c. Reconnecting
      addListener(RoomEvent.Reconnecting, () => {
        if (this.disposed) return;
        this.callbacks.onSystemEvent('LiveKit reconnecting…');
      });

      addListener(RoomEvent.Reconnecting, () => {
        if (this.disposed || !this.hasActiveScreenShare()) return;
        console.log(LOG, `screen share stopped due to LiveKit reconnect — ts: ${Date.now()}`);
        this.stopScreenShare()
          .catch(() => {
            this.clearScreenShareRuntimeState();
          });
        this.callbacks.onSystemEvent('Screen share stopped due to reconnect');
      });

      // d. Reconnected
      addListener(RoomEvent.Reconnected, () => {
        if (this.disposed) return;
        this.callbacks.onSystemEvent('LiveKit reconnected');
        // Re-apply output device routing after reconnect — the room's internal
        // audio elements are recreated and lose the previous sinkId.
        if (DEBUG_AUDIO_OUTPUT) console.log(LOG, '[audio-output] RoomEvent.Reconnected — re-applying output device');
        this.applyAudioOutputDevice();
        this.applyAudioInputDevice();
      });

      // e0. TrackPublished — diagnostic (debug only): log all publications to trace
      // whether ScreenShareAudio reaches the viewer.
      if (DEBUG_SHARE_TRACK_SUB) {
        addListener(RoomEvent.TrackPublished, (
          publication: RemoteTrackPublication,
          participant: RemoteParticipant,
        ) => {
          console.log(LOG, `[diag] TrackPublished — participant: ${participant.identity}, source: ${publication.source}, kind: ${publication.kind}, sid: ${publication.trackSid}`);
        });
      }

      addListener(RoomEvent.TrackPublished, (
        publication: RemoteTrackPublication,
        participant: RemoteParticipant,
      ) => {
        if (this.disposed) return;
        this.syncParticipantMicMute(participant, publication);
        if (publication.source !== Track.Source.ScreenShareAudio) return;
        this.screenShareAudioPublications.set(participant.identity, publication);
        if (!this.screenShareAudioPending.has(participant.identity)) {
          publication.setSubscribed(false);
        }
      });

      // e. TrackSubscribed
      addListener(RoomEvent.TrackSubscribed, (
        track: RemoteTrack,
        publication: RemoteTrackPublication,
        participant: RemoteParticipant,
      ) => {
        if (this.disposed) return;
        this.syncParticipantMicMute(participant, publication);
        if (track.kind === Track.Kind.Audio) {
          // Defer screen share audio — only attach when user opens the viewer
          if (this.isDeferredScreenShareAudioTrack(participant, publication, track)) {
            if (publication.source === Track.Source.ScreenShareAudio) {
              this.screenShareAudioPublications.set(participant.identity, publication);
            }
            this.screenShareAudioTracks.set(participant.identity, { track, participant });
            const isPending = this.screenShareAudioPending.has(participant.identity);
            if (!isPending && typeof publication.setSubscribed === 'function') {
              publication.setSubscribed(false);
            }
            console.log(LOG, `[mac-share-audio] TrackSubscribed ScreenShareAudio — identity=${participant.identity} muted=${track.isMuted} readyState=${track.mediaStreamTrack.readyState} enabled=${track.mediaStreamTrack.enabled} isPending=${isPending}`);
            console.log(LOG, `deferred screen share audio for ${participant.identity}`);
            if (DEBUG_SHARE_TRACK_SUB || DEBUG_SHARE_AUDIO) {
              const mst = track.mediaStreamTrack;
              const settings = typeof mst.getSettings === 'function' ? mst.getSettings() : undefined;
              console.log(LOG, '[screen-share-audio] TrackSubscribed diagnostics', {
                participantIdentity: participant.identity,
                trackSid: track.sid,
                publicationTrackSid: publication.trackSid,
                streamState: publication.track ? publication.track.streamState : undefined,
                isMuted: publication.isMuted,
                audioContextSampleRate: this.audioContext?.sampleRate ?? null,
                mediaStreamTrackId: mst.id,
                mediaStreamTrackLabel: mst.label,
                mediaStreamTrackReadyState: mst.readyState,
                mediaStreamTrackMuted: mst.muted,
                settings,
              });
            }
            // If a viewer already has this participant's window open, attach now
            if (isPending) {
              this.attachScreenShareAudio(participant.identity);
            }
          } else {
            this.attachAudioTrack(participant, track);
          }
        } else if (track.kind === Track.Kind.Video && publication.source === Track.Source.ScreenShare) {
          // Force the track to stay enabled — with adaptiveStream: true,
          // LiveKit pauses video tracks not attached to a visible <video>
          // element. We pipe screen shares through a WebRTC loopback bridge
          // to a child window, so the track is never in the main window DOM.
          publication.setEnabled(true);

          const stream = new MediaStream([track.mediaStreamTrack]);

          // Attach a hidden <video> element so LiveKit's adaptive stream
          // considers this track "consumed" and keeps sending frames.
          const dummyVideo = document.createElement('video');
          dummyVideo.srcObject = stream;
          dummyVideo.muted = true;
          dummyVideo.style.cssText = 'position:fixed;top:-9999px;left:-9999px;width:1px;height:1px;pointer-events:none;opacity:0;';
          document.body.appendChild(dummyVideo);
          dummyVideo.play().catch(() => {});

          this.screenShareElements.set(participant.identity, {
            stream,
            startedAtMs: Date.now(),
            trackSid: track.sid ?? '',
            dummyVideo,
            trackEndedCleanup: this.monitorScreenShareTrack(participant, publication, track),
          });
          if (DEBUG_CAPTURE) console.log(LOG, `screen share subscribed for ${participant.identity} — trackSid: ${track.sid}, readyState: ${track.mediaStreamTrack.readyState} [initial subscription]`);
          this.callbacks.onScreenShareSubscribed(participant.identity, stream);
          // Also mark participant as sharing (for late joiners who get TrackSubscribed before share_state)
        }
      });

      // f. TrackUnsubscribed
      addListener(RoomEvent.TrackUnsubscribed, (
        track: RemoteTrack,
        publication: RemoteTrackPublication,
        participant: RemoteParticipant,
      ) => {
        if (this.disposed) return;
        if (track.kind === Track.Kind.Audio) {
          if (this.isDeferredScreenShareAudioTrack(participant, publication, track)) {
            // Clean up deferred screen share audio
            this.screenShareAudioTracks.delete(participant.identity);
            // Also clean up if it was attached
            this.cleanupParticipantAudio(`${participant.identity}:screen-share`);
          } else {
            this.cleanupParticipantAudio(participant.identity);
          }
        } else if (track.kind === Track.Kind.Video && publication.source === Track.Source.ScreenShare) {
          const entry = this.screenShareElements.get(participant.identity);
          if (entry && entry.trackSid === track.sid) {
            entry.trackEndedCleanup?.();
            if (entry.dummyVideo) {
              entry.dummyVideo.srcObject = null;
              entry.dummyVideo.remove();
            }
            this.screenShareElements.delete(participant.identity);
            this.callbacks.onScreenShareUnsubscribed(participant.identity);
          }
        }
      });

      // g. ActiveSpeakersChanged
      addListener(RoomEvent.ActiveSpeakersChanged, (speakers: Participant[]) => {
        if (this.disposed) return;
        // Write to pending levels for rAF coalescing
        for (const speaker of speakers) {
          this.pendingLevels.set(speaker.identity, { isSpeaking: true, rmsLevel: 1.0 });
        }
        this.scheduleAudioLevelFlush();
        // Also fire the direct callback for speaking resolution precedence
        this.callbacks.onActiveSpeakers(speakers.map(s => s.identity));
        // Feed the local level indicator from server-reported data.
        // At < 100% the startLocalMicMonitor interval provides smoother 50ms updates;
        // this fallback drives the self-indicator at 100% with no Web Audio tap.
        const localLevel = this.room?.localParticipant?.audioLevel ?? 0;
        this.callbacks.onLocalAudioLevel(localLevel);
      });

      // h. ParticipantDisconnected
      addListener(RoomEvent.ParticipantDisconnected, (participant: RemoteParticipant) => {
        if (this.disposed) return;
        this.cleanupParticipantAudio(participant.identity);
        // Clean up deferred screen share audio
        this.screenShareAudioTracks.delete(participant.identity);
        this.screenShareAudioPublications.delete(participant.identity);
        this.cleanupParticipantAudio(`${participant.identity}:screen-share`);
        if (this.screenShareElements.has(participant.identity)) {
          const entry = this.screenShareElements.get(participant.identity);
          entry?.trackEndedCleanup?.();
          if (entry?.dummyVideo) {
            entry.dummyVideo.srcObject = null;
            entry.dummyVideo.remove();
          }
          this.screenShareElements.delete(participant.identity);
          this.callbacks.onScreenShareUnsubscribed(participant.identity);
        }
      });

      // i. ParticipantConnected — recover screen share audio missed by the
      // "not present" race (track-added signal arrived before participant-added).
      // Fires for existing participants on room join as well as new joiners.
      addListener(RoomEvent.ParticipantConnected, (participant: RemoteParticipant) => {
        if (this.disposed) return;
        const micPub = participant.getTrackPublication(Track.Source.Microphone);
        this.syncParticipantMicMute(participant, micPub);
        const screenShareAudioPub = participant.getTrackPublication(Track.Source.ScreenShareAudio);
        if (screenShareAudioPub) {
          this.screenShareAudioPublications.set(participant.identity, screenShareAudioPub);
          if (!this.screenShareAudioPending.has(participant.identity)) {
            screenShareAudioPub.setSubscribed(false);
          }
        }
        // Only act if we're already waiting for this participant's audio
        // (viewer window opened before TrackSubscribed could fire).
        if (!this.screenShareAudioPending.has(participant.identity)) return;
        for (const pub of participant.trackPublications.values()) {
          if (pub.source === Track.Source.ScreenShareAudio && pub.track) {
            const track = pub.track as RemoteTrack;
            this.screenShareAudioTracks.set(participant.identity, { track, participant });
            console.log(LOG, `[screen-share-audio] ParticipantConnected recovery for ${participant.identity}`);
            this.attachScreenShareAudio(participant.identity);
            break;
          }
        }
      });

      // j. ConnectionQualityChanged
      addListener(RoomEvent.ConnectionQualityChanged, (_quality: unknown, _participant: Participant) => {
        if (this.disposed) return;
        console.log(LOG, 'connection quality changed', _quality, _participant.identity);
      });

      // j. LocalTrackPublished
      addListener(RoomEvent.LocalTrackPublished, (publication: LocalTrackPublication, _participant: LocalParticipant) => {
        if (this.disposed) return;
        if (publication.track?.kind === Track.Kind.Audio) {
          // Suppress local playback on screen share audio tracks — the LiveKit SDK
          // strips suppressLocalAudioPlayback from getDisplayMedia() options, so we
          // apply it post-capture on the MediaStreamTrack directly.
          if (publication.source === Track.Source.ScreenShareAudio) {
            this.suppressLocalAudioOnTrack(publication.track.mediaStreamTrack);
          } else {
            micReady = true;
            // Capture mst before async block so the closure has it.
            const mst = publication.track.mediaStreamTrack;
            this.localMicTrack = publication.track as LocalAudioTrack;
            this.logNoiseSuppressionCapabilities(mst, 'local_track_published');
            this.syncMicProcessor('local_track_published', { originalTrack: mst }).catch((err) => {
              this.setNoiseSuppressionActive(false);
              console.warn(LOG, 'mic processor attach failed:', err);
              this.callbacks.onSystemEvent(
                `mic processor fallback — using plain mic (${err instanceof Error ? err.message : String(err)})`,
              );
            });
            checkReady();
          }
        }
      });

      // k. LocalTrackUnpublished
      addListener(RoomEvent.LocalTrackUnpublished, (publication: LocalTrackPublication, _participant: LocalParticipant) => {
        if (this.disposed) return;
        if (publication.source === Track.Source.ScreenShare) {
          this.callbacks.onLocalScreenShareEnded();
        }
      });

      // l. MediaDevicesError
      addListener(RoomEvent.MediaDevicesError, (error: Error) => {
        if (this.disposed) return;
        this.callbacks.onSystemEvent(`media device error: ${error.message}`);
      });

      // m. TrackMuted — remote participant muted their audio
      addListener(RoomEvent.TrackMuted, (publication: RemoteTrackPublication, participant: Participant) => {
        if (this.disposed) return;
        if (
          publication.kind === Track.Kind.Audio &&
          publication.source === Track.Source.Microphone &&
          participant !== this.room?.localParticipant
        ) {
          this.callbacks.onParticipantMuteChanged(participant.identity, true);
        }
      });

      // n. TrackUnmuted — remote participant unmuted their audio
      addListener(RoomEvent.TrackUnmuted, (publication: RemoteTrackPublication, participant: Participant) => {
        if (this.disposed) return;
        if (
          publication.kind === Track.Kind.Audio &&
          publication.source === Track.Source.Microphone &&
          participant !== this.room?.localParticipant
        ) {
          this.callbacks.onParticipantMuteChanged(participant.identity, false);
        }
      });

      // o. TrackMuted/TrackUnmuted for LOCAL participant — detect external mute changes
      // (e.g. LiveKit re-enabling mic on reconnect, or OS-level mute)
      addListener(RoomEvent.TrackMuted, (publication: TrackPublication, participant: Participant) => {
        if (this.disposed) return;
        if (
          participant === this.room?.localParticipant &&
          publication.kind === Track.Kind.Audio &&
          publication.source === Track.Source.Microphone
        ) {
          this.callbacks.onParticipantMuteChanged(participant.identity, true);
        }
      });
      addListener(RoomEvent.TrackUnmuted, (publication: TrackPublication, participant: Participant) => {
        if (this.disposed) return;
        if (
          participant === this.room?.localParticipant &&
          publication.kind === Track.Kind.Audio &&
          publication.source === Track.Source.Microphone
        ) {
          this.callbacks.onParticipantMuteChanged(participant.identity, false);
        }
      });

      // p. TrackStreamStateChanged — detect paused/resumed screen share video (adaptive stream)
      addListener(RoomEvent.TrackStreamStateChanged, (
        publication: RemoteTrackPublication,
        streamState: Track.StreamState,
        participant: RemoteParticipant,
      ) => {
        if (this.disposed) return;
        if (
          publication.source === Track.Source.ScreenShare &&
          publication.kind === Track.Kind.Video
        ) {
          if (streamState === Track.StreamState.Paused) {
            console.log(LOG, `screen share paused for ${participant.identity} — trackSid: ${publication.trackSid}, ts: ${Date.now()}`);
            if (DEBUG_CAPTURE) console.log(LOG, `screen share paused — enabled: ${publication.isEnabled}`);
            publication.setEnabled(true);
            // Don't re-emit the stream here — wait for Active state to confirm
            // the track actually resumed. Re-emitting a paused stream causes
            // the viewer to attach a dead MediaStream.
          } else if (streamState === Track.StreamState.Active) {
            console.log(LOG, `screen share resumed for ${participant.identity} — trackSid: ${publication.trackSid}, ts: ${Date.now()}`);
            // Track is confirmed active — re-emit so the viewer can re-attach
            const entry = this.screenShareElements.get(participant.identity);
            if (entry) {
              if (DEBUG_CAPTURE) console.log(LOG, `screen share resumed — streamId: ${entry.stream.id}, re-emitting onScreenShareSubscribed`);
              this.callbacks.onScreenShareSubscribed(participant.identity, entry.stream);
            }
          }
        }
      });

      // 6. Stats polling (10s interval).
      // Every cycle: per-receiver stats (RemoteTrack.receiver.getStats()) for audio quality
      // (jitter buffer, concealment) and video receive (screen share fps, freeze, etc.).
      // Publisher PC polled every other cycle for RTT, bandwidth, and candidate type.
      // RTT, loss, jitter are carried forward so they don't oscillate between cycles.
      if (this.statsInterval !== null) clearInterval(this.statsInterval);
      let pollPublisher = true; // start with a publisher poll on first tick
      this.statsInterval = setInterval(async () => {
        if (this.disposed || !this.room) return;
        try {
          // eslint-disable-next-line @typescript-eslint/no-explicit-any
          const engine = (this.room as any).engine;
          // eslint-disable-next-line @typescript-eslint/no-explicit-any
          const pubTransport: any = engine?.pcManager?.publisher ?? engine?.publisher;

          // Per-receiver stats: call receiver.getStats() directly on each remote track.
          // This bypasses PC topology entirely — works regardless of whether LiveKit
          // uses single-PC (publisher-only) or dual-PC (subscriber-primary/publisher-primary) mode.
          // The subscriber PCTransport is optional and may be absent; per-track receivers are always present.
          let audioReceiverReport: RTCStatsReport | null = null;
          let videoReceiverReport: RTCStatsReport | null = null;
          for (const participant of this.room.remoteParticipants.values()) {
            for (const pub of participant.trackPublications.values()) {
              const remoteTrack = pub.track as RemoteTrack | undefined;
              if (!remoteTrack?.receiver) continue;
              // First remote microphone audio track → jitter buffer, concealment, loss, jitter
              if (!audioReceiverReport && pub.kind === Track.Kind.Audio && pub.source !== Track.Source.ScreenShareAudio) {
                try { audioReceiverReport = await remoteTrack.receiver.getStats(); } catch { /* ignore */ }
              }
              // Screen share video track → video receive stats (fps, decode time, freeze, etc.)
              if (!videoReceiverReport && pub.source === Track.Source.ScreenShare && pub.kind === Track.Kind.Video) {
                try { videoReceiverReport = await remoteTrack.receiver.getStats(); } catch { /* ignore */ }
              }
            }
          }
          if (this.disposed) return;

          if (audioReceiverReport) {
            const sub = extractStatsFromReports([audioReceiverReport]);
            if (sub.jitterBufferDelayMs > 0) this.lastJitterBufferDelayMs = sub.jitterBufferDelayMs;
            const concDelta = sub.concealmentEventsTotal - this.prevConcealmentEventsTotal;
            this.lastConcealmentEventsPerInterval = Math.max(0, concDelta);
            this.prevConcealmentEventsTotal = sub.concealmentEventsTotal;
            if (sub.rttMs > 0) this.lastRttMs = sub.rttMs;
            if (sub.packetLossPercent > 0) this.lastPacketLossPercent = sub.packetLossPercent;
            if (sub.jitterMs > 0) this.lastJitterMs = sub.jitterMs;
          }

          if (videoReceiverReport) {
            this.extractVideoReceiveStats(videoReceiverReport);
          }

          // Poll publisher every other cycle — candidate-pair RTT + bandwidth are most reliable
          // on the publisher PC (always present, always has an active ICE candidate pair).
          if (pollPublisher) {
            // eslint-disable-next-line @typescript-eslint/no-explicit-any
            async function resolveStats(transport: any): Promise<RTCStatsReport | null> {
              if (!transport) return null;
              if (typeof transport.getStats === 'function') return transport.getStats() as Promise<RTCStatsReport>;
              const pc = transport.pc ?? transport._pc;
              if (pc && typeof pc.getStats === 'function') return pc.getStats() as Promise<RTCStatsReport>;
              return null;
            }
            const pubReport = await resolveStats(pubTransport);
            if (this.disposed) return;
            if (pubReport) {
              const pub = extractStatsFromReports([pubReport]);
              if (pub.candidateType !== 'unknown') this.lastCandidateType = pub.candidateType;
              if (pub.availableBandwidthKbps > 0) this.lastAvailableBandwidthKbps = pub.availableBandwidthKbps;
              if (pub.rttMs > 0) this.lastRttMs = pub.rttMs;
              if (pub.packetLossPercent > 0) this.lastPacketLossPercent = pub.packetLossPercent;
              if (pub.jitterMs > 0) this.lastJitterMs = pub.jitterMs;
            }
          }
          pollPublisher = !pollPublisher;

          this.callbacks.onConnectionQuality({
            rttMs: this.lastRttMs,
            packetLossPercent: this.lastPacketLossPercent,
            jitterMs: this.lastJitterMs,
            jitterBufferDelayMs: this.lastJitterBufferDelayMs,
            concealmentEventsPerInterval: this.lastConcealmentEventsPerInterval,
            candidateType: this.lastCandidateType,
            availableBandwidthKbps: this.lastAvailableBandwidthKbps,
          });
        } catch {
          // ignore stats errors
        }
      }, 10_000);

      // 7. Connect to SFU
      await this.room.connect(sfuUrl, token, { autoSubscribe: true });
    } catch (err) {
      this.callbacks.onMediaFailed(err instanceof Error ? err.message : String(err));
    }
  }

  /** Disconnect from LiveKit SFU. Cleans up all resources. Idempotent. */
  disconnect(): void {
    // 1. Idempotent guard
    if (this.disposed) return;

    // 2. Mark as disposed
    this.disposed = true;

    // 3. Cancel pending rAF
    if (this.rafId !== null) cancelAnimationFrame(this.rafId);

    // 4. Clear stats polling
    if (this.statsInterval !== null) clearInterval(this.statsInterval);

    // 4b. Clear post-publish retry timeout
    if (this.postPublishRetryTimeout !== null) clearTimeout(this.postPublishRetryTimeout);

    // 4c. Clear screen share stats polling
    this.stopScreenShareStatsPolling();

    // 4d. Clean up native capture bridge (Windows custom share)
    if (this.nativeCapturePollInterval !== null) {
      clearInterval(this.nativeCapturePollInterval);
      this.nativeCapturePollInterval = null;
    }
    this.nativeCaptureUnlisten = null;
    this.nativeCaptureFrameHandler = null;
    this.nativeCaptureEarlyFrames = [];
    if (this.nativeCapturePublication) {
      const track = this.nativeCapturePublication.track?.mediaStreamTrack;
      if (track) track.stop();
      this.nativeCapturePublication = null;
    }
    if (this.nativeCaptureCanvas) {
      this.nativeCaptureCanvas.remove();
      this.nativeCaptureCanvas = null;
    }

    // 4e. Clear analyser polling
    if (this.analyserInterval !== null) clearInterval(this.analyserInterval);
    this.analyserMap.clear();

    // 4f. Clear local mic monitor
    this.stopLocalMicMonitor();

    // 4g. Clean up WASAPI audio bridge (unlisten Tauri events, disconnect worklet/dest nodes,
    //     close AudioContext). Must happen before room.disconnect() so the track can be
    //     unpublished cleanly. Fire-and-forget is safe: Tauri listeners are unregistered
    //     synchronously inside stopWasapiAudioBridge before the first await.
    this.stopWasapiAudioBridge().catch(() => {});

    // 4g2. Clean up native mic bridge. Tauri listener is unregistered synchronously
    //      inside NativeMicBridge.stop() before the first await.
    if (this.nativeMicBridge) {
      this.nativeMicBridge.stop().catch(() => {});
      this.nativeMicBridge = null;
      this.callbacks.onNativeMicBridgeState?.(false);
    }

    // 4h. Destroy mic processor
    this.micAudioProcessor?.destroy().catch(() => {});
    this.micAudioProcessor = null;
    this.setNoiseSuppressionActive(false);
    this.localMicTrack = null;

    // 5. Room cleanup (null-safe — room may never have been assigned)
    if (this.room !== null) {
      for (const entry of this.listeners) {
        this.room.off(entry.event, entry.handler);
      }
      this.room.disconnect();
    }

    // 6. Clear listeners registry
    this.listeners = [];

    // 7. Clean up audio elements
    for (const el of this.audioElementMap.values()) {
      el.pause();
      el.srcObject = null;
      el.remove();
    }
    this.audioElementMap.clear();

    // 8. Clear screen share entries and remove dummy video elements
    for (const entry of this.screenShareElements.values()) {
      entry.trackEndedCleanup?.();
      if (entry.dummyVideo) {
        entry.dummyVideo.srcObject = null;
        entry.dummyVideo.remove();
      }
    }
    this.screenShareElements.clear();

    // 8b. Clear deferred screen share audio tracks
    this.screenShareAudioTracks.clear();
    this.screenShareAudioPublications.clear();
    this.screenShareAudioPending.clear();

    // 8c. Clean up any attached screen share audio elements
    for (const key of this.audioElementMap.keys()) {
      if (key.endsWith(':screen-share')) {
        const el = this.audioElementMap.get(key);
        if (el) { el.pause(); el.srcObject = null; el.remove(); }
        this.audioElementMap.delete(key);
        const gain = this.participantGains.get(key);
        if (gain) { gain.disconnect(); this.participantGains.delete(key); }
      }
    }

    // 9. Disconnect participant gain nodes
    for (const gain of this.participantGains.values()) {
      gain.disconnect();
    }
    this.participantGains.clear();

    // 10. Disconnect master gain
    if (this.masterGain) this.masterGain.disconnect();

    // 11. Close AudioContext (ignore errors)
    // Note: wasapiAudioCtx is closed by stopWasapiAudioBridge() called above (step 4g).
    if (this.audioContext) this.audioContext.close().catch(() => {});

    // 12. Remove gesture listeners and device watcher
    if (this.gestureListener) {
      document.removeEventListener('click', this.gestureListener);
      document.removeEventListener('keydown', this.gestureListener);
    }
    this.stopDeviceChangeWatcher();

    // 13. Null out references
    this.room = null;
    this.audioContext = null;
    this.wasapiAudioCtx = null;
    this.masterGain = null;
    this.rafId = null;
    this.statsInterval = null;
    this.analyserInterval = null;
    this.postPublishRetryTimeout = null;
    this.adaptiveState = null;
    this.gestureListener = null;
    this.prevConcealmentEventsTotal = 0;
    this.prevSharePliCount = 0;
    this.prevShareNackCount = 0;
    this.lastJitterBufferDelayMs = 0;
    this.lastConcealmentEventsPerInterval = 0;
    this.lastCandidateType = 'unknown';
    this.lastAvailableBandwidthKbps = 0;
    this.lastRttMs = 0;
    this.lastPacketLossPercent = 0;
    this.lastJitterMs = 0;
    this.prevVideoRecvFramesDropped = 0;
    this.prevVideoRecvFreezeCount = 0;
    this.prevVideoRecvTotalFreezesDuration = 0;
    this.prevVideoRecvPliCount = 0;
    this.prevVideoRecvNackCount = 0;

    // 14. Log
    console.log(LOG, 'disconnected');
  }

  /** Enable/disable local microphone track. */
  async setMicEnabled(enabled: boolean): Promise<void> {
    if (!this.room) return;
    // When the native mic bridge is active, the published track is a custom
    // MediaStreamTrack — toggle it directly instead of calling setMicrophoneEnabled
    // which would try to re-request getUserMedia.
    if (this.nativeMicBridge && this.localMicTrack) {
      this.localMicTrack.mediaStreamTrack.enabled = enabled;
      return;
    }
    await this.room.localParticipant.setMicrophoneEnabled(enabled);
  }

  /** Set per-participant volume (0–100 → perceptual gain curve). */
  setParticipantVolume(participantIdentity: string, volume: number): void {
    this.desiredParticipantVolumes.set(participantIdentity, volume);
    const gain = this.participantGains.get(participantIdentity);
    if (gain) {
      gain.gain.setValueAtTime(perceptualGain(volume), this.audioContext?.currentTime ?? 0);
    }
  }

  /** Set master volume (0–100 → perceptual gain curve). */
  setScreenShareAudioVolume(participantIdentity: string, volume: number): void {
    const key = `${participantIdentity}:screen-share`;
    this.desiredParticipantVolumes.set(key, volume);
    const gain = this.participantGains.get(key);
    if (gain) {
      gain.gain.setValueAtTime(perceptualGain(volume), this.audioContext?.currentTime ?? 0);
    }
  }

  setMasterVolume(volume: number): void {
    if (this.masterGain) {
      this.masterGain.gain.setValueAtTime(perceptualGain(volume), this.audioContext?.currentTime ?? 0);
    }
  }


  /**
   * Start screen share via browser/WebView getDisplayMedia.
   * This is the path normal Windows/macOS `/share` uses, so leak diagnostics
   * for those repros must be added here, not only in the native picker flow.
   * Returns true if successful, false on cancel/denial.
   */
  async startScreenShare(): Promise<boolean> {
    if (!this.room) return false;

    this.syncProfileFromPreset();
    const profile = this.currentCaptureProfile;
    const nativeShareAudio = usesNativeScreenShareAudio();
    console.log(LOG, '[wasapi-diag] startScreenShare: nativeShareAudio=%s profile.audio=%s userAgent=%s',
      nativeShareAudio, profile.audio, navigator.userAgent.slice(0, 60));
    const pubOpts = this.currentPublishOptions;

    const captureOpts = {
      resolution: {
        width: profile.resolution.width,
        height: profile.resolution.height,
        frameRate: profile.frameRate,
      },
      contentHint: profile.contentHint,
      surfaceSwitching: profile.surfaceSwitching,
      selfBrowserSurface: profile.selfBrowserSurface,
      // Windows and macOS capture system audio via the native Rust bridge, so
      // getDisplayMedia stays video-only and audio can be toggled independently.
      audio: nativeShareAudio ? false : profile.audio,
      suppressLocalAudioPlayback: profile.suppressLocalAudioPlayback,
    };

    const publishOpts = {
      screenShareEncoding: pubOpts.screenShareEncoding,
      videoCodec: pubOpts.videoCodec,
      backupCodec: pubOpts.backupCodec,
      degradationPreference: pubOpts.degradationPreference,
      screenShareSimulcastLayers: pubOpts.screenShareSimulcastLayers.map(
        (l) => new VideoPreset({ width: l.width, height: l.height, maxBitrate: 0 }),
      ),
    };

    if (isWindows()) {
      // Windows users usually land here, not in beginNativeCaptureLeakSession().
      // Keep this session start in sync with any future leak logging changes.
      this.beginShareLeakSession({
        mode: 'screen_audio',
        sourceId: 'browser-display-media',
        sourceName: 'Browser Screen Share Picker',
        startStage: 'browser_capture_start',
      });
    }

    try {
      this.noteShareLeakPublishStart();
      await this.room.localParticipant.setScreenShareEnabled(true, captureOpts, publishOpts);
      this.captureShareLeakPublishDiagnostics();
      if (this.nativeCaptureLeakSession) {
        this.nativeCaptureLeakSession.activeRaw = await this.captureRawShareLeakMemorySnapshot();
      }
      this.markNativeCaptureLeakStage('publish_track_done');
      if (!nativeShareAudio) {
        this.suppressLocalScreenShareAudio();
      }
      if (nativeShareAudio && profile.audio) {
        await this.startWasapiScreenShareAudio();
        this.suppressLocalScreenShareAudio();
      }
      // Initialize adaptive quality state
      this.adaptiveState = {
        currentTier: 'full',
        consecutiveLossPolls: 0,
        consecutiveRecoveryPolls: 0,
        consecutiveBandwidthPolls: 0,
        basePreset: this.currentQuality,
      };
      // Schedule post-publish tuning after a short delay to let the track become live
      this.postPublishRetryTimeout = setTimeout(() => {
        this.postPublishRetryTimeout = null;
        this.applyPostPublishTuning();
      }, 100);
      this.startScreenShareStatsPolling();

      return true;
    } catch (err) {
      // Fall back to browser defaults on constraint rejection
      const isOverconstrained = err instanceof Error &&
        (err.name === 'OverconstrainedError' || err.name.includes('Overconstrained'));
      if (isOverconstrained) {
        console.warn(LOG, 'capture constraints rejected, falling back to defaults:', err.message);
        this.callbacks.onSystemEvent('capture constraints rejected — using browser defaults');
        try {
          this.noteShareLeakPublishStart();
          await this.room.localParticipant.setScreenShareEnabled(true);
          this.captureShareLeakPublishDiagnostics();
          if (this.nativeCaptureLeakSession) {
            this.nativeCaptureLeakSession.activeRaw = await this.captureRawShareLeakMemorySnapshot();
          }
          this.markNativeCaptureLeakStage('publish_track_done');
          if (!nativeShareAudio) {
            this.suppressLocalScreenShareAudio();
          }
          if (nativeShareAudio && profile.audio) {
            await this.startWasapiScreenShareAudio();
            this.suppressLocalScreenShareAudio();
          }
          // Initialize adaptive quality state for fallback path too
          this.adaptiveState = {
            currentTier: 'full',
            consecutiveLossPolls: 0,
            consecutiveRecoveryPolls: 0,
            consecutiveBandwidthPolls: 0,
            basePreset: this.currentQuality,
          };
          // Schedule post-publish tuning for fallback path too
          this.postPublishRetryTimeout = setTimeout(() => {
            this.postPublishRetryTimeout = null;
            this.applyPostPublishTuning();
          }, 100);
          this.startScreenShareStatsPolling();
          return true;
        } catch (fallbackErr) {
          this.markNativeCaptureFailure(fallbackErr instanceof Error ? fallbackErr.message : String(fallbackErr));
          await this.finalizeNativeCaptureLeakSession();
          console.log(LOG, 'screen share fallback failed:', fallbackErr instanceof Error ? fallbackErr.message : String(fallbackErr));
          this.callbacks.onSystemEvent(
            `screen share failed: ${fallbackErr instanceof Error ? fallbackErr.message : String(fallbackErr)}`,
          );
          return false;
        }
      }
      // Platform-level permission denial (e.g. macOS Screen Recording permission not
      // granted in System Preferences). Distinct from a user-cancelled picker, which
      // throws NotAllowedError with "Permission denied". Rethrow so the caller can
      // surface a useful OS-level hint rather than silently returning false.
      const isPlatformDenied = err instanceof Error &&
        err.name === 'NotAllowedError' &&
        err.message.toLowerCase().includes('not allowed by the user agent');
      if (isPlatformDenied) {
        this.markNativeCaptureFailure(err.message);
        await this.finalizeNativeCaptureLeakSession();
        throw err;
      }
      this.markNativeCaptureFailure(err instanceof Error ? err.message : String(err));
      await this.finalizeNativeCaptureLeakSession();
      console.log(LOG, 'screen share failed:', err instanceof Error ? err.message : String(err));
      this.callbacks.onSystemEvent(
        `screen share failed: ${err instanceof Error ? err.message : String(err)}`,
      );
      return false;
    }
  }

  /** Check if screen share audio track exists and is not muted. */
  hasScreenShareAudio(): boolean {
    if (!this.room) return false;
    const pub = this.room.localParticipant.getTrackPublication(Track.Source.ScreenShareAudio);
    return !!(pub?.track && !pub.isMuted);
  }

  private hasActiveScreenShare(): boolean {
    return this.room?.localParticipant.getTrackPublication(Track.Source.ScreenShare) != null;
  }

  private clearScreenShareRuntimeState(): void {
    if (this.postPublishRetryTimeout !== null) {
      clearTimeout(this.postPublishRetryTimeout);
      this.postPublishRetryTimeout = null;
    }
    this.stopScreenShareStatsPolling();
    this.adaptiveState = null;
  }

  /**
   * Windows-specific hard teardown for the browser-managed screen share track.
   * Explicitly unpublishes the local track so the browser capture is torn down
   * deterministically, then best-effort clears any residual SDK screen-share
   * state. We still capture the MediaStreamTrack up front so we can force-stop
   * it in finally even if LiveKit has already dropped its publication reference.
   */
  private async hardTeardownLocalScreenShare(
    reason: 'stop' | 'change-source',
    options: { stopNativeAudio: boolean },
  ): Promise<void> {
    if (!this.room) return;

    this.clearScreenShareRuntimeState();

    const publication = this.room.localParticipant.getTrackPublication(Track.Source.ScreenShare);
    const localTrack = publication?.track;
    const mediaTrack = localTrack?.mediaStreamTrack;
    const settings = getTrackSettingsSafe(mediaTrack);
    const displaySurface =
      typeof settings.displaySurface === 'string' ? settings.displaySurface : 'unknown';
    const beforeState = mediaTrack?.readyState ?? 'missing';
    const expectedTrackId = mediaTrack?.id ?? null;

    console.log(
      LOG,
      `screen share teardown [${reason}]: publication=${!!localTrack} readyState=${beforeState} displaySurface=${displaySurface} nativeAudio=${options.stopNativeAudio}`,
    );

    const leakSession = this.nativeCaptureLeakSession;
    if (leakSession && leakSession.stopRequestedAtMs === null) {
      leakSession.stopRequestedAtMs = Date.now();
      leakSession.activeRaw = await this.captureRawShareLeakMemorySnapshot();
      leakSession.summary.browserWebRtcBeforeStop = this.captureBrowserWebRtcSnapshot(expectedTrackId);
      this.markNativeCaptureLeakStage('share_stop_requested');
    }

    if (options.stopNativeAudio && usesNativeScreenShareAudio()) {
      await this.stopWasapiScreenShareAudio();
    }

    let disableFailed = false;
    let manualFallbackUsed = false;
    try {
      if (localTrack) {
        if (leakSession) {
          leakSession.summary.cleanupFlags.unpublishAttempted = true;
        }
        await this.room.localParticipant.unpublishTrack(mediaTrack ?? localTrack);
        manualFallbackUsed = true;
        if (leakSession) {
          leakSession.summary.cleanupFlags.unpublishSucceeded = true;
        }
        this.markNativeCaptureLeakStage('unpublish_done');
        try {
          await this.room.localParticipant.setScreenShareEnabled(false);
        } catch (disableErr) {
          disableFailed = true;
          console.warn(
            LOG,
            `screen share teardown [${reason}] setScreenShareEnabled(false) failed after unpublish: ${disableErr instanceof Error ? disableErr.message : String(disableErr)}`,
          );
        }
      } else {
        if (leakSession) {
          leakSession.summary.cleanupFlags.unpublishAttempted = true;
        }
        await this.room.localParticipant.setScreenShareEnabled(false);
        if (leakSession) {
          leakSession.summary.cleanupFlags.unpublishSucceeded = true;
        }
        this.markNativeCaptureLeakStage('unpublish_done');
      }
    } catch (err) {
      if (localTrack) {
        console.warn(
          LOG,
          `screen share teardown [${reason}] unpublishTrack failed: ${err instanceof Error ? err.message : String(err)}`,
        );
      } else {
        disableFailed = true;
        console.warn(
          LOG,
          `screen share teardown [${reason}] setScreenShareEnabled(false) failed: ${err instanceof Error ? err.message : String(err)}`,
        );
      }

      throw err;
    } finally {
      if (leakSession) {
        leakSession.summary.cleanupFlags.publicationCleared =
          this.room.localParticipant.getTrackPublication(Track.Source.ScreenShare) === undefined;
      }
      if (mediaTrack) {
        try {
          mediaTrack.stop();
        } catch {
          // best-effort finalizer — releasing the capture matters more than stop() errors
        }
        if (leakSession) {
          leakSession.summary.cleanupFlags.trackStopped = true;
        }
        this.markNativeCaptureLeakStage('track_stopped');
        console.log(
          LOG,
          `screen share teardown [${reason}] finalized: readyState=${mediaTrack.readyState} displaySurface=${displaySurface} disableFailed=${disableFailed} manualFallbackUsed=${manualFallbackUsed}`,
        );
      } else {
        console.log(
          LOG,
          `screen share teardown [${reason}] finalized: readyState=missing displaySurface=${displaySurface} disableFailed=${disableFailed} manualFallbackUsed=${manualFallbackUsed}`,
        );
      }
      if (leakSession) {
        leakSession.summary.browserWebRtcAfterStop = this.captureBrowserWebRtcSnapshot(expectedTrackId);
      }
      await this.finalizeNativeCaptureLeakSession();
    }
  }

  /** Stop screen share. */
  async stopScreenShare(): Promise<void> {
    if (!this.room) return;
    if (isWindows()) {
      await this.hardTeardownLocalScreenShare('stop', { stopNativeAudio: true });
      return;
    }
    this.clearScreenShareRuntimeState();
    if (usesNativeScreenShareAudio()) {
      await this.stopWasapiScreenShareAudio();
    }
    await this.room.localParticipant.setScreenShareEnabled(false);
  }

  /** List available audio devices. */
  async listDevices(): Promise<{ inputs: MediaDeviceInfo[]; outputs: MediaDeviceInfo[] }> {
    try {
      const [inputs, outputs] = await Promise.all([
        Room.getLocalDevices('audioinput'),
        Room.getLocalDevices('audiooutput'),
      ]);
      return { inputs, outputs };
    } catch (err) {
      console.warn(LOG, 'device enumeration failed:', err instanceof Error ? err.message : String(err));
      return { inputs: [], outputs: [] };
    }
  }

  /** Set microphone input volume (0–100). Persists and immediately updates the
   *  composite mic processor if a mic track is active. */
  async setInputVolume(volume: number): Promise<void> {
    await setInputVolume(volume);
    if (!this.localMicTrack) return;
    await this.syncMicProcessor('input_volume_changed', { originalTrack: this.localMicTrack.mediaStreamTrack });
  }

  /** Toggle JS-side noise suppression on the active mic track. */
  async setDenoiseEnabled(enabled: boolean): Promise<void> {
    if (this.nativeMicBridge) {
      await this.nativeMicBridge.setDenoiseEnabled(enabled);
      return;
    }
    this.jsDenoise = this.shouldUseJsNoiseSuppression(enabled);
    if (!this.localMicTrack) return;
    this.logNoiseSuppressionCapabilities(this.localMicTrack.mediaStreamTrack, 'toggle');
    await this.syncMicProcessor('toggle', { originalTrack: this.localMicTrack.mediaStreamTrack });
  }

  /** Switch audio input device. Persists the choice and immediately applies it
   *  when a room is connected (mirrors setOutputDevice behaviour). */
  async setInputDevice(deviceId: string): Promise<void> {
    // Persist first so applyAudioInputDevice reads the new value.
    await setStoreValue(STORE_KEYS.audioInputDevice, deviceId);
    if (this.nativeMicBridge) {
      await this.nativeMicBridge.setInputDevice(deviceId);
      return;
    }
    console.log(NS_LOG, `input_device_switch device=${deviceId}`);
    await this.applyAudioInputDevice();
  }

  /** Switch audio output device. Persists the choice and immediately reroutes
   *  the Web Audio context so Wavis audio leaves the system default endpoint. */
  async setOutputDevice(deviceId: string): Promise<void> {
    if (!this.room) return;
    if (DEBUG_AUDIO_OUTPUT) console.log(LOG, '[audio-output] setOutputDevice called — persisting deviceId:', deviceId);
    // Persist first so applyAudioOutputDevice reads the new value.
    await setStoreValue(STORE_KEYS.audioOutputDevice, deviceId);
    await this.applyAudioOutputDevice();
  }

  /**
   * Apply a quality preset to the active screen share track.
   * Looks up the preset from QUALITY_PRESETS, applies constraints and contentHint.
   * On failure, logs a warning and retains the previous quality setting.
   */
  async setScreenShareQuality(quality: ShareQuality): Promise<void> {
    if (!this.room) return;
    const pub = this.room.localParticipant.getTrackPublication(Track.Source.ScreenShare);
    if (!pub || !pub.track) return;

    const preset = QUALITY_PRESETS[quality];
    const previousQuality = this.currentQuality;
    try {
      const track = pub.track.mediaStreamTrack;
      track.contentHint = preset.contentHint;
      await track.applyConstraints({
        width: { ideal: preset.resolution.width },
        height: { ideal: preset.resolution.height },
        frameRate: { max: preset.maxFramerate },
      });
      this.currentQuality = quality;
      this.callbacks.onSystemEvent(`screen share quality: ${quality}`);
      // Re-report actual quality info so the UI updates
      const settings = getTrackSettingsSafe(track);
      this.callbacks.onShareQualityInfo?.({
        width: (settings.width as number) || 0,
        height: (settings.height as number) || 0,
        frameRate: (settings.frameRate as number) || 0,
      });
    } catch (err) {
      console.warn(LOG, 'setScreenShareQuality failed:', err);
      this.currentQuality = previousQuality;
    }
  }

  /**
   * Restart screen share with audio enabled/disabled.
   * On Windows/macOS: decouples audio from video — starts/stops native audio
   * capture only, video track stays live (no getDisplayMedia restart).
   * On other platforms: LiveKit SDK requires re-publishing to toggle audio.
   * Preserves the current capture profile and publish options.
   * Returns true if successful.
   */
  async restartScreenShareWithAudio(withAudio: boolean): Promise<boolean> {
    if (!this.room) return false;

    // Windows/macOS: audio is decoupled from video — toggle the native bridge
    // only, keeping the video track live.
    if (usesNativeScreenShareAudio()) {
      try {
        if (withAudio) {
          await this.startWasapiScreenShareAudio();
        } else {
          await this.stopWasapiScreenShareAudio();
        }
        this.callbacks.onSystemEvent(`screen share audio: ${withAudio ? 'on' : 'off'}`);
        return true;
      } catch (err) {
        console.log(LOG, 'restartScreenShareWithAudio (WASAPI) failed:', err instanceof Error ? err.message : String(err));
        this.callbacks.onSystemEvent(
          `screen share audio toggle failed: ${err instanceof Error ? err.message : String(err)}`,
        );
        return false;
      }
    }

    // ── macOS / Linux fallback ────────────────────────────────────────────────
    //
    // Preferred path: mute/unmute the existing ScreenShareAudio track.
    // This never calls getDisplayMedia and works from any call context
    // (including Tauri IPC callbacks where user-gesture propagation is lost
    // between windows). Only falls back to a full setScreenShareEnabled restart
    // when no audio track exists yet (share started without audio).
    const audioPublication = this.room.localParticipant.getTrackPublication(
      Track.Source.ScreenShareAudio,
    );
    const hasUserGesture =
      typeof navigator !== 'undefined' &&
      (navigator as { userActivation?: { isActive: boolean } }).userActivation?.isActive === true;

    if (DEBUG_SHARE_AUDIO) {
      console.log(LOG, '[share-audio] restartScreenShareWithAudio', {
        withAudio,
        hasAudioTrack: !!audioPublication?.track,
        trackMuted: audioPublication?.isMuted,
        hasUserGesture,
        userActivationIsActive: (navigator as { userActivation?: { isActive: boolean } }).userActivation?.isActive,
      });
    }

    // Fast path: mute/unmute existing track — no getDisplayMedia needed.
    if (audioPublication?.track) {
      try {
        if (withAudio) {
          if (DEBUG_SHARE_AUDIO) console.log(LOG, '[share-audio] unmuting existing ScreenShareAudio track');
          await audioPublication.track.unmute();
        } else {
          if (DEBUG_SHARE_AUDIO) console.log(LOG, '[share-audio] muting existing ScreenShareAudio track');
          await audioPublication.track.mute();
        }
        this.callbacks.onSystemEvent(`screen share audio: ${withAudio ? 'on' : 'off'}`);
        return true;
      } catch (muteErr) {
        if (DEBUG_SHARE_AUDIO) console.warn(LOG, '[share-audio] mute/unmute failed, falling through to restart:', muteErr);
        // Fall through to full restart below.
      }
    }

    // Slow path: no audio track exists yet (share started without audio) — need
    // a full setScreenShareEnabled restart, which calls getDisplayMedia.
    // This requires a direct user gesture in THIS window. On macOS, gestures from
    // a child Tauri window (e.g. ScreenSharePage) do NOT transfer here via IPC.
    if (DEBUG_SHARE_AUDIO) console.log(LOG, '[share-audio] no existing audio track — full restart required, hasUserGesture:', hasUserGesture);
    if (!hasUserGesture) {
      if (DEBUG_SHARE_AUDIO) console.warn(LOG, '[share-audio] skipping restart: no user gesture in this window');
      this.callbacks.onSystemEvent(
        'screen share audio toggle skipped — use the audio button in the main window',
      );
      return false;
    }

    this.syncProfileFromPreset();
    const profile = this.currentCaptureProfile;
    const pubOpts = this.currentPublishOptions;

    const captureOpts = {
      resolution: {
        width: profile.resolution.width,
        height: profile.resolution.height,
        frameRate: profile.frameRate,
      },
      contentHint: profile.contentHint,
      surfaceSwitching: profile.surfaceSwitching,
      selfBrowserSurface: profile.selfBrowserSurface,
      audio: usesNativeScreenShareAudio() ? false : withAudio,
      suppressLocalAudioPlayback: profile.suppressLocalAudioPlayback,
    };

    const publishOpts = {
      screenShareEncoding: pubOpts.screenShareEncoding,
      videoCodec: pubOpts.videoCodec,
      backupCodec: pubOpts.backupCodec,
      degradationPreference: pubOpts.degradationPreference,
      screenShareSimulcastLayers: pubOpts.screenShareSimulcastLayers.map(
        (l) => new VideoPreset({ width: l.width, height: l.height, maxBitrate: 0 }),
      ),
    };

    try {
      if (DEBUG_SHARE_AUDIO) console.log(LOG, '[share-audio] calling setScreenShareEnabled(false) then setScreenShareEnabled(true, audio:', withAudio, ')');
      this.clearScreenShareRuntimeState();
      await this.room.localParticipant.setScreenShareEnabled(false);
      await this.room.localParticipant.setScreenShareEnabled(true, captureOpts, publishOpts);
      // Suppress local audio playback on the screen share audio track
      if (withAudio) this.suppressLocalScreenShareAudio();
      // Reinitialize adaptive quality state on restart
      this.adaptiveState = {
        currentTier: 'full',
        consecutiveLossPolls: 0,
        consecutiveRecoveryPolls: 0,
        consecutiveBandwidthPolls: 0,
        basePreset: this.currentQuality,
      };
      // Re-apply post-publish tuning after the new track is published
      this.postPublishRetryTimeout = setTimeout(() => {
        this.postPublishRetryTimeout = null;
        this.applyPostPublishTuning();
      }, 100);
      this.startScreenShareStatsPolling();
      this.callbacks.onSystemEvent(`screen share audio: ${withAudio ? 'on' : 'off'}`);
      return true;
    } catch (err) {
      console.log(LOG, 'restartScreenShareWithAudio failed:', err instanceof Error ? err.message : String(err));
      this.callbacks.onSystemEvent(
        `screen share restart failed: ${err instanceof Error ? err.message : String(err)}`,
      );
      return false;
    }
  }

  /**
   * Change the screen share source (re-pick window/screen).
   *
   * Acquire-before-drop: shows the OS picker while the existing publication
   * stays live, so remote viewers never see an interruption. The old track is
   * replaced in-place only after the user confirms a new source. If the user
   * cancels the picker, the existing share continues uninterrupted.
   *
   * When no publication is currently active (share not yet started), falls
   * through to the standard setScreenShareEnabled path.
   *
   * Returns true if the source was changed successfully.
   */
  async changeScreenShareSource(): Promise<boolean> {
    if (!this.room) return false;

    this.syncProfileFromPreset();
    const profile = this.currentCaptureProfile;
    const pubOpts = this.currentPublishOptions;

    const captureOpts = {
      resolution: {
        width: profile.resolution.width,
        height: profile.resolution.height,
        frameRate: profile.frameRate,
      },
      contentHint: profile.contentHint,
      surfaceSwitching: profile.surfaceSwitching,
      selfBrowserSurface: profile.selfBrowserSurface,
      audio: usesNativeScreenShareAudio() ? false : profile.audio,
      suppressLocalAudioPlayback: profile.suppressLocalAudioPlayback,
    };

    const publishOpts = {
      screenShareEncoding: pubOpts.screenShareEncoding,
      videoCodec: pubOpts.videoCodec,
      backupCodec: pubOpts.backupCodec,
      degradationPreference: pubOpts.degradationPreference,
      screenShareSimulcastLayers: pubOpts.screenShareSimulcastLayers.map(
        (l) => new VideoPreset({ width: l.width, height: l.height, maxBitrate: 0 }),
      ),
    };

    const publication = this.room.localParticipant.getTrackPublication(Track.Source.ScreenShare);
    const existingLocalTrack = publication?.track;

    try {
      if (existingLocalTrack) {
        // Acquire-before-drop: show the OS picker while the current share stays live.
        // getDisplayMedia throws (NotAllowedError / AbortError) if the user cancels,
        // at which point we return false without touching the existing publication.
        const newStream = await navigator.mediaDevices.getDisplayMedia({
          video: {
            width: captureOpts.resolution.width,
            height: captureOpts.resolution.height,
            frameRate: captureOpts.resolution.frameRate,
            // Non-standard Chrome constraints — cast to avoid TS errors.
            surfaceSwitching: captureOpts.surfaceSwitching,
            selfBrowserSurface: captureOpts.selfBrowserSurface,
          } as MediaTrackConstraints,
          audio: captureOpts.audio
            ? ({ suppressLocalAudioPlayback: captureOpts.suppressLocalAudioPlayback } as MediaTrackConstraints)
            : false,
        });

        const newVideoTrack = newStream.getVideoTracks()[0];
        if (!newVideoTrack) {
          newStream.getTracks().forEach((t) => t.stop());
          return false;
        }

        // Apply content hint to the new track before replacing.
        (newVideoTrack as MediaStreamTrack & { contentHint: string }).contentHint =
          captureOpts.contentHint;

        // Replace the track in-place — existing publication stays live for remote viewers.
        try {
          await (existingLocalTrack as LocalVideoTrack).replaceTrack(newVideoTrack, {
            userProvidedTrack: true,
          });
        } catch (replaceErr) {
          // Stop the newly acquired track so we don't leak the capture handle.
          newVideoTrack.stop();
          throw replaceErr;
        }

        // Suppress local audio playback on browser-managed screen share audio.
        // Native WASAPI audio is left running — consistent with the original
        // change-source path which also kept native audio alive (stopNativeAudio: false).
        if (!usesNativeScreenShareAudio() && profile.audio) {
          this.suppressLocalScreenShareAudio();
        }
      } else {
        // No existing publication — use the standard capture-and-publish path.
        await this.room.localParticipant.setScreenShareEnabled(true, captureOpts, publishOpts);
        if (!usesNativeScreenShareAudio() && profile.audio) this.suppressLocalScreenShareAudio();
      }

      // Reinitialize adaptive quality and stats for the new source.
      this.clearScreenShareRuntimeState();
      this.adaptiveState = {
        currentTier: 'full',
        consecutiveLossPolls: 0,
        consecutiveRecoveryPolls: 0,
        consecutiveBandwidthPolls: 0,
        basePreset: this.currentQuality,
      };
      // Re-apply post-publish tuning after the new track is published.
      this.postPublishRetryTimeout = setTimeout(() => {
        this.postPublishRetryTimeout = null;
        this.applyPostPublishTuning();
      }, 100);
      this.startScreenShareStatsPolling();
      return true;
    } catch (err) {
      console.log(LOG, 'changeScreenShareSource failed:', err instanceof Error ? err.message : String(err));
      // Do NOT tear down the existing publication on failure — the user may have
      // cancelled the picker, in which case the original share is still active.
      return false;
    }
  }

  /**
   * Returns true when a local screen share publication is currently active.
   * Used by voice-room.ts to reconcile backend signaling state after a failed
   * source change.
   */
  hasActiveScreenShareTrack(): boolean {
    if (!this.room) return false;
    return this.room.localParticipant.getTrackPublication(Track.Source.ScreenShare)?.track !== undefined;
  }

  /**
   * Monitor a remote screen share track's underlying MediaStreamTrack for
   * replacement. When the streamer changes which window they're sharing,
   * LiveKit replaces the mediaStreamTrack on the RemoteTrack. The old track
   * fires 'ended', but our stored MediaStream still references it — causing
   * a black screen in the viewer. This method detects the replacement and
   * rebuilds the stream.
   *
   * Returns a cleanup function to remove the listener.
   */
  private monitorScreenShareTrack(
    participant: RemoteParticipant,
    publication: RemoteTrackPublication,
    track: RemoteTrack,
  ): () => void {
    const mst = track.mediaStreamTrack;
    let cleaned = false;

    const onEnded = () => {
      if (this.disposed || cleaned) return;
      console.log(LOG, `monitorScreenShareTrack: track ended for ${participant.identity} — trackSid: ${track.sid ?? '?'}, readyState: ${mst.readyState}, ts: ${Date.now()}`);

      // The 'ended' event may fire before LiveKit has swapped in the new
      // track on the publication. Try immediately, then retry with backoff
      // to cover longer reconnection windows.
      const tryRebuild = (): boolean => {
        const currentTrack = publication.track;
        if (
          !currentTrack ||
          currentTrack.mediaStreamTrack === mst ||
          currentTrack.mediaStreamTrack.readyState === 'ended'
        ) {
          return false;
        }

        const entry = this.screenShareElements.get(participant.identity);
        if (!entry) return true; // entry gone, nothing to rebuild

        console.log(LOG, `screen share track replaced for ${participant.identity}, rebuilding stream — oldTrackSid: ${entry.trackSid}, newTrackSid: ${currentTrack.sid ?? '?'}, ts: ${Date.now()}`);

        // Build a new MediaStream from the replacement track
        const newStream = new MediaStream([currentTrack.mediaStreamTrack]);

        // Update the dummy video element
        if (entry.dummyVideo) {
          entry.dummyVideo.srcObject = newStream;
          entry.dummyVideo.play().catch(() => {});
        }

        // Keep the publication enabled
        publication.setEnabled(true);

        // Clean up old listener and install a new one for the replacement track
        cleaned = true;
        mst.removeEventListener('ended', onEnded);

        // Update the entry with the new stream and a fresh monitor
        this.screenShareElements.set(participant.identity, {
          stream: newStream,
          startedAtMs: entry.startedAtMs,
          trackSid: currentTrack.sid ?? entry.trackSid,
          dummyVideo: entry.dummyVideo,
          trackEndedCleanup: this.monitorScreenShareTrack(participant, publication, currentTrack),
        });

        // Re-emit so the loopback bridge rebuilds with the new stream
        this.callbacks.onScreenShareSubscribed(participant.identity, newStream);
        console.log(LOG, `monitorScreenShareTrack: rebuild succeeded for ${participant.identity} — newStreamId: ${newStream.id}, newTrackSid: ${currentTrack.sid ?? '?'}, ts: ${Date.now()}`);
        return true;
      };

      if (tryRebuild()) return;

      // Retry with backoff so longer LiveKit reconnections can still recover.
      const delays = [200, 400, 800];
      let attempt = 0;

      const retryLoop = () => {
        if (attempt >= delays.length) {
          console.log(
            LOG,
            `screen share track ended for ${participant.identity}, no replacement after ${attempt + 1} attempts — trackSid: ${track.sid ?? '?'}, ts: ${Date.now()}`,
          );
          return;
        }

        setTimeout(() => {
          if (this.disposed || cleaned) return;
          if (tryRebuild()) return;
          attempt += 1;
          retryLoop();
        }, delays[attempt]);
      };

      retryLoop();
    };

    mst.addEventListener('ended', onEnded);

    return () => {
      cleaned = true;
      mst.removeEventListener('ended', onEnded);
    };
  }

  /** Return list of active remote screen shares, sorted by startedAtMs descending (most recent first). */
  getActiveScreenShares(): Array<{ identity: string; stream: MediaStream; startedAtMs: number }> {
    return Array.from(this.screenShareElements.entries())
      .map(([identity, entry]) => ({ identity, stream: entry.stream, startedAtMs: entry.startedAtMs }))
      .sort((a, b) => b.startedAtMs - a.startedAtMs);
  }


  /* ─── Post-Publish Track Tuning ──────────────────────────────── */

  /**
   * Apply post-publish tuning to the screen share track:
   * - Set contentHint to 'detail' for text/UI clarity
   * - Apply constraints for ideal resolution and minimum frame rate
   * - Retry once on failure, then log warning and continue
   */
  private applyPostPublishTuning(): void {
    if (!this.room) return;

    const pub = this.room.localParticipant.getTrackPublication(Track.Source.ScreenShare);
    const mediaTrack = pub?.track?.mediaStreamTrack;
    if (!mediaTrack) {
      console.warn(LOG, 'post-publish tuning: no screen share track found');
      return;
    }

    // Set content hint for text/UI clarity
    mediaTrack.contentHint = 'detail';

    // Apply constraints with retry logic — derive from active preset so the
    // floor/ideal values never conflict with the preset's maxFramerate.
    const preset = QUALITY_PRESETS[this.currentQuality];
    const idealFps = preset.maxFramerate;
    const constraints: MediaTrackConstraints = {
      width: { ideal: preset.resolution.width },
      height: { ideal: preset.resolution.height },
      frameRate: { min: 24, ideal: idealFps },
    };

    const reportQualityInfo = () => {
      const settings = getTrackSettingsSafe(mediaTrack);
      const info: ShareQualityInfo = {
        width: (settings.width as number) || 0,
        height: (settings.height as number) || 0,
        frameRate: (settings.frameRate as number) || 0,
      };
      console.log(LOG, `screen share actual: ${info.width}x${info.height} @ ${info.frameRate}fps`);
      this.callbacks.onShareQualityInfo?.(info);
    };

    mediaTrack.applyConstraints(constraints).then(() => {
      reportQualityInfo();
    }).catch(() => {
      // First failure — retry once after 300ms
      this.postPublishRetryTimeout = setTimeout(() => {
        this.postPublishRetryTimeout = null;
        mediaTrack.applyConstraints(constraints).then(() => {
          reportQualityInfo();
        }).catch((retryErr: unknown) => {
          // Second failure — log warning, report whatever we got, and continue
          console.warn(
            LOG,
            'post-publish applyConstraints failed after retry:',
            retryErr instanceof Error ? retryErr.message : String(retryErr),
          );
          reportQualityInfo();
        });
      }, 300);
    });
  }

  /* ─── Screen Share Sender Stats Polling ───────────────────────── */

  /**
   * Start periodic sender stats logging for the active screen share.
   * Polls every 5 seconds, logging outbound bitrate, fps, and quality limitation reason.
   * On polling failure, logs a warning and skips that cycle.
   */
  private startScreenShareStatsPolling(): void {
    this.stopScreenShareStatsPolling();
    let prevBytesSent = 0;
    let prevTimestamp = 0;
    this.screenShareStatsInterval = setInterval(() => {
      if (this.disposed || !this.room) {
        this.stopScreenShareStatsPolling();
        return;
      }
      try {
        // eslint-disable-next-line @typescript-eslint/no-explicit-any
        const engine = (this.room as any).engine;
        const publisher = engine?.pcManager?.publisher;
        if (publisher && typeof publisher.getStats === 'function') {
          publisher.getStats().then((report: RTCStatsReport) => {
            if (this.disposed) return;
            try {
              let bytesSent = 0;
              let timestamp = 0;
              let fps = 0;
              let qualityLimitation = 'none';
              let packetsSent = 0;
              let packetsLost = 0;
              let frameWidth = 0;
              let frameHeight = 0;
              let pliCountCumulative = 0;
              let nackCountCumulative = 0;
              let availableBandwidthKbps = 0;
              report.forEach((stat: Record<string, unknown>) => {
                if (stat.type === 'outbound-rtp' && stat.kind === 'video') {
                  if (typeof stat.bytesSent === 'number') bytesSent = stat.bytesSent as number;
                  if (typeof stat.timestamp === 'number') timestamp = stat.timestamp as number;
                  if (typeof stat.framesPerSecond === 'number') fps = stat.framesPerSecond as number;
                  if (typeof stat.qualityLimitationReason === 'string') qualityLimitation = stat.qualityLimitationReason as string;
                  if (typeof stat.packetsSent === 'number') packetsSent = stat.packetsSent as number;
                  if (typeof stat.frameWidth === 'number') frameWidth = stat.frameWidth as number;
                  if (typeof stat.frameHeight === 'number') frameHeight = stat.frameHeight as number;
                  if (typeof stat.pliCount === 'number') pliCountCumulative = stat.pliCount as number;
                  if (typeof stat.nackCount === 'number') nackCountCumulative = stat.nackCount as number;
                }
                // Read packet loss from remote-inbound-rtp (RTCP receiver reports for our outbound stream)
                if (stat.type === 'remote-inbound-rtp' && stat.kind === 'video') {
                  if (typeof stat.packetsLost === 'number') packetsLost = stat.packetsLost as number;
                }
                // Available outgoing bandwidth from nominated ICE candidate pair
                if (stat.type === 'candidate-pair' && stat.nominated === true) {
                  if (typeof stat.availableOutgoingBitrate === 'number') {
                    availableBandwidthKbps = Math.round((stat.availableOutgoingBitrate as number) / 1000);
                  }
                }
              });
              let bitrateKbps = 0;
              if (prevTimestamp > 0 && timestamp > prevTimestamp) {
                const deltaBytes = bytesSent - prevBytesSent;
                const deltaSec = (timestamp - prevTimestamp) / 1000;
                bitrateKbps = Math.round((deltaBytes * 8) / deltaSec / 1000);
              }
              prevBytesSent = bytesSent;
              prevTimestamp = timestamp;

              // Compute outbound packet loss percentage
              const totalPackets = packetsSent + packetsLost;
              const packetLossPercent = totalPackets > 0
                ? Math.round((packetsLost / totalPackets) * 1000) / 10
                : 0;

              // Compute per-interval deltas for PLI and NACK (avoid reporting ever-growing cumulative totals)
              const pliCount = Math.max(0, pliCountCumulative - this.prevSharePliCount);
              const nackCount = Math.max(0, nackCountCumulative - this.prevShareNackCount);
              this.prevSharePliCount = pliCountCumulative;
              this.prevShareNackCount = nackCountCumulative;

              console.log(LOG, `screen share stats: bitrate=${bitrateKbps}kbps fps=${fps} ${frameWidth}x${frameHeight} qualityLimitation=${qualityLimitation} loss=${packetLossPercent}% pli=${pliCount} nack=${nackCount}`);

              // Feed packet loss and quality limitation to adaptive quality logic
              this.processAdaptiveQuality(packetLossPercent, qualityLimitation);

              // Forward stats to diagnostics window (optional callback, 5s cadence)
              this.callbacks.onShareStats?.({
                bitrateKbps,
                fps,
                qualityLimitationReason: qualityLimitation,
                packetLossPercent,
                frameWidth,
                frameHeight,
                pliCount,
                nackCount,
                availableBandwidthKbps,
              });
            } catch {
              console.warn(LOG, 'screen share stats parsing failed, skipping cycle');
            }
          }).catch(() => {
            console.warn(LOG, 'screen share stats polling failed, skipping cycle');
          });
        }
      } catch {
        console.warn(LOG, 'screen share stats polling failed, skipping cycle');
      }
    }, 5000);
  }

  /* ─── Adaptive Quality Processing ──────────────────────────────── */

  /**
   * Process adaptive quality based on outbound packet loss and quality limitation.
   * Called from stats polling every 5 seconds.
   *
   * Tier transitions (step-down triggers — whichever fires first):
   * - Packet loss: full → reduced-fps at >5% for 2 polls; reduced-fps → reduced-resolution at >15% for 2 polls
   * - Bandwidth limitation: any tier → next lower tier when qualityLimitationReason === 'bandwidth' for 3 consecutive polls (15s)
   *
   * Recovery: any reduced tier → previous tier when loss < 3% AND no bandwidth limitation for 3 consecutive polls (15s)
   */
  private processAdaptiveQuality(packetLossPercent: number, qualityLimitation: string): void {
    if (!this.adaptiveState || !this.room) return;

    const state = this.adaptiveState;
    const oldTier = state.currentTier;
    const isBandwidthLimited = qualityLimitation === 'bandwidth';

    // --- Bandwidth limitation step-down (works at any tier except reduced-resolution) ---
    if (isBandwidthLimited && oldTier !== 'reduced-resolution') {
      state.consecutiveBandwidthPolls++;
      if (state.consecutiveBandwidthPolls >= ADAPTIVE_BANDWIDTH_STEPDOWN_POLLS) {
        const newTier: AdaptiveTier = oldTier === 'full' ? 'reduced-fps' : 'reduced-resolution';
        state.currentTier = newTier;
        state.consecutiveBandwidthPolls = 0;
        state.consecutiveLossPolls = 0;
        state.consecutiveRecoveryPolls = 0;
        this.applyAdaptiveTierConstraints(oldTier, newTier, packetLossPercent);
        return;
      }
    } else {
      state.consecutiveBandwidthPolls = 0;
    }

    // --- Packet loss step-down logic ---
    if (oldTier === 'full' && packetLossPercent > ADAPTIVE_LOSS_THRESHOLD_MODERATE) {
      state.consecutiveLossPolls++;
      state.consecutiveRecoveryPolls = 0;
      if (state.consecutiveLossPolls >= ADAPTIVE_STEPDOWN_POLLS) {
        state.currentTier = 'reduced-fps';
        state.consecutiveLossPolls = 0;
        this.applyAdaptiveTierConstraints('full', 'reduced-fps', packetLossPercent);
        return;
      }
    } else if (oldTier === 'reduced-fps' && packetLossPercent > ADAPTIVE_LOSS_THRESHOLD_SEVERE) {
      state.consecutiveLossPolls++;
      state.consecutiveRecoveryPolls = 0;
      if (state.consecutiveLossPolls >= ADAPTIVE_STEPDOWN_POLLS) {
        state.currentTier = 'reduced-resolution';
        state.consecutiveLossPolls = 0;
        this.applyAdaptiveTierConstraints('reduced-fps', 'reduced-resolution', packetLossPercent);
        return;
      }
    } else if (oldTier === 'full') {
      state.consecutiveLossPolls = 0;
    } else if (oldTier === 'reduced-fps' && packetLossPercent <= ADAPTIVE_LOSS_THRESHOLD_SEVERE) {
      if (packetLossPercent >= ADAPTIVE_RECOVERY_THRESHOLD) {
        state.consecutiveLossPolls = 0;
      }
    }

    // --- Recovery logic (requires both low loss AND no bandwidth limitation) ---
    if (oldTier !== 'full' && packetLossPercent < ADAPTIVE_RECOVERY_THRESHOLD && !isBandwidthLimited) {
      state.consecutiveRecoveryPolls++;
      state.consecutiveLossPolls = 0;
      if (state.consecutiveRecoveryPolls >= ADAPTIVE_RECOVERY_POLLS) {
        const newTier: AdaptiveTier = oldTier === 'reduced-resolution' ? 'reduced-fps' : 'full';
        state.currentTier = newTier;
        state.consecutiveRecoveryPolls = 0;
        this.applyAdaptiveTierConstraints(oldTier, newTier, packetLossPercent);
        return;
      }
    } else if (oldTier !== 'full') {
      // Either loss is too high or bandwidth is still limited — reset recovery counter
      if (packetLossPercent >= ADAPTIVE_RECOVERY_THRESHOLD || isBandwidthLimited) {
        state.consecutiveRecoveryPolls = 0;
      }
    }
  }

  /**
   * Apply constraints for an adaptive tier transition.
   * Uses applyConstraints on the capture track (not simulcast layer switching).
   */
  private applyAdaptiveTierConstraints(
    oldTier: AdaptiveTier,
    newTier: AdaptiveTier,
    packetLossPercent: number,
  ): void {
    if (!this.room) return;

    const pub = this.room.localParticipant.getTrackPublication(Track.Source.ScreenShare);
    const mediaTrack = pub?.track?.mediaStreamTrack;
    if (!mediaTrack) {
      console.warn(LOG, 'adaptive quality: no screen share track for tier change');
      return;
    }

    const preset = QUALITY_PRESETS[this.adaptiveState?.basePreset ?? this.currentQuality];

    // Determine target resolution and fps based on the new tier
    let targetWidth = preset.resolution.width;
    let targetHeight = preset.resolution.height;
    let targetFps = preset.maxFramerate;

    if (newTier === 'reduced-fps') {
      // Reduce FPS by one tier from the base preset's fps
      const currentFpsIdx = FPS_TIERS.indexOf(targetFps);
      if (currentFpsIdx >= 0 && currentFpsIdx < FPS_TIERS.length - 1) {
        targetFps = FPS_TIERS[currentFpsIdx + 1];
      } else if (currentFpsIdx < 0) {
        // Base fps not in tiers — just halve it
        targetFps = Math.max(15, Math.round(targetFps / 2));
      }
    } else if (newTier === 'reduced-resolution') {
      // Keep reduced FPS from reduced-fps tier
      const baseFpsIdx = FPS_TIERS.indexOf(preset.maxFramerate);
      if (baseFpsIdx >= 0 && baseFpsIdx < FPS_TIERS.length - 1) {
        targetFps = FPS_TIERS[baseFpsIdx + 1];
      } else {
        targetFps = Math.max(15, Math.round(preset.maxFramerate / 2));
      }
      // Reduce resolution by one tier from the base preset's resolution
      const currentResIdx = RESOLUTION_TIERS.findIndex(
        r => r.width === preset.resolution.width && r.height === preset.resolution.height,
      );
      if (currentResIdx >= 0 && currentResIdx < RESOLUTION_TIERS.length - 1) {
        targetWidth = RESOLUTION_TIERS[currentResIdx + 1].width;
        targetHeight = RESOLUTION_TIERS[currentResIdx + 1].height;
      } else if (currentResIdx < 0) {
        // Base resolution not in tiers — step down to 1080p or 720p
        targetWidth = 1920;
        targetHeight = 1080;
      }
    }
    // newTier === 'full' → use base preset values (already set above)

    const constraints: MediaTrackConstraints = {
      width: { ideal: targetWidth },
      height: { ideal: targetHeight },
      frameRate: { max: targetFps },
    };

    console.log(
      LOG,
      `adaptive quality: ${oldTier} → ${newTier} (loss=${packetLossPercent}%) target=${targetWidth}x${targetHeight}@${targetFps}fps`,
    );

    mediaTrack.applyConstraints(constraints).catch((err: unknown) => {
      console.warn(
        LOG,
        'adaptive quality applyConstraints failed:',
        err instanceof Error ? err.message : String(err),
      );
    });
  }

  /**
   * Extract inbound-rtp video stats from a subscriber RTCStatsReport.
   * Computes per-interval deltas for cumulative counters and fires onVideoReceiveStats.
   * No-ops when there is no inbound video track (i.e. no remote screen share active).
   */
  private extractVideoReceiveStats(report: RTCStatsReport): void {
    let fps = 0;
    let frameWidth = 0;
    let frameHeight = 0;
    let framesDroppedCum = 0;
    let packetsReceived = 0;
    let packetsLost = 0;
    let jitterBufferDelayMs = 0;
    let freezeCountCum = 0;
    let totalFreezesDurationCum = 0; // seconds (float)
    let pliCountCum = 0;
    let nackCountCum = 0;
    let totalDecodeTimeSec = 0; // seconds (float)
    let framesDecoded = 0;
    let hasVideo = false;

    report.forEach((entry) => {
      if (entry.type !== 'inbound-rtp' || entry.kind !== 'video') return;
      hasVideo = true;
      if (typeof entry.framesPerSecond === 'number') fps = entry.framesPerSecond;
      if (typeof entry.frameWidth === 'number') frameWidth = entry.frameWidth;
      if (typeof entry.frameHeight === 'number') frameHeight = entry.frameHeight;
      if (typeof entry.framesDropped === 'number') framesDroppedCum = entry.framesDropped;
      if (typeof entry.packetsReceived === 'number') packetsReceived = entry.packetsReceived;
      if (typeof entry.packetsLost === 'number') packetsLost = entry.packetsLost;
      // Both jitterBufferTargetDelay and jitterBufferDelay are cumulative totals (seconds)
      // across all emitted samples — divide by jitterBufferEmittedCount to get the average.
      // Prefer jitterBufferTargetDelay (target) over jitterBufferDelay (actual) when present.
      if (jitterBufferDelayMs === 0 && typeof entry.jitterBufferTargetDelay === 'number' && typeof entry.jitterBufferEmittedCount === 'number' && entry.jitterBufferEmittedCount > 0) {
        jitterBufferDelayMs = Math.round((entry.jitterBufferTargetDelay / entry.jitterBufferEmittedCount) * 1000);
      }
      if (jitterBufferDelayMs === 0 && typeof entry.jitterBufferDelay === 'number' && typeof entry.jitterBufferEmittedCount === 'number' && entry.jitterBufferEmittedCount > 0) {
        jitterBufferDelayMs = Math.round((entry.jitterBufferDelay / entry.jitterBufferEmittedCount) * 1000);
      }
      if (typeof entry.freezeCount === 'number') freezeCountCum = entry.freezeCount;
      if (typeof entry.totalFreezesDuration === 'number') totalFreezesDurationCum = entry.totalFreezesDuration;
      if (typeof entry.pliCount === 'number') pliCountCum = entry.pliCount;
      if (typeof entry.nackCount === 'number') nackCountCum = entry.nackCount;
      if (typeof entry.totalDecodeTime === 'number') totalDecodeTimeSec = entry.totalDecodeTime;
      if (typeof entry.framesDecoded === 'number') framesDecoded = entry.framesDecoded;
    });

    if (!hasVideo) {
      // No remote video track — nothing to report
      return;
    }

    const totalPackets = packetsReceived + packetsLost;
    const packetLossPercent = totalPackets > 0
      ? Math.round((packetsLost / totalPackets) * 1000) / 10
      : 0;

    const framesDropped = Math.max(0, framesDroppedCum - this.prevVideoRecvFramesDropped);
    const freezeCount = Math.max(0, freezeCountCum - this.prevVideoRecvFreezeCount);
    const freezeDurationMs = Math.round(
      Math.max(0, totalFreezesDurationCum - this.prevVideoRecvTotalFreezesDuration) * 1000,
    );
    const pliCount = Math.max(0, pliCountCum - this.prevVideoRecvPliCount);
    const nackCount = Math.max(0, nackCountCum - this.prevVideoRecvNackCount);

    this.prevVideoRecvFramesDropped = framesDroppedCum;
    this.prevVideoRecvFreezeCount = freezeCountCum;
    this.prevVideoRecvTotalFreezesDuration = totalFreezesDurationCum;
    this.prevVideoRecvPliCount = pliCountCum;
    this.prevVideoRecvNackCount = nackCountCum;

    const avgDecodeTimeMs = framesDecoded > 0
      ? Math.round((totalDecodeTimeSec / framesDecoded) * 1000 * 10) / 10
      : 0;

    const stats: VideoReceiveStats = {
      fps,
      frameWidth,
      frameHeight,
      framesDropped,
      packetLossPercent,
      jitterBufferDelayMs,
      freezeCount,
      freezeDurationMs,
      pliCount,
      nackCount,
      avgDecodeTimeMs,
    };

    this.callbacks.onVideoReceiveStats?.(stats);
  }

  /** Stop screen share sender stats polling. */
  private stopScreenShareStatsPolling(): void {
    if (this.screenShareStatsInterval !== null) {
      clearInterval(this.screenShareStatsInterval);
      this.screenShareStatsInterval = null;
    }
  }

  /* ─── Audio Level Coalescing ───────────────────────────────────── */

  private scheduleAudioLevelFlush(): void {
    if (this.rafId !== null) return; // already scheduled
    this.rafId = requestAnimationFrame(() => {
      this.rafId = null;
      if (this.disposed) return;
      if (this.pendingLevels.size > 0) {
        this.callbacks.onAudioLevels(new Map(this.pendingLevels));
        this.pendingLevels.clear();
      }
    });
  }

  /* ─── Local Mic Level Monitor ───────────────────────────────────── */

  /**
   * Monitor the local mic track's RMS level via a Web Audio AnalyserNode.
   * Fires onLocalAudioLevel every 50ms so the self participant's voice
   * indicator works on Windows/macOS (where LiveKitModule is the active path).
   * Without this, the self green circle only appears via server-side
   * ActiveSpeakersChanged which can be delayed or unreliable.
   */
  private startLocalMicMonitor(mediaStreamTrack: MediaStreamTrack): void {
    this.stopLocalMicMonitor();
    try {
      const ctx = this.ensureAudioContext();
      const stream = new MediaStream([mediaStreamTrack]);
      this.localMicSource = ctx.createMediaStreamSource(stream);
      this.localMicAnalyser = ctx.createAnalyser();
      this.localMicAnalyser.fftSize = 2048;
      // Connect source → analyser only (no output — we don't want to hear ourselves)
      this.localMicSource.connect(this.localMicAnalyser);

      const buf = new Float32Array(2048);
      this.localMicInterval = setInterval(() => {
        if (this.disposed || !this.localMicAnalyser) return;
        this.localMicAnalyser.getFloatTimeDomainData(buf);
        let sumSq = 0;
        const len = this.localMicAnalyser.fftSize;
        for (let i = 0; i < len; i++) {
          sumSq += buf[i] * buf[i];
        }
        const rms = Math.sqrt(sumSq / len);
        this.callbacks.onLocalAudioLevel(rms);
      }, 50);
      console.log(LOG, 'local mic monitor started');
    } catch (err) {
      console.warn(LOG, 'failed to start local mic monitor:', err instanceof Error ? err.message : String(err));
    }
  }

  private stopLocalMicMonitor(): void {
    if (this.localMicInterval !== null) {
      clearInterval(this.localMicInterval);
      this.localMicInterval = null;
    }
    if (this.localMicSource) {
      this.localMicSource.disconnect();
      this.localMicSource = null;
    }
    this.localMicAnalyser = null;
  }

  /* ─── AnalyserNode RMS Polling ────────────────────────────────── */

  /**
   * Poll AnalyserNodes every 50ms to compute real RMS levels for remote
   * participants. This provides audio level data independent of LiveKit's
   * ActiveSpeakersChanged event, which may not fire for participants
   * connected via the native Rust SDK path.
   */
  private startAnalyserPolling(): void {
    if (this.analyserInterval !== null) return; // already running
    const buf = new Float32Array(2048);
    this.analyserInterval = setInterval(() => {
      if (this.disposed || this.analyserMap.size === 0) return;
      const levels = new Map<string, { isSpeaking: boolean; rmsLevel: number }>();
      let hasNonZero = false;
      for (const [identity, analyser] of this.analyserMap) {
        analyser.getFloatTimeDomainData(buf);
        let sumSq = 0;
        const len = analyser.fftSize;
        for (let i = 0; i < len; i++) {
          sumSq += buf[i] * buf[i];
        }
        const rms = Math.sqrt(sumSq / len);
        if (rms > 0.001) hasNonZero = true;
        levels.set(identity, { isSpeaking: rms > 0.02, rmsLevel: rms });
      }
      // Only emit when there's actual audio signal to avoid flooding
      // the voice-room layer with zero-level updates
      if (hasNonZero && levels.size > 0) {
        this.callbacks.onAudioLevels(levels);
      }
    }, 50);
  }

  /* ─── Native Mic Bridge (Windows + denoiseEnabled) ──────────── */

  /** Start the native mic bridge, publish the resulting track to LiveKit,
   *  and notify voice-room via onNativeMicBridgeState. */
  private async startNativeMicBridge(denoiseEnabled: boolean): Promise<void> {
    const bridge = new NativeMicBridge();
    this.nativeMicBridge = bridge;

    const savedInputDeviceId = await getAudioInputDevice().catch(() => undefined);
    const track = await bridge.start(denoiseEnabled, savedInputDeviceId || undefined);

    if (!this.room) throw new Error('room disconnected while starting native mic bridge');

    await this.room.localParticipant.publishTrack(track, {
      source: Track.Source.Microphone,
      name: 'native-mic',
    });

    this.callbacks.onNativeMicBridgeState?.(true);
    console.log(LOG, 'native mic bridge started and Microphone track published');
  }

  /* ─── Placeholder methods (implemented in later tasks) ─────── */

  /* ─── WASAPI Audio Bridge ─────────────────────────────────────── */

  /**
   * Start the WASAPI audio bridge: load an AudioWorklet, create a
   * MediaStreamTrack from its output, and publish it via LiveKit as
   * ScreenShareAudio. Called from voice-room.ts after `audio_share_start`
   * succeeds on the Rust side.
   */
  async startWasapiAudioBridge(loopbackExclusionAvailable = false): Promise<void> {
    if (!this.room) throw new Error('room not connected');
    this.wasapiFrameCount = 0; // reset diagnostic counter
    // Tear down any existing bridge before starting a new one (e.g. share switch).
    if (this.wasapiWorkletNode || this.wasapiAudioPublication) {
      if (DEBUG_WASAPI) console.log(LOG, '[wasapi] existing bridge active — stopping before restart');
      await this.stopWasapiAudioBridge();
    }
    if (DEBUG_WASAPI) console.log(LOG, '[wasapi] startWasapiAudioBridge — room ok, getting audio context');
    if (!this.wasapiAudioCtx || this.wasapiAudioCtx.state === 'closed') {
      this.wasapiAudioCtx = new AudioContext({ sampleRate: 48_000 });
    }
    const ctx = this.wasapiAudioCtx;
    if (DEBUG_WASAPI) console.log(LOG, '[wasapi] AudioContext state:', ctx.state, 'sampleRate:', ctx.sampleRate);

    // Resume the AudioContext if suspended — WebKit (macOS) creates contexts
    // in suspended state and is stricter than WebView2 (Windows) about
    // auto-resuming. Without an explicit resume the worklet's process()
    // method never runs and the published track is silent.
    if (ctx.state === 'suspended') {
      try {
        await ctx.resume();
        if (DEBUG_WASAPI) console.log(LOG, '[wasapi] AudioContext resumed, state now:', ctx.state);
        if (DEBUG_MAC_SHARE_AUDIO) console.log(LOG, '[mac-share-audio] AudioContext was suspended — resumed, state now: %s', ctx.state);
      } catch (e) {
        console.warn(LOG, '[wasapi] AudioContext resume failed (will try anyway):', e);
      }
    }
    if (DEBUG_MAC_SHARE_AUDIO) console.log(LOG, '[mac-share-audio] AudioContext state before worklet load: %s sampleRate: %d', ctx.state, ctx.sampleRate);

    // Load the worklet processor (Vite resolves the URL at build time).
    const workletUrl = new URL('./wasapi-audio-worklet.js', import.meta.url).href;
    if (DEBUG_WASAPI) console.log(LOG, '[wasapi] loading AudioWorklet module from:', workletUrl);
    try {
      await ctx.audioWorklet.addModule(workletUrl);
      if (DEBUG_WASAPI) console.log(LOG, '[wasapi] AudioWorklet module loaded');
    } catch (e) {
      if (DEBUG_WASAPI) console.error(LOG, '[wasapi] AudioWorklet addModule FAILED:', e);
      throw e;
    }

    // Create worklet node (mono output, 48 kHz — matches Rust capture format).
    if (DEBUG_WASAPI) console.log(LOG, '[wasapi] creating AudioWorkletNode wasapi-audio-processor');
    this.wasapiWorkletNode = new AudioWorkletNode(ctx, 'wasapi-audio-processor', {
      outputChannelCount: [1],
    });
    if (DEBUG_WASAPI) console.log(LOG, '[wasapi] AudioWorkletNode created');

    // Route worklet output to a MediaStream so we can publish it as a track.
    if (DEBUG_WASAPI) console.log(LOG, '[wasapi] creating MediaStreamAudioDestinationNode');
    this.wasapiDestNode = ctx.createMediaStreamDestination();
    this.wasapiWorkletNode.connect(this.wasapiDestNode);

    const audioTrack = this.wasapiDestNode.stream.getAudioTracks()[0];
    if (DEBUG_WASAPI) console.log(LOG, '[wasapi] audio tracks on dest stream:', this.wasapiDestNode.stream.getAudioTracks().length, 'track:', audioTrack);
    if (!audioTrack) throw new Error('no audio track from WASAPI worklet destination');

    // Publish as ScreenShareAudio via LiveKit.
    if (DEBUG_WASAPI) console.log(LOG, '[wasapi] publishing ScreenShareAudio track via LiveKit');
    this.wasapiAudioPublication = await this.room.localParticipant.publishTrack(
      audioTrack,
      {
        source: Track.Source.ScreenShareAudio,
        // Keep the synthetic share-audio track grouped under a stable logical
        // stream so remote subscribers can map it back to the publisher instead
        // of treating the browser-generated MediaStream UUID as a participant sid.
        stream: Track.Source.ScreenShare,
      },
    );
    if (DEBUG_WASAPI) console.log(LOG, '[wasapi] ScreenShareAudio published, publication:', this.wasapiAudioPublication);
    if (DEBUG_MAC_SHARE_AUDIO) {
      const pub = this.wasapiAudioPublication;
      const mst = pub?.track?.mediaStreamTrack;
      console.log(LOG, '[mac-share-audio] ScreenShareAudio published — trackSid=%s source=%s muted=%s readyState=%s enabled=%s ctxState=%s',
        pub?.trackSid ?? 'null',
        pub?.source ?? 'null',
        pub?.isMuted ?? 'null',
        mst?.readyState ?? 'null',
        mst?.enabled ?? 'null',
        this.wasapiAudioCtx?.state ?? 'null',
      );
    }
    console.log(LOG, 'WASAPI audio bridge started — publishing ScreenShareAudio');

    // ── Mute local LiveKit playback to prevent echo ────────────────────────
    // On Windows, WASAPI loopback captures all process audio including
    // WKWebView playback, so masterGain is muted during sharing to break
    // the echo loop.
    //
    // On macOS, both capture paths report loopbackExclusionAvailable=true:
    //   - SCK (12.3–14.1): setExcludesCurrentProcessAudio handles WebKit helpers
    //   - Tap (14.2+): CATapDescription excludes Wavis PIDs directly
    // No muting needed — doing so prevents the sharer from hearing peers.
    // Three-tier macOS behavior: tap and virtual-device paths isolate room audio,
    // while bare SCK falls back to the SharePicker echo warning instead of muting.
    const shouldMuteForEchoPrevention = !loopbackExclusionAvailable && !isMac();
    if (!loopbackExclusionAvailable && isMac() && DEBUG_SHARE_AUDIO) {
      console.log(LOG, '[share-audio] macOS bare-SCK fallback active - keeping masterGain live and relying on echo warning UX');
    }
    if (shouldMuteForEchoPrevention && this.masterGain && this.audioContext) {
      if (this.preShareGain === null) {         // guard: don't overwrite if already muted
        this.preShareGain = this.masterGain.gain.value;
      }
      this.masterGain.gain.setValueAtTime(0, this.audioContext.currentTime);
      console.log(LOG, '[share-audio] local playback muted for echo prevention (preShareGain=%f)', this.preShareGain);
      if (DEBUG_SHARE_AUDIO) console.log(LOG, '[share-audio] masterGain was %f → 0', this.preShareGain);
    } else if (loopbackExclusionAvailable) {
      if (DEBUG_SHARE_AUDIO) console.log(LOG, '[share-audio] echo prevention skipped — loopbackExclusionAvailable=true');
    } else {
      if (DEBUG_SHARE_AUDIO) console.log(LOG, '[share-audio] masterGain not available yet — echo prevention skipped');
    }

    // Register Tauri event listeners to feed PCM frames from the Rust capture
    // thread into the AudioWorklet. Stored on the instance for cleanup.
    this.wasapiFrameUnlisten = await listen<string>('wasapi_audio_frame', (event) => {
      this.onWasapiAudioFrame(event.payload);
    });
    this.wasapiStoppedUnlisten = await listen('wasapi_audio_stopped', async () => {
      console.warn(LOG, '[wasapi] wasapi_audio_stopped event received — tearing down bridge (framesReceived=%d)', this.wasapiFrameCount);
      await this.stopWasapiAudioBridge();
    });
    console.log(LOG, '[wasapi] Tauri event listeners registered');

    // Watchdog: warn if no frames arrive 5s after bridge start.
    // This fires unconditionally so we can see it without debug flags.
    const frameCountAtStart = this.wasapiFrameCount;
    setTimeout(() => {
      if (this.wasapiWorkletNode && this.wasapiFrameCount === frameCountAtStart) {
        console.warn(LOG, '[wasapi] WATCHDOG: 0 audio frames received 5s after bridge start — Rust SCK may not be emitting events');
      } else if (this.wasapiWorkletNode) {
        console.log(LOG, `[wasapi] WATCHDOG: ${this.wasapiFrameCount} frames received in first 5s ✓`);
      }
    }, 5000);
  }

  /**
   * Feed a base64-encoded i16 LE PCM frame into the AudioWorklet.
   * Called for every "wasapi_audio_frame" Tauri event (~50 Hz, 960 samples each).
   */
  onWasapiAudioFrame(b64Data: string): void {
    if (!this.wasapiWorkletNode) return;

    // Decode base64 → Uint8Array → Int16Array → Float32Array.
    const raw = atob(b64Data);
    const bytes = new Uint8Array(raw.length);
    for (let i = 0; i < raw.length; i++) bytes[i] = raw.charCodeAt(i);
    const i16 = new Int16Array(bytes.buffer);
    const f32 = new Float32Array(i16.length);
    for (let i = 0; i < i16.length; i++) f32[i] = i16[i] / 32768;

    // Diagnostic: log first frame and every 500th frame (~10s at 50Hz) if debug enabled
    this.wasapiFrameCount++;
    if (DEBUG_WASAPI && (this.wasapiFrameCount === 1 || this.wasapiFrameCount % 500 === 0)) {
      const peak = f32.reduce((max, s) => Math.max(max, Math.abs(s)), 0);
      console.log(LOG, `[wasapi] frame #${this.wasapiFrameCount} — samples: ${f32.length}, peak: ${peak.toFixed(4)}`);
    }
    if (DEBUG_MAC_SHARE_AUDIO && (this.wasapiFrameCount <= 3 || this.wasapiFrameCount % 250 === 0)) {
      const peak = f32.reduce((max, s) => Math.max(max, Math.abs(s)), 0);
      const rms = Math.sqrt(f32.reduce((sum, s) => sum + s * s, 0) / f32.length);
      console.log(LOG, `[mac-share-audio] worklet frame #${this.wasapiFrameCount} — samples: ${f32.length}, peak: ${peak.toFixed(4)}, rms: ${rms.toFixed(4)}, ctx: ${this.wasapiAudioCtx?.state ?? 'null'}`);
    }

    // Post to the worklet's ring buffer.
    this.wasapiWorkletNode.port.postMessage(f32);
  }

  /**
   * Stop the WASAPI audio bridge: unpublish the track, disconnect nodes,
   * and send a poison pill to the worklet to stop processing.
   */
  async stopWasapiAudioBridge(): Promise<void> {
    if (DEBUG_MAC_SHARE_AUDIO) {
      const hasWorklet = !!this.wasapiWorkletNode;
      const hasPub = !!this.wasapiAudioPublication;
      console.log(LOG, '[mac-share-audio] stopWasapiAudioBridge called — hasWorklet=%s hasPub=%s framesReceived=%d',
        hasWorklet, hasPub, this.wasapiFrameCount);
      if (hasWorklet || hasPub) console.trace('[mac-share-audio] stopWasapiAudioBridge call stack');
    }
    // Unregister Tauri event listeners first to stop feeding frames.
    if (this.wasapiFrameUnlisten) {
      this.wasapiFrameUnlisten();
      this.wasapiFrameUnlisten = null;
    }
    if (this.wasapiStoppedUnlisten) {
      this.wasapiStoppedUnlisten();
      this.wasapiStoppedUnlisten = null;
    }
    const wasapiAudioCtx = this.wasapiAudioCtx;
    this.wasapiAudioCtx = null;
    const closeAudioCtxPromise = wasapiAudioCtx
      ? wasapiAudioCtx.close().catch(() => {})
      : Promise.resolve();
    if (this.room) {
      // Find and unpublish any screen_share_audio track — works even if the
      // stored publication object has a stale/null .track reference.
      const screenAudioPub = [...this.room.localParticipant.trackPublications.values()]
        .find(p => p.source === Track.Source.ScreenShareAudio);
      if (screenAudioPub?.track) {
        try {
          await this.room.localParticipant.unpublishTrack(screenAudioPub.track);
        } catch { /* best-effort */ }
      }
    }
    this.wasapiAudioPublication = null;
    if (this.wasapiWorkletNode) {
      this.wasapiWorkletNode.port.postMessage(null); // poison pill
      this.wasapiWorkletNode.disconnect();
      this.wasapiWorkletNode = null;
    }
    if (this.wasapiDestNode) {
      this.wasapiDestNode.disconnect();
      this.wasapiDestNode = null;
    }
    await closeAudioCtxPromise;

    // ── Restore local LiveKit playback ─────────────────────────────────────
    if (this.preShareGain !== null) {
      if (this.masterGain && this.audioContext) {
        this.masterGain.gain.setValueAtTime(this.preShareGain, this.audioContext.currentTime);
        console.log(LOG, '[share-audio] local playback restored (gain=%f)', this.preShareGain);
        if (DEBUG_SHARE_AUDIO) console.log(LOG, '[share-audio] masterGain restored to %f', this.preShareGain);
      } else {
        // masterGain was torn down before stop (e.g. room disconnect) — just clear state.
        if (DEBUG_SHARE_AUDIO) console.log(LOG, '[share-audio] masterGain gone on stop — clearing preShareGain without restore');
      }
      this.preShareGain = null; // always clear, regardless of whether restore succeeded
    }

    console.log(LOG, 'WASAPI audio bridge stopped');
  }

  /**
   * Start native process-exclusion audio capture for screen share
   * (Windows/macOS). Invokes the Rust `audio_share_start` command and starts
   * the JS AudioWorklet bridge.
   */
  private async startWasapiScreenShareAudio(): Promise<void> {
    if (DEBUG_WASAPI) console.log(LOG, '[wasapi] startWasapiScreenShareAudio — invoking audio_share_start sourceId=system');
    // Stop any existing native capture session before starting a new one
    // (handles share switching without the Rust-side "already in progress" guard firing).
    await this.stopWasapiScreenShareAudio().catch(() => {});
    try {
      const result = await invoke<AudioShareStartResult>('audio_share_start', { sourceId: 'system' });
      // Always log loopback_exclusion_available — it determines whether echo prevention is needed.
      console.log(LOG, '[share-audio] audio_share_start → loopback_exclusion_available=%s',
        result?.loopback_exclusion_available);
      if (result?.real_output_device_id) {
        console.log(LOG, '[share-audio] audio_share_start -> real_output_device_id=%s name=%s', result.real_output_device_id, result.real_output_device_name ?? '(none)');
        await this.pinShareAudioOutputToRealDevice(result.real_output_device_id, result.real_output_device_name);
      }
      if (DEBUG_SHARE_AUDIO && !result?.loopback_exclusion_available && isMac()) {
        console.warn(LOG, '[share-audio] loopback isolation unavailable on macOS bare-SCK fallback â€” SharePicker echo warning stays active and local playback remains live');
      }
      if (DEBUG_SHARE_AUDIO && !result?.loopback_exclusion_available && !isMac()) {
        console.warn(LOG, '[share-audio] loopback exclusion NOT available (macOS <14.2 or Windows) — ' +
          'masterGain will be zeroed during share to prevent echo');
      }
      await this.startWasapiAudioBridge(result?.loopback_exclusion_available ?? false);
      console.log(LOG, 'native screen share audio started');
    } catch (err) {
      await this.stopWasapiScreenShareAudio().catch(() => {});
      console.warn(LOG, '[wasapi] startWasapiScreenShareAudio FAILED:', err instanceof Error ? err.message : String(err), err);
      throw err instanceof Error ? err : new Error(String(err));
    }
  }

  /**
   * Stop native share-audio capture for screen share (Windows/macOS).
   * Stops the AudioWorklet bridge and invokes the Rust `audio_share_stop`
   * command.
   */
  private async stopWasapiScreenShareAudio(): Promise<void> {
    if (DEBUG_WASAPI) console.log(LOG, '[wasapi] stopWasapiScreenShareAudio');
    await this.stopWasapiAudioBridge();
    try {
      await invoke('audio_share_stop');
      if (DEBUG_WASAPI) console.log(LOG, '[wasapi] audio_share_stop invoked');
    } catch (e) {
      if (DEBUG_WASAPI) console.warn(LOG, '[wasapi] audio_share_stop failed (best-effort):', e);
    } finally {
      await this.restoreAudioOutputDeviceAfterShare().catch((err) => {
        console.warn(LOG, '[share-audio] failed to restore room audio output after share stop:', err instanceof Error ? err.message : String(err));
      });
    }
  }

  private attachAudioTrack(participant: RemoteParticipant, track: RemoteTrack): void {
    const identity = participant.identity;

    // Reuse or create audio element
    let audioEl = this.audioElementMap.get(identity);
    if (audioEl) {
      audioEl.pause();
      audioEl.srcObject = null;
    } else {
      audioEl = document.createElement('audio');
      audioEl.autoplay = true;
      audioEl.muted = true; // playback via Web Audio graph only — muted prevents double-output
      document.body.appendChild(audioEl);
      this.audioElementMap.set(identity, audioEl);
    }

    // Set the track's MediaStream on the element
    const stream = new MediaStream([track.mediaStreamTrack]);
    audioEl.srcObject = stream;

    // Web Audio routing: source → analyser → participantGain → masterGain → destination
    const ctx = this.ensureAudioContext();
    const source = ctx.createMediaStreamSource(stream);
    const analyser = ctx.createAnalyser();
    analyser.fftSize = 2048;
    const gain = ctx.createGain();
    const desiredVol = this.desiredParticipantVolumes.get(identity) ?? 70;
    gain.gain.setValueAtTime(perceptualGain(desiredVol), ctx.currentTime);
    source.connect(analyser);
    analyser.connect(gain);
    gain.connect(this.masterGain!);
    this.participantGains.set(identity, gain);
    this.analyserMap.set(identity, analyser);

    // Start the shared analyser polling interval if not already running
    this.startAnalyserPolling();
  }

  /**
   * Linux/WebKit can occasionally surface a share-audio subscription through
   * the generic audio path. If we already attached the participant's primary
   * voice audio and they also have an active screen share, treat that extra
   * audio track as deferred screen-share audio.
   */
  private isDeferredScreenShareAudioTrack(
    participant: RemoteParticipant,
    publication: RemoteTrackPublication,
    track: RemoteTrack,
  ): boolean {
    if (track.kind !== Track.Kind.Audio) return false;
    if (publication.source === Track.Source.ScreenShareAudio) return true;

    const screenShareAudioKey = `${participant.identity}:screen-share`;
    if (this.screenShareAudioTracks.has(participant.identity) || this.audioElementMap.has(screenShareAudioKey)) {
      return false;
    }

    if (!this.audioElementMap.has(participant.identity)) return false;

    const trackPublications = participant.trackPublications;
    if (!trackPublications || typeof trackPublications.values !== 'function') {
      return false;
    }

    for (const pub of trackPublications.values()) {
      if (pub.source === Track.Source.ScreenShare) {
        return true;
      }
    }

    return false;
  }


  /**
   * Apply suppressLocalAudioPlayback on a MediaStreamTrack post-capture.
   *
   * The LiveKit SDK's screenCaptureToDisplayMediaStreamOptions() strips
   * suppressLocalAudioPlayback before passing options to getDisplayMedia(),
   * so the constraint never reaches the browser. We apply it directly on
   * the track after capture to prevent the sharer from hearing their own
   * captured audio through local speakers.
   *
   * Supported in Chromium 109+ / WebView2. Silently ignored elsewhere.
   */
  private suppressLocalAudioOnTrack(track: MediaStreamTrack | undefined): void {
    if (!track) return;
    try {
      // eslint-disable-next-line @typescript-eslint/no-explicit-any
      if ('suppressLocalAudioPlayback' in track && typeof (track as any).suppressLocalAudioPlayback !== 'undefined') {
        // eslint-disable-next-line @typescript-eslint/no-explicit-any
        (track as any).suppressLocalAudioPlayback = true;
        console.log(LOG, 'suppressLocalAudioPlayback applied on screen share audio track');
      } else {
        // Fallback: try applyConstraints (older Chromium path)
        track.applyConstraints({ suppressLocalAudioPlayback: true } as MediaTrackConstraints)
          .then(() => console.log(LOG, 'suppressLocalAudioPlayback applied via applyConstraints'))
          .catch(() => console.log(LOG, 'suppressLocalAudioPlayback not supported by this webview'));
      }
    } catch {
      console.log(LOG, 'suppressLocalAudioPlayback not supported by this webview');
    }
  }

  /**
   * Find and suppress local playback on all local screen share audio tracks.
   * Called after setScreenShareEnabled(true) to work around the LiveKit SDK
   * stripping suppressLocalAudioPlayback from getDisplayMedia() options.
   */
  private suppressLocalScreenShareAudio(): void {
    if (!this.room) return;
    for (const pub of this.room.localParticipant.trackPublications.values()) {
      if (pub.source === Track.Source.ScreenShareAudio && pub.track?.mediaStreamTrack) {
        this.suppressLocalAudioOnTrack(pub.track.mediaStreamTrack);
      }
    }
  }

  private cleanupParticipantAudio(identity: string): void {
    const audioEl = this.audioElementMap.get(identity);
    if (audioEl) {
      audioEl.pause();
      audioEl.srcObject = null;
      audioEl.remove();
      this.audioElementMap.delete(identity);
    }

    const gain = this.participantGains.get(identity);
    if (gain) {
      gain.disconnect();
      this.participantGains.delete(identity);
    }

    this.analyserMap.delete(identity);
    // Stop polling if no more analysers
    if (this.analyserMap.size === 0 && this.analyserInterval !== null) {
      clearInterval(this.analyserInterval);
      this.analyserInterval = null;
    }
  }

  /**
   * Attach deferred screen share audio for a participant.
   * Called when the user opens the screen share viewer window.
   */
  attachScreenShareAudio(participantIdentity: string): void {
    const publication = this.screenShareAudioPublications.get(participantIdentity);
    if (publication && typeof publication.setSubscribed === 'function') {
      publication.setSubscribed(true);
    }
    if (DEBUG_SHARE_TRACK_SUB) {
      console.log(LOG, `[screen-share-audio] attachScreenShareAudio called for ${participantIdentity}, cached: ${this.screenShareAudioTracks.has(participantIdentity)}, pending: ${this.screenShareAudioPending.has(participantIdentity)}`);
    }
    let entry = this.screenShareAudioTracks.get(participantIdentity);
    if (!entry) {
      // Not in our cache — scan the room directly in case TrackSubscribed was
      // dropped during the LiveKit "participant not present" race on join.
      const participant = this.room?.remoteParticipants.get(participantIdentity);
      if (participant) {
        for (const pub of participant.trackPublications.values()) {
          if (pub.source === Track.Source.ScreenShareAudio && pub.track) {
            entry = { track: pub.track as RemoteTrack, participant };
            this.screenShareAudioTracks.set(participantIdentity, entry);
            console.log(LOG, `[screen-share-audio] recovered track for ${participantIdentity} via room scan`);
            break;
          }
        }
      }
    }
    if (!entry) {
      // Track not yet available — remember to attach when it arrives
      this.screenShareAudioPending.add(participantIdentity);
      return;
    }

    const audioKey = `${participantIdentity}:screen-share`;
    // Avoid double-attach
    if (this.audioElementMap.has(audioKey)) return;

    const audioEl = document.createElement('audio');
    audioEl.autoplay = true;
    audioEl.muted = true; // playback via Web Audio graph only
    document.body.appendChild(audioEl);
    this.audioElementMap.set(audioKey, audioEl);

    const stream = new MediaStream([entry.track.mediaStreamTrack]);
    audioEl.srcObject = stream;

    const ctx = this.ensureAudioContext();
    if (DEBUG_SHARE_TRACK_SUB || DEBUG_SHARE_AUDIO) {
      const mst = entry.track.mediaStreamTrack;
      const settings = typeof mst.getSettings === 'function' ? mst.getSettings() : undefined;
      const constraints = typeof mst.getConstraints === 'function' ? mst.getConstraints() : undefined;
      console.log(LOG, '[screen-share-audio] receiver attach diagnostics', {
        participantIdentity,
        trackId: mst.id,
        trackLabel: mst.label,
        trackKind: mst.kind,
        trackReadyState: mst.readyState,
        trackMuted: mst.muted,
        audioContextSampleRate: ctx.sampleRate,
        audioContextState: ctx.state,
        settings,
        constraints,
      });
    }
    const source = ctx.createMediaStreamSource(stream);
    const gain = ctx.createGain();
    gain.gain.setValueAtTime(perceptualGain(70), ctx.currentTime);
    source.connect(gain);
    gain.connect(this.masterGain!);
    this.participantGains.set(audioKey, gain);

    console.log(LOG, `[mac-share-audio] attached screen share audio — identity=${participantIdentity} trackReadyState=${entry.track.mediaStreamTrack.readyState} trackMuted=${entry.track.isMuted} gainValue=${gain.gain.value.toFixed(3)} masterGainValue=${this.masterGain?.gain.value.toFixed(3) ?? 'null'} audioCtxState=${this.audioContext?.state ?? 'null'}`);
  }

  /**
   * Detach screen share audio for a participant.
   * Called when the user closes the screen share viewer window.
   */
  detachScreenShareAudio(participantIdentity: string): void {
    this.screenShareAudioPending.delete(participantIdentity);
    const publication = this.screenShareAudioPublications.get(participantIdentity);
    if (publication && typeof publication.setSubscribed === 'function') {
      publication.setSubscribed(false);
    }
    this.cleanupParticipantAudio(`${participantIdentity}:screen-share`);
    console.log(LOG, `detached screen share audio for ${participantIdentity}`);
  }

  /* ─── Native Capture Bridge (Windows custom share picker) ────── */

  /** No-op marker — set to a dummy function when prepareNativeCapture has run. */
  private nativeCaptureUnlisten: (() => void) | null = null;
  /** The published LocalTrackPublication for the native capture track. */
  private nativeCapturePublication: LocalTrackPublication | null = null;
  /** DOM-attached canvas used by the captureStream fallback (removed on stop). */
  private nativeCaptureCanvas: HTMLCanvasElement | null = null;
  /** Buffered frames received between prepareNativeCapture and startNativeCapture. */
  private nativeCaptureEarlyFrames: Array<{ frame: string; width: number; height: number }> = [];
  /**
   * Mutable frame handler ref. prepareNativeCapture installs a buffering
   * function, startNativeCapture upgrades it to the real processing function.
   * The polling loop calls feedNativeFrame() which delegates here.
   */
  private nativeCaptureFrameHandler: ((payload: { frame: string; width: number; height: number }) => void) | null = null;
  /** Polling interval ID for screen_share_poll_frame (Windows JS SDK path). */
  private nativeCapturePollInterval: ReturnType<typeof setInterval> | null = null;
  /** Last seen sequence number from poll — used to skip duplicate frames. */
  private nativeCapturePollLastSeq = 0;

  /**
   * Pre-register the frame buffering handler so that frames arriving via
   * the Tauri Channel are captured immediately.
   *
   * Must be called BEFORE `invoke('screen_share_start_source')` to
   * eliminate the race where Rust sends frames before the JS handler
   * exists. Buffered frames are drained by `startNativeCapture()`.
   *
   * This is now synchronous — no Tauri event listener needed because
   * frames arrive via a Tauri Channel (direct IPC pipe), not events.
   *
   * Safe to call multiple times — subsequent calls are no-ops.
   */
  prepareNativeCapture(): void {
    if (this.nativeCaptureUnlisten) return;

    this.nativeCaptureEarlyFrames = [];
    if (this.nativeCaptureLeakSession) {
      this.nativeCaptureLeakSession.summary.counters.earlyFrameBufferPeak = 0;
    }

    // Install the buffering handler — will be upgraded by startNativeCapture.
    this.nativeCaptureFrameHandler = (payload) => {
      // Buffer frames until startNativeCapture upgrades the handler.
      // Cap at 30 frames (~1s) to bound memory.
      if (this.nativeCaptureEarlyFrames.length < 30) {
        this.nativeCaptureEarlyFrames.push(payload);
        if (this.nativeCaptureLeakSession) {
          this.nativeCaptureLeakSession.summary.counters.earlyFrameBufferPeak = Math.max(
            this.nativeCaptureLeakSession.summary.counters.earlyFrameBufferPeak,
            this.nativeCaptureEarlyFrames.length,
          );
        }
      }
      if (this.nativeCaptureEarlyFrames.length === 1) {
        console.log(LOG, `native capture: first early frame buffered (${payload.width}x${payload.height})`);
      }
    };

    // Set a no-op unlisten marker so subsequent calls are no-ops and
    // startNativeCapture knows preparation has happened.
    this.nativeCaptureUnlisten = () => { /* no-op — Channel doesn't need unlistening */ };

    console.log(LOG, 'native capture: prepared frame handler (Channel mode, synchronous)');
    if (DEBUG_CAPTURE) console.log(LOG, 'native capture: pre-registration complete, timestamp:', performance.now());
  }

  /**
   * Feed a single frame into the frame handler.
   * Used internally by the polling loop and available for external callers.
   */
  feedNativeFrame(payload: { frame: string; width: number; height: number }): void {
    if (this.nativeCaptureFrameHandler) {
      this.nativeCaptureFrameHandler(payload);
    }
  }

  /**
   * Start receiving frames from the Rust native capture pipeline and publish
   * them as a screen share track via the LiveKit JS SDK.
   *
   * On Windows, the custom share picker captures frames via the Graphics
   * Capture API (Rust) and writes them to a shared buffer. This method
   * polls that buffer via `invoke('screen_share_poll_frame')` which uses
   * the `ipc://` custom protocol (HTTP-like request/response), completely
   * bypassing PostMessage and the Windows message queue. This makes it
   * immune to HWND corruption when child windows (SharePicker) open/close.
   *
   * If `prepareNativeCapture()` was called first, the handler is already
   * active and buffered frames are drained immediately — no frames lost.
   */
  async startNativeCapture(): Promise<void> {
    if (!this.room) throw new Error('not connected to a room');
    // If already fully active (not just prepared), skip.
    if (this.nativeCapturePublication) return;

    this.syncProfileFromPreset();
    const pubOpts = this.currentPublishOptions;
    const targetFps = pubOpts.screenShareEncoding.maxFramerate || 30;

    // ── Strategy: MediaStreamTrackGenerator (preferred) or canvas fallback ──
    // MediaStreamTrackGenerator (WebCodecs API, Chromium 94+) writes
    // VideoFrames directly into a MediaStreamTrack without canvas quirks.
    // Falls back to a DOM-attached canvas + captureStream if unavailable.

    // eslint-disable-next-line @typescript-eslint/no-explicit-any
    const hasTrackGenerator = typeof (globalThis as any).MediaStreamTrackGenerator === 'function';

    let videoTrack: MediaStreamTrack;
    // eslint-disable-next-line @typescript-eslint/no-explicit-any
    let trackWriter: any = null; // WritableStreamDefaultWriter<VideoFrame>
    let canvas: HTMLCanvasElement | null = null;
    let ctx: CanvasRenderingContext2D | null = null;
    let canvasStream: MediaStream | null = null;

    if (hasTrackGenerator) {
      // ── Primary path: MediaStreamTrackGenerator ──
      // eslint-disable-next-line @typescript-eslint/no-explicit-any
      const generator = new (globalThis as any).MediaStreamTrackGenerator({ kind: 'video' });
      trackWriter = generator.writable.getWriter();
      videoTrack = generator as MediaStreamTrack;
      console.log(LOG, 'native capture: using MediaStreamTrackGenerator');
    } else {
      // ── Fallback: canvas.captureStream ──
      // The canvas MUST be in the DOM for WebView2's compositor to process
      // it — a detached canvas produces a permanently muted track.
      canvas = document.createElement('canvas');
      canvas.width = 1920;
      canvas.height = 1080;
      canvas.style.cssText = 'position:fixed;top:-9999px;left:-9999px;pointer-events:none;opacity:0;';
      document.body.appendChild(canvas);
      this.nativeCaptureCanvas = canvas;
      ctx = canvas.getContext('2d');
      if (!ctx) throw new Error('failed to create canvas 2d context');

      // Paint a non-transparent frame before captureStream to prime the track
      ctx.fillStyle = '#000001';
      ctx.fillRect(0, 0, canvas.width, canvas.height);

      // Use fps-driven captureStream instead of manual requestFrame() —
      // captureStream(0) + requestFrame() has known Chromium bugs in WebView2.
      canvasStream = canvas.captureStream(targetFps);
      videoTrack = canvasStream.getVideoTracks()[0];
      if (!videoTrack) throw new Error('canvas captureStream produced no video track');
      console.log(LOG, `native capture: using canvas.captureStream(${targetFps}) fallback (DOM-attached)`);
    }

    // Fast base64 → ArrayBuffer via fetch (avoids slow atob + char-by-char copy)
    const decodeBase64 = async (b64: string): Promise<ArrayBuffer> => {
      const resp = await fetch(`data:application/octet-stream;base64,${b64}`);
      return resp.arrayBuffer();
    };

    // ── Frame handler shared by both early-frame drain and live listener ──
    let frameCount = 0;
    let firstFrameResolve: (() => void) | null = null;
    const firstFramePromise = new Promise<void>((resolve) => { firstFrameResolve = resolve; });

    const handleFrame = (payload: { frame: string; width: number; height: number }) => {
      const { frame, width, height } = payload;
      if (DEBUG_CAPTURE) console.log(LOG, 'native capture: frame received #' + frameCount, width + 'x' + height);
      if (frameCount === 0) {
        this.markNativeCaptureLeakStage('first_js_frame_seen');
        console.log(LOG, `native capture: first event received (${width}x${height}, payload_len=${frame.length})`);
      }

      if (trackWriter) {
        // ── MediaStreamTrackGenerator path: decode → VideoFrame → write ──
        decodeBase64(frame).then((buf) => {
          const blob = new Blob([buf], { type: 'image/jpeg' });
          return createImageBitmap(blob, { resizeWidth: width, resizeHeight: height });
        }).then((bitmap) => {
          // eslint-disable-next-line @typescript-eslint/no-explicit-any
          const vf = new (globalThis as any).VideoFrame(bitmap, {
            timestamp: performance.now() * 1000, // microseconds
          });
          bitmap.close();
          // Write then close the VideoFrame to prevent backpressure stall
          trackWriter.write(vf).then(() => {
            vf.close();
            frameCount++;
            if (frameCount === 1) {
              if (DEBUG_CAPTURE) console.log(LOG, 'native capture: first VideoFrame written to generator');
              if (firstFrameResolve) {
                firstFrameResolve();
                firstFrameResolve = null;
              }
            }
            if (frameCount === 1 || frameCount % 60 === 0) {
              console.log(LOG, `native capture: wrote VideoFrame #${frameCount} (${width}x${height})`);
            }
          }).catch(() => {
            vf.close();
          });
        }).catch((err) => {
          if (this.nativeCaptureLeakSession) {
            this.nativeCaptureLeakSession.summary.counters.decodeFailures++;
          }
          if (frameCount === 0) {
            console.warn(LOG, 'native capture: first frame decode failed:', err);
          }
        });
      } else if (canvas && ctx) {
        // ── Canvas fallback path ──
        if (canvas.width !== width || canvas.height !== height) {
          canvas.width = width;
          canvas.height = height;
          const newCtx = canvas.getContext('2d');
          if (newCtx) ctx = newCtx;
        }

        const img = new Image();
        img.onload = () => {
          ctx!.drawImage(img, 0, 0);
          frameCount++;
          if (DEBUG_CAPTURE && frameCount === 1) {
            console.log(LOG, 'native capture: first canvas frame painted, timestamp:', performance.now());
          }
          if (frameCount === 1) {
            if (firstFrameResolve) {
              firstFrameResolve();
              firstFrameResolve = null;
            }
          }
          if (frameCount === 1 || frameCount % 60 === 0) {
            console.log(LOG, `native capture: painted frame #${frameCount} (${width}x${height})`);
          }
        };
        img.onerror = () => {
          if (this.nativeCaptureLeakSession) {
            this.nativeCaptureLeakSession.summary.counters.decodeFailures++;
          }
          console.warn(LOG, 'native capture: failed to decode JPEG frame');
        };
        img.src = `data:image/jpeg;base64,${frame}`;
      }
    };

    // ── Start polling loop: fetch frames from Rust via invoke ──────────
    // Uses the ipc:// custom protocol (HTTP request/response), which does
    // NOT use PostMessage or the Windows message queue. Completely immune
    // to HWND corruption from child windows (SharePicker).
    this.nativeCaptureFrameHandler = handleFrame;
    this.nativeCapturePollLastSeq = 0;

    // Drain any early-buffered frames first (from prepareNativeCapture).
    const earlyFrames = this.nativeCaptureEarlyFrames;
    this.nativeCaptureEarlyFrames = [];
    if (earlyFrames.length > 0) {
      console.log(LOG, `native capture: draining ${earlyFrames.length} buffered early frame(s)`);
      for (const ef of earlyFrames) {
        handleFrame(ef);
      }
    }

    // Mark as prepared if not already.
    if (!this.nativeCaptureUnlisten) {
      this.nativeCaptureUnlisten = () => { /* no-op */ };
    }

    // Poll at ~60Hz via setInterval. Each poll calls invoke('screen_share_poll_frame')
    // which returns the latest frame from the Rust shared buffer, or null.
    const POLL_INTERVAL_MS = 16; // ~60fps
    this.nativeCapturePollInterval = setInterval(async () => {
      if (this.nativeCaptureLeakSession) {
        this.nativeCaptureLeakSession.summary.counters.pollTicks++;
      }
      try {
        const result = await invoke<{ frame: string; width: number; height: number; seq: number } | null>(
          'screen_share_poll_frame',
        );
        if (result && result.seq > this.nativeCapturePollLastSeq) {
          if (this.nativeCaptureLeakSession && !this.nativeCaptureLeakSession.summary.stages.first_rust_frame) {
            this.markNativeCaptureLeakStage('first_rust_frame');
          }
          this.nativeCapturePollLastSeq = result.seq;
          if (this.nativeCaptureLeakSession) {
            this.nativeCaptureLeakSession.summary.counters.newFrames++;
          }
          handleFrame({ frame: result.frame, width: result.width, height: result.height });
        } else if (result && this.nativeCaptureLeakSession) {
          this.nativeCaptureLeakSession.summary.counters.duplicateFrameSkips++;
        }
      } catch {
        // invoke failed — capture may have stopped, ignore
      }
    }, POLL_INTERVAL_MS);

    if (DEBUG_CAPTURE) console.log(LOG, 'native capture: polling loop started, timestamp:', performance.now());

    // ── Wait for first frame with timeout ──
    // If early frames already resolved the gate, this resolves immediately.
    const FIRST_FRAME_TIMEOUT_MS = 5000;
    await Promise.race([
      firstFramePromise,
      new Promise<void>((_, reject) =>
        setTimeout(() => reject(new Error('native capture: first frame timeout (5s)')), FIRST_FRAME_TIMEOUT_MS),
      ),
    ]).catch((err) => {
      this.markNativeCaptureFailure(err instanceof Error ? err.message : String(err));
      throw err;
    });

    // Check if stopNativeCapture() was called during the first-frame await
    if (!this.nativeCaptureUnlisten) {
      console.log(LOG, 'native capture: aborted — stopNativeCapture called during startup');
      return;
    }

    // ── Publish track AFTER first frame confirms pipeline is live ──
    // Wrap in try/catch: if publishTrack throws, the polling interval is already
    // running and must be stopped to avoid an orphaned 60Hz IPC loop.
    if (DEBUG_CAPTURE) console.log(LOG, 'native capture: about to call publishTrack, timestamp:', performance.now());
    let publication;
    try {
      publication = await this.room.localParticipant.publishTrack(videoTrack, {
        name: 'native-screen-share',
        source: Track.Source.ScreenShare,
        simulcast: false,
        videoEncoding: {
          maxBitrate: pubOpts.screenShareEncoding.maxBitrate,
          maxFramerate: targetFps,
        },
      });
    } catch (err) {
      this.markNativeCaptureFailure(err instanceof Error ? err.message : String(err));
      await this.stopNativeCapture();
      throw err;
    }
    if (DEBUG_CAPTURE) console.log(LOG, 'native capture: publishTrack completed, timestamp:', performance.now());
    this.nativeCapturePublication = publication;
    this.markNativeCaptureLeakStage('publish_track_done');

    console.log(LOG, `native capture bridge started (fps=${targetFps})`);
  }

  /**
   * Stop the native capture bridge: unpublish the screen share track,
   * stop listening for Tauri events, and clean up the canvas.
   */
  async stopNativeCapture(): Promise<void> {
    const leakSession = this.nativeCaptureLeakSession;
    if (leakSession && leakSession.stopRequestedAtMs === null) {
      leakSession.stopRequestedAtMs = Date.now();
      leakSession.activeRaw = await this.captureRawShareLeakMemorySnapshot();
      this.markNativeCaptureLeakStage('share_stop_requested');
    }

    // Stop the polling loop
    if (this.nativeCapturePollInterval !== null) {
      clearInterval(this.nativeCapturePollInterval);
      this.nativeCapturePollInterval = null;
    }
    if (leakSession) {
      leakSession.summary.cleanupFlags.pollIntervalCleared = this.nativeCapturePollInterval === null;
    }

    // Clear the no-op marker
    this.nativeCaptureUnlisten = null;

    // Clear handler ref and buffered frames
    this.nativeCaptureFrameHandler = null;
    this.nativeCaptureEarlyFrames = [];
    this.nativeCapturePollLastSeq = 0;
    if (leakSession) {
      leakSession.summary.cleanupFlags.frameHandlerCleared = this.nativeCaptureFrameHandler === null;
      leakSession.summary.cleanupFlags.earlyFramesCleared = this.nativeCaptureEarlyFrames.length === 0;
    }

    // Unpublish the track from LiveKit. Null the reference eagerly (before the
    // async unpublish) so a concurrent stop call sees null and skips double-unpublish.
    const pub = this.nativeCapturePublication;
    this.nativeCapturePublication = null;
    if (leakSession) {
      leakSession.summary.cleanupFlags.publicationCleared = this.nativeCapturePublication === null;
    }
    if (pub && this.room) {
      const track = pub.track?.mediaStreamTrack;
      if (track) {
        if (leakSession) {
          leakSession.summary.cleanupFlags.unpublishAttempted = true;
        }
        try {
          await this.room.localParticipant.unpublishTrack(track);
          if (leakSession) {
            leakSession.summary.cleanupFlags.unpublishSucceeded = true;
          }
        } catch { /* best-effort */ }
        this.markNativeCaptureLeakStage('unpublish_done');
        track.stop();
        if (leakSession) {
          leakSession.summary.cleanupFlags.trackStopped = true;
        }
        this.markNativeCaptureLeakStage('track_stopped');
      }
    }

    // Remove DOM-attached canvas (captureStream fallback)
    if (this.nativeCaptureCanvas) {
      this.nativeCaptureCanvas.remove();
      this.nativeCaptureCanvas = null;
    }
    if (leakSession) {
      leakSession.summary.cleanupFlags.canvasRemoved = this.nativeCaptureCanvas === null;
    }

    console.log(LOG, 'native capture bridge stopped');
    await this.finalizeNativeCaptureLeakSession();
  }

  /** Whether the module is currently connected. */
  get isConnected(): boolean {
    return this.room !== null && !this.disposed;
  }
}
