/**
 * Bug Condition Exploration Test — First-Frame Gate Ensures Frames Before Publish
 *
 * **Validates: Requirements 1.1, 1.2, 1.3**
 *
 * This test encodes the CORRECT post-fix behavior:
 *   1. The polling loop starts BEFORE publishTrack()
 *   2. All emitted frames are received by the frame handler (zero dropped)
 *   3. At least one VideoFrame is written to the generator before publishTrack completes
 *
 * The implementation prefers a binary I420 stream and keeps non-overlapping
 * invoke('screen_share_poll_frame') polling as fallback. The first-frame gate
 * ensures publishTrack is only called after at least one frame has been
 * successfully processed.
 */

import { describe, it, expect, vi, beforeEach, afterEach } from 'vitest';
import fc from 'fast-check';

// ─── Operation ordering tracker ────────────────────────────────────

/** Records the order of key operations to detect the race. */
let operationLog: string[];

// ─── Mock: @tauri-apps/api/event ───────────────────────────────────

const mockUnlisten = vi.fn();

vi.mock('@tauri-apps/api/event', () => ({
  listen: vi.fn(async () => mockUnlisten),
}));

// ─── Mock: @tauri-apps/api/core (invoke for polling) ───────────────

/** Sequence counter for poll frames. */
let pollSeq = 0;
/** Number of frames the poll mock should return before returning null. */
let pollFrameCount = 0;
/** Count of frames delivered via invoke polling. */
let pollFramesDelivered = 0;
/** Optional delay for each poll invoke to model a slow Tauri IPC response. */
let pollDelayMs = 0;
/** Number of poll invokes issued. */
let pollInvokeCalls = 0;
/** Whether the raw I420 poll command should return frames. */
let pollI420Available = false;
/** Current number of unresolved poll invokes. */
let activePollInvokes = 0;
/** Highest number of concurrent unresolved poll invokes observed. */
let maxConcurrentPollInvokes = 0;

vi.mock('@tauri-apps/api/core', () => ({
  invoke: vi.fn(async (cmd: string) => {
    if (cmd === 'screen_share_poll_i420_frame') {
      if (!pollI420Available) return null;
      pollInvokeCalls++;
      activePollInvokes++;
      maxConcurrentPollInvokes = Math.max(maxConcurrentPollInvokes, activePollInvokes);
      try {
        if (pollFramesDelivered < pollFrameCount) {
          pollSeq++;
          pollFramesDelivered++;
          operationLog.push('poll_i420_frame');
          return {
            frame: new Array((1920 * 1080 * 3) / 2).fill(128),
            width: 1920,
            height: 1080,
            timestampUs: pollSeq * 16_667,
            seq: pollSeq,
          };
        }
      } finally {
        activePollInvokes--;
      }
      return null;
    }
    if (cmd === 'screen_share_poll_frame') {
      pollInvokeCalls++;
      activePollInvokes++;
      maxConcurrentPollInvokes = Math.max(maxConcurrentPollInvokes, activePollInvokes);
      try {
        if (pollDelayMs > 0) {
          await new Promise<void>((resolve) => {
            setTimeout(resolve, pollDelayMs);
          });
        }
        if (pollFramesDelivered < pollFrameCount) {
          pollSeq++;
          pollFramesDelivered++;
          operationLog.push('poll_frame');
          return { frame: 'AAAA', width: 1920, height: 1080, seq: pollSeq };
        }
      } finally {
        activePollInvokes--;
      }
    }
    return null;
  }),
}));

// ─── Mock: livekit-client ──────────────────────────────────────────

/** Delay (ms) for publishTrack — controlled by fast-check. */
let publishDelayMs: number;

/** Count of VideoFrame writes to the generator. */
let generatorWrites: number;

/** Count of generator writes at the moment publishTrack completes. */
let generatorWritesAtPublishComplete: number;
let createdBitmaps: Array<{ width: number; height: number; close: ReturnType<typeof vi.fn> }> = [];

