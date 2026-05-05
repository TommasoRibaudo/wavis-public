import { useState, useEffect, useRef, useCallback } from 'react';
import { type Channel, fetchChannels } from './channels';
import { fetchVoiceStatusWithFallback } from './channel-detail';
import { ChannelRoleBadge } from './ChannelRoleBadge';
import { usePolling } from '@shared/hooks/usePolling';

const POLL_MS = 15_000;

interface ChannelSwitcherPanelProps {
  onChannelSelect: (ch: Channel) => void;
  onClose: () => void;
  currentChannelId?: string;
}

export function ChannelSwitcherPanel({ onChannelSelect, onClose, currentChannelId }: ChannelSwitcherPanelProps) {
  const [channels, setChannels] = useState<Channel[]>([]);
  const [loading, setLoading] = useState(true);
  const [voiceStatus, setVoiceStatus] = useState<Map<string, { active: boolean; participantCount: number }>>(new Map());
  const requestInFlightRef = useRef(false);

  const loadChannels = useCallback(async (showLoading: boolean) => {
    if (showLoading) setLoading(true);
    requestInFlightRef.current = true;
    try {
      const result = await fetchChannels();
      setChannels(result);
      const ids = result.map((ch) => ch.id);
      fetchVoiceStatusWithFallback(ids).then((status) => {
        setVoiceStatus(status);
      }).catch(() => { /* silent */ });
    } catch {
      // silent — switcher is non-critical
    } finally {
      requestInFlightRef.current = false;
      if (showLoading) setLoading(false);
    }
  }, []);

  useEffect(() => {
    loadChannels(true);
  }, [loadChannels]);

  usePolling(() => {
    if (requestInFlightRef.current) return;
    loadChannels(false);
  }, POLL_MS);

  return (
    <div className="flex-1 flex flex-col min-h-0">
      <div className="px-3 py-3 border-b border-wavis-text-secondary h-[4.5rem] flex items-center justify-between">
        <div className="font-bold text-sm">CHANNELS</div>
        <button
          onClick={onClose}
          className="text-wavis-text-secondary hover:text-wavis-text transition-colors text-xs px-1"
          aria-label="Close channel switcher"
        >[x]</button>
      </div>
      <div className="flex-1 overflow-y-auto p-3 space-y-1">
        {loading && (
          <div className="text-wavis-text-secondary text-xs">loading...</div>
        )}
        {!loading && channels.length === 0 && (
          <div className="text-wavis-text-secondary text-xs">no channels</div>
        )}
        {!loading && channels.map((ch) => {
          const isCurrent = ch.id === currentChannelId;
          return (
            <div
              key={ch.id}
              onClick={() => { if (!isCurrent) onChannelSelect(ch); }}
              className={`flex items-center justify-between gap-3 px-3 py-2 border transition-colors cursor-pointer ${
                isCurrent
                  ? 'border-wavis-accent bg-wavis-accent/8 cursor-default'
                  : 'border-wavis-text-secondary hover:border-wavis-accent'
              }`}
            >
              <span className="min-w-0 truncate text-sm">{ch.name}</span>
              <div className="flex items-center gap-2 shrink-0">
                {voiceStatus.get(ch.id)?.active && (
                  <span className="text-wavis-accent text-xs flex items-center gap-1">
                    <span>●</span>
                    <span>{voiceStatus.get(ch.id)?.participantCount}</span>
                  </span>
                )}
                <ChannelRoleBadge role={ch.role} variant="list" />
                {isCurrent && (
                  <span className="text-wavis-accent text-[0.625rem]">current</span>
                )}
              </div>
            </div>
          );
        })}
      </div>
    </div>
  );
}
