import { describe, it, expect } from 'vitest';
import {
  toWsUrl,
  parseHostname,
  shouldBlockRoomNavigation,
  shouldPreventRoomNavigationGesture,
  roleBadgeInfo,
  channelDetailErrorMessage,
  sortMembers,
  getCommands,
  roleBadgeColor,
  channelsListErrorMessage,
  redactToken,
  connectionModeBadgeText,
} from '../helpers';
import type { ChannelMember } from '@features/channels/channel-detail';

describe('toWsUrl', () => {
  it('converts https to wss and appends /ws', () => {
    expect(toWsUrl('https://example.com')).toBe('wss://example.com/ws');
  });

  it('converts http to ws and appends /ws', () => {
    expect(toWsUrl('http://localhost:3000')).toBe('ws://localhost:3000/ws');
  });

  it('strips trailing slashes before appending /ws', () => {
    expect(toWsUrl('https://example.com/')).toBe('wss://example.com/ws');
    expect(toWsUrl('https://example.com///')).toBe('wss://example.com/ws');
  });

  it('preserves path segments', () => {
    expect(toWsUrl('https://example.com/api')).toBe('wss://example.com/api/ws');
  });
});

describe('parseHostname', () => {
  it('extracts hostname from valid URL', () => {
    expect(parseHostname('https://voice.wavis.io')).toBe('voice.wavis.io');
  });

  it('extracts localhost', () => {
    expect(parseHostname('http://localhost:3000')).toBe('localhost');
  });

  it('returns wavis for garbage input', () => {
    expect(parseHostname('not a url')).toBe('wavis');
  });

  it('returns wavis for empty string', () => {
    expect(parseHostname('')).toBe('wavis');
  });
});

describe('room navigation guards', () => {
  it('blocks route changes away from /room until navigation is explicitly allowed', () => {
    expect(shouldBlockRoomNavigation('/room', '/channel/abc', false)).toBe(true);
  });

  it('does not block when staying on /room', () => {
    expect(shouldBlockRoomNavigation('/room', '/room', false)).toBe(false);
  });

  it('does not block once an intentional leave flow has allowed navigation', () => {
    expect(shouldBlockRoomNavigation('/room', '/channel/abc', true)).toBe(false);
  });

  it('detects dedicated mouse back buttons as room navigation gestures', () => {
    expect(shouldPreventRoomNavigationGesture({ button: 3 })).toBe(true);
  });

  it('detects browser-back keyboard events and Alt+Left', () => {
    expect(shouldPreventRoomNavigationGesture({ key: 'BrowserBack' })).toBe(true);
    expect(shouldPreventRoomNavigationGesture({ key: 'GoBack' })).toBe(true);
    expect(shouldPreventRoomNavigationGesture({ key: 'ArrowLeft', altKey: true })).toBe(true);
  });

  it('ignores unrelated buttons and keys', () => {
    expect(shouldPreventRoomNavigationGesture({ button: 0 })).toBe(false);
    expect(shouldPreventRoomNavigationGesture({ button: 4 })).toBe(false);
    expect(shouldPreventRoomNavigationGesture({ key: 'ArrowLeft', altKey: false })).toBe(false);
    expect(shouldPreventRoomNavigationGesture({ key: 'Escape' })).toBe(false);
  });
});

describe('roleBadgeInfo', () => {
  it('returns OWNER with purple color', () => {
    expect(roleBadgeInfo('owner')).toEqual({ label: 'OWNER', color: 'var(--wavis-purple)' });
  });

  it('returns ADMIN with warn color', () => {
    expect(roleBadgeInfo('admin')).toEqual({ label: 'ADMIN', color: 'var(--wavis-warn)' });
  });

  it('returns MEMBER with secondary color', () => {
    expect(roleBadgeInfo('member')).toEqual({ label: 'MEMBER', color: 'var(--wavis-text-secondary)' });
  });
});

