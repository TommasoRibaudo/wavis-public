import { useEffect, useRef } from 'react';

import type { RoomParticipant, VoiceRoomState } from './voice-room';
import {
  listenTrayEvents,
  updateTrayState,
  type TrayAction,
  type TrayStateUpdate,
} from './tray-bridge';

interface TrayRoomState {
  machineState: VoiceRoomState['machineState'];
  participants: ReadonlyArray<Pick<RoomParticipant, 'id' | 'isMuted'>>;
  selfParticipantId: string | null;
  isDeafened: boolean;
}

interface UseTrayIntegrationOptions {
  roomState: TrayRoomState | null;
  channelId: string | undefined;
  onLeave: () => void;
  onToggleMute: () => void;
  onToggleDeafen: () => void;
}

export function deriveTrayState(roomState: TrayRoomState): TrayStateUpdate {
  const selfParticipant = roomState.participants.find(
    (participant) => participant.id === roomState.selfParticipantId,
  );

  return {
    inVoiceSession: roomState.machineState === 'active',
    isMuted: selfParticipant?.isMuted ?? false,
    isDeafened: roomState.isDeafened,
  };
}

export function useTrayIntegration({
  roomState,
  channelId,
  onLeave,
  onToggleMute,
  onToggleDeafen,
}: UseTrayIntegrationOptions): void {
  const handlersRef = useRef({ onLeave, onToggleMute, onToggleDeafen });

  useEffect(() => {
    handlersRef.current = { onLeave, onToggleMute, onToggleDeafen };
  }, [onLeave, onToggleMute, onToggleDeafen]);

  useEffect(() => {
    return listenTrayEvents((action: TrayAction) => {
      switch (action) {
        case 'toggle-mute':
          handlersRef.current.onToggleMute();
          break;
        case 'toggle-deafen':
          handlersRef.current.onToggleDeafen();
          break;
        case 'leave':
          handlersRef.current.onLeave();
          break;
        case 'show':
          // Rust owns showing and focusing the main window for this action.
          break;
      }
    });
  }, [channelId]);

  const machineState = roomState?.machineState;
  const participants = roomState?.participants;
  const selfParticipantId = roomState?.selfParticipantId;
  const isDeafened = roomState?.isDeafened;

  useEffect(() => {
    if (machineState === undefined || participants === undefined || isDeafened === undefined)
      return;
    updateTrayState(
      deriveTrayState({
        machineState,
        participants,
        selfParticipantId: selfParticipantId ?? null,
        isDeafened,
      }),
    );
  }, [machineState, participants, selfParticipantId, isDeafened]);

  useEffect(() => {
    return () => {
      updateTrayState({ inVoiceSession: false, isMuted: false, isDeafened: false });
    };
  }, []);
}
