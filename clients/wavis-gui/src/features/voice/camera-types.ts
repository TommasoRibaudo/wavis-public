// Camera quality tiers.
//
// Bitrates are calibrated against LiveKit's WebRTC bitrate guide
// (https://livekit.io/webrtc/bitrate-guide), VP8 webcam column, VMAF ~90 target.
// We sit at or slightly under those numbers to keep bandwidth low for a small
// invite-only voice app where the camera is a secondary feature, not the focus.
//
//   Resolution   LiveKit VP8 (VMAF 90)   Wavis HIGH   Notes
//   1280x720     ~1.0 Mbps               (not used)   Too costly for our use
//   960x540      ~600 kbps               500 kbps     -17% — still readable
//   640x360      ~400 kbps               (not used)   Reserve for future MID
//   320x240      ~140 kbps               120 kbps     LOW tier, fallback when
//                                                     a screen share is active
//
// Frame rate is kept lower than typical (24 fps for HIGH, 15 fps for LOW) since
// faces in a voice call rarely need cinematic motion smoothness. Dropping fps
// is the cheapest way to reduce bitrate without touching resolution.
export type CameraQuality =
  | { tier: 'high'; width: 960; height: 540; maxFps: 24; maxBitrate: 500_000; codec: 'vp8' }
  | { tier: 'low'; width: 320; height: 240; maxFps: 15; maxBitrate: 120_000; codec: 'vp8' };

export const CAMERA_QUALITY_HIGH: CameraQuality = {
  tier: 'high',
  width: 960,
  height: 540,
  maxFps: 24,
  maxBitrate: 500_000,
  codec: 'vp8',
};

export const CAMERA_QUALITY_LOW: CameraQuality = {
  tier: 'low',
  width: 320,
  height: 240,
  maxFps: 15,
  maxBitrate: 120_000,
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