vi.mock('livekit-client', () => ({
  Room: vi.fn(function (this: Record<string, unknown>) {
    this.connect = vi.fn(async () => {});
    this.disconnect = vi.fn();
    this.on = vi.fn(() => this);
    this.off = vi.fn(() => this);
    this.remoteParticipants = new Map();
    this.localParticipant = {
      setMicrophoneEnabled: vi.fn(async () => {}),
      publishTrack: vi.fn(async () => {
        operationLog.push('publishTrack:start');
        await new Promise<void>((resolve) => {
          setTimeout(() => {
            generatorWritesAtPublishComplete = generatorWrites;
            operationLog.push('publishTrack:end');
            resolve();
          }, publishDelayMs);
        });
        return {
          track: { mediaStreamTrack: { stop: vi.fn() } },
          isMuted: false,
        };
      }),
      unpublishTrack: vi.fn(async () => {}),
      identity: 'self',
      connectionQuality: 'excellent',
      trackPublications: new Map(),
      getTrackPublication: vi.fn(() => undefined),
    };
    this.switchActiveDevice = vi.fn(async () => {});
    return this;
  }),
  RoomEvent: {
    Connected: 'connected',
    Disconnected: 'disconnected',
    Reconnecting: 'reconnecting',
    Reconnected: 'reconnected',
    TrackSubscribed: 'trackSubscribed',
    TrackUnsubscribed: 'trackUnsubscribed',
    ActiveSpeakersChanged: 'activeSpeakersChanged',
    ParticipantDisconnected: 'participantDisconnected',
    ConnectionQualityChanged: 'connectionQualityChanged',
    LocalTrackPublished: 'localTrackPublished',
    LocalTrackUnpublished: 'localTrackUnpublished',
    MediaDevicesError: 'mediaDevicesError',
    TrackMuted: 'trackMuted',
    TrackUnmuted: 'trackUnmuted',
    TrackStreamStateChanged: 'trackStreamStateChanged',
  },
  Track: {
    Kind: { Audio: 'audio', Video: 'video' },
    Source: { Microphone: 'microphone', ScreenShare: 'screen_share' },
    StreamState: { Paused: 'paused', Active: 'active' },
  },
  VideoPreset: vi.fn(),
  ConnectionQuality: {
    Excellent: 'excellent',
    Good: 'good',
    Poor: 'poor',
    Lost: 'lost',
  },
}));

// ─── Mock: Web APIs (MediaStreamTrackGenerator, VideoFrame, etc.) ──

const mockWriter = {
  write: vi.fn(async () => {
    generatorWrites++;
  }),
  close: vi.fn(async () => {}),
  releaseLock: vi.fn(),
};

const mockWritable = {
  getWriter: vi.fn(() => mockWriter),
};

vi.stubGlobal(
  'MediaStreamTrackGenerator',
  function MockMediaStreamTrackGenerator(this: Record<string, unknown>) {
    this.kind = 'video';
    this.writable = mockWritable;
    this.readyState = 'live';
    this.enabled = true;
    this.id = 'mock-generator-track';
    this.stop = vi.fn();
    return this;
  },
);

vi.stubGlobal(
  'VideoFrame',
  function MockVideoFrame(this: Record<string, unknown>, _bitmap: unknown, _opts: unknown) {
    this.close = vi.fn();
    return this;
  },
);

vi.stubGlobal(
  'createImageBitmap',
  vi.fn(async () => {
    const bitmap = {
      width: 1920,
      height: 1080,
      close: vi.fn(),
    };
    createdBitmaps.push(bitmap);
    return bitmap;
  }),
);

vi.stubGlobal(
  'fetch',
  vi.fn(async () => ({
    arrayBuffer: async () => new ArrayBuffer(100),
  })),
);

vi.stubGlobal('navigator', {
  userAgent: '',
  mediaDevices: {
    addEventListener: vi.fn(),
    removeEventListener: vi.fn(),
    enumerateDevices: vi.fn(async () => []),
  },
});

