import { describe, expect, it } from 'vitest';
import fc from 'fast-check';

import { redactAll, redactText } from '../redaction';

const base64UrlChars = [
  ...'ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789-_',
];
const lowerAlphaChars = [...'abcdefghijklmnopqrstuvwxyz'];
const hexChars = [...'0123456789abcdefABCDEF'];
const safeTextChars = [...'ABCDEFGHILMNOPQRTUVWXYZ abcdefghilmnopqrtuvwxyz'];
const operationalContexts = ['room_id', 'channel_id', 'peer_id', 'message_id'] as const;

const base64UrlChunkArb = fc
  .array(fc.constantFrom(...base64UrlChars), { minLength: 10, maxLength: 32 })
  .map((chars) => chars.join(''));

const inviteCodeArb = fc
  .array(fc.constantFrom(...base64UrlChars), { minLength: 22, maxLength: 22 })
  .map((chars) => chars.join(''));

const lowerWordArb = fc
  .array(fc.constantFrom(...lowerAlphaChars), { minLength: 2, maxLength: 8 })
  .map((chars) => chars.join(''));

const recoveryPhraseArb = fc
  .tuple(
    fc.array(lowerWordArb, { minLength: 3, maxLength: 8 }),
    fc.constantFrom(' ', '-'),
  )
  .map(([words, separator]) => words.join(separator));

const jwtArb = fc
  .tuple(base64UrlChunkArb, base64UrlChunkArb, base64UrlChunkArb)
  .map(([headerTail, payload, signature]) => `eyJ${headerTail}.${payload}.${signature}`);

const apiKeyArb = fc.oneof(
  base64UrlChunkArb.map((suffix) => `sk-${suffix}`),
  base64UrlChunkArb.map((suffix) => `sk-ant-api03-${suffix}`),
);

const ipv4Arb = fc
  .tuple(
    fc.integer({ min: 0, max: 255 }),
    fc.integer({ min: 0, max: 255 }),
    fc.integer({ min: 0, max: 255 }),
    fc.integer({ min: 0, max: 255 }),
  )
  .map((parts) => parts.join('.'));

const ipv6GroupArb = fc
  .array(fc.constantFrom(...hexChars), { minLength: 1, maxLength: 4 })
  .map((chars) => chars.join(''));

const ipv6Arb = fc
  .tuple(
    ipv6GroupArb,
    ipv6GroupArb,
    ipv6GroupArb,
    ipv6GroupArb,
    ipv6GroupArb,
    ipv6GroupArb,
    ipv6GroupArb,
    ipv6GroupArb,
  )
  .map((groups) => groups.join(':'));

const nonSensitiveTextArb = fc
  .array(fc.constantFrom(...safeTextChars), { minLength: 0, maxLength: 200 })
  .map((chars) => chars.join(''));

function countOccurrences(text: string, needle: string): number {
  return text.split(needle).length - 1;
}

describe('redaction', () => {
  it('redacts each sensitive category in the expected placeholders', () => {
    const input = [
      'jwt=eyJhbGciOiJIUzI1NiJ9.eyJzdWIiOiIxMjMifQ.signatureValue',
      'key=sk-ant-api03-secretSecretSecret',
      'phrase=alpha beta gamma delta',
      'user_id=123e4567-e89b-12d3-a456-426614174000',
      'inviteCode=AbCdEfGhIjKlMnOpQrStUv',
      'ipv4=192.168.10.24',
      'ipv6=2001:db8::1',
    ].join(' | ');

    const result = redactText(input);

    expect(result).toContain('[REDACTED_TOKEN]');
    expect(result).toContain('[REDACTED_API_KEY]');
    expect(result).toContain('[REDACTED_PHRASE]');
    expect(result).toContain('[REDACTED_UUID]');
    expect(result).toContain('[REDACTED_INVITE]');
    expect(countOccurrences(result, '[REDACTED_IP]')).toBe(2);
  });

  it('redacts tokens and API keys that end with URL-safe punctuation', () => {
    const input = [
      'token=eyJAAAAAAAAAA.AAAAAAAAAA.AAAAAAAAA-',
      'api_key=sk-AAAAAAAAA-',
    ].join(' | ');

    const result = redactText(input);

    expect(result).toBe(
      'token=[REDACTED_TOKEN] | api_key=[REDACTED_API_KEY]',
    );
  });

  it('preserves operational UUID contexts', () => {
    const input = [
      'room_id=123e4567-e89b-12d3-a456-426614174000',
      'channel_id=123e4567-e89b-12d3-a456-426614174001',
      'peer_id=123e4567-e89b-12d3-a456-426614174002',
      'message_id=123e4567-e89b-12d3-a456-426614174003',
    ].join(' | ');

    expect(redactText(input)).toBe(input);
  });

  it('does not redact bare 22-char base64url strings without invite context', () => {
    const inviteLikeValue = 'AbCdEfGhIjKlMnOpQrStUv';
    expect(redactText(inviteLikeValue)).toBe(inviteLikeValue);
  });

  it('redactAll maps over every input string', () => {
    expect(
      redactAll([
        'invite_code=AbCdEfGhIjKlMnOpQrStUv',
        'safe text',
      ]),
    ).toEqual([
      'invite_code=[REDACTED_INVITE]',
      'safe text',
    ]);
  });
});

