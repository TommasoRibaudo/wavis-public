import { describe, it, expect } from 'vitest';
import { isShareEnabled, shareButtonLabel } from '../voice-room';

describe('isShareEnabled', () => {
  it('returns true for non-host when permission is anyone, room active, media connected', () => {
    expect(isShareEnabled('anyone', false, 'active', 'connected')).toBe(true);
  });

  it('returns false for non-host when permission is host_only', () => {
    expect(isShareEnabled('host_only', false, 'active', 'connected')).toBe(false);
  });

  it('returns true for host even when permission is host_only', () => {
    expect(isShareEnabled('host_only', true, 'active', 'connected')).toBe(true);
  });

  it('returns false when machineState is reconnecting, even with anyone permission', () => {
    expect(isShareEnabled('anyone', false, 'reconnecting', 'connected')).toBe(false);
  });

  it('returns false when machineState is connecting', () => {
    expect(isShareEnabled('anyone', false, 'connecting', 'connected')).toBe(false);
  });

  it('returns false when mediaState is not connected', () => {
    expect(isShareEnabled('anyone', false, 'active', 'connecting')).toBe(false);
    expect(isShareEnabled('anyone', false, 'active', 'disconnected')).toBe(false);
  });

  it('returns false for host when machineState is not active', () => {
    expect(isShareEnabled('anyone', true, 'reconnecting', 'connected')).toBe(false);
  });
});

describe('shareButtonLabel', () => {
  it('shows /stopshare when self is sharing', () => {
    expect(shareButtonLabel(true, true, 'anyone', false)).toBe('/stopshare');
    expect(shareButtonLabel(false, true, 'host_only', false)).toBe('/stopshare');
  });

  it('shows /share when sharing is enabled and not self-sharing', () => {
    expect(shareButtonLabel(true, false, 'anyone', false)).toBe('/share');
    expect(shareButtonLabel(true, false, 'host_only', true)).toBe('/share');
  });

  it('shows /share (host only) only when permission is host_only and user is not host', () => {
    expect(shareButtonLabel(false, false, 'host_only', false)).toBe('/share (host only)');
  });

  it('does NOT show host only when reconnecting with anyone permission — proves the bug', () => {
    // Before Fix 1: this returned '/share (host only)' because !shareEnabled was sufficient.
    // After Fix 1: must return '/share' because permission is 'anyone'.
    expect(shareButtonLabel(false, false, 'anyone', false)).toBe('/share');
  });

  it('does NOT show host only when machineState gates it but permission is anyone', () => {
    // shareEnabled=false because reconnecting; permission is 'anyone' and user is not host
    expect(shareButtonLabel(false, false, 'anyone', false)).toBe('/share');
  });

  it('shows /share (not host only) when host is disabled for non-permission reasons', () => {
    // Host sees plain /share when disabled (e.g. reconnecting) — not "host only"
    expect(shareButtonLabel(false, false, 'host_only', true)).toBe('/share');
  });
});