vi.stubGlobal('performance', { now: vi.fn(() => Date.now()) });

vi.stubGlobal('document', {
  createElement: vi.fn(() => ({
    width: 0,
    height: 0,
    style: { cssText: '' },
    getContext: vi.fn(() => ({
      fillStyle: '',
      fillRect: vi.fn(),
      drawImage: vi.fn(),
    })),
    captureStream: vi.fn(() => ({
      getVideoTracks: () => [{ id: 'canvas-track', stop: vi.fn() }],
    })),
    remove: vi.fn(),
  })),
  body: { appendChild: vi.fn() },
  addEventListener: vi.fn(),
  removeEventListener: vi.fn(),
});

vi.stubGlobal('AudioContext', function MockAudioContext(this: Record<string, unknown>) {
  this.state = 'running';
  this.currentTime = 0;
  this.destination = {};
  this.createGain = vi.fn(() => {
    const node = {
      gain: {
        value: 1,
        setValueAtTime: vi.fn((value: number) => {
          node.gain.value = value;
        }),
      },
      connect: vi.fn(),
      disconnect: vi.fn(),
    };
    return node;
  });
  this.createAnalyser = vi.fn(() => ({
    fftSize: 2048,
    connect: vi.fn(),
    disconnect: vi.fn(),
    getFloatTimeDomainData: vi.fn(),
  }));
  this.createMediaStreamSource = vi.fn(() => ({
    connect: vi.fn(),
    disconnect: vi.fn(),
  }));
  this.close = vi.fn(async () => {});
  this.resume = vi.fn(async () => {});
  return this;
});

vi.stubGlobal('MediaStream', function MockMediaStream(this: Record<string, unknown>) {
  this.id = 'mock-stream';
  this.getTracks = () => [];
  return this;
});

vi.stubGlobal(
  'requestAnimationFrame',
  vi.fn((cb: () => void) => {
    cb();
    return 1;
  }),
);
vi.stubGlobal('cancelAnimationFrame', vi.fn());
vi.stubGlobal('Image', function MockImage(this: Record<string, unknown>) {
  this.onload = null;
  this.onerror = null;
  this.src = '';
  return this;
});

// ─── Import module under test ──────────────────────────────────────

import { LiveKitModule, type MediaCallbacks } from '../livekit-media';

// ─── Helpers ───────────────────────────────────────────────────────

function createMockCallbacks(): MediaCallbacks {
  return {
    onMediaConnected: vi.fn(),
    onMediaFailed: vi.fn(),
    onMediaDisconnected: vi.fn(),
    onAudioLevels: vi.fn(),
    onLocalAudioLevel: vi.fn(),
    onActiveSpeakers: vi.fn(),
    onConnectionQuality: vi.fn(),
    onScreenShareSubscribed: vi.fn(),
    onScreenShareUnsubscribed: vi.fn(),
    onLocalScreenShareEnded: vi.fn(),
    onParticipantMuteChanged: vi.fn(),
    onSystemEvent: vi.fn(),
    onShareStats: vi.fn(),
  };
}

async function driveToConnected(mod: LiveKitModule): Promise<void> {
  await mod.connect('wss://sfu.test', 'test-token');

  const room = (mod as any).room;
  if (room && room.on.mock) {
    for (const call of room.on.mock.calls) {
      if (call[0] === 'connected') {
        call[1]();
      }
    }
  }
}

// ─── Test Suite ────────────────────────────────────────────────────

