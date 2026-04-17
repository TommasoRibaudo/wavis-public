/**
 * Property 2: Preservation — Non-Race Paths Unchanged
 *
 * **Validates: Requirements 3.1, 3.3, 3.4, 3.5, 3.6**
 *
 * These tests verify that non-race code paths in startNativeCapture() and
 * stopNativeCapture() behave correctly on UNFIXED code. They must PASS
 * before the fix is applied, confirming these paths are not affected by
 * the listener-after-publish race bug.
 *
 * 3a. Canvas fallback path preservation
 * 3b. Cleanup preservation (stopNativeCapture)
 * 3c. Guard clause preservation
 */

import { describe, it, expect, vi, beforeEach } from 'vitest';
import fc from 'fast-check';

// ─── Mock: @tauri-apps/api/event ───────────────────────────────────

const mockUnlisten = vi.fn();

vi.mock('@tauri-apps/api/event', () => ({
  listen: vi.fn(async () => mockUnlisten),
}));

// ─── Mock: @tauri-apps/api/core (invoke for polling) ───────────────

/** Sequence counter for poll frames — incremented each call. */
let pollSeq = 0;
/** Whether invoke should return frames (true) or null (false). */
let pollReturnsFrames = true;

vi.mock('@tauri-apps/api/core', () => ({
  invoke: vi.fn(async (cmd: string) => {
    if (cmd === 'screen_share_poll_frame' && pollReturnsFrames) {
      pollSeq++;
      return { frame: 'AAAA', width: 1920, height: 1080, seq: pollSeq };
    }
    return null;
  }),
}));

// ─── Mock: livekit-client ──────────────────────────────────────────

const mockPublishTrack = vi.fn(async () => ({
  track: { mediaStreamTrack: { stop: vi.fn() } },
  isMuted: false,
}));

const mockUnpublishTrack = vi.fn(async () => {});

