/**
 * Regression tests for video-feed bug fixes.
 *
 * Each test pins a specific bug that the existing property/example coverage
 * did NOT catch:
 *
 *   - Bug 3: rapid toggleCameraIntent calls must not interleave (publish-while-
 *     unpublishing race that left intent and publication out of sync).
 *   - Bug 4: lastRequestedCameraQualityTier must not be recorded before the
 *     attempt succeeds — after retry exhaustion, a follow-up handler that
 *     re-evaluates the SAME desired tier MUST trigger a fresh attempt rather
 *     than being short-circuited at the dedupe check.
 *   - Bug 6: setCameraQuality must transparently downgrade to fps-only
 *     constraints on OverconstrainedError rather than rejecting and retrying.
 */

import { describe, expect, it, vi } from 'vitest';

import { setupVoiceRoomCameraHarness } from './voice-room-camera.test-helpers';

/** Run several microtasks so async chains in voice-room can settle. */
async function flush(harness: { tick: () => Promise<void> }, times = 6): Promise<void> {
  for (let i = 0; i < times; i += 1) {
    await harness.tick();
  }
}

/* ─── Bug 3: toggle serialisation ─────────────────────────────── */

describe('Bug 3: toggleCameraIntent serialises concurrent invocations', () => {
  it('does not interleave a publish and an unpublish under rapid clicks', async () => {
    const harness = await setupVoiceRoomCameraHarness();

    try {
      await harness.driveToActive();
      await harness.connectMedia();

      // Make publishCamera slow so the second click lands while the first is in flight.
      let resolvePublish: ((value: { trackId: string }) => void) | null = null;
      harness.state.publishCameraImpl = (module, opts) => new Promise((resolve) => {
        const stop = vi.fn();
        module.localCameraTrack = {
          id: `camera-${opts.deviceId ?? 'default'}`,
          kind: 'video',
          stop,
        } as unknown as MediaStreamTrack;
        resolvePublish = resolve;
      });

      const firstToggle = harness.voiceRoom.toggleCameraIntent();
      // Fire a second toggle while the first is still pending.
      const secondToggle = harness.voiceRoom.toggleCameraIntent();

      // Let the first toggle's async chain reach the publishCamera mock.
      await flush(harness);
      // Critical: only one publish should be visible to the SDK at this point.
      expect(harness.state.lastLiveKitModule!.publishCameraCalls).toHaveLength(1);
      // No unpublish has been dispatched yet — the second toggle is queued
      // behind the chain rather than racing the first.
      expect(harness.state.lastLiveKitModule!.unpublishCameraCalls).toBe(0);

      // Now let the publish complete.
      resolvePublish!({ trackId: 'first-publish' });
      await firstToggle;
      await secondToggle;

      // Ending state: two clicks, parity even, intent and publication agree.
      expect(harness.voiceRoom.getState().cameraIntent).toBe(false);
      expect(harness.voiceRoom.getState().cameraPublication).toBe('idle');
      expect(harness.state.lastLiveKitModule!.publishCameraCalls).toHaveLength(1);
      expect(harness.state.lastLiveKitModule!.unpublishCameraCalls).toBe(1);
      // The operation log must show publish-before-unpublish, never overlapping.
      expect(
        harness.state.lastLiveKitModule!.cameraOperationLog
          .filter((op) => op.type === 'publish' || op.type === 'unpublish')
          .map((op) => op.type),
      ).toEqual(['publish', 'unpublish']);
    } finally {
      harness.cleanup();
    }
  });

  it('preserves intent/publication agreement across many rapid toggles', async () => {
    const harness = await setupVoiceRoomCameraHarness();

    try {
      await harness.driveToActive();
      await harness.connectMedia();

      // Fire 8 toggles concurrently — chain ordering must keep them sequential.
      const promises = Array.from({ length: 8 }, () => harness.voiceRoom.toggleCameraIntent());
      await Promise.all(promises);

      // 8 toggles → ends idle. publishes == unpublishes (both 4).
      expect(harness.voiceRoom.getState().cameraIntent).toBe(false);
      expect(harness.voiceRoom.getState().cameraPublication).toBe('idle');
      expect(harness.state.lastLiveKitModule!.publishCameraCalls).toHaveLength(4);
      expect(harness.state.lastLiveKitModule!.unpublishCameraCalls).toBe(4);
    } finally {
      harness.cleanup();
    }
  });
});

/* ─── Bug 4: post-exhaustion dedupe recovery ──────────────────── */

