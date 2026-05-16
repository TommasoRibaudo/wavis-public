import { describe, expect, it, vi } from 'vitest';

import { setupVoiceRoomCameraHarness } from './voice-room-camera.test-helpers';

describe('VoiceRoom camera quality retry timing', () => {
  it('retries failed quality updates three times with 1s spacing and then stops', async () => {
    vi.useFakeTimers();
    const harness = await setupVoiceRoomCameraHarness();

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

      await vi.advanceTimersByTimeAsync(999);
      expect(harness.state.lastLiveKitModule!.setCameraQualityCalls).toHaveLength(1);

      await vi.advanceTimersByTimeAsync(1);
      expect(harness.state.lastLiveKitModule!.setCameraQualityCalls.map((call) => call.tier)).toEqual([
        'low',
        'low',
      ]);

      await vi.advanceTimersByTimeAsync(999);
      expect(harness.state.lastLiveKitModule!.setCameraQualityCalls).toHaveLength(2);

      await vi.advanceTimersByTimeAsync(1);
      expect(harness.state.lastLiveKitModule!.setCameraQualityCalls.map((call) => call.tier)).toEqual([
        'low',
        'low',
        'low',
      ]);
      expect(
        harness.voiceRoom.getState().events.some((event) =>
          event.message.includes('camera quality update failed after 3 attempts (low)')),
      ).toBe(true);

      await vi.advanceTimersByTimeAsync(1_000);
      expect(harness.state.lastLiveKitModule!.setCameraQualityCalls).toHaveLength(3);
    } finally {
      harness.cleanup();
      vi.useRealTimers();
    }
  });
});
