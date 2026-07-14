/**
 * RTCStatsReport entry typing.
 *
 * `RTCStatsReport` (DOM lib) is a `Map<string, any>` with no local override,
 * so iterating it directly makes every field read `no-unsafe-member-access`.
 * One entry shape per RTCStats `type`/`kind` combination actually read by
 * livekit-media.ts today. Fields are optional exactly where the current
 * typeof-guards treat them as possibly absent — this matches real-world
 * variability in the W3C webrtc-stats spec (not every entry emits every
 * field), so existing `typeof entry.x === 'number'` guards stay meaningful
 * after typing.
 */

export interface CandidatePairStatsEntry {
  type: 'candidate-pair';
  id: string;
  nominated?: boolean;
  currentRoundTripTime?: number;
  availableOutgoingBitrate?: number;
  localCandidateId?: string;
}

export interface LocalCandidateStatsEntry {
  type: 'local-candidate';
  id: string;
  candidateType?: 'host' | 'srflx' | 'prflx' | 'relay';
}

export interface InboundRtpAudioStatsEntry {
  type: 'inbound-rtp';
  kind: 'audio';
  id: string;
  jitter?: number;
  packetsReceived?: number;
  packetsLost?: number;
  jitterBufferTargetDelay?: number;
  jitterBufferEmittedCount?: number;
  jitterBufferDelay?: number;
  concealmentEvents?: number;
}

export interface InboundRtpVideoStatsEntry {
  type: 'inbound-rtp';
  kind: 'video';
  id: string;
  framesPerSecond?: number;
  frameWidth?: number;
  frameHeight?: number;
  framesDropped?: number;
  packetsReceived?: number;
  packetsLost?: number;
  jitterBufferTargetDelay?: number;
  jitterBufferEmittedCount?: number;
  jitterBufferDelay?: number;
  freezeCount?: number;
  totalFreezesDuration?: number;
  pliCount?: number;
  nackCount?: number;
  totalDecodeTime?: number;
  framesDecoded?: number;
}

/** Any other RTCStats entry type/kind this module doesn't branch on (outbound-rtp, remote-inbound-rtp, ...). */
export interface OtherStatsEntry {
  type: string;
  id: string;
  [key: string]: unknown;
}

export type RtcStatsEntry =
  | CandidatePairStatsEntry
  | LocalCandidateStatsEntry
  | InboundRtpAudioStatsEntry
  | InboundRtpVideoStatsEntry
  | OtherStatsEntry;

/** The one audited cast — isolates RTCStatsReport's built-in `Map<string, any>` typing. */
export function toStatsEntries(report: RTCStatsReport): RtcStatsEntry[] {
  const out: RtcStatsEntry[] = [];
  report.forEach((entry) => out.push(entry as RtcStatsEntry));
  return out;
}

// Explicit type-predicate guards, not `entry.type === 'x'` narrowing: OtherStatsEntry's
// `type: string` overlaps every literal, so TS can't exclude it from a narrowed union via
// equality checks alone (it would leave e.g. `.localCandidateId` typed as `unknown`).

export function isCandidatePairEntry(entry: RtcStatsEntry): entry is CandidatePairStatsEntry {
  return entry.type === 'candidate-pair';
}

export function isLocalCandidateEntry(entry: RtcStatsEntry): entry is LocalCandidateStatsEntry {
  return entry.type === 'local-candidate';
}

export function isInboundRtpAudioEntry(entry: RtcStatsEntry): entry is InboundRtpAudioStatsEntry {
  return entry.type === 'inbound-rtp' && (entry as { kind?: unknown }).kind === 'audio';
}

export function isInboundRtpVideoEntry(entry: RtcStatsEntry): entry is InboundRtpVideoStatsEntry {
  return entry.type === 'inbound-rtp' && (entry as { kind?: unknown }).kind === 'video';
}
