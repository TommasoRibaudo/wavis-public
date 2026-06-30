import { describe, expect, it } from 'vitest';

import { setupVoiceRoomCameraHarness } from './voice-room-camera.test-helpers';

describe('VoiceRoom camera lifecycle', () => {
  it('delegates start, stop, and leave teardown to the media module', async () => {
    const harness = await setupVoiceRoomCameraHarness();

    try {
      await harness.driveToActive();
      await harness.connectMedia();

      await harness.voiceRoom.toggleCameraIntent();
      expect(harness.state.lastLiveKitModule).not.toBeNull();
      expect(harness.state.lastLiveKitModule!.publishCameraCalls).toHaveLength(1);
      expect(harness.voiceRoom.getState().cameraPublication).toBe('published');

      await harness.voiceRoom.toggleCameraIntent();
      expect(harness.state.lastLiveKitModule!.unpublishCameraCalls).toBe(1);
      expect(harness.voiceRoom.getState().cameraPublication).toBe('idle');

      await harness.voiceRoom.toggleCameraIntent();
      expect(harness.state.lastLiveKitModule!.publishCameraCalls).toHaveLength(2);
      expect(harness.voiceRoom.getState().cameraPublication).toBe('published');

      harness.voiceRoom.leaveRoom();
      await harness.tick();

      expect(harness.state.lastLiveKitModule!.unpublishCameraCalls).toBe(2);
      expect(harness.state.lastLiveKitModule!.disconnectCalls).toBe(1);
      expect(harness.voiceRoom.getState().machineState).toBe('idle');
      expect(harness.voiceRoom.getState().cameraIntent).toBe(false);
    } finally {
      harness.cleanup();
    }
  });

  it('shuts down camera when the session is disconnected remotely', async () => {
    const harness = await setupVoiceRoomCameraHarness();

    try {
      await harness.driveToActive();
      await harness.connectMedia();
      await harness.voiceRoom.toggleCameraIntent();

      harness.emitMessage({ type: 'session_displaced' });
      await harness.tick();

      expect(harness.state.lastLiveKitModule!.unpublishCameraCalls).toBe(1);
      expect(harness.state.lastLiveKitModule!.disconnectCalls).toBe(1);
      expect(harness.voiceRoom.getState().machineState).toBe('idle');
      expect(harness.voiceRoom.getState().cameraIntent).toBe(false);
      expect(harness.voiceRoom.getState().cameraPublication).toBe('idle');
      expect(harness.voiceRoom.getState().videoTilesById).toEqual({});
    } finally {
      harness.cleanup();
    }
  });

  it('clears visible shares when the session is disconnected remotely', async () => {
    const harness = await setupVoiceRoomCameraHarness();

    try {
      await harness.driveToActive();
      await harness.connectMedia();
      harness.emitMessage({
        type: 'share_started',
        participantId: 'self-peer',
        displayName: 'TestUser',
        shareType: 'screen_audio',
      });

      expect(harness.voiceRoom.getState().participants.some((participant) => participant.isSharing)).toBe(true);

      harness.emitMessage({ type: 'session_displaced' });
      await harness.tick();

      expect(harness.state.lastLiveKitModule!.stopScreenShare).toHaveBeenCalled();
      expect(harness.voiceRoom.getState().participants.some((participant) => participant.isSharing)).toBe(false);
      expect(harness.voiceRoom.getState().activeVideoShare).toBeNull();
      expect(harness.voiceRoom.getState().activeAudioShare).toBeNull();
      expect(harness.voiceRoom.getState().screenShareStreams.size).toBe(0);
    } finally {
      harness.cleanup();
    }
  });

  it('fails early with no camera configured when no video inputs are enumerable', async () => {
    const harness = await setupVoiceRoomCameraHarness();
    harness.state.videoInputDevice = 'camera-missing';
    harness.state.enumeratedVideoDevices = [];

    try {
      await harness.driveToActive();
      await harness.connectMedia();
      await harness.voiceRoom.toggleCameraIntent();

      expect(harness.state.lastLiveKitModule).not.toBeNull();
      expect(harness.state.lastLiveKitModule!.publishCameraCalls).toHaveLength(0);
      expect(harness.state.lastLiveKitModule!.unpublishCameraCalls).toBe(1);
      expect(harness.voiceRoom.getState().cameraIntent).toBe(false);
      expect(harness.voiceRoom.getState().cameraPublication).toBe('idle');
      expect(
        harness.voiceRoom.getState().events.some((event) => event.message.includes('no camera available')),
      ).toBe(true);
    } finally {
      harness.cleanup();
    }
  });

  it('passes a null deviceId to the media module when the selected camera source is unset', async () => {
    const harness = await setupVoiceRoomCameraHarness();
    harness.state.videoInputDevice = null;
    harness.state.enumeratedVideoDevices = ['camera-1'];

    try {
      await harness.driveToActive();
      await harness.connectMedia();
      await harness.voiceRoom.toggleCameraIntent();

      expect(harness.state.lastLiveKitModule).not.toBeNull();
      expect(harness.state.lastLiveKitModule!.publishCameraCalls).toHaveLength(1);
      expect(harness.state.lastLiveKitModule!.publishCameraCalls[0].deviceId).toBeNull();
      expect(harness.voiceRoom.getState().cameraSelectedDeviceId).toBeNull();
    } finally {
      harness.cleanup();
    }
  });

  it('replaces the published camera device and persists the new selection', async () => {
    const harness = await setupVoiceRoomCameraHarness();
    harness.state.videoInputDevice = 'camera-1';
    harness.state.enumeratedVideoDevices = ['camera-1', 'camera-2'];

    try {
      await harness.driveToActive();
      await harness.connectMedia();
      await harness.voiceRoom.toggleCameraIntent();
      await harness.voiceRoom.changeSelectedCamera('camera-2');

      expect(harness.state.lastLiveKitModule).not.toBeNull();
      expect(harness.state.lastLiveKitModule!.replaceCameraDeviceCalls).toEqual(['camera-2']);
      expect(harness.state.videoInputDevice).toBe('camera-2');
      expect(harness.voiceRoom.getState().cameraSelectedDeviceId).toBe('camera-2');
      expect(harness.voiceRoom.getState().cameraPublication).toBe('published');
    } finally {
      harness.cleanup();
    }
  });
});
