import { memo, useCallback, useState } from 'react';
import { Tooltip, TooltipTrigger, TooltipContent } from '../../components/ui/tooltip';
import type { VoicePassthroughState, VoiceSubRoom } from './participant-volume-model';
import { ParticipantRow, type ParticipantRowViewModel } from './ParticipantRow';
import type { useShareViewerWindows } from './useShareViewerWindows';
import type { VoiceRoomState } from './voice-room';
import {
  leaveSubRoom,
  joinSubRoom,
  clearPassthrough,
  setPassthrough,
  createSubRoom,
  stopAllShares,
  setParticipantVolume,
  kickParticipant,
  muteParticipant,
  unmuteParticipant,
} from './voice-room';

export const ROOM_REMOVAL_COUNTDOWN_INTERVAL_MS = 10_000;

export function roomRemovalCountdownText(deleteAtMs: number | null, nowMs: number): string | null {
  if (deleteAtMs === null) return null;
  const remainingMs = Math.max(0, deleteAtMs - nowMs);
  const seconds = Math.max(
    10,
    Math.ceil(remainingMs / ROOM_REMOVAL_COUNTDOWN_INTERVAL_MS) *
      (ROOM_REMOVAL_COUNTDOWN_INTERVAL_MS / 1000),
  );
  return `Removing in less than ${seconds} seconds`;
}

type ShareViewerWindowsSlice = Pick<
  ReturnType<typeof useShareViewerWindows>,
  | 'watchingShareIds'
  | 'shareVolumes'
  | 'watchAllOpen'
  | 'getSavedShareVolume'
  | 'getSavedShareMuted'
  | 'syncScreenShareVolume'
  | 'syncScreenShareMuted'
  | 'openShareWindow'
  | 'closeShareWindow'
  | 'toggleWatchAllWindow'
>;

export interface ParticipantsPanelProps {
  participants: ParticipantRowViewModel[];
  subRooms: VoiceSubRoom[];
  joinedSubRoomId: string | null;
  selfParticipantId: string | null;
  passthrough: VoicePassthroughState | null;
  passthroughEnabled: boolean;
  isHost: boolean;
  sharersCount: number;
  countdownNowMs: number;
  selfIsDeafened: boolean;
  hasActiveVideoShare: boolean;
  hasActiveAudioShare: boolean;
  expandedSections: Record<string, boolean>;
  toggleSection: (key: string) => void;
  watchAllHotkeyLabel: string;
  shareViewerWindows: ShareViewerWindowsSlice;
  getRoomSnapshot: () => VoiceRoomState | null;
}

function areParticipantsPanelPropsEqual(
  prev: ParticipantsPanelProps,
  next: ParticipantsPanelProps,
): boolean {
  if (prev.participants.length !== next.participants.length) return false;
  for (let i = 0; i < prev.participants.length; i++) {
    const a = prev.participants[i];
    const b = next.participants[i];
    if (
      a.id !== b.id ||
      a.displayName !== b.displayName ||
      a.color !== b.color ||
      a.isMuted !== b.isMuted ||
      a.isHostMuted !== b.isHostMuted ||
      a.isDeafened !== b.isDeafened ||
      a.isSpeaking !== b.isSpeaking ||
      a.isSharing !== b.isSharing ||
      a.mediaConnected !== b.mediaConnected ||
      a.volume !== b.volume ||
      a.cameraOn !== b.cameraOn ||
      a.isAudioOnlySharer !== b.isAudioOnlySharer ||
      a.hasVideoShare !== b.hasVideoShare ||
      a.hasScreenShareStream !== b.hasScreenShareStream
    )
      return false;
  }

  if (prev.subRooms.length !== next.subRooms.length) return false;
  for (let i = 0; i < prev.subRooms.length; i++) {
    const a = prev.subRooms[i];
    const b = next.subRooms[i];
    if (
      a.id !== b.id ||
      a.deleteAtMs !== b.deleteAtMs ||
      a.participantIds.length !== b.participantIds.length ||
      a.participantIds.some((id, idx) => id !== b.participantIds[idx])
    )
      return false;
  }

  return (
    prev.joinedSubRoomId === next.joinedSubRoomId &&
    prev.selfParticipantId === next.selfParticipantId &&
    prev.passthrough?.sourceSubRoomId === next.passthrough?.sourceSubRoomId &&
    prev.passthrough?.targetSubRoomId === next.passthrough?.targetSubRoomId &&
    prev.passthrough?.label === next.passthrough?.label &&
    prev.passthroughEnabled === next.passthroughEnabled &&
    prev.isHost === next.isHost &&
    prev.sharersCount === next.sharersCount &&
    prev.countdownNowMs === next.countdownNowMs &&
    prev.selfIsDeafened === next.selfIsDeafened &&
    prev.hasActiveVideoShare === next.hasActiveVideoShare &&
    prev.hasActiveAudioShare === next.hasActiveAudioShare &&
    prev.expandedSections === next.expandedSections &&
    prev.toggleSection === next.toggleSection &&
    prev.watchAllHotkeyLabel === next.watchAllHotkeyLabel &&
    // Coarse check: these come from useShareViewerWindows' own React state, which
    // (unlike voice-room.ts) replaces rather than mutates its Map/Set in place, so
    // size-based comparison is a reasonable proxy without re-deriving full identity.
    prev.shareViewerWindows.watchingShareIds.size ===
      next.shareViewerWindows.watchingShareIds.size &&
    prev.shareViewerWindows.shareVolumes.size === next.shareViewerWindows.shareVolumes.size &&
    prev.shareViewerWindows.watchAllOpen === next.shareViewerWindows.watchAllOpen
  );
}

