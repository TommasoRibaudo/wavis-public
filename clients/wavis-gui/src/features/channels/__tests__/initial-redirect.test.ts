import { describe, expect, it } from 'vitest';
import { resolveInitialRedirect } from '../InitialRedirect';
import type { Channel } from '../channels';

const channels: Channel[] = [
  {
    id: 'channel-current',
    name: 'current',
    role: 'member',
    ownerUserId: 'owner-1',
    createdAt: '2026-05-29T00:00:00Z',
  },
];

describe('resolveInitialRedirect', () => {
  it('shows the channel list when there is no saved channel', () => {
    expect(resolveInitialRedirect(null, channels)).toEqual({ kind: 'list' });
  });

  it('redirects to the saved channel when it is still accessible', () => {
    expect(
      resolveInitialRedirect(
        { id: 'channel-current', name: 'old name', role: 'owner' },
        channels,
      ),
    ).toEqual({ kind: 'room', channel: channels[0] });
  });

  it('shows the channel list when saved channel is not in the current list', () => {
    expect(
      resolveInitialRedirect(
        { id: 'channel-old-account', name: 'old account channel', role: 'member' },
        channels,
      ),
    ).toEqual({ kind: 'list' });
  });
});