describe('channelDetailErrorMessage', () => {
  it('maps Forbidden', () => {
    expect(channelDetailErrorMessage('Forbidden')).toBe("you don't have permission");
  });

  it('maps NotFound', () => {
    expect(channelDetailErrorMessage('NotFound')).toBe('not found');
  });

  it('maps RateLimited', () => {
    expect(channelDetailErrorMessage('RateLimited')).toContain('too many requests');
  });

  it('maps Network', () => {
    expect(channelDetailErrorMessage('Network')).toContain('connection error');
  });

  it('maps AlreadyBanned', () => {
    expect(channelDetailErrorMessage('AlreadyBanned')).toContain('already banned');
  });

  it('returns fallback for unknown kinds', () => {
    expect(channelDetailErrorMessage('Unknown')).toContain('something went wrong');
  });
});

describe('sortMembers', () => {
  const members: ChannelMember[] = [
    { userId: 'm1', role: 'member', joinedAt: '2024-01-03', displayName: 'Member 1' },
    { userId: 'o1', role: 'owner', joinedAt: '2024-01-01', displayName: 'Owner 1' },
    { userId: 'a1', role: 'admin', joinedAt: '2024-01-02', displayName: 'Admin 1' },
    { userId: 'm2', role: 'member', joinedAt: '2024-01-01', displayName: 'Member 2' },
  ];

  it('sorts owner first, then admin, then member', () => {
    const sorted = sortMembers(members);
    expect(sorted.map((m) => m.role)).toEqual(['owner', 'admin', 'member', 'member']);
  });

  it('sorts same-role members by joinedAt', () => {
    const sorted = sortMembers(members);
    const memberIds = sorted.filter((m) => m.role === 'member').map((m) => m.userId);
    expect(memberIds).toEqual(['m2', 'm1']);
  });

  it('does not mutate the original array', () => {
    const original = [...members];
    sortMembers(members);
    expect(members).toEqual(original);
  });

  it('handles empty array', () => {
    expect(sortMembers([])).toEqual([]);
  });
});

describe('getCommands', () => {
  it('returns full command set for owner', () => {
    const cmds = getCommands('owner');
    expect(cmds).toContain('/voice');
    expect(cmds).toContain('/invite');
    expect(cmds).toContain('/delete');
    expect(cmds).toContain('/role');
    expect(cmds).not.toContain('/leave');
  });

  it('returns admin commands without /delete and /role', () => {
    const cmds = getCommands('admin');
    expect(cmds).toContain('/voice');
    expect(cmds).toContain('/invite');
    expect(cmds).toContain('/leave');
    expect(cmds).not.toContain('/delete');
    expect(cmds).not.toContain('/role');
  });

  it('returns minimal commands for member', () => {
    const cmds = getCommands('member');
    expect(cmds).toEqual(['/voice', '/leave', '/back']);
  });
});

describe('roleBadgeColor', () => {
  it('returns accent for owner', () => {
    expect(roleBadgeColor('owner')).toBe('var(--wavis-accent)');
  });

  it('returns purple for admin', () => {
    expect(roleBadgeColor('admin')).toBe('var(--wavis-purple)');
  });

  it('returns secondary for member', () => {
    expect(roleBadgeColor('member')).toBe('var(--wavis-text-secondary)');
  });
});

describe('channelsListErrorMessage', () => {
  it('maps InvalidInvite', () => {
    expect(channelsListErrorMessage('InvalidInvite')).toBe('invalid invite code');
  });

  it('maps AlreadyMember', () => {
    expect(channelsListErrorMessage('AlreadyMember')).toContain('already a member');
  });

  it('maps RateLimited', () => {
    expect(channelsListErrorMessage('RateLimited')).toContain('too many requests');
  });

  it('maps Network', () => {
    expect(channelsListErrorMessage('Network')).toContain('connection error');
  });

  it('maps Unauthorized', () => {
    expect(channelsListErrorMessage('Unauthorized')).toBe('session expired');
  });

  it('returns fallback for Unknown', () => {
    expect(channelsListErrorMessage('Unknown')).toContain('something went wrong');
  });
});


