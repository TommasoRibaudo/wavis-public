import { describe, it, expect } from 'vitest';
import { normalizeChannelName } from '../channels';

describe('normalizeChannelName', () => {
  it('lowercases and replaces spaces with hyphens', () => {
    expect(normalizeChannelName('My Channel')).toBe('my-channel');
  });

  it('replaces special characters with hyphens', () => {
    expect(normalizeChannelName('test@#$channel')).toBe('test-channel');
  });

  it('collapses consecutive hyphens', () => {
    expect(normalizeChannelName('a---b')).toBe('a-b');
  });

  it('trims leading and trailing hyphens', () => {
    expect(normalizeChannelName('--hello--')).toBe('hello');
  });

  it('returns empty string for empty input', () => {
    expect(normalizeChannelName('')).toBe('');
  });

  it('passes through already-clean names', () => {
    expect(normalizeChannelName('clean-name-123')).toBe('clean-name-123');
  });

  it('handles all-special-character input', () => {
    expect(normalizeChannelName('!@#$%')).toBe('');
  });

  it('handles unicode characters', () => {
    expect(normalizeChannelName('café-résumé')).toBe('caf-r-sum');
  });
});
