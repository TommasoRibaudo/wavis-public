import { describe, it, expect } from 'vitest';
import { validateStep1 } from '../DeviceSetup';

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