vi.mock('livekit-client', () => ({
  Room: vi.fn(function (this: Record<string, unknown>) {
    this.connect = vi.fn(async () => {});
    this.disconnect = vi.fn();
    this.on = vi.fn(() => this);
    this.off = vi.fn(() => this);
    this.localParticipant = {
      setMicrophoneEnabled: vi.fn(async () => {}),
      publishTrack: mockPublishTrack,
      unpublishTrack: mockUnpublishTrack,
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

// ─── Mock: Web APIs ────────────────────────────────────────────────

const mockCanvasRemove = vi.fn();
const mockFillRect = vi.fn();
const mockCaptureStream = vi.fn();
const mockAppendChild = vi.fn();
let mockGetContext: ReturnType<typeof vi.fn>;


// Canvas mock factory — returns a fresh canvas mock for each test
function createMockCanvas() {
  const ctxMock = {
    fillStyle: '',
    fillRect: mockFillRect,
    drawImage: vi.fn(),
  };
  mockGetContext = vi.fn(() => ctxMock);
  const canvasTrackStop = vi.fn();
  mockCaptureStream.mockReturnValue({
    getVideoTracks: () => [{ id: 'canvas-track', stop: canvasTrackStop }],
  });
  return {
    width: 0,
    height: 0,
    style: { cssText: '' },
    getContext: mockGetContext,
    captureStream: mockCaptureStream,
    remove: mockCanvasRemove,
  };
}

vi.stubGlobal('document', {
  createElement: vi.fn(() => createMockCanvas()),
  body: { appendChild: mockAppendChild },
  addEventListener: vi.fn(),
  removeEventListener: vi.fn(),
});

vi.stubGlobal('performance', { now: vi.fn(() => Date.now()) });

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
  // Auto-fire onload when src is set — simulates successful image decode
  let _src = '';
  Object.defineProperty(this, 'src', {
    get() { return _src; },
    set(v: string) {
      _src = v;
      if (v && typeof this.onload === 'function') {
        setTimeout(() => (this.onload as () => void)(), 0);
      }
    },
  });
  return this;
});

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

vi.stubGlobal('createImageBitmap', vi.fn(async () => ({
  width: 1920,
  height: 1080,
  close: vi.fn(),
})));

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

/** Drive a LiveKitModule to connected state so this.room is set. */
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

describe('Property 2: Preservation — Non-Race Paths Unchanged', () => {
  beforeEach(() => {
    pollSeq = 0;
    pollReturnsFrames = true;
    mockUnlisten.mockClear();
    mockPublishTrack.mockClear();
    mockUnpublishTrack.mockClear();
    mockCanvasRemove.mockClear();
    mockFillRect.mockClear();
    mockCaptureStream.mockClear();
    mockAppendChild.mockClear();
    (document.createElement as ReturnType<typeof vi.fn>).mockClear();

    // Ensure MediaStreamTrackGenerator is NOT available for canvas fallback tests
    // (individual tests override this as needed)
    delete (globalThis as Record<string, unknown>).MediaStreamTrackGenerator;
    delete (globalThis as Record<string, unknown>).VideoFrame;
  });

  // ── 3a. Canvas fallback path preservation ──────────────────────

  describe('3a. Canvas fallback path preservation', () => {
    it('creates canvas, appends to DOM, primes with #000001, calls captureStream(targetFps), and publishes track with ScreenShare source', async () => {
      /**
       * **Validates: Requirements 3.3**
       *
       * Property: For all targetFps in [15, 60], when MediaStreamTrackGenerator
       * is absent, startNativeCapture() creates a canvas (1920×1080), appends it
       * to document.body, primes it with #000001, calls captureStream(targetFps),
       * and publishes the resulting track with source: ScreenShare.
       *
       * The implementation uses invoke('screen_share_poll_frame') polling via
       * setInterval. The mock invoke returns frames which resolve the first-frame
       * gate, then publishTrack proceeds.
       *
       * We vary the quality preset (low=60fps, high=30fps, max=60fps) which
       * controls the targetFps via syncProfileFromPreset().
       */
      const presetFpsMap: Record<string, number> = {
        low: 60,
        high: 30,
        max: 60,
      };

      vi.useFakeTimers({ shouldAdvanceTime: true });

      try {
        await fc.assert(
          fc.asyncProperty(
            fc.constantFrom('low', 'high', 'max'), // quality preset
            async (quality) => {
              const expectedFps = presetFpsMap[quality];

              // Reset mocks for each property run
              pollSeq = 0;
              pollReturnsFrames = true;
              mockPublishTrack.mockClear();
              mockAppendChild.mockClear();
              mockFillRect.mockClear();
              mockCaptureStream.mockClear();
              mockUnlisten.mockClear();
              (document.createElement as ReturnType<typeof vi.fn>).mockClear();

              // Ensure no MediaStreamTrackGenerator — force canvas fallback
              delete (globalThis as Record<string, unknown>).MediaStreamTrackGenerator;

              const cbs = createMockCallbacks();
              const mod = new LiveKitModule(cbs);
              await driveToConnected(mod);

              // Set the quality preset — syncProfileFromPreset() will derive fps
              // eslint-disable-next-line @typescript-eslint/no-explicit-any
              (mod as any).currentQuality = quality;

              // Start capture — don't await yet; we need to advance timers
              // so the polling interval fires and the Image mock's onload
              // resolves the first-frame gate.
              const capturePromise = mod.startNativeCapture();

              // Advance timers to let the polling interval fire (~16ms)
              // and Image onload (0ms setTimeout) complete
              for (let i = 0; i < 20; i++) {
                await vi.advanceTimersByTimeAsync(20);
              }

              await capturePromise;

              // 1. Canvas was created
              expect(document.createElement).toHaveBeenCalledWith('canvas');

              // 2. Canvas was appended to document.body
              expect(mockAppendChild).toHaveBeenCalled();

              // 3. Canvas was primed with fillRect (the #000001 fill)
              expect(mockFillRect).toHaveBeenCalledWith(0, 0, 1920, 1080);

              // 4. captureStream was called with the expected fps from the preset
              expect(mockCaptureStream).toHaveBeenCalledWith(expectedFps);

              // 5. publishTrack was called with source: ScreenShare
              expect(mockPublishTrack).toHaveBeenCalledTimes(1);
              const publishArgs = mockPublishTrack.mock.calls[0] as unknown[];
              expect(publishArgs[1]).toMatchObject({
                source: 'screen_share',
              });

              // Cleanup
              await mod.stopNativeCapture();
            },
          ),
          { numRuns: 10 },
        );
      } finally {
        vi.useRealTimers();
      }
    });
  });

  // ── 3b. Cleanup preservation (stopNativeCapture) ───────────────

  describe('3b. Cleanup preservation (stopNativeCapture)', () => {
    it('unpublishes iff publication exists, removes canvas iff it exists, and nulls all references', async () => {
      /**
       * **Validates: Requirements 3.4, 3.5**
       *
       * Property: For all combinations of (hasUnlisten, hasPublication, hasCanvas),
       * stopNativeCapture() nulls unlisten, unpublishes iff publication
       * exists, removes canvas iff it exists, and sets all references to null.
       *
       * Note: The current implementation sets nativeCaptureUnlisten = null
       * (clearing the no-op marker) rather than calling it as a function.
       */
      await fc.assert(
        fc.asyncProperty(
          fc.boolean(), // hasUnlisten
          fc.boolean(), // hasPublication
          fc.boolean(), // hasCanvas
          async (hasUnlisten, hasPublication, hasCanvas) => {
            // Reset mocks
            mockUnpublishTrack.mockClear();
            mockCanvasRemove.mockClear();

            const cbs = createMockCallbacks();
            const mod = new LiveKitModule(cbs);
            await driveToConnected(mod);

            // eslint-disable-next-line @typescript-eslint/no-explicit-any
            const modAny = mod as any;

            // Manually set private properties to simulate various cleanup states
            const trackStopFn = vi.fn();
            if (hasUnlisten) {
              modAny.nativeCaptureUnlisten = () => {};
            } else {
              modAny.nativeCaptureUnlisten = null;
            }

            if (hasPublication) {
              modAny.nativeCapturePublication = {
                track: {
                  mediaStreamTrack: { stop: trackStopFn },
                },
              };
            } else {
              modAny.nativeCapturePublication = null;
            }

            if (hasCanvas) {
              modAny.nativeCaptureCanvas = { remove: mockCanvasRemove };
            } else {
              modAny.nativeCaptureCanvas = null;
            }

            await mod.stopNativeCapture();

            // Unpublish called iff publication existed
            if (hasPublication) {
              expect(mockUnpublishTrack).toHaveBeenCalledTimes(1);
              expect(trackStopFn).toHaveBeenCalledTimes(1);
            } else {
              expect(mockUnpublishTrack).not.toHaveBeenCalled();
            }

            // Canvas removed iff it existed
            if (hasCanvas) {
              expect(mockCanvasRemove).toHaveBeenCalledTimes(1);
            } else {
              expect(mockCanvasRemove).not.toHaveBeenCalled();
            }

            // All references nulled
            expect(modAny.nativeCaptureUnlisten).toBeNull();
            expect(modAny.nativeCapturePublication).toBeNull();
            expect(modAny.nativeCaptureCanvas).toBeNull();
          },
        ),
        { numRuns: 8 }, // 2^3 = 8 covers all boolean combinations
      );
    });
  });

  // ── 3c. Guard clause preservation ──────────────────────────────

  describe('3c. Guard clause preservation', () => {
    it('throws when room is null, returns early when nativeCapturePublication is already set', async () => {
      /**
       * **Validates: Requirements 3.1, 3.6**
       *
       * Property: For all calls where room is null, the function throws
       * 'not connected to a room'; for all calls where nativeCapturePublication
       * is already set, the function returns without side effects.
       */
      await fc.assert(
        fc.asyncProperty(
          fc.boolean(), // testNullRoom (true = null room, false = already-active guard)
          async (testNullRoom) => {
            mockPublishTrack.mockClear();

            const cbs = createMockCallbacks();
            const mod = new LiveKitModule(cbs);

            if (testNullRoom) {
              // Room is null — should throw
              // eslint-disable-next-line @typescript-eslint/no-explicit-any
              expect((mod as any).room).toBeNull();
              await expect(mod.startNativeCapture()).rejects.toThrow('not connected to a room');
              // No side effects
              expect(mockPublishTrack).not.toHaveBeenCalled();
            } else {
              // Drive to connected, then set nativeCapturePublication to simulate active capture
              await driveToConnected(mod);
              // eslint-disable-next-line @typescript-eslint/no-explicit-any
              (mod as any).nativeCapturePublication = {
                track: { mediaStreamTrack: { stop: vi.fn() } },
              };

              // Should return early (no-op)
              await mod.startNativeCapture();

              // No publishTrack call — early return
              expect(mockPublishTrack).not.toHaveBeenCalled();
            }
          },
        ),
        { numRuns: 10 },
      );
    });
  });

  // ── 3d. Concurrent stop during startup ─────────────────────────

  describe('3d. Concurrent stop during startup', () => {
    it('calling stopNativeCapture() during first-frame await leaves zero dangling publications and all references null', async () => {
      /**
       * **Validates: Requirements 3.4, 3.5**
       *
       * Property: For all stop timings in [0ms, 500ms], calling
       * stopNativeCapture() while startNativeCapture() is awaiting the
       * first frame leaves zero dangling publications, and all capture
       * references set to null.
       *
       * The implementation waits for the first frame before publishing.
       * When stopNativeCapture() is called during this gate:
       * 1. stop nulls nativeCaptureUnlisten and clears the poll interval
       * 2. The first-frame promise never resolves (no frames from polling)
       * 3. The 5s timeout fires → catch block cleans up
       * 4. The code after Promise.race checks !nativeCaptureUnlisten
       *    and returns early (aborted)
       *
       * Either way: no publishTrack, no leaked state, all refs null.
       */

      vi.useFakeTimers({ shouldAdvanceTime: false });

      try {
        await fc.assert(
          fc.asyncProperty(
            fc.integer({ min: 0, max: 500 }), // stopDelay in ms
            async (stopDelay) => {
              // Reset mocks for each property run
              pollSeq = 0;
              pollReturnsFrames = false; // No frames — first-frame gate stays pending
              mockPublishTrack.mockClear();
              mockUnpublishTrack.mockClear();
              mockCanvasRemove.mockClear();
              (document.createElement as ReturnType<typeof vi.fn>).mockClear();

              // Ensure no MediaStreamTrackGenerator — use canvas fallback
              delete (globalThis as Record<string, unknown>).MediaStreamTrackGenerator;
              delete (globalThis as Record<string, unknown>).VideoFrame;

              const cbs = createMockCallbacks();
              const mod = new LiveKitModule(cbs);
              await driveToConnected(mod);

              // Start capture — don't await; we'll call stop during the
              // first-frame await. Attach a no-op catch immediately to
              // prevent unhandled rejection warnings.
              const capturePromise = mod.startNativeCapture().catch(() => {
                // Expected — timeout error after stop during startup
              });

              // Advance past the stopDelay, then call stopNativeCapture
              if (stopDelay > 0) {
                await vi.advanceTimersByTimeAsync(stopDelay);
              }

              // Call stop — this nulls nativeCaptureUnlisten and clears poll interval
              await mod.stopNativeCapture();

              // Advance timers past the 5s first-frame timeout so the
              // Promise.race rejects and startNativeCapture settles
              await vi.advanceTimersByTimeAsync(6000);

              // Wait for capturePromise to settle
              await capturePromise;

              // eslint-disable-next-line @typescript-eslint/no-explicit-any
              const modAny = mod as any;

              // ── Assertions ──

              // 1. publishTrack was NOT called (publish never happened)
              expect(mockPublishTrack).not.toHaveBeenCalled();

              // 2. All references are null (clean state)
              expect(modAny.nativeCaptureUnlisten).toBeNull();
              expect(modAny.nativeCapturePublication).toBeNull();
              expect(modAny.nativeCaptureCanvas).toBeNull();
            },
          ),
          { numRuns: 20 },
        );
      } finally {
        vi.useRealTimers();
      }
    });
  });
});
