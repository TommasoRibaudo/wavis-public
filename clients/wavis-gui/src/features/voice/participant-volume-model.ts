/**
 * Participant Volume & Sub-Room Model
 *
 * Pure, deterministic rules for participant audibility across synchronized
 * sub-rooms: passthrough pairing, effective volume attenuation, participant
 * list merging across reconnects, and the participant→sub-room reverse index.
 *
 * Zero IO — no state, no LiveKit, no Tauri. Extracted from voice-room.ts;
 * exported for direct consumption and property testing. The state-touching
 * wrappers (applyEffectiveParticipantVolume, markParticipantMediaConnected)
 * stay in voice-room and call this pure core.
 */

/* ─── Types ─────────────────────────────────────────────────────── */

export interface VoiceSubRoom {
  id: string;
  roomNumber: number;
  isDefault: boolean;
  participantIds: string[];
  deleteAtMs: number | null;
}

export interface VoicePassthroughState {
  sourceSubRoomId: string;
  targetSubRoomId: string;
  label: string;
}

/* ─── Rules ─────────────────────────────────────────────────────── */

/**
 * The sub-room paired with `joinedSubRoomId` through the active passthrough
 * link, or null when the joined room is not part of the pair.
 */
export function pairedSubRoomId(
  joinedSubRoomId: string,
  passthrough: Pick<VoicePassthroughState, 'sourceSubRoomId' | 'targetSubRoomId'>,
): string | null {
  if (passthrough.sourceSubRoomId === joinedSubRoomId) return passthrough.targetSubRoomId;
  if (passthrough.targetSubRoomId === joinedSubRoomId) return passthrough.sourceSubRoomId;
  return null;
}

export function computeEffectiveParticipantVolume(
  manualVolume: number,
  participantId: string,
  selfParticipantId: string | null,
  joinedSubRoomId: string | null,
  participantSubRoomById: Record<string, string>,
  passthrough: VoicePassthroughState | null,
  passthroughVolumeFraction = 0.2,
): number {
  if (participantId === selfParticipantId) return manualVolume;
  if (!joinedSubRoomId) return 0;
  const participantSubRoomId = participantSubRoomById[participantId] ?? null;
  if (participantSubRoomId === joinedSubRoomId) return manualVolume;
  if (!participantSubRoomId || !passthrough) return 0;

  if (participantSubRoomId !== pairedSubRoomId(joinedSubRoomId, passthrough)) return 0;
  return Math.round(manualVolume * passthroughVolumeFraction);
}

/**
 * Pure function: merge old and new participant lists, preserving per-participant
 * volume settings across reconnects. Matched by id: present in both → keep old
 * volume; only in new → default volume; only in old → discarded.
 */
export function mergeParticipantsWithVolume<
  P extends { id: string; volume: number; mediaConnected: boolean },
>(oldList: P[], newList: P[]): P[] {
  const preserved = new Map(
    oldList.map((p) => [
      p.id,
      {
        volume: p.volume,
        mediaConnected: p.mediaConnected,
      },
    ]),
  );
  return newList.map((p) => ({
    ...p,
    volume: preserved.get(p.id)?.volume ?? p.volume,
    mediaConnected: preserved.get(p.id)?.mediaConnected ?? p.mediaConnected,
  }));
}

/** Derived reverse index of participant id → synchronized sub-room id. */
export function deriveParticipantSubRoomById(
  subRooms: Array<Pick<VoiceSubRoom, 'id' | 'participantIds'>>,
): Record<string, string> {
  const assignments: Record<string, string> = {};
  for (const room of subRooms) {
    for (const participantId of room.participantIds) {
      assignments[participantId] = room.id;
    }
  }
  return assignments;
}
