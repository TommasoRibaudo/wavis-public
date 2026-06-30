import { describe, expect, it } from 'vitest';
import { quickActionMuteAriaLabel, quickActionMuteTitle } from '../QuickActionButtons';

describe('QuickActionButtons mute state', () => {
  it('renders the no-room default unmuted intent as /mute', () => {
    expect(quickActionMuteTitle(false)).toBe('/mute');
    expect(quickActionMuteAriaLabel(false)).toBe('Mute yourself');
  });

  it('renders muted intent as /unmute', () => {
    expect(quickActionMuteTitle(true)).toBe('/unmute');
    expect(quickActionMuteAriaLabel(true)).toBe('Unmute yourself');
  });
});
