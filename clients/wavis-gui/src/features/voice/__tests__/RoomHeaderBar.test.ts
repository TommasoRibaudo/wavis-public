import { describe, expect, it } from 'vitest';

import { combinedStatusBadge } from '../RoomHeaderBar';

describe('combinedStatusBadge', () => {
  it('shows READY (light green) once signaling is active but media has not started yet — not OFFLINE', () => {
    expect(combinedStatusBadge('active', 'disconnected')).toEqual({
      text: 'READY',
      color: 'var(--wavis-accent)',
    });
  });

  it('still shows OFFLINE (grey) when there is no active session at all', () => {
    expect(combinedStatusBadge('idle', 'disconnected')).toEqual({
      text: 'OFFLINE',
      color: 'var(--wavis-text-secondary)',
    });
  });

  it('shows LIVE once both signaling and media are connected', () => {
    expect(combinedStatusBadge('active', 'connected')).toEqual({
      text: 'LIVE',
      color: 'var(--wavis-accent)',
    });
  });

  it('shows CONNECTING while joining', () => {
    expect(combinedStatusBadge('joining', 'disconnected')).toEqual({
      text: 'CONNECTING',
      color: 'var(--wavis-warn)',
    });
  });

  it('shows FAILED when media fails, even if signaling is active', () => {
    expect(combinedStatusBadge('active', 'failed')).toEqual({
      text: 'FAILED',
      color: 'var(--wavis-danger)',
    });
  });

  it('shows RECONNECTING when media is reconnecting', () => {
    expect(combinedStatusBadge('active', 'reconnecting')).toEqual({
      text: 'RECONNECTING',
      color: 'var(--wavis-warn)',
    });
  });
});