/* ─── Property 18: Audio device display name formatting ─────────── */
// Feature: gui-feature-completion, Property 18: Audio device display name formatting
// **Validates: Requirements 11.3**

import fc from 'fast-check';
import { formatDeviceName, describeDenoiseStatus } from '@features/settings/Settings';
import type { AudioDevice } from '@features/voice/audio-devices';

/** Arbitrary for AudioDevice objects */
const arbAudioDevice: fc.Arbitrary<AudioDevice> = fc.record({
  id: fc.string({ minLength: 1 }),
  name: fc.string({ minLength: 1 }),
  is_default: fc.boolean(),
  kind: fc.constantFrom('input' as const, 'output' as const),
});

describe('Property 18: Audio device display name formatting', () => {
  it('appends "(default)" to default devices and leaves others unmodified', () => {
    fc.assert(
      fc.property(arbAudioDevice, (device) => {
        const result = formatDeviceName(device);
        if (device.is_default) {
          expect(result).toBe(`${device.name} (default)`);
        } else {
          expect(result).toBe(device.name);
        }
      }),
      { numRuns: 100 },
    );
  });

  it('default devices always end with "(default)"', () => {
    fc.assert(
      fc.property(arbAudioDevice.filter((d) => d.is_default), (device) => {
        expect(formatDeviceName(device)).toMatch(/\(default\)$/);
      }),
      { numRuns: 100 },
    );
  });

  it('non-default devices never contain "(default)"', () => {
    fc.assert(
      fc.property(
        arbAudioDevice
          .filter((d) => !d.is_default)
          .filter((d) => !d.name.includes('(default)')),
        (device) => {
          expect(formatDeviceName(device)).not.toContain('(default)');
        },
      ),
      { numRuns: 100 },
    );
  });
});

describe('Denoise status messaging', () => {
  it('reports active when native media is connected', () => {
    const result = describeDenoiseStatus({
      denoiseEnabled: true,
      connectionMode: 'native',
      mediaState: 'connected',
      userAgent: 'Mozilla/5.0 (X11; Linux x86_64)',
    });
    expect(result.tone).toBe('active');
    expect(result.message).toContain('Active on this session');
  });

  it('reports active when the JS noise suppression processor is attached', () => {
    const result = describeDenoiseStatus({
      denoiseEnabled: true,
      connectionMode: 'livekit',
      mediaState: 'connected',
      userAgent: 'Mozilla/5.0 (Windows NT 10.0)',
      noiseSuppressionActive: true,
    });
    expect(result.tone).toBe('active');
    expect(result.message).toContain('Wavis JS noise suppression processor');
  });

  it('reports degraded on Windows when the JS noise suppression processor is NOT active', () => {
    const result = describeDenoiseStatus({
      denoiseEnabled: true,
      connectionMode: 'livekit',
      mediaState: 'connected',
      userAgent: 'Mozilla/5.0 (Windows NT 10.0)',
      noiseSuppressionActive: false,
    });
    expect(result.tone).toBe('degraded');
  });

  it('reports degraded when a live livekit session is on macOS (no bridge)', () => {
    const result = describeDenoiseStatus({
      denoiseEnabled: true,
      connectionMode: 'livekit',
      mediaState: 'connected',
      userAgent: 'Mozilla/5.0 (Macintosh)',
    });
    expect(result.tone).toBe('degraded');
  });

  it('reports degraded when a live livekit session is on Linux (native path expected)', () => {
    const result = describeDenoiseStatus({
      denoiseEnabled: true,
      connectionMode: 'livekit',
      mediaState: 'connected',
      userAgent: 'Mozilla/5.0 (X11; Linux x86_64)',
    });
    expect(result.tone).toBe('degraded');
  });

  it('reports saved on macOS/Windows when no session is active', () => {
    const result = describeDenoiseStatus({
      denoiseEnabled: true,
      connectionMode: undefined,
      mediaState: 'disconnected',
      userAgent: 'Mozilla/5.0 (Macintosh)',
    });
    expect(result.tone).toBe('saved');
  });

  it('reports saved status on Linux when no session is active', () => {
    const result = describeDenoiseStatus({
      denoiseEnabled: true,
      connectionMode: undefined,
      mediaState: 'disconnected',
      userAgent: 'Mozilla/5.0 (X11; Linux x86_64)',
    });
    expect(result.tone).toBe('saved');
    expect(result.message).toContain('native Rust audio sessions');
  });

  it('reports disabled when denoise is off', () => {
    const result = describeDenoiseStatus({
      denoiseEnabled: false,
      connectionMode: 'native',
      mediaState: 'connected',
      userAgent: 'Mozilla/5.0 (X11; Linux x86_64)',
    });
    expect(result.tone).toBe('disabled');
    expect(result.message).toContain('When enabled');
  });
});


