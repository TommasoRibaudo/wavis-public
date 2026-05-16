import { describe, expect, it, vi } from 'vitest';

import { setupVoiceRoomCameraHarness } from './voice-room-camera.test-helpers';

describe('VoiceRoom camera quality retry timing', () => {
  it('retries failed quality updates three times with 1s spacing and then stops', async () => {
    vi.useFakeTimers();
    const harness = await setupVoiceRoomCameraHarness();
    const { CAMERA_QUALITY_RETRY_INTERVAL_MS, CAMERA_QUALITY_MAX_ATTEMPTS } =
      harness.voiceRoom.__CAMERA_TEST_HOOKS__;

    try {
      await harness.driveToActive();
      await harness.connectMedia();
      await harness.voiceRoom.toggleCameraIntent();

      harness.state.setCameraQualityImpl = vi.fn(async () => {
        throw new Error('quality update failed');
      });

      harness.emitMessage({
        type: 'share_started',
        participantId: 'peer-2',
        displayName: 'Alice',
        shareType: 'screen_audio',
      });
      await harness.tick();

      expect(harness.state.lastLiveKitModule).not.toBeNull();
      expect(harness.state.lastLiveKitModule!.setCameraQualityCalls.map((call) => call.tier)).toEqual(['low']);

      // First retry — just before the interval, no new attempt.
      await vi.advanceTimersByTimeAsync(CAMERA_QUALITY_RETRY_INTERVAL_MS - 1);
      expect(harness.state.lastLiveKitModule!.setCameraQualityCalls).toHaveLength(1);

      await vi.advanceTimersByTimeAsync(1);
      expect(harness.state.lastLiveKitModule!.setCameraQualityCalls.map((call) => call.tier)).toEqual([
        'low',
        'low',
      ]);

      // Second retry, same spacing.
      await vi.advanceTimersByTimeAsync(CAMERA_QUALITY_RETRY_INTERVAL_MS - 1);
      expect(harness.state.lastLiveKitModule!.setCameraQualityCalls).toHaveLength(2);

      await vi.advanceTimersByTimeAsync(1);
      expect(harness.state.lastLiveKitModule!.setCameraQualityCalls.map((call) => call.tier)).toEqual([
        'low',
        'low',
        'low',
      ]);
      expect(
        harness.voiceRoom.getState().events.some((event) =>
          event.message.includes(`camera quality update failed after ${CAMERA_QUALITY_MAX_ATTEMPTS} attempts (low)`)),
      ).toBe(true);

      // After the cap, no further attempts even if more time passes.
      await vi.advanceTimersByTimeAsync(CAMERA_QUALITY_RETRY_INTERVAL_MS);
      expect(harness.state.lastLiveKitModule!.setCameraQualityCalls).toHaveLength(CAMERA_QUALITY_MAX_ATTEMPTS);
    } finally {
      harness.cleanup();
      vi.useRealTimers();
    }
  });
});