// Property 4: redaction removes all sensitive patterns
describe('Feature: in-app-bug-report, Property 4: Redaction removes all sensitive patterns', () => {
  /**
   * Validates that the redaction engine removes every sensitive pattern class
   * required by the client-side design without touching operational UUIDs.
   */
  it('replaces every sensitive value with its placeholder', () => {
    fc.assert(
      fc.property(
        jwtArb,
        apiKeyArb,
        recoveryPhraseArb,
        fc.uuid(),
        fc.uuid(),
        fc.uuid(),
        inviteCodeArb,
        ipv4Arb,
        ipv6Arb,
        (jwt, apiKey, phrase, userId, deviceId, recoveryId, inviteCode, ipv4, ipv6) => {
          const input = [
            `token=${jwt}`,
            `api_key=${apiKey}`,
            `recovery_phrase=${phrase}`,
            `user_id=${userId}`,
            `deviceId=${deviceId}`,
            `recovery_id=${recoveryId}`,
            `inviteCode=${inviteCode}`,
            `ipv4=${ipv4}`,
            `ipv6=${ipv6}`,
          ].join(' | ');

          const result = redactText(input);

          expect(result).not.toContain(jwt);
          expect(result).not.toContain(apiKey);
          expect(result).not.toContain(phrase);
          expect(result).not.toContain(userId);
          expect(result).not.toContain(deviceId);
          expect(result).not.toContain(recoveryId);
          expect(result).not.toContain(inviteCode);
          expect(result).not.toContain(ipv4);
          expect(result).not.toContain(ipv6);

          expect(result).toContain('[REDACTED_TOKEN]');
          expect(result).toContain('[REDACTED_API_KEY]');
          expect(result).toContain('[REDACTED_PHRASE]');
          expect(countOccurrences(result, '[REDACTED_UUID]')).toBe(3);
          expect(result).toContain('[REDACTED_INVITE]');
          expect(countOccurrences(result, '[REDACTED_IP]')).toBe(2);
        },
      ),
      { numRuns: 200 },
    );
  });
});

// Property 5: redaction preserves operational UUIDs
describe('Feature: in-app-bug-report, Property 5: Redaction preserves operational UUIDs', () => {
  /**
   * Validates that only privacy UUID contexts are redacted. Operational UUID
   * contexts must remain intact.
   */
  it('keeps room_id, channel_id, peer_id, and message_id values unchanged', () => {
    fc.assert(
      fc.property(
        fc.constantFrom(...operationalContexts),
        fc.uuid(),
        fc.uuid(),
        (operationalKey, operationalUuid, privacyUuid) => {
          const input = [
            `${operationalKey}=${operationalUuid}`,
            `user_id=${privacyUuid}`,
          ].join(' | ');

          const result = redactText(input);

          expect(result).toContain(`${operationalKey}=${operationalUuid}`);
          expect(result).not.toContain(`user_id=${privacyUuid}`);
          expect(result).toContain('user_id=[REDACTED_UUID]');
        },
      ),
      { numRuns: 200 },
    );
  });
});

// Property 6: redaction preserves non-sensitive text
describe('Feature: in-app-bug-report, Property 6: Redaction preserves non-sensitive text', () => {
  /**
   * Validates that benign text with no sensitive patterns is returned byte-for-byte.
   */
  it('returns non-sensitive text unchanged', () => {
    fc.assert(
      fc.property(nonSensitiveTextArb, (text) => {
        expect(redactText(text)).toBe(text);
      }),
      { numRuns: 200 },
    );
  });
});
