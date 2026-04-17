export type NativeShareLeakStage =
  | 'browser_capture_start'
  | 'native_capture_start'
  | 'first_rust_frame'
  | 'first_js_frame_seen'
  | 'publish_track_done'
  | 'share_stop_requested'
  | 'unpublish_done'
  | 'track_stopped'
  | 'session_closed';

export type ShareLeakCaptureBackend = 'browser-display-media' | 'native-poll';

export interface ShareLeakMemorySample {
  capturedAt: string;
  rssMb: number | null;
  childProcessCount: number | null;
  jsHeapUsedMb: number | null;
  jsHeapTotalMb: number | null;
  domNodes: number;
  deltaRssMb: number | null;
  deltaJsHeapUsedMb: number | null;
  deltaDomNodes: number | null;
}

export interface ShareLeakCounters {
  pollTicks: number;
  newFrames: number;
  duplicateFrameSkips: number;
  decodeFailures: number;
  earlyFrameBufferPeak: number;
  firstFrameLatencyMs: number | null;
  stopCleanupLatencyMs: number | null;
}

export interface ShareLeakCleanupFlags {
  pollIntervalCleared: boolean | null;
  frameHandlerCleared: boolean | null;
  earlyFramesCleared: boolean | null;
  canvasRemoved: boolean | null;
  publicationCleared: boolean | null;
  unpublishAttempted: boolean | null;
  unpublishSucceeded: boolean | null;
  trackStopped: boolean | null;
}

export interface ShareLeakBrowserWebRtcSnapshot {
  capturedAt: string;
  publisherPeerConnectionId: string | null;
  publicationExists: boolean;
  expectedTrackId: string | null;
  publicationTrackId: string | null;
  localScreenSharePublicationCount: number;
  localVideoPublicationCount: number;
  senderCount: number | null;
  videoSenderCount: number | null;
  transceiverCount: number | null;
  screenShareSenderCount: number | null;
  liveVideoSenderTrackIds: string[];
  endedVideoSenderTrackIds: string[];
  transceivers: Array<{
    index: number;
    mid: string | null;
    direction: RTCRtpTransceiverDirection | null;
    currentDirection: RTCRtpTransceiverDirection | null;
    stopped: boolean | null;
    senderTrackId: string | null;
    senderTrackKind: string | null;
    senderTrackReadyState: string | null;
    receiverTrackKind: string | null;
  }>;
}

export interface ShareLeakSenderReuseEvent {
  capturedAt: string;
  name: 'publish_started' | 'publish_snapshot_captured' | 'reuse_inferred';
  detail: string;
}

export interface ShareLeakDegradationPreferenceResult {
  senderWasReused: boolean;
  attemptedPreferences: string[];
  finalErrorName: string | null;
  finalErrorMessage: string | null;
  invalidStateSkipped: boolean;
}

export interface ShareLeakSenderReuseDiagnostics {
  publishWebRtcSnapshot: ShareLeakBrowserWebRtcSnapshot | null;
  reuseExpected: boolean;
  events: ShareLeakSenderReuseEvent[];
  finalSetParametersError: string | null;
  degradationPreferenceResult?: ShareLeakDegradationPreferenceResult | null;
}

export interface ShareSessionLeakSummary {
  shareSessionId: string;
  sourceId: string;
  sourceName: string;
  mode: 'screen_audio' | 'window';
  captureBackend: ShareLeakCaptureBackend;
  startedAt: string;
  endedAt: string;
  stages: Partial<Record<NativeShareLeakStage, string>>;
  counters: ShareLeakCounters;
  cleanupFlags: ShareLeakCleanupFlags;
  browserWebRtcBeforeStop: ShareLeakBrowserWebRtcSnapshot | null;
  browserWebRtcAfterStop: ShareLeakBrowserWebRtcSnapshot | null;
  senderReuseDiagnostics?: ShareLeakSenderReuseDiagnostics | null;
  baselineMemory: ShareLeakMemorySample | null;
  activeMemory: ShareLeakMemorySample | null;
  cleanupMemory: ShareLeakMemorySample | null;
  error: string | null;
}
