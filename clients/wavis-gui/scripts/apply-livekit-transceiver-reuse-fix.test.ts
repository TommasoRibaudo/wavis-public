import { describe, expect, it, vi } from 'vitest';

import {
  applyLivekitTransceiverReuseFix,
  applyTransceiverReusePatch,
  applyDegradationPrefPatch,
  verifyPatchMarkers,
  LIVEKIT_FIX_LOG_MARKERS,
  DEGRADATION_PREF_MARKERS,
  REQUIRED_POST_PATCH_MARKERS,
  livekitFixTestFixtures,
} from './apply-livekit-transceiver-reuse-fix.mjs';

type TestWavisSenderDataStoreHost = typeof globalThis & {
  __wavisSenderData?: WeakMap<object, {
    reused: boolean;
    degradationPreferenceConfigured: boolean;
    attemptedPreferences: string[];
    invalidStateSkipped: boolean;
    lastErrorName: string | null;
    lastErrorMessage: string | null;
  }>;
};

/** Helper: build a fake bundle from a transceiver fixture + degradation fixture */
function bundle(transceiver: string, degradation = livekitFixTestFixtures.degradationPrefBefore) {
  return transceiver + degradation;
}

describe('apply-livekit-transceiver-reuse-fix', () => {
  // ── Transceiver reuse patch ────────────────────────────────────────

  it('patches the unpatched (before) fixture', () => {
    const patched = applyLivekitTransceiverReuseFix(bundle(livekitFixTestFixtures.before));
    for (const marker of LIVEKIT_FIX_LOG_MARKERS) {
      expect(patched).toContain(marker);
    }
  });

  it('upgrades the patchedWithoutLogs fixture', () => {
    const patched = applyLivekitTransceiverReuseFix(bundle(livekitFixTestFixtures.patchedWithoutLogs));
    for (const marker of LIVEKIT_FIX_LOG_MARKERS) {
      expect(patched).toContain(marker);
    }
  });

  it('upgrades the patchedWithLogs fixture', () => {
    const patched = applyLivekitTransceiverReuseFix(bundle(livekitFixTestFixtures.patchedWithLogs));
    for (const marker of LIVEKIT_FIX_LOG_MARKERS) {
      expect(patched).toContain(marker);
    }
    expect(patched).toContain('[livekit-fix] sender.replaceTrack success');
  });

  it('upgrades the patchedCrashGuard (intermediate) fixture', () => {
    const patched = applyLivekitTransceiverReuseFix(bundle(livekitFixTestFixtures.patchedCrashGuard));
    for (const marker of LIVEKIT_FIX_LOG_MARKERS) {
      expect(patched).toContain(marker);
    }
    expect(patched).toContain('[livekit-fix] sender.replaceTrack success');
  });

  // ── Degradation preference patch ───────────────────────────────────

  it('patches setDegradationPreference to yield setParameters and add logging', () => {
    const patched = applyLivekitTransceiverReuseFix(bundle(livekitFixTestFixtures.before));
    for (const marker of DEGRADATION_PREF_MARKERS) {
      expect(patched).toContain(marker);
    }
    // The key fix: setParameters must be yielded (awaited)
    expect(patched).toContain('yield this.sender.setParameters(params)');
  });

  it('skips InvalidStateError for reused senders in degradationPreference path', async () => {
    const warnSpy = vi.spyOn(console, 'warn').mockImplementation(() => {});
    const logSpy = vi.spyOn(console, 'log').mockImplementation(() => {});
    try {
      const __awaiter = (
        thisArg: unknown,
        _arguments: unknown,
        P: PromiseConstructor,
        generator: () => Generator<Promise<unknown>, void, unknown>,
      ) => new (P || Promise)((resolve, reject) => {
        const iterator = generator.call(thisArg);
        const step = (result: IteratorResult<Promise<unknown>, void>) => {
          if (result.done) {
            resolve(result.value);
            return;
          }
          Promise.resolve(result.value).then(
            (value) => step(iterator.next(value)),
            (error) => step(iterator.throw(error)),
          );
        };
        step(iterator.next());
      });
      const factory = new Function(
        '__awaiter',
        `return class PatchedTrack {
          constructor(sender, log) {
            this.sender = sender;
            this.log = log;
            this.logContext = { scope: 'test' };
            this.degradationPreference = null;
          }
          ${livekitFixTestFixtures.degradationPrefAfter}
        };`,
      );
      const PatchedTrack = factory(__awaiter);
      const sender = {
        track: { id: 'sender-track-1' },
        getParameters: () => ({}),
        setParameters: async () => {
          const error = new Error("Failed to execute 'setParameters' on 'RTCRtpSender'");
          error.name = 'InvalidStateError';
          throw error;
        },
      };
      const log = {
        debug: () => {},
        warn: () => {},
      };
      (globalThis as TestWavisSenderDataStoreHost).__wavisSenderData = new WeakMap();
      (globalThis as TestWavisSenderDataStoreHost).__wavisSenderData?.set(sender, {
        reused: true,
        degradationPreferenceConfigured: false,
        attemptedPreferences: [],
        invalidStateSkipped: false,
        lastErrorName: null,
        lastErrorMessage: null,
      });
      const track = new PatchedTrack(sender, log);

      await expect(track.setDegradationPreference('maintain-resolution')).resolves.toBeUndefined();
      expect((globalThis as TestWavisSenderDataStoreHost).__wavisSenderData?.get(sender)).toMatchObject({
        attemptedPreferences: ['maintain-resolution'],
        invalidStateSkipped: true,
        lastErrorName: 'InvalidStateError',
        lastErrorMessage: "Failed to execute 'setParameters' on 'RTCRtpSender'",
      });
      expect(
        warnSpy.mock.calls.some((args) => args.some((arg) =>
          typeof arg === 'string' && arg.includes('skipped_reused_sender')
        )),
      ).toBe(true);
      expect(
        logSpy.mock.calls.some((args) => args.some((arg) =>
          typeof arg === 'string' && arg.includes('degradationPreference.setParameters attempt')
        )),
      ).toBe(true);
    } finally {
      warnSpy.mockRestore();
      logSpy.mockRestore();
    }
  });

  it('applies post-replaceTrack combined setParameters and marks the sender configured', async () => {
    const logSpy = vi.spyOn(console, 'log').mockImplementation(() => {});
    const assertSpy = vi.spyOn(console, 'assert').mockImplementation(() => {});
    try {
      const __awaiter = (
        thisArg: unknown,
        _arguments: unknown,
        P: PromiseConstructor,
        generator: () => Generator<Promise<unknown>, void, unknown>,
      ) => new (P || Promise)((resolve, reject) => {
        const iterator = generator.call(thisArg);
        const step = (result: IteratorResult<Promise<unknown>, void>) => {
          if (result.done) {
            resolve(result.value);
            return;
          }
          Promise.resolve(result.value).then(
            (value) => step(iterator.next(value)),
            (error) => step(iterator.throw(error)),
          );
        };
        step(iterator.next());
      });
      const transceiverMethods = applyTransceiverReusePatch(livekitFixTestFixtures.patchedWithLogs);
      if (transceiverMethods === null) {
        throw new Error('failed to build patched transceiver fixture');
      }
      const factory = new Function(
        '__awaiter',
        `return class PatchedPublisher {
          constructor(pcManager, log) {
            this.pcManager = pcManager;
            this.log = log;
            this.logContext = { scope: 'test' };
          }
          ${transceiverMethods}
        };`,
      );
      const PatchedPublisher = factory(__awaiter);
      let getParametersCall = 0;
      const sender = {
        track: null as { id: string } | null,
        getParameters: vi.fn(() => {
          getParametersCall++;
          if (getParametersCall === 1) {
            return { encodings: [{ rid: 'f' }] };
          }
          return { encodings: [] as Array<{ rid: string }> };
        }),
        setParameters: vi.fn(async () => {}),
        replaceTrack: vi.fn(async (track: { id: string }) => {
          sender.track = track;
        }),
      };
      const transceiver = {
        mid: 'video-1',
        direction: 'inactive',
        currentDirection: 'inactive',
        stopped: false,
        sender,
        receiver: { track: { kind: 'video' } },
      };
      const publisher = new PatchedPublisher({
        publisher: {
          getTransceivers: () => [transceiver],
        },
      }, {
        warn: () => {},
      });
      (globalThis as TestWavisSenderDataStoreHost).__wavisSenderData = undefined;

      const track = {
        kind: 'video',
        mediaStreamTrack: { id: 'share-track-1' },
      };
      const encodings = [{ rid: 'f' }];

      const reusedSender = await publisher.tryReuseInactivePublisherSender(track, encodings);

      expect(reusedSender).toBe(sender);
      expect(sender.setParameters).toHaveBeenCalledTimes(2);
      expect(sender.setParameters).toHaveBeenNthCalledWith(2, {
        encodings: [{ rid: 'f' }],
        degradationPreference: 'maintain-resolution',
      });
      expect((globalThis as TestWavisSenderDataStoreHost).__wavisSenderData?.get(sender)).toMatchObject({
        reused: true,
        degradationPreferenceConfigured: true,
        attemptedPreferences: ['maintain-resolution-combined'],
        invalidStateSkipped: false,
        lastErrorName: null,
        lastErrorMessage: null,
      });
      expect(
        logSpy.mock.calls.some((args) => args.some((arg) =>
          typeof arg === 'string' && arg.includes('post-replaceTrack combined setParameters success')
        )),
      ).toBe(true);
      expect(assertSpy).toHaveBeenCalled();
    } finally {
      logSpy.mockRestore();
      assertSpy.mockRestore();
      (globalThis as TestWavisSenderDataStoreHost).__wavisSenderData = undefined;
    }
  });

  it('skips setDegradationPreference when the combined post-replace call already configured the sender', async () => {
    const logSpy = vi.spyOn(console, 'log').mockImplementation(() => {});
    try {
      const __awaiter = (
        thisArg: unknown,
        _arguments: unknown,
        P: PromiseConstructor,
        generator: () => Generator<Promise<unknown>, void, unknown>,
      ) => new (P || Promise)((resolve, reject) => {
        const iterator = generator.call(thisArg);
        const step = (result: IteratorResult<Promise<unknown>, void>) => {
          if (result.done) {
            resolve(result.value);
            return;
          }
          Promise.resolve(result.value).then(
            (value) => step(iterator.next(value)),
            (error) => step(iterator.throw(error)),
          );
        };
        step(iterator.next());
      });
      const factory = new Function(
        '__awaiter',
        `return class PatchedTrack {
          constructor(sender, log) {
            this.sender = sender;
            this.log = log;
            this.logContext = { scope: 'test' };
            this.degradationPreference = null;
          }
          ${livekitFixTestFixtures.degradationPrefAfter}
        };`,
      );
      const PatchedTrack = factory(__awaiter);
      const sender = {
        track: { id: 'sender-track-1' },
        getParameters: vi.fn(() => ({})),
        setParameters: vi.fn(async () => {}),
      };
      const log = {
        debug: () => {},
        warn: () => {},
      };
      (globalThis as TestWavisSenderDataStoreHost).__wavisSenderData = new WeakMap();
      (globalThis as TestWavisSenderDataStoreHost).__wavisSenderData?.set(sender, {
        reused: true,
        degradationPreferenceConfigured: true,
        attemptedPreferences: ['maintain-resolution-combined'],
        invalidStateSkipped: false,
        lastErrorName: null,
        lastErrorMessage: null,
      });
      const track = new PatchedTrack(sender, log);

      await expect(track.setDegradationPreference('maintain-resolution')).resolves.toBeUndefined();
      expect(sender.getParameters).not.toHaveBeenCalled();
      expect(sender.setParameters).not.toHaveBeenCalled();
      expect((globalThis as TestWavisSenderDataStoreHost).__wavisSenderData?.get(sender)).toMatchObject({
        degradationPreferenceConfigured: true,
        attemptedPreferences: ['maintain-resolution-combined'],
        invalidStateSkipped: false,
        lastErrorName: null,
        lastErrorMessage: null,
      });
      expect(
        logSpy.mock.calls.some((args) => args.some((arg) =>
          typeof arg === 'string' && arg.includes('setDegradationPreference skipped (already configured via combined call)')
        )),
      ).toBe(true);
    } finally {
      logSpy.mockRestore();
      (globalThis as TestWavisSenderDataStoreHost).__wavisSenderData = undefined;
    }
  });

  it('does not double-patch an already-patched bundle', () => {
    const first = applyLivekitTransceiverReuseFix(bundle(livekitFixTestFixtures.before));
    const second = applyLivekitTransceiverReuseFix(first);
    expect(second).toBe(first);
  });

  // ── Verification ───────────────────────────────────────────────────

  it('verifyPatchMarkers passes when all required markers are present', () => {
    const patched = applyLivekitTransceiverReuseFix(bundle(livekitFixTestFixtures.before));
    expect(() => verifyPatchMarkers(patched)).not.toThrow();
  });

  it('verifyPatchMarkers throws when markers are missing', () => {
    expect(() => verifyPatchMarkers('some random content')).toThrow(
      /post-patch verification failed/
    );
  });

  // ── Strict anchor guards ───────────────────────────────────────────

  it('throws when createTransceiverRTCRtpSender anchor is missing', () => {
    expect(() => applyLivekitTransceiverReuseFix(
      'no matching anchor here' + livekitFixTestFixtures.degradationPrefBefore
    )).toThrow(/could not find createTransceiverRTCRtpSender anchor/);
  });

  it('throws when setDegradationPreference anchor is missing', () => {
    expect(() => applyLivekitTransceiverReuseFix(
      livekitFixTestFixtures.before + 'no degradation anchor here'
    )).toThrow(/could not find setDegradationPreference anchor/);
  });

  // ── Combined markers ───────────────────────────────────────────────

  it('patched output contains all required post-patch markers', () => {
    const patched = applyLivekitTransceiverReuseFix(bundle(livekitFixTestFixtures.before));
    for (const marker of REQUIRED_POST_PATCH_MARKERS) {
      expect(patched).toContain(marker);
    }
  });

  it('patched output from crashGuard intermediate also contains all markers', () => {
    const patched = applyLivekitTransceiverReuseFix(bundle(livekitFixTestFixtures.patchedCrashGuard));
    for (const marker of REQUIRED_POST_PATCH_MARKERS) {
      expect(patched).toContain(marker);
    }
  });
});
