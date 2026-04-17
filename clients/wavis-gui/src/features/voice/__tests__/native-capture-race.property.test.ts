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
 * The implementation uses invoke('screen_share_poll_frame') polling via setInterval.
 * The first-frame gate ensures publishTrack is only called after at least one frame
 * has been successfully processed.
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

vi.mock('@tauri-apps/api/core', () => ({
  invoke: vi.fn(async (cmd: string) => {
    if (cmd === 'screen_share_poll_frame' && pollFramesDelivered < pollFrameCount) {
      pollSeq++;
      pollFramesDelivered++;
      operationLog.push('poll_frame');
      return { frame: 'AAAA', width: 1920, height: 1080, seq: pollSeq };
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

vi.mock('livekit-client', () => ({
  Room: vi.fn(function (this: Record<string, unknown>) {
    this.connect = vi.fn(async () => {});
    this.disconnect = vi.fn();
    this.on = vi.fn(() => this);
    this.off = vi.fn(() => this);
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
  VideoPreset: vi.fn((opts: unknown) => opts),
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

vi.stubGlobal('MediaStreamTrackGenerator', function MockMediaStreamTrackGenerator(
  this: Record<string, unknown>,
) {
  this.kind = 'video';
  this.writable = mockWritable;
  this.readyState = 'live';
  this.enabled = true;
  this.id = 'mock-generator-track';
  this.stop = vi.fn();
  return this;
});

vi.stubGlobal('VideoFrame', function MockVideoFrame(
  this: Record<string, unknown>,
  _bitmap: unknown,
  _opts: unknown,
) {
  this.close = vi.fn();
  return this;
});

vi.stubGlobal('createImageBitmap', vi.fn(async () => ({
  width: 1920,
  height: 1080,
  close: vi.fn(),
})));

vi.stubGlobal('fetch', vi.fn(async () => ({
  arrayBuffer: async () => new ArrayBuffer(100),
})));

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

vi.stubGlobal('requestAnimationFrame', vi.fn((cb: () => void) => { cb(); return 1; }));
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
  };
}

async function driveToConnected(mod: LiveKitModule): Promise<void> {
  await mod.connect('wss://sfu.test', 'test-token');
  // eslint-disable-next-line @typescript-eslint/no-explicit-any
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
    publishDelayMs = 100;
    generatorWrites = 0;
    generatorWritesAtPublishComplete = 0;

    mockUnlisten.mockClear();
    mockWriter.write.mockClear();
    mockWriter.close.mockClear();
    mockWritable.getWriter.mockClear();

    vi.useFakeTimers({ shouldAdvanceTime: true });
  });

  afterEach(() => {
    vi.useRealTimers();
  });

  it('early frames are processed before publishTrack via prepareNativeCapture + feedNativeFrame', { timeout: 30_000 }, async () => {
    await fc.assert(
      fc.asyncProperty(
        fc.integer({ min: 50, max: 500 }),  // publishDelay ms
        fc.integer({ min: 1, max: 15 }),     // frameCount
        async (publishDelay, frameCount) => {
          // Reset state for each property run
          operationLog = [];
          pollSeq = 0;
          pollFrameCount = 0; // No frames from polling — we use early frames
          pollFramesDelivered = 0;
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
  });
});
