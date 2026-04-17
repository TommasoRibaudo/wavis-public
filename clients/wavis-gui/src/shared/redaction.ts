/**
 * Wavis Client-Side Text Redaction Engine
 *
 * Pure functions that strip sensitive patterns (JWTs, API keys, IPs,
 * privacy-sensitive UUIDs, invite codes, recovery phrases) from text
 * before it leaves the client. Operational UUIDs are preserved.
 */

// ─── Types ─────────────────────────────────────────────────────────

type ContextualRedactionRule = {
  pattern: RegExp;
  replacement: string;
};

type LiteralRedactionRule = {
  pattern: RegExp;
  replacement: string;
};

// ─── Constants ─────────────────────────────────────────────────────

const JWT_PATTERN =
  /(?<![A-Za-z0-9_-])eyJ[A-Za-z0-9_-]*\.[A-Za-z0-9_-]+\.[A-Za-z0-9_-]+(?![A-Za-z0-9_-])/g;

const API_KEY_PATTERN =
  /(?<![A-Za-z0-9_-])(?:sk-ant-api03-[A-Za-z0-9_-]{10,}|sk-[A-Za-z0-9_-]{10,})(?![A-Za-z0-9_-])/g;

const UUID_VALUE_PATTERN =
  '[0-9a-fA-F]{8}-[0-9a-fA-F]{4}-[0-9a-fA-F]{4}-[0-9a-fA-F]{4}-[0-9a-fA-F]{12}';

const PHRASE_VALUE_PATTERN =
  '[A-Za-z]{2,}(?:[ -][A-Za-z]{2,}){2,23}';

const INVITE_VALUE_PATTERN = '[A-Za-z0-9_-]{22}';

const IPV4_PATTERN =
  /(?<![\d.])(?:25[0-5]|2[0-4]\d|1\d\d|[1-9]?\d)(?:\.(?:25[0-5]|2[0-4]\d|1\d\d|[1-9]?\d)){3}(?![\d.])/g;

const IPV6_PATTERN =
  /(?<![A-Fa-f0-9:])(?:(?:[A-Fa-f0-9]{1,4}:){7}[A-Fa-f0-9]{1,4}|(?:[A-Fa-f0-9]{1,4}:){1,7}:|(?:[A-Fa-f0-9]{1,4}:){1,6}:[A-Fa-f0-9]{1,4}|(?:[A-Fa-f0-9]{1,4}:){1,5}(?::[A-Fa-f0-9]{1,4}){1,2}|(?:[A-Fa-f0-9]{1,4}:){1,4}(?::[A-Fa-f0-9]{1,4}){1,3}|(?:[A-Fa-f0-9]{1,4}:){1,3}(?::[A-Fa-f0-9]{1,4}){1,4}|(?:[A-Fa-f0-9]{1,4}:){1,2}(?::[A-Fa-f0-9]{1,4}){1,5}|[A-Fa-f0-9]{1,4}:(?:(?::[A-Fa-f0-9]{1,4}){1,6})|:(?:(?::[A-Fa-f0-9]{1,4}){1,7}|:))(?![A-Fa-f0-9:])/g;

const PHRASE_CONTEXTS = [
  'phrase',
  'recovery_phrase',
  'recovery phrase',
  'recoveryPhrase',
  'seed_phrase',
  'seed phrase',
  'seedPhrase',
  'mnemonic',
  'passphrase',
  'current_phrase',
  'current phrase',
  'currentPhrase',
  'new_phrase',
  'new phrase',
  'newPhrase',
].join('|');

const PRIVACY_UUID_CONTEXTS = [
  'user_id',
  'userId',
  'device_id',
  'deviceId',
  'recovery_id',
  'recoveryId',
].join('|');

const INVITE_CONTEXTS = [
  'invite_code',
  'inviteCode',
  'invite code',
  'invite-code',
  'join_code',
  'joinCode',
  'join code',
  'code',
].join('|');

// ─── Helpers (private) ─────────────────────────────────────────────

const contextualPattern = (contexts: string, valuePattern: string): RegExp =>
  new RegExp(
    `((?:"(?:${contexts})"|(?:${contexts}))\\s*[:=]\\s*["']?)(${valuePattern})(["']?)`,
    'gi',
  );

const CONTEXTUAL_RULES: ContextualRedactionRule[] = [
  {
    pattern: contextualPattern(PHRASE_CONTEXTS, PHRASE_VALUE_PATTERN),
    replacement: '[REDACTED_PHRASE]',
  },
  {
    pattern: contextualPattern(PRIVACY_UUID_CONTEXTS, UUID_VALUE_PATTERN),
    replacement: '[REDACTED_UUID]',
  },
  {
    pattern: contextualPattern(INVITE_CONTEXTS, INVITE_VALUE_PATTERN),
    replacement: '[REDACTED_INVITE]',
  },
];

const LITERAL_RULES: LiteralRedactionRule[] = [
  { pattern: JWT_PATTERN, replacement: '[REDACTED_TOKEN]' },
  { pattern: API_KEY_PATTERN, replacement: '[REDACTED_API_KEY]' },
  { pattern: IPV4_PATTERN, replacement: '[REDACTED_IP]' },
  { pattern: IPV6_PATTERN, replacement: '[REDACTED_IP]' },
];

function applyLiteralRule(
  text: string,
  rule: LiteralRedactionRule,
): string {
  return text.replace(rule.pattern, rule.replacement);
}

function applyContextualRule(
  text: string,
  rule: ContextualRedactionRule,
): string {
  return text.replace(
    rule.pattern,
    (_match: string, prefix: string, _value: string, suffix?: string) =>
      `${prefix}${rule.replacement}${suffix ?? ''}`,
  );
}

// ─── API Functions (exported) ──────────────────────────────────────

export function redactText(input: string): string {
  // Rule order: contextual phrase runs before literal API key to prevent
  // recovery phrases like "sk-word-word" from being swallowed by API_KEY_PATTERN.
  // JWT -> recovery phrase -> API key -> privacy UUID -> invite -> IPv4 -> IPv6
  let redacted = input;

  redacted = applyLiteralRule(redacted, LITERAL_RULES[0]);
  redacted = applyContextualRule(redacted, CONTEXTUAL_RULES[0]);
  redacted = applyLiteralRule(redacted, LITERAL_RULES[1]);
  redacted = applyContextualRule(redacted, CONTEXTUAL_RULES[1]);
  redacted = applyContextualRule(redacted, CONTEXTUAL_RULES[2]);
  redacted = applyLiteralRule(redacted, LITERAL_RULES[2]);
  redacted = applyLiteralRule(redacted, LITERAL_RULES[3]);

  return redacted;
}

export function redactAll(texts: string[]): string[] {
  return texts.map((text) => redactText(text));
}
