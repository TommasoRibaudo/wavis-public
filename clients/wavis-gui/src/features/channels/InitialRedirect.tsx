import { useState, useEffect } from 'react';
import { useNavigate, useLocation } from 'react-router';
import { getLastChannel } from '@features/settings/settings-store';
import ChannelsList from './ChannelsList';
import { fetchChannels, type Channel } from './channels';

type LastChannel = { id: string; name: string; role: Channel['role'] };

export function resolveInitialRedirect(
  lastChannel: LastChannel | null,
  channels: Channel[],
): { kind: 'room'; channel: Channel } | { kind: 'list' } {
  if (!lastChannel) return { kind: 'list' };
  const channel = channels.find((ch) => ch.id === lastChannel.id);
  if (!channel) return { kind: 'list' };
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
    void (async () => {
      const lastChannel = await getLastChannel();
      if (!lastChannel) {
        // `cancelled` is flipped by the effect's cleanup closure while
        // getLastChannel() is in flight — invisible to the linter's local
        // narrowing, but a real guard against post-unmount state updates.
        // eslint-disable-next-line @typescript-eslint/no-unnecessary-condition
        if (!cancelled) setShowChannelsList(true);
        return;
      }

      try {
        const channels = await fetchChannels();
        // Same cross-closure race: cleanup can set `cancelled` while
        // fetchChannels() is in flight.
        // eslint-disable-next-line @typescript-eslint/no-unnecessary-condition
        if (cancelled) return;

        const target = resolveInitialRedirect(lastChannel, channels);
        if (target.kind === 'room') {
          const ch = target.channel;
          void navigate('/room', {
            state: { channelId: ch.id, channelName: ch.name, channelRole: ch.role },
            replace: true,
          });
          return;
        }

        setShowChannelsList(true);
      } catch {
        // Reachable with cancelled === true: fetchChannels() can reject after
        // cleanup ran, jumping straight here without passing the guard above.
        // eslint-disable-next-line @typescript-eslint/no-unnecessary-condition
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
