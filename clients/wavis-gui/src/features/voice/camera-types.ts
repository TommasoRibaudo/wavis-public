export type CameraQuality =
  | { tier: 'high'; width: 1280; height: 720; maxFps: 30; maxBitrate: 800_000; codec: 'vp8' }
  | { tier: 'low'; width: 320; height: 240; maxFps: 15; maxBitrate: 150_000; codec: 'vp8' };

export const CAMERA_QUALITY_HIGH: CameraQuality = {
  tier: 'high',
  width: 1280,
  height: 720,
  maxFps: 30,
  maxBitrate: 800_000,
  codec: 'vp8',
};

export const CAMERA_QUALITY_LOW: CameraQuality = {
  tier: 'low',
  width: 320,
  height: 240,
  maxFps: 15,
  maxBitrate: 150_000,
  codec: 'vp8',
};

export type CameraStartError =
  | { kind: 'permission_denied' }
  | { kind: 'device_unavailable' }
  | { kind: 'device_in_use' }
  | { kind: 'timeout' }
  | { kind: 'no_camera_configured' }
  | { kind: 'publish_failed' };

export type CameraStartWarning =
  | { kind: 'device_not_found'; missingDeviceId: string }
  | { kind: 'permission_denied' }
  | { kind: 'device_in_use' }
  | { kind: 'hardware_error' };

export interface VideoTileViewModel {
  participantId: string;
  displayName: string;
  color: string;
  track: MediaStreamTrack | null;
  isSelf: boolean;
  isMuted: boolean;
  hasError: boolean;
}

export interface VideoTileSnapshot {
  participantId: string;
  displayName: string;
  color: string;
  hasTrack: boolean;
  isSelf: boolean;
  isMuted: boolean;
  hasError: boolean;
}

export interface PanelTabInput {
  anyVideoActive: boolean;
  manualOverride: 'logs' | 'video' | null;
  hadAnyVideoActive: boolean;
}

/**
 * Extension shape for the four remote-camera callbacks. Task 3.2 will merge
 * these signatures into the canonical `MediaCallbacks` interface in
 * `livekit-media.ts` so the live `LiveKitModule` instance carries them.
 * Consumers of this module MUST NOT re-implement these signatures separately.
 */
export interface CameraMediaCallbacks {
  onRemoteCameraPublished(participantId: string): void;
  onRemoteCameraReady(participantId: string, track: MediaStreamTrack): void;
  onRemoteCameraMutedChanged(participantId: string, muted: boolean): void;
  onRemoteCameraUnpublished(participantId: string): void;
}
