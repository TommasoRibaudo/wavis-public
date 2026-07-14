import { memo, type RefObject } from 'react';
import { useAutoScrollAnchor } from '@shared/hooks/useAutoScrollAnchor';
import {
  buildRoomEventDisplayItems,
  formatTime,
  type RoomEvent,
  type RoomEventType,
} from './chat-display-model';

function getEventColor(type: RoomEventType): string {
  switch (type) {
    case 'join':
      return 'var(--wavis-accent)';
    case 'leave':
    case 'kicked':
      return 'var(--wavis-danger)';
    case 'host-mute':
      return 'var(--wavis-warn)';
    case 'host-unmute':
      return 'var(--wavis-accent)';
    case 'share-start':
    case 'share-stop':
      return 'var(--wavis-purple)';
    case 'share-permission':
      return 'var(--wavis-warn)';
    case 'deafen':
      return 'var(--wavis-warn)';
    case 'undeafen':
      return 'var(--wavis-accent)';
    case 'muted':
    case 'unmuted':
      return 'var(--wavis-text)';
    default:
      return 'var(--wavis-text)';
  }
}

function getUserColor(
  participants: Array<{ id: string; color: string }>,
  participantId?: string,
): string {
  if (!participantId) return 'var(--wavis-text)';
  const p = participants.find((pp) => pp.id === participantId);
  return p?.color ?? 'var(--wavis-text)';
}

function getEventUsername(event: RoomEvent): string | null {
  const msg = event.message;
  const patterns = [
    ' joined',
    ' muted',
    ' unmuted',
    ' started',
    ' stopped',
    ' was kicked',
    ' was muted',
    ' was unmuted',
  ];
  for (const pat of patterns) {
    const idx = msg.indexOf(pat);
    if (idx > 0) return msg.slice(0, idx);
  }
  return null;
}

export interface LogsPanelProps {
  events: RoomEvent[];
  participants: Array<{ id: string; color: string }>;
  cliInput: string;
  onCliInputChange: (value: string) => void;
  onCliKeyDown: (e: React.KeyboardEvent<HTMLInputElement>) => void;
  cliFocused: boolean;
  onCliFocus: () => void;
  onCliBlur: () => void;
  cliInputRef: RefObject<HTMLInputElement | null>;
}

function areLogsPanelPropsEqual(prev: LogsPanelProps, next: LogsPanelProps): boolean {
  if (prev.cliInput !== next.cliInput || prev.cliFocused !== next.cliFocused) return false;
  if (prev.events.length !== next.events.length) return false;
  if (prev.events.length > 0) {
    const lastIdx = prev.events.length - 1;
    if (prev.events[lastIdx].id !== next.events[lastIdx].id) return false;
  }
  return true;
}

function LogsPanelImpl({
  events,
  participants,
  cliInput,
  onCliInputChange,
  onCliKeyDown,
  cliFocused,
  onCliFocus,
  onCliBlur,
  cliInputRef,
}: LogsPanelProps) {
  const logEndRef = useAutoScrollAnchor<HTMLDivElement>(events.length);

  return (
    <>
      <div className="flex-1 overflow-y-auto p-4 space-y-1 text-sm">
        {buildRoomEventDisplayItems(events).map((item) => {
          if (item.type === 'date-divider') {
            return (
              <div key={item.id} className="text-wavis-text-secondary text-xs py-1 text-center">
                {'─'.repeat(12)} {item.label} {'─'.repeat(12)}
              </div>
            );
          }

          const evt = item.event;
          const username = getEventUsername(evt);
          const userColor = getUserColor(participants, evt.participantId);
          return (
            <div
              key={evt.id}
              style={{ whiteSpace: evt.message.includes('\n') ? 'pre-line' : undefined }}
            >
              <span className="text-wavis-text-secondary">[{formatTime(evt.timestamp)}]</span>{' '}
              {username && evt.participantId ? (
                <>
                  <span style={{ color: userColor }}>{username}</span>{' '}
                  <span style={{ color: getEventColor(evt.type) }}>
                    {evt.message.slice(username.length + 1)}
                  </span>
                </>
              ) : (
                <span style={{ color: getEventColor(evt.type) }}>{evt.message}</span>
              )}
            </div>
          );
        })}
        <div ref={logEndRef} />
      </div>
      <div className="p-4 border-t border-wavis-text-secondary">
        <div className="flex items-center gap-2">
          <span className="text-wavis-accent">&gt;</span>
          <input
            type="text"
            value={cliInput}
            onChange={(e) => onCliInputChange(e.target.value)}
            onKeyDown={onCliKeyDown}
            onFocus={onCliFocus}
            onBlur={onCliBlur}
            ref={cliInputRef}
            data-cli-input="true"
            className="flex-1 bg-transparent border-b border-wavis-text-secondary outline-none px-2 py-1 font-mono text-wavis-text"
            placeholder={cliFocused ? '' : 'type command... try /help'}
            autoFocus
          />
        </div>
      </div>
    </>
  );
}

export const LogsPanel = memo(LogsPanelImpl, areLogsPanelPropsEqual);