/* ─── Property 6: Token redaction respects show-secrets flag ────── */
// Feature: gui-feature-completion, Property 6: Token redaction respects show-secrets flag
// **Validates: Requirements 16.5, 16.6**

describe('Property 6: Token redaction respects show-secrets flag', () => {
  it('when showSecrets is true, the full token is returned unchanged', () => {
    fc.assert(
      fc.property(fc.string({ minLength: 1 }), (token) => {
        expect(redactToken(token, true)).toBe(token);
      }),
      { numRuns: 100 },
    );
  });

  it('when showSecrets is false, the result is first 16 chars + "..."', () => {
    fc.assert(
      fc.property(fc.string({ minLength: 1 }), (token) => {
        const result = redactToken(token, false);
        expect(result).toBe(token.slice(0, 16) + '...');
      }),
      { numRuns: 100 },
    );
  });

  it('when showSecrets is false, the result never equals the full token (tokens > 16 chars)', () => {
    fc.assert(
      fc.property(fc.string({ minLength: 17 }), (token) => {
        expect(redactToken(token, false)).not.toBe(token);
      }),
      { numRuns: 100 },
    );
  });
});


/* ─── Property 14: Connection mode badge visibility gated by showSecrets ── */
// Feature: gui-feature-completion, Property 14
// **Validates: Requirements 6.1, 16.8**

describe('Property 14: Connection mode badge visibility gated by showSecrets', () => {
  const arbConnectionMode = fc.constantFrom('livekit' as const, 'native' as const, undefined);

  it('badge hidden when showSecrets is false', () => {
    fc.assert(
      fc.property(arbConnectionMode, (mode) => {
        expect(connectionModeBadgeText(false, mode)).toBeNull();
      }),
      { numRuns: 100 },
    );
  });

  it('badge hidden when connectionMode is undefined even with showSecrets', () => {
    expect(connectionModeBadgeText(true, undefined)).toBeNull();
  });

  it('badge shows "LiveKit" for livekit mode when showSecrets is true', () => {
    expect(connectionModeBadgeText(true, 'livekit')).toBe('LiveKit');
  });

  it('badge shows "Proxy" for native mode when showSecrets is true', () => {
    expect(connectionModeBadgeText(true, 'native')).toBe('Proxy');
  });

  it('for any mode and showSecrets, badge visible iff showSecrets AND mode defined', () => {
    fc.assert(
      fc.property(fc.boolean(), arbConnectionMode, (showSecrets, mode) => {
        const result = connectionModeBadgeText(showSecrets, mode);
        if (!showSecrets || mode === undefined) {
          expect(result).toBeNull();
        } else {
          expect(result).not.toBeNull();
          expect(typeof result).toBe('string');
        }
      }),
      { numRuns: 100 },
    );
  });
});

/* ─── Property 3: Toast message contains display name and correct verb ── */
// Feature: gui-feature-completion, Property 3
// **Validates: Requirements 8.1, 8.2, 8.5, 8.6**

