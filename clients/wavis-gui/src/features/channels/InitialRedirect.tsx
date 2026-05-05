import { useState, useEffect } from 'react';
import { useNavigate } from 'react-router';
import { getLastChannel } from '@features/settings/settings-store';
import ChannelsList from './ChannelsList';

/**
 * Index route component that redirects returning users to their last channel's
 * room view. New users (no stored channel) see the ChannelsList as before.
 */
export default function InitialRedirect() {
  const navigate = useNavigate();
  const [showChannelsList, setShowChannelsList] = useState(false);

  useEffect(() => {
    getLastChannel().then((ch) => {
      if (ch) {
        navigate('/room', {
          state: { channelId: ch.id, channelName: ch.name, channelRole: ch.role },
          replace: true,
        });
      } else {
        setShowChannelsList(true);
      }
    });
  }, [navigate]);

  if (!showChannelsList) return null;
  return <ChannelsList />;
}