function ParticipantsPanelImpl({
  participants,
  subRooms,
  joinedSubRoomId,
  selfParticipantId,
  passthrough,
  passthroughEnabled,
  isHost,
  sharersCount,
  countdownNowMs,
  selfIsDeafened,
  hasActiveVideoShare,
  hasActiveAudioShare,
  expandedSections,
  toggleSection,
  watchAllHotkeyLabel,
  shareViewerWindows,
  getRoomSnapshot,
}: ParticipantsPanelProps) {
  const [expandedUser, setExpandedUser] = useState<string | null>(null);
  // Local mute state: key = participantId, value = pre-mute volume (presence means muted).
  const [localMicMuted, setLocalMicMuted] = useState<Map<string, number>>(new Map());

  const toggleLocalMicMute = useCallback((participantId: string, currentVolume: number) => {
    setLocalMicMuted((prev) => {
      const next = new Map(prev);
      if (next.has(participantId)) {
        const saved = next.get(participantId)!;
        next.delete(participantId);
        setParticipantVolume(participantId, saved);
      } else {
        next.set(participantId, currentVolume || 70);
        setParticipantVolume(participantId, 0);
      }
      return next;
    });
  }, []);

  const {
    watchingShareIds,
    shareVolumes,
    watchAllOpen,
    getSavedShareVolume,
    getSavedShareMuted,
    syncScreenShareVolume,
    syncScreenShareMuted,
    openShareWindow,
    closeShareWindow,
    toggleWatchAllWindow,
  } = shareViewerWindows;

  return (
    <div className="flex-1 overflow-y-auto">
      {subRooms.map((subRoom) => {
        const sectionKey = `sub-room:${subRoom.id}`;
        const roomPanelId = `sub-room-panel-${subRoom.id}`;
        const isExpanded = expandedSections[sectionKey] ?? true;
        const roomParticipantIds = new Set(subRoom.participantIds);
        const roomParticipants = participants.filter((participant) =>
          roomParticipantIds.has(participant.id),
        );
        const roomRemoteSharers = roomParticipants.filter(
          (participant) => participant.isSharing && participant.id !== selfParticipantId,
        );
        const isJoinedRoom = joinedSubRoomId === subRoom.id;
        const pairedSubRoomId =
          passthrough?.sourceSubRoomId === joinedSubRoomId
            ? passthrough.targetSubRoomId
            : passthrough?.targetSubRoomId === joinedSubRoomId
              ? passthrough.sourceSubRoomId
              : null;
        const isWatchAllScopedRoom = isJoinedRoom || pairedSubRoomId === subRoom.id;
        const showEnabledWatchAll = isWatchAllScopedRoom && roomRemoteSharers.length > 0;
        const showDisabledWatchAll = !isWatchAllScopedRoom && roomRemoteSharers.length > 0;
        const roomRemovalText =
          roomParticipants.length === 0
            ? roomRemovalCountdownText(subRoom.deleteAtMs, countdownNowMs)
            : null;
        const activePassthroughInvolvesRoom =
          !!passthrough &&
          (passthrough.sourceSubRoomId === subRoom.id ||
            passthrough.targetSubRoomId === subRoom.id);
        const activePassthroughInvolvesLocalRoom =
          !!passthrough &&
          !!joinedSubRoomId &&
          (passthrough.sourceSubRoomId === joinedSubRoomId ||
            passthrough.targetSubRoomId === joinedSubRoomId);
        const canSetPassthrough =
          passthroughEnabled && !passthrough && !!joinedSubRoomId && !isJoinedRoom;
        const canClearPassthrough =
          activePassthroughInvolvesRoom && activePassthroughInvolvesLocalRoom;
        const passthroughDisabled = !(canSetPassthrough || canClearPassthrough);
        // activePassthroughInvolvesRoom being true doesn't narrow passthrough's type here
        // (it's a separately-computed boolean) — passthrough is still genuinely
        // VoicePassthroughState | null at this point, so the ?. is load-bearing.
        const passthroughLabel =
          // eslint-disable-next-line @typescript-eslint/no-unnecessary-condition
          activePassthroughInvolvesRoom && passthrough?.label ? `“${passthrough.label}”` : '“ ”';
        const passthroughClassName = activePassthroughInvolvesRoom
          ? 'border-wavis-danger text-wavis-danger hover:bg-wavis-danger hover:text-wavis-bg'
          : passthroughDisabled
            ? 'border-wavis-text-secondary text-wavis-text-secondary opacity-60 cursor-not-allowed'
            : 'border-wavis-accent text-wavis-accent hover:bg-wavis-accent hover:text-wavis-bg';
        const passthroughButton = (
          <Tooltip>
            <TooltipTrigger asChild>
              <button
                type="button"
                disabled={passthroughDisabled}
                aria-label="Passthrough: listen and talk to this room at a lower volume"
                onPointerDown={(event) => {
                  event.stopPropagation();
                }}
                onClick={(event) => {
                  event.stopPropagation();
                  if (canClearPassthrough) {
                    clearPassthrough();
                  } else if (canSetPassthrough) {
                    setPassthrough(subRoom.id);
                  }
                }}
                className={`min-w-7 text-xs py-0.5 px-1 border transition-colors cursor-pointer disabled:cursor-not-allowed ${passthroughClassName}`}
              >
                {passthroughLabel}
              </button>
            </TooltipTrigger>
            <TooltipContent
              side="bottom"
              className="bg-wavis-panel text-wavis-text border border-wavis-text-secondary font-mono text-xs"
            >
              Passthrough: listen and talk to this room at a lower volume
            </TooltipContent>
          </Tooltip>
        );
        const roomActionButton = isJoinedRoom ? (
          <button
            type="button"
            onPointerDown={(event) => {
              event.stopPropagation();
            }}
            onClick={(event) => {
              event.stopPropagation();
              leaveSubRoom();
            }}
            className="text-xs py-0.5 px-1 border border-wavis-danger text-wavis-danger transition-colors hover:bg-wavis-danger hover:text-wavis-bg cursor-pointer"
          >
            /leave
          </button>
        ) : (
          <button
            type="button"
            onPointerDown={(event) => {
              event.stopPropagation();
            }}
            onClick={(event) => {
              event.stopPropagation();
              joinSubRoom(subRoom.id);
            }}
            className="text-xs py-0.5 px-1 border border-wavis-accent text-wavis-accent transition-colors hover:bg-wavis-accent hover:text-wavis-bg cursor-pointer"
          >
            /join
          </button>
        );

        return (
          <div key={subRoom.id} className="border-b border-wavis-text-secondary">
            <div
              role="button"
              tabIndex={0}
              aria-expanded={isExpanded}
              aria-controls={roomPanelId}
              onPointerDown={(event) => {
                if (!event.isPrimary || event.button !== 0) return;
                event.preventDefault();
                event.currentTarget.focus();
                toggleSection(sectionKey);
              }}
              onKeyDown={(event) => {
                if (event.key !== 'Enter' && event.key !== ' ') return;
                event.preventDefault();
                toggleSection(sectionKey);
              }}
              className="w-full px-3 py-2 flex items-center gap-2 text-sm text-left hover:opacity-80 cursor-pointer"
            >
              <div className="flex items-center gap-2 min-w-0 flex-1">
                <span className="text-wavis-text-secondary">{isExpanded ? '[-]' : '[+]'}</span>
                <span>{`ROOM ${subRoom.roomNumber}`}</span>
                <span className="text-wavis-text-secondary">({roomParticipants.length})</span>
              </div>
              <div className="shrink-0 flex items-center gap-1">
                {passthroughButton}
                {roomActionButton}
              </div>
            </div>
            {isExpanded && (
              <div id={roomPanelId} className="px-3 py-2 space-y-1 text-sm">
                {roomParticipants.length > 0 ? (
                  roomParticipants.map((participant) => (
                    <ParticipantRow
                      key={participant.id}
                      participant={participant}
                      isSelf={participant.id === selfParticipantId}
                      selfIsDeafened={selfIsDeafened}
                      hasActiveVideoShare={hasActiveVideoShare}
                      hasActiveAudioShare={hasActiveAudioShare}
                      isHost={isHost}
                      isExpanded={expandedUser === participant.id}
                      onToggleExpanded={() =>
                        setExpandedUser((prev) => (prev === participant.id ? null : participant.id))
                      }
                      isLocallyMuted={localMicMuted.has(participant.id)}
                      onToggleLocalMicMute={() =>
                        toggleLocalMicMute(participant.id, participant.volume)
                      }
                      onSetParticipantVolume={(v) => {
                        if (localMicMuted.has(participant.id)) {
                          setLocalMicMuted((prev) => {
                            const next = new Map(prev);
                            next.delete(participant.id);
                            return next;
                          });
                        }
                        setParticipantVolume(participant.id, v);
                      }}
                      shareVolume={
                        shareVolumes.get(participant.id) ?? getSavedShareVolume(participant.id)
                      }
                      shareMuted={getSavedShareMuted(participant.id)}
                      onSyncShareVolume={(v) => syncScreenShareVolume(participant.id, v)}
                      onSyncShareMuted={(muted) => syncScreenShareMuted(participant.id, muted)}
                      isWatchingShare={watchingShareIds.has(participant.id)}
                      onToggleWatchShare={() => {
                        if (watchingShareIds.has(participant.id)) {
                          closeShareWindow(participant.id);
                          return;
                        }
                        const rs = getRoomSnapshot();
                        const liveParticipant = rs?.participants.find(
                          (pp) => pp.id === participant.id,
                        );
                        if (!liveParticipant) return;
                        void openShareWindow(
                          participant.id,
                          liveParticipant,
                          rs?.screenShareStreams.get(participant.id) ?? null,
                        );
                      }}
                      onKick={() => kickParticipant(participant.id)}
                      onHostMute={() => muteParticipant(participant.id)}
                      onHostUnmute={() => unmuteParticipant(participant.id)}
                    />
                  ))
                ) : (
                  <div className="pl-8 text-xs text-wavis-text-secondary space-y-1">
                    <div>No participants in this room.</div>
                    {roomRemovalText && <div>{roomRemovalText}</div>}
                  </div>
                )}
                {(showEnabledWatchAll || showDisabledWatchAll) && (
                  <div className="pt-2 flex items-center gap-2">
                    {showEnabledWatchAll && (
                      <button
                        type="button"
                        onClick={() => {
                          void toggleWatchAllWindow();
                        }}
                        title={`${watchAllOpen ? '/close-all' : '/watch-all'} (${watchAllHotkeyLabel})`}
                        className={`w-full block text-sm font-bold py-1 px-2 border transition-colors cursor-pointer ${watchAllOpen ? 'border-wavis-purple bg-wavis-purple/8 text-wavis-purple hover:bg-wavis-purple hover:text-wavis-bg' : 'border-wavis-text-secondary bg-wavis-text-secondary/8 text-wavis-text hover:bg-wavis-text-secondary hover:text-wavis-text-contrast'}`}
                      >
                        {watchAllOpen ? '/close-all' : '/watch-all'}
                      </button>
                    )}
                    {showDisabledWatchAll && (
                      <button
                        type="button"
                        disabled
                        aria-disabled="true"
                        title="Join this room to watch all streams together."
                        className="text-xs py-0.5 px-1 border border-wavis-text-secondary text-wavis-text-secondary opacity-60 cursor-not-allowed"
                      >
                        /watch-all
                      </button>
                    )}
                  </div>
                )}
              </div>
            )}
          </div>
        );
      })}
      <div className="px-3 py-2 flex items-center justify-between gap-2 border-b border-wavis-text-secondary">
        <button
          onClick={() => createSubRoom()}
          className="text-sm font-bold py-1 px-2 border border-wavis-accent bg-wavis-accent/8 text-wavis-accent transition-colors hover:bg-wavis-accent hover:text-wavis-bg"
        >
          /create room
        </button>
        {isHost && sharersCount > 1 ? (
          <div className="flex items-center gap-2">
            <button
              onClick={stopAllShares}
              className="text-wavis-danger text-xs border border-wavis-danger py-0.5 px-1 hover:bg-wavis-danger hover:text-wavis-bg transition-colors"
            >
              /stopall
            </button>
          </div>
        ) : (
          <span />
        )}
      </div>
    </div>
  );
}

export const ParticipantsPanel = memo(ParticipantsPanelImpl, areParticipantsPanelPropsEqual);