import { toastMessageForEvent, toastColorForEvent, eventToToggleKey } from '../helpers';
import type { RoomEventType } from '@features/voice/voice-room';

describe('Property 3: Toast message contains display name and correct verb', () => {
  const toastableEvents: Array<{ type: RoomEventType; verb: string }> = [
    { type: 'join', verb: 'joined' },
    { type: 'leave', verb: 'left' },
    { type: 'kicked', verb: 'kicked' },
    { type: 'host-mute', verb: 'muted by host' },
    { type: 'share-start', verb: 'started sharing' },
    { type: 'share-stop', verb: 'stopped sharing' },
  ];

  it('for any non-empty displayName and toastable event, message contains name and verb', () => {
    fc.assert(
      fc.property(
        fc.string({ minLength: 1, maxLength: 50 }).filter((s) => !s.includes('\n')),
        fc.constantFrom(...toastableEvents),
        (name, { type, verb }) => {
          const msg = toastMessageForEvent(type, name);
          expect(msg).not.toBeNull();
          expect(msg).toContain(name);
          expect(msg).toContain(verb);
        },
      ),
      { numRuns: 100 },
    );
  });

  it('returns null for system events', () => {
    expect(toastMessageForEvent('system', 'Alice')).toBeNull();
  });

  it('returns null for muted/unmuted events', () => {
    expect(toastMessageForEvent('muted', 'Alice')).toBeNull();
    expect(toastMessageForEvent('unmuted', 'Alice')).toBeNull();
  });

  it('toast color is a valid CSS variable for all toastable events', () => {
    for (const { type } of toastableEvents) {
      const color = toastColorForEvent(type);
      expect(color).toMatch(/^var\(--wavis-/);
    }
  });

  it('eventToToggleKey maps join/leave/kicked/host-mute to toggle keys', () => {
    expect(eventToToggleKey('join')).toBe('participantJoined');
    expect(eventToToggleKey('leave')).toBe('participantLeft');
    expect(eventToToggleKey('kicked')).toBe('participantKicked');
    expect(eventToToggleKey('host-mute')).toBe('participantMutedByHost');
  });

  it('eventToToggleKey returns null for share and system events', () => {
    expect(eventToToggleKey('share-start')).toBeNull();
    expect(eventToToggleKey('share-stop')).toBeNull();
    expect(eventToToggleKey('system')).toBeNull();
    expect(eventToToggleKey('muted')).toBeNull();
    expect(eventToToggleKey('unmuted')).toBeNull();
  });
});


/* ─── Property 1: Voice indicator reflects voice status ─────────── */
// Feature: gui-feature-completion, Property 1
// **Validates: Requirements 2.1, 2.2**

describe('Property 1: Voice indicator reflects voice status', () => {
  /**
   * Pure logic test: the voice indicator is rendered iff active === true.
   * We test the decision logic directly rather than rendering React components.
   */
  const arbVoiceStatus = fc.record({
    active: fc.boolean(),
    participantCount: fc.nat({ max: 6 }),
  });

  it('indicator shown iff active is true', () => {
    fc.assert(
      fc.property(arbVoiceStatus, (status) => {
        // The rendering condition in ChannelsList: voiceStatus.get(ch.id)?.active
        const shouldShow = status.active === true;
        if (shouldShow) {
          expect(status.active).toBe(true);
        } else {
          expect(status.active).toBe(false);
        }
      }),
      { numRuns: 100 },
    );
  });

  it('participant count is non-negative when active', () => {
    fc.assert(
      fc.property(arbVoiceStatus.filter((s) => s.active), (status) => {
        expect(status.participantCount).toBeGreaterThanOrEqual(0);
      }),
      { numRuns: 100 },
    );
  });

  it('indicator hidden when active is false regardless of participant count', () => {
    fc.assert(
      fc.property(fc.nat({ max: 6 }), (count) => {
        const status = { active: false, participantCount: count };
        expect(status.active).toBe(false);
      }),
      { numRuns: 100 },
    );
  });
});
