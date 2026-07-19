import { describe, it, expect } from 'vitest';
import { validateStep1, validateInviteCode } from '../DeviceSetup';

describe('validateStep1', () => {
  it('rejects mismatched passwords', () => {
    const result = validateStep1('user', 'password', 'different');

    expect(result.valid).toBe(false);
    expect(result.errors.confirmPhrase).toBe('Passwords must match');
  });

  it('accepts a valid username and matching passwords', () => {
    const result = validateStep1('user', 'password', 'password');

    expect(result.valid).toBe(true);
    expect(result.errors).toEqual({});
  });

  it('rejects usernames longer than 64 characters', () => {
    const result = validateStep1('a'.repeat(65), 'password', 'password');

    expect(result.valid).toBe(false);
    expect(result.errors.username).toBe('username must be 64 characters or less');
  });
});

describe('validateInviteCode', () => {
  it('rejects an empty invite code', () => {
    expect(validateInviteCode('')).toBe('Invite code is required');
  });

  it('rejects a whitespace-only invite code', () => {
    expect(validateInviteCode('   ')).toBe('Invite code is required');
  });

  it('accepts a non-empty invite code', () => {
    expect(validateInviteCode('ALPHA-CODE-1')).toBeNull();
  });
});
