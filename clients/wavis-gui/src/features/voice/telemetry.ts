import { RingBuffer } from '@shared/ring-buffer';

const DEBUG_SHARE_TELEMETRY = import.meta.env.VITE_DEBUG_SHARE_TELEMETRY === 'true';
const TELEMETRY_BUFFER_CAPACITY = 2_000;
const TELEMETRY_LOG_PREFIX = '[wavis:share-telemetry]';

export interface LayerFloor {
  minWidth: number;
  minHeight: number;
  minBitrate: number;
  minFramerate: number;
}

export type WindowsSharePath = 'browser' | 'native';
export type ShareProfileId = 'detail' | 'motion';

export type TelemetryEvent =
  | { name: 'share.path.selected'; path: WindowsSharePath | 'linux_native' | 'browser_mac'; reason: string; ts: number }
  | { name: 'share.profile.switched'; from: ShareProfileId; to: ShareProfileId; reason: 'auto_in' | 'auto_out' | 'init'; ts: number }
  | { name: 'share.floor.hit'; profileId: 'detail'; floor: LayerFloor; attemptedWidth: number; attemptedHeight: number; attemptedBitrate: number; ts: number }
  | { name: 'share.leak.stranded_sender'; trackId: string; direction: RTCRtpTransceiverDirection; ts: number }
  | { name: 'share.leak.transceiver_cap'; before: number; after: number; ts: number }
  | { name: 'share.reuse_patch.missing'; ts: number }
  | { name: 'network.event'; kind: 'loss' | 'rtt' | 'bwe_drop'; detail: Record<string, number>; ts: number }
  | { name: 'capture.path.selected'; os: 'windows' | 'macos' | 'linux'; path: string; sourceKind?: 'screen' | 'window'; backend?: string; ts: number }
  | { name: 'capture.fallback.activated'; os: 'windows' | 'macos' | 'linux'; from: string; to: string; reason: string; ts: number }
  | { name: 'capture.native.failed'; os: 'windows'; sourceKind: 'screen' | 'window'; backend: string; reason: string; ts: number }
  | { name: 'codec.session.start'; primary: 'vp9' | 'vp8' | 'av1'; ts: number }
  | { name: 'codec.session.fallback'; from: 'vp9' | 'vp8' | 'av1'; to: 'vp9' | 'vp8'; reason: string; ts: number }
  | { name: 'codec.session.end'; smoothnessFreezeMs: number; publisherCpuPctMedian: number; ts: number };

const telemetryBuffer = new RingBuffer<TelemetryEvent>(TELEMETRY_BUFFER_CAPACITY);

export function emitTelemetryEvent(event: TelemetryEvent): TelemetryEvent {
  telemetryBuffer.push(event);
  if (DEBUG_SHARE_TELEMETRY) {
    console.info(TELEMETRY_LOG_PREFIX, event.name, event);
  }
  return event;
}

export function getTelemetrySnapshot(): TelemetryEvent[] {
  return telemetryBuffer.snapshot();
}

export function clearTelemetrySnapshot(): void {
  telemetryBuffer.clear();
}
