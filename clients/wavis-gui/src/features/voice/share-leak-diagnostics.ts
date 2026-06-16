export type NativeShareLeakStage =
  | 'browser_capture_start'
  | 'native_capture_start'
  | 'quality_sync_done'
  | 'poll_started'
  | 'poll_error'
  | 'first_poll_frame'
  | 'first_js_decode_start'
  | 'first_js_frame_decoded'
  | 'first_generator_write_queued'
  | 'publish_track_start'
  | 'first_rust_frame'
  | 'first_js_frame_seen'
  | 'publish_track_done'
  | 'stats_polling_start'
  | 'share_stop_requested'
  | 'unpublish_done'
  | 'track_stopped'
  | 'session_closed';

export type ShareLeakCaptureBackend =
  | 'browser-display-media'
  | 'native-wgc'
  | 'native-gdi-screen'
  | 'native-gdi-window';

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
  pollErrors: number;
  newFrames: number;
  duplicateFrameSkips: number;
  coalescedFrames: number;
  skippedPollsFrameWorkInFlight: number;
  overlappingPollSkips: number;
  decodeFailures: number;
  keepaliveWrites: number;
  earlyFrameBufferPeak: number;
  firstFrameLatencyMs: number | null;
  stopCleanupLatencyMs: number | null;
}

export interface WindowsNativeCaptureDiagnostics {
  backend: string;
  sourceKind: 'screen' | 'window';
  startupStage: string;
  currentOperation: string;
  startupElapsedMs: number | null;
  startupStepTimings: WindowsNativeStartupStepTiming[];
  captureThreadStartedAtMs: number | null;
  lastStageUpdateAtMs: number | null;
  captureThreadAlive: boolean;
  itemWidth: number;
  itemHeight: number;
  adapterDescription: string | null;
  adapterVendorId: number | null;
  adapterDeviceId: number | null;
  adapterDedicatedVideoMemoryMb: number | null;
  adapterDedicatedSystemMemoryMb: number | null;
  adapterSharedSystemMemoryMb: number | null;
  environment: WindowsNativeCaptureEnvironment;
  timing: WindowsNativeCaptureTiming;
  frameArrivedCallbacks: number;
  rawBackendCallbackFps: number;
  usableFrames: number;
  emittedPollableFrames: number;
  emittedPollableFrameFps: number;
  throttleDropCount: number;
  throttleDropFps: number;
  capDownscaleAvgMs: number;
  i420ConvertAvgMs: number;
  rgbaToRgbAvgMs: number;
  jpegEncodeAvgMs: number;
  base64EncodeAvgMs: number;
  latestFrameWriteAvgMs: number;
  rawToPollableFrameAvgMs: number;
  activeBackend: string;
  previousBackend: string | null;
  retryReason: string | null;
  retryAtMs: number | null;
  tryGetNextFrameFailures: number;
  surfaceFailures: number;
  surfaceCastFailures: number;
  textureInterfaceFailures: number;
  zeroDimensionFrames: number;
  stagingTextureFailures: number;
  mapFailures: number;
  firstError: string | null;
  firstRawFrameLatencyMs: number | null;
  firstBufferedJpegLatencyMs: number | null;
  pollCalls: number;
  pollHits: number;
  pollMisses: number;
  firstPollHitLatencyMs: number | null;
  latestPolledSeq: number | null;
}

export interface WindowsNativeStartupStepTiming {
  stage: string;
  elapsedMs: number;
}

export interface WindowsNativeCaptureEnvironment {
  processId: number;
  globalCpuPercent: number | null;
  processTreeRssMb: number | null;
  processTreeCpuPercent: number | null;
  childProcessCount: number | null;
  webviewProcessCount: number | null;
  monitorCount: number | null;
  selectedSourceWidth: number;
  selectedSourceHeight: number;
  targetFps: number;
  jpegQuality: number;
  videoDriverProvider: string | null;
  videoDriverVersion: string | null;
  videoDriverDate: string | null;
  videoDriverQueryError: string | null;
  overlayProcesses: Array<{ name: string; pid: number }>;
}

export interface WindowsNativeCaptureTiming {
  firstRawFrameLatencyMs: number | null;
  firstCapDownscaleMs: number | null;
  firstI420ConvertMs: number | null;
  firstRgbaToRgbMs: number | null;
  firstJpegEncodeMs: number | null;
  firstBase64EncodeMs: number | null;
  firstLatestFrameWriteMs: number | null;
  firstRawToPollableFrameMs: number | null;
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
  videoSenderCount: number | null;
  reuseExpected: boolean;
  events: ShareLeakSenderReuseEvent[];
  finalSetParametersError: string | null;
  degradationPreferenceResult?: ShareLeakDegradationPreferenceResult | null;
}

export interface ShareLeakStrandedSender {
  trackId: string;
  direction: RTCRtpTransceiverDirection;
}

export interface ShareLeakStrandedSenderInvariant {
  expected: number | null;
  videoSenderCount: number | null;
  stranded: number;
  satisfied: boolean | null;
}

export interface ShareSessionLeakSummary {
  shareSessionId: string;
  sourceId: string;
  sourceName: string;
  mode: 'screen_audio' | 'window';
  captureBackend: ShareLeakCaptureBackend;
  sourceKind?: 'screen' | 'window';
  startedAt: string;
  endedAt: string;
  stages: Partial<Record<NativeShareLeakStage, string>>;
  counters: ShareLeakCounters;
  cleanupFlags: ShareLeakCleanupFlags;
  browserWebRtcBeforeStop: ShareLeakBrowserWebRtcSnapshot | null;
  browserWebRtcAfterStop: ShareLeakBrowserWebRtcSnapshot | null;
  senderReuseDiagnostics?: ShareLeakSenderReuseDiagnostics | null;
  strandedSenders?: ShareLeakStrandedSender[];
  strandedSenderInvariant?: ShareLeakStrandedSenderInvariant | null;
  baselineMemory: ShareLeakMemorySample | null;
  activeMemory: ShareLeakMemorySample | null;
  cleanupMemory: ShareLeakMemorySample | null;
  error: string | null;
  windowsNativeCaptureDiagnostics?: WindowsNativeCaptureDiagnostics | null;
}