describe('Property 1: Fault Condition — First-Frame Gate Ensures Frames Before Publish', () => {
  beforeEach(() => {
    operationLog = [];
    pollSeq = 0;
    pollFrameCount = 0;
    pollFramesDelivered = 0;
    pollDelayMs = 0;
    pollInvokeCalls = 0;
    pollI420Available = false;
    activePollInvokes = 0;
    maxConcurrentPollInvokes = 0;
    publishDelayMs = 100;
    generatorWrites = 0;
    generatorWritesAtPublishComplete = 0;
    createdBitmaps = [];

    mockUnlisten.mockClear();
    mockWriter.write.mockClear();
    mockWriter.write.mockImplementation(async () => {
      generatorWrites++;
    });
    mockWriter.close.mockClear();
    mockWritable.getWriter.mockClear();
    (globalThis.fetch as unknown as ReturnType<typeof vi.fn>).mockClear();
    (globalThis.createImageBitmap as unknown as ReturnType<typeof vi.fn>).mockClear();

    vi.useFakeTimers({ shouldAdvanceTime: true });
  });

  afterEach(() => {
    vi.useRealTimers();
  });

  it(
    'early frames are processed before publishTrack via prepareNativeCapture + feedNativeFrame',
    { timeout: 30_000 },
    async () => {
      await fc.assert(
        fc.asyncProperty(
          fc.integer({ min: 50, max: 500 }), // publishDelay ms
          fc.integer({ min: 1, max: 15 }), // frameCount
          async (publishDelay, frameCount) => {
            // Reset state for each property run
            operationLog = [];
            pollSeq = 0;
            pollFrameCount = 0; // No frames from polling — we use early frames
            pollFramesDelivered = 0;
            pollDelayMs = 0;
            pollInvokeCalls = 0;
            pollI420Available = false;
            activePollInvokes = 0;
            maxConcurrentPollInvokes = 0;
            generatorWrites = 0;
            generatorWritesAtPublishComplete = 0;
            publishDelayMs = publishDelay;
            mockWriter.write.mockClear();

            const cbs = createMockCallbacks();
            const mod = new LiveKitModule(cbs);
            await driveToConnected(mod);

            // Prepare native capture — installs the buffering handler
            mod.prepareNativeCapture();

            // Feed frames into the early buffer BEFORE startNativeCapture
            for (let i = 0; i < frameCount; i++) {
              mod.feedNativeFrame({ frame: 'AAAA', width: 1920, height: 1080 });
            }

            operationLog.push('early_frames_fed');

            // Start native capture — drains early frames, first-frame gate
            // resolves from the buffered frames, then publishTrack is called
            const capturePromise = mod.startNativeCapture();

            // Advance timers to let async frame processing complete
            // (decode → createImageBitmap → VideoFrame → write)
            for (let i = 0; i < frameCount + 30; i++) {
              await vi.advanceTimersByTimeAsync(20);
            }

            // Advance past the publish delay
            await vi.advanceTimersByTimeAsync(publishDelay + 200);

            await capturePromise.catch(() => {});

            // ── Assertions encoding CORRECT post-fix behavior ──

            // 1. Early frames were fed before publishTrack
            const earlyIdx = operationLog.indexOf('early_frames_fed');
            const publishIdx = operationLog.indexOf('publishTrack:start');
            expect(earlyIdx).toBeGreaterThanOrEqual(0);
            expect(publishIdx).toBeGreaterThanOrEqual(0);
            expect(earlyIdx).toBeLessThan(publishIdx);

            // 2. At least one VideoFrame was written before publishTrack completed
            expect(generatorWritesAtPublishComplete).toBeGreaterThanOrEqual(1);
          },
        ),
        { numRuns: 20 },
      );
    },
  );

  it('publishes after the first generator write is queued, not after write() resolves', async () => {
    let resolveWrite: (() => void) | null = null;
    mockWriter.write.mockImplementation(() => {
      generatorWrites++;
      return new Promise<void>((resolve) => {
        resolveWrite = resolve;
      });
    });
    publishDelayMs = 0;

    const mod = new LiveKitModule(createMockCallbacks());
    await driveToConnected(mod);
    mod.beginNativeCaptureLeakSession({
      shareSessionId: 'coalesce-test',
      mode: 'window',
      sourceId: 'window-1',
      sourceName: 'Window 1',
      sourceKind: 'window',
      captureBackend: 'native-gdi-window',
    });
    mod.prepareNativeCapture();
    mod.feedNativeFrame({ frame: 'AAAA', width: 1920, height: 1080 });

    const capturePromise = mod.startNativeCapture();

    for (let i = 0; i < 10; i++) {
      await vi.advanceTimersByTimeAsync(20);
    }

    expect(generatorWrites).toBe(1);
    expect(operationLog).toContain('publishTrack:start');
    expect(resolveWrite).not.toBeNull();

    resolveWrite!();
    await vi.advanceTimersByTimeAsync(20);
    await capturePromise;
    await mod.stopNativeCapture();
  });

  it('coalesces pending frames while a generator write is in flight', async () => {
    let resolveWrite: (() => void) | null = null;
    mockWriter.write.mockImplementation(() => {
      generatorWrites++;
      return new Promise<void>((resolve) => {
        resolveWrite = resolve;
      });
    });

    const mod = new LiveKitModule(createMockCallbacks());
    await driveToConnected(mod);
    mod.beginNativeCaptureLeakSession({
      shareSessionId: 'coalesce-test',
      mode: 'window',
      sourceId: 'window-1',
      sourceName: 'Window 1',
      sourceKind: 'window',
      captureBackend: 'native-gdi-window',
    });
    mod.prepareNativeCapture();
    for (let i = 0; i < 5; i++) {
      mod.feedNativeFrame({ frame: 'AAAA', width: 1920 + i, height: 1080 });
    }

    const capturePromise = mod.startNativeCapture();
    for (let i = 0; i < 10; i++) {
      await vi.advanceTimersByTimeAsync(20);
    }

    expect(generatorWrites).toBe(1);
    expect(
      (mod as any).nativeCaptureLeakSession?.summary.counters.coalescedFrames ?? 0,
    ).toBeGreaterThanOrEqual(4);

    resolveWrite!();
    await vi.advanceTimersByTimeAsync(20);
    expect(generatorWrites).toBe(2);

    resolveWrite!();
    await vi.advanceTimersByTimeAsync(20);
    await capturePromise;
    await mod.stopNativeCapture();
  });

  it('does not issue overlapping poll invokes when the native poll is slow', async () => {
    publishDelayMs = 0;
    pollFrameCount = 10;
    pollDelayMs = 200;

    const mod = new LiveKitModule(createMockCallbacks());
    await driveToConnected(mod);
    mod.prepareNativeCapture();
    mod.feedNativeFrame({ frame: 'AAAA', width: 1920, height: 1080 });

    const capturePromise = mod.startNativeCapture();
    for (let i = 0; i < 20; i++) {
      await vi.advanceTimersByTimeAsync(20);
    }
    await capturePromise;

    for (let i = 0; i < 20; i++) {
      await vi.advanceTimersByTimeAsync(67);
    }
    await vi.advanceTimersByTimeAsync(250);

    expect(pollInvokeCalls).toBeGreaterThan(0);
    expect(maxConcurrentPollInvokes).toBeLessThanOrEqual(1);

    await mod.stopNativeCapture();
  });

  it('pauses steady polling while generator writes are in flight and resumes afterward', async () => {
    let resolveWrite: (() => void) | null = null;
    mockWriter.write.mockImplementation(() => {
      generatorWrites++;
      return new Promise<void>((resolve) => {
        resolveWrite = resolve;
      });
    });
    publishDelayMs = 0;
    pollFrameCount = 20;

    const mod = new LiveKitModule(createMockCallbacks());
    await driveToConnected(mod);
    mod.beginNativeCaptureLeakSession({
      shareSessionId: 'poll-backpressure-test',
      mode: 'window',
      sourceId: 'window-1',
      sourceName: 'Window 1',
      sourceKind: 'window',
      captureBackend: 'native-gdi-window',
    });

    const capturePromise = mod.startNativeCapture();
    for (let i = 0; i < 10; i++) {
      await vi.advanceTimersByTimeAsync(20);
    }
    await capturePromise;

    const callsAfterStartup = pollInvokeCalls;
    for (let i = 0; i < 5; i++) {
      await vi.advanceTimersByTimeAsync(67);
    }

    expect(resolveWrite).not.toBeNull();
    expect(pollInvokeCalls).toBe(callsAfterStartup);
    expect(
      (mod as any).nativeCaptureLeakSession?.summary.counters.skippedPollsFrameWorkInFlight ?? 0,
    ).toBeGreaterThan(0);

    resolveWrite!();
    await vi.advanceTimersByTimeAsync(67);
    await vi.advanceTimersByTimeAsync(20);
    expect(pollInvokeCalls).toBeGreaterThan(callsAfterStartup);

    resolveWrite = null;
    await mod.stopNativeCapture();
  });

  it('startNativeCapture uses the selected preset FPS instead of capping at 15', async () => {
    publishDelayMs = 0;
    pollFrameCount = 1;

    const cbs = createMockCallbacks();
    const mod = new LiveKitModule(cbs);
    await driveToConnected(mod);
    await mod.setScreenShareQuality('max');

    const capturePromise = mod.startNativeCapture();
    for (let i = 0; i < 20; i++) {
      await vi.advanceTimersByTimeAsync(20);
    }
    await capturePromise;

    const publishCall = operationLog.indexOf('publishTrack:start');
    expect(publishCall).toBeGreaterThanOrEqual(0);

    const room = (mod as any).room;
    const publishArgs = room.localParticipant.publishTrack.mock.calls[0];
    expect(publishArgs[1].videoEncoding.maxFramerate).toBe(60);

    const shareStatsCalls = (cbs.onShareStats as ReturnType<typeof vi.fn>).mock.calls;
    expect(shareStatsCalls.at(-1)?.[0].nativeBridge).toEqual(
      expect.objectContaining({
        rustTargetFps: 60,
        jsBridgeFps: 60,
      }),
    );

    await mod.stopNativeCapture();
  });

  it('startNativeCapture maps high quality to the DETAIL 1440p30 native bridge profile', async () => {
    publishDelayMs = 0;
    pollFrameCount = 1;

    const cbs = createMockCallbacks();
    const mod = new LiveKitModule(cbs);
    await driveToConnected(mod);
    await mod.setScreenShareQuality('high');

    const capturePromise = mod.startNativeCapture();
    for (let i = 0; i < 20; i++) {
      await vi.advanceTimersByTimeAsync(20);
    }
    await capturePromise;

    const room = (mod as any).room;
    const publishArgs = room.localParticipant.publishTrack.mock.calls[0];
    expect(publishArgs[1].videoEncoding.maxFramerate).toBe(30);

    const shareStatsCalls = (cbs.onShareStats as ReturnType<typeof vi.fn>).mock.calls;
    expect(shareStatsCalls.at(-1)?.[0].nativeBridge).toEqual(
      expect.objectContaining({
        rustTargetFps: 30,
        jsBridgeFps: 30,
      }),
    );

    await mod.stopNativeCapture();
  });

  it('writes raw I420 VideoFrames without JPEG/base64 decode when raw polling is available', async () => {
    publishDelayMs = 0;
    pollFrameCount = 1;
    pollI420Available = true;

    const cbs = createMockCallbacks();
    const mod = new LiveKitModule(cbs);
    await driveToConnected(mod);

    const capturePromise = mod.startNativeCapture();
    for (let i = 0; i < 20; i++) {
      await vi.advanceTimersByTimeAsync(20);
    }
    await capturePromise;

    expect(operationLog).toContain('poll_i420_frame');
    expect(generatorWrites).toBeGreaterThan(0);
    expect(globalThis.fetch as unknown as ReturnType<typeof vi.fn>).not.toHaveBeenCalled();
    expect(
      globalThis.createImageBitmap as unknown as ReturnType<typeof vi.fn>,
    ).not.toHaveBeenCalled();

    const stats = (mod as any).nativeBridgeCadenceStats;
    expect(stats.rawI420Frames).toBeGreaterThan(0);
    expect(stats.jpegFallbackFrames).toBe(0);

    await mod.stopNativeCapture();
  });

  it('generated-track keepalive writes cached decoded bitmap without re-decoding, then stops and closes cache', async () => {
    publishDelayMs = 0;
    pollFrameCount = 1;

    const cbs = createMockCallbacks();
    const mod = new LiveKitModule(cbs);
    await driveToConnected(mod);

    const capturePromise = mod.startNativeCapture();
    for (let i = 0; i < 20; i++) {
      await vi.advanceTimersByTimeAsync(20);
    }
    await capturePromise;

    await vi.advanceTimersByTimeAsync(800);
    await vi.advanceTimersByTimeAsync(20);

    const statsBeforeStop = (mod as any).nativeBridgeCadenceStats;
    expect(statsBeforeStop.keepaliveWrites).toBeGreaterThan(0);
    expect(statsBeforeStop.jsDecodedFrames).toBe(1);
    expect(globalThis.fetch as unknown as ReturnType<typeof vi.fn>).toHaveBeenCalledTimes(1);
    expect(
      globalThis.createImageBitmap as unknown as ReturnType<typeof vi.fn>,
    ).toHaveBeenCalledTimes(1);
    expect(createdBitmaps).toHaveLength(1);
    expect(createdBitmaps[0].close).not.toHaveBeenCalled();

    await mod.stopNativeCapture();
    const writesAtStop = generatorWrites;
    expect(createdBitmaps[0].close).toHaveBeenCalledTimes(1);

    await vi.advanceTimersByTimeAsync(1000);
    expect(generatorWrites).toBe(writesAtStop);
  });

  it('new Rust sequence replaces cached decoded bitmap and closes the previous bitmap', async () => {
    publishDelayMs = 0;
    pollFrameCount = 2;

    const mod = new LiveKitModule(createMockCallbacks());
    await driveToConnected(mod);

    const capturePromise = mod.startNativeCapture();
    for (let i = 0; i < 20; i++) {
      await vi.advanceTimersByTimeAsync(20);
    }
    await capturePromise;

    for (let i = 0; i < 20 && createdBitmaps.length < 2; i++) {
      await vi.advanceTimersByTimeAsync(20);
    }

    expect(createdBitmaps).toHaveLength(2);
    expect(createdBitmaps[0].close).toHaveBeenCalledTimes(1);
    expect(createdBitmaps[1].close).not.toHaveBeenCalled();
    expect((mod as any).nativeBridgeCadenceStats.jsDecodedFrames).toBe(2);

    await mod.stopNativeCapture();
    expect(createdBitmaps[1].close).toHaveBeenCalledTimes(1);
  });

  it('replaceNativeCaptureSource releases the previous decoded cache before accepting the new source', async () => {
    publishDelayMs = 0;
    pollFrameCount = 1;

    const mod = new LiveKitModule(createMockCallbacks());
    await driveToConnected(mod);

    const capturePromise = mod.startNativeCapture();
    for (let i = 0; i < 20; i++) {
      await vi.advanceTimersByTimeAsync(20);
    }
    await capturePromise;

    expect(createdBitmaps).toHaveLength(1);
    const previousBitmap = createdBitmaps[0];
    expect(previousBitmap.close).not.toHaveBeenCalled();

    pollFrameCount = 2;
    const replacePromise = mod.replaceNativeCaptureSource();
    expect(previousBitmap.close).toHaveBeenCalledTimes(1);

    for (let i = 0; i < 20; i++) {
      await vi.advanceTimersByTimeAsync(20);
    }
    await replacePromise;

    expect(createdBitmaps).toHaveLength(2);
    expect(createdBitmaps[1].close).not.toHaveBeenCalled();

    await mod.stopNativeCapture();
    expect(createdBitmaps[1].close).toHaveBeenCalledTimes(1);
  });
});
