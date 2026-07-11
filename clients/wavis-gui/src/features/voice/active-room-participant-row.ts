import type { RoomParticipant } from './voice-room';

export function participantNameVisualState(
  p: RoomParticipant,
  isSelf = false,
): {
  color: string;
  opacity: number;
  animation: string;
  filter: string;
  showConnecting: boolean;
} {
  const mediaConnected = p.mediaConnected;
  return {
    color: mediaConnected ? p.color : 'var(--wavis-text-secondary)',
    opacity: mediaConnected ? 1 : 0.68,
    animation:
      mediaConnected && p.isSpeaking && !p.isMuted ? 'pulse 3s ease-in-out infinite' : 'none',
    filter: mediaConnected && p.isSpeaking && !p.isMuted ? 'brightness(1.5)' : 'brightness(0.7)',
    showConnecting: isSelf && !mediaConnected,
  };
}
