/**
 * Video Tile View Model
 *
 * Pure, deterministic construction of the camera tile grid and the room
 * panel tab selection: remote/self tile assembly, panel tab policy, and
 * the share-aware camera quality decision.
 *
 * Zero IO — no state, no LiveKit, no Tauri. Extracted from voice-room.ts;
 * exported for direct consumption and property testing
 * (voice-room-camera.property.test.ts).
 */

import {
  CAMERA_QUALITY_HIGH,
  CAMERA_QUALITY_LOW,
  type CameraQuality,
  type PanelTabInput,
  type VideoTileViewModel,
} from './camera-types';
import { colorFor } from './chat-display-model';
import { screenShareActiveInRoom } from './share-slot-policy';

/* ─── Types ─────────────────────────────────────────────────────── */

export type CameraPublicationState = 'idle' | 'opening' | 'publishing' | 'published' | 'failing';

export interface RemoteCameraTileRuntimeState {
  track: MediaStreamTrack | null;
  isMuted: boolean;
  hasError: boolean;
}

/* ─── View-model construction ───────────────────────────────────── */

export function computeRoomPanelTab(input: PanelTabInput): 'logs' | 'video' {
  if (!input.anyVideoActive) {
    return 'logs';
  }
  if (input.manualOverride !== null) {
    return input.manualOverride;
  }
  return 'video';
}

export function buildVideoTilesById(input: {
  participants: Array<{ id: string; displayName: string; color: string }>;
  selfParticipantId: string | null;
  cameraPublication: CameraPublicationState;
  localTrack: MediaStreamTrack | null;
  remoteTilesById: Record<string, RemoteCameraTileRuntimeState>;
}): Record<string, VideoTileViewModel> {
  const tilesById: Record<string, VideoTileViewModel> = {};
  const participantsById = new Map(
    input.participants.map((participant) => [participant.id, participant]),
  );

  for (const [participantId, remoteTile] of Object.entries(input.remoteTilesById)) {
    const participant = participantsById.get(participantId);
    tilesById[participantId] = {
      participantId,
      displayName: participant?.displayName || participantId,
      color: participant?.color || colorFor({ id: participantId }),
      track: remoteTile.track,
      isSelf: false,
      isMuted: remoteTile.isMuted,
      hasError: remoteTile.hasError,
    };
  }

  if (input.cameraPublication === 'published' && input.selfParticipantId) {
    const participant = participantsById.get(input.selfParticipantId);
    tilesById[input.selfParticipantId] = {
      participantId: input.selfParticipantId,
      displayName: participant?.displayName || input.selfParticipantId,
      color: participant?.color || colorFor({ id: input.selfParticipantId }),
      track: input.localTrack,
      isSelf: true,
      isMuted: false,
      hasError: false,
    };
  }

  return tilesById;
}

/**
 * Cameras drop to the low publish quality while a screen share is active in
 * the joined sub-room, freeing bandwidth for the share.
 */
export function desiredCameraQualityForState(currentState: {
  joinedSubRoomId: string | null;
  participants: Array<{ id: string; isSharing: boolean }>;
  participantSubRoomById: Record<string, string>;
}): CameraQuality {
  return screenShareActiveInRoom(currentState) ? CAMERA_QUALITY_LOW : CAMERA_QUALITY_HIGH;
}
