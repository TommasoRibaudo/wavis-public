/**
 * Signaling message parser.
 *
 * Strict on the `type` tag, lenient on payload fields: dispatchMessage's
 * case bodies today read fields via ad-hoc casts and tolerate missing
 * values (fallback operators, typeof guards). A field-validating parser
 * that rejected malformed known-type messages would silently drop
 * messages the current code partially handles — a behavior change. So
 * this only classifies the envelope; per-field trust level lives in the
 * `ServerMessage` variant shapes in messages.ts.
 */

import { KNOWN_SERVER_MESSAGE_TYPES, type ServerMessage } from './messages';

export type ParseResult =
  | { kind: 'known'; msg: ServerMessage }
  | { kind: 'unknown'; type: string; raw: Record<string, unknown> }
  | { kind: 'invalid' };

export function parseSignalingMessage(raw: unknown): ParseResult {
  if (typeof raw !== 'object' || raw === null) {
    return { kind: 'invalid' };
  }
  const record = raw as Record<string, unknown>;
  const type = record.type;
  if (typeof type !== 'string') {
    return { kind: 'invalid' };
  }
  if (!KNOWN_SERVER_MESSAGE_TYPES.has(type as ServerMessage['type'])) {
    return { kind: 'unknown', type, raw: record };
  }
  return { kind: 'known', msg: raw as ServerMessage };
}
