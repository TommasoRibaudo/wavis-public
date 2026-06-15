import { useState, useEffect } from 'react';
import { useNavigate, useLocation } from 'react-router';
import { clearLastChannel, getLastChannel } from '@features/settings/settings-store';
import ChannelsList from './ChannelsList';
import { fetchChannels, type Channel } from './channels';

type LastChannel = { id: string; name: string; role: Channel['role'] };

export function resolveInitialRedirect(
  lastChannel: LastChannel | null,
  channels: Channel[],
): { kind: 'room'; channel: Channel } | { kind: 'list'; clearStaleLastChannel: boolean } {
  if (!lastChannel) return { kind: 'list', clearStaleLastChannel: false };
  const channel = channels.find((ch) => ch.id === lastChannel.id);
  if (!channel) return { kind: 'list', clearStaleLastChannel: true };
  return { kind: 'room', channel };
}

/**
 * Index route component that redirects returning users to their last channel's
 * room view. New users (no stored channel) see the ChannelsList as before.
 */
export default function InitialRedirect() {
  const navigate = useNavigate();
  const location = useLocation();
  const [showChannelsList, setShowChannelsList] = useState(false);

  useEffect(() => {
    // If we arrived here after an explicit room leave, skip the auto-redirect to
    // /room — the user intentionally left and should see the channels list.
    if ((location.state as Record<string, unknown> | null)?.skipAutoRedirect) {
      setShowChannelsList(true);
      return;
    }

    let cancelled = false;
    (async () => {
      const lastChannel = await getLastChannel();
      if (!lastChannel) {
        if (!cancelled) setShowChannelsList(true);
        return;
      }

      try {
        const channels = await fetchChannels();
        if (cancelled) return;

        const target = resolveInitialRedirect(lastChannel, channels);
        if (target.kind === 'room') {
          const ch = target.channel;
          navigate('/room', {
            state: { channelId: ch.id, channelName: ch.name, channelRole: ch.role },
            replace: true,
          });
          return;
        }

        if (target.clearStaleLastChannel) {
          await clearLastChannel();
        }
        if (!cancelled) setShowChannelsList(true);
      } catch {
        if (!cancelled) setShowChannelsList(true);
      }
    })();

    return () => {
      cancelled = true;
    };
  }, [navigate, location.state]);

  if (!showChannelsList) return null;
  return <ChannelsList />;
}