describe('Bug 4: quality update controller does not poison the dedupe cache on retry exhaustion', () => {
  it('a follow-up applyCameraQualityForShareState that desires the same failed tier still attempts a fresh apply', async () => {
    vi.useFakeTimers();
    const harness = await setupVoiceRoomCameraHarness();
    const { CAMERA_QUALITY_RETRY_INTERVAL_MS, CAMERA_QUALITY_MAX_ATTEMPTS } =
      harness.voiceRoom.__CAMERA_TEST_HOOKS__;

    try {
      await harness.driveToActive();
      await harness.connectMedia();
      // Publish at HIGH (no share active) — sets lastRequestedCameraQualityTier='high'.
      await harness.voiceRoom.toggleCameraIntent();

      // Now make every setCameraQuality call fail so we drive the retry loop
      // to exhaustion. With the OLD bug, lastRequestedCameraQualityTier would
      // be poisoned to 'low' before the first attempt; after exhaustion the
      // very next handler that desires 'low' would be skipped at the dedupe.
      let failNextCalls = true;
      harness.state.setCameraQualityImpl = vi.fn(async () => {
        if (failNextCalls) throw new Error('quality update failed');
      });

      // share_started → desires LOW. Drive retries to exhaustion.
      harness.emitMessage({
        type: 'share_started',
        participantId: 'peer-2',
        displayName: 'Alice',
        shareType: 'screen_audio',
      });
      await harness.tick();
      expect(harness.state.lastLiveKitModule!.setCameraQualityCalls).toHaveLength(1);

      for (let attempt = 1; attempt < CAMERA_QUALITY_MAX_ATTEMPTS; attempt += 1) {
        await vi.advanceTimersByTimeAsync(CAMERA_QUALITY_RETRY_INTERVAL_MS);
      }
      expect(harness.state.lastLiveKitModule!.setCameraQualityCalls).toHaveLength(CAMERA_QUALITY_MAX_ATTEMPTS);
      expect(
        harness.voiceRoom.getState().events.some((event) =>
          event.message.includes(`camera quality update failed after ${CAMERA_QUALITY_MAX_ATTEMPTS} attempts (low)`)),
      ).toBe(true);

      // Re-enable success on the next call.
      failNextCalls = false;

      // Now a non-transition handler re-evaluates the same desired tier.
      // sub_room_state with the same membership doesn't change isSharing or
      // joinedSubRoomId, but it does call applyCameraQualityForShareState.
      // Desired = LOW (share is still active). With the OLD bug,
      // lastRequestedCameraQualityTier === 'low' would short-circuit this call
      // and the camera would stay stuck at HIGH. With the fix, last is still
      // 'high' (only mutated on success), so the call goes through.
      const callsBefore = harness.state.lastLiveKitModule!.setCameraQualityCalls.length;
      harness.emitMessage({
        type: 'sub_room_state',
        rooms: [
          {
            subRoomId: 'room-1',
            roomNumber: 1,
            isDefault: true,
            participantIds: ['self-peer', 'peer-2'],
          },
        ],
      });
      await flush(harness);

      const callsAfter = harness.state.lastLiveKitModule!.setCameraQualityCalls.length;
      expect(callsAfter).toBeGreaterThan(callsBefore);
      expect(
        harness.state.lastLiveKitModule!.setCameraQualityCalls.at(-1)?.tier,
      ).toBe('low');
    } finally {
      harness.cleanup();
      vi.useRealTimers();
    }
  });
});

/* ─── Bug 6: OverconstrainedError fallback (orchestrator side) ── */

describe('Bug 6: setCameraQuality fallback prevents the retry loop from firing', () => {
  it('a successful setCameraQuality (real module already absorbed OverconstrainedError) does not trigger retries', async () => {
    vi.useFakeTimers();
    const harness = await setupVoiceRoomCameraHarness();
    const { CAMERA_QUALITY_RETRY_INTERVAL_MS, CAMERA_QUALITY_MAX_ATTEMPTS } =
      harness.voiceRoom.__CAMERA_TEST_HOOKS__;

    try {
      await harness.driveToActive();
      await harness.connectMedia();
      await harness.voiceRoom.toggleCameraIntent();

      // The real LiveKitModule.setCameraQuality catches OverconstrainedError
      // internally and re-applies fps-only — from voice-room's perspective
      // this is just a plain successful single call. We model that here.
      let setQualityCalls = 0;
      harness.state.setCameraQualityImpl = vi.fn(async () => {
        setQualityCalls += 1;
      });

      harness.emitMessage({
        type: 'share_started',
        participantId: 'peer-2',
        displayName: 'Alice',
        shareType: 'screen_audio',
      });
      await harness.tick();

      // Even after letting the retry-interval pass several times, only one
      // attempt should have been made.
      await vi.advanceTimersByTimeAsync(CAMERA_QUALITY_RETRY_INTERVAL_MS * (CAMERA_QUALITY_MAX_ATTEMPTS + 1));
      expect(setQualityCalls).toBe(1);
      expect(
        harness.voiceRoom.getState().events.some((event) =>
          event.message.includes('camera quality update failed')),
      ).toBe(false);
    } finally {
      harness.cleanup();
      vi.useRealTimers();
    }
  });
});
