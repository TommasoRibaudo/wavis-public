import { describe, it, expect } from 'vitest';
import fc from 'fast-check';
import type { ShareMode } from '@features/screen-share/share-types';
import type { VoiceRoomState } from '../voice-room';
import { shareIndicatorForType } from '@shared/helpers';
import { isAnyShareActive, canStartShare } from '../voice-room';

/* ─── Arbitraries ───────────────────────────────────────────────── */

const arbShareMode: fc.Arbitrary<ShareMode> = fc.constantFrom(
  'screen_audio' as const,
  'window' as const,
  'audio_only' as const,
);

const arbVideoShare: fc.Arbitrary<VoiceRoomState['activeVideoShare']> = fc.oneof(
  fc.constant(null as VoiceRoomState['activeVideoShare']),
  fc.record({
    mode: fc.constantFrom('screen_audio' as const, 'window' as const),
    sourceName: fc.string({ minLength: 1, maxLength: 64 }),
    withAudio: fc.boolean(),
  }),
);

const arbAudioShare: fc.Arbitrary<VoiceRoomState['activeAudioShare']> = fc.oneof(
  fc.constant(null as VoiceRoomState['activeAudioShare']),
  fc.record({
    sourceId: fc.string({ minLength: 1, maxLength: 64 }),
    sourceName: fc.string({ minLength: 1, maxLength: 64 }),
  }),
);

/* ═══ Property 13: Share button disabled while sharing ══════════════ */

describe('Property 13: Share button disabled while sharing (two-slot)', () => {
  it('button shows stop when any share slot is occupied', () => {
    fc.assert(
      fc.property(arbVideoShare, arbAudioShare, (video, audio) => {
        const anyActive = isAnyShareActive(video, audio);
        if (video !== null || audio !== null) {
          expect(anyActive).toBe(true);
        } else {
          expect(anyActive).toBe(false);
        }
      }),
      { numRuns: 100 },
    );
  });

  it('button shows share when both slots are empty', () => {
    expect(isAnyShareActive(null, null)).toBe(false);
  });

  it('button shows stop when only video slot is occupied', () => {
    fc.assert(
      fc.property(
        arbVideoShare.filter((v) => v !== null),
        (video) => {
          expect(isAnyShareActive(video, null)).toBe(true);
        },
      ),
      { numRuns: 100 },
    );
  });

  it('button shows stop when only audio slot is occupied', () => {
    fc.assert(
      fc.property(
        arbAudioShare.filter((a) => a !== null),
        (audio) => {
          expect(isAnyShareActive(null, audio)).toBe(true);
        },
      ),
      { numRuns: 100 },
    );
  });
});

/* ═══ Property 14: Concurrent share slot independence ═══════════════ */

describe('Property 14: Concurrent share slot independence', () => {
  it('can start audio share while video share is active', () => {
    fc.assert(
      fc.property(
        arbVideoShare.filter((v) => v !== null),
        (video) => {
          const sel = { mode: 'audio_only' as const, sourceId: 'test', sourceName: 'Test', withAudio: false };
          const result = canStartShare(sel, video, null);
          expect(result.allowed).toBe(true);
        },
      ),
      { numRuns: 100 },
    );
  });

  it('can start video share while audio share is active', () => {
    fc.assert(
      fc.property(
        arbAudioShare.filter((a) => a !== null),
        (audio) => {
          const sel = { mode: 'screen_audio' as const, sourceId: 'test', sourceName: 'Test', withAudio: true };
          const result = canStartShare(sel, null, audio);
          expect(result.allowed).toBe(true);
        },
      ),
      { numRuns: 100 },
    );
  });

  it('cannot start second video share when video slot is occupied', () => {
    fc.assert(
      fc.property(
        arbVideoShare.filter((v) => v !== null),
        fc.constantFrom('screen_audio' as const, 'window' as const),
        (video, mode) => {
          const sel = { mode, sourceId: 'test', sourceName: 'Test', withAudio: false };
          const result = canStartShare(sel, video, null);
          expect(result.allowed).toBe(false);
        },
      ),
      { numRuns: 100 },
    );
  });

  it('cannot start second audio share when audio slot is occupied', () => {
    fc.assert(
      fc.property(
        arbAudioShare.filter((a) => a !== null),
        (audio) => {
          const sel = { mode: 'audio_only' as const, sourceId: 'test', sourceName: 'Test', withAudio: false };
          const result = canStartShare(sel, null, audio);
          expect(result.allowed).toBe(false);
        },
      ),
      { numRuns: 100 },
    );
  });
});

/* ═══ Property 16: shareType backward compatibility ═════════════════ */

describe('Property 16: shareType backward compatibility', () => {
  it('undefined shareType defaults to screen share indicator', () => {
    const indicator = shareIndicatorForType(undefined);
    expect(indicator.char).toBe('▲');
    expect(indicator.label).toBe('sharing screen');
  });

  it('unrecognized shareType defaults to screen share indicator', () => {
    fc.assert(
      fc.property(
        fc.string({ minLength: 1, maxLength: 32 }).filter(
          (s) => !['screen_audio', 'window', 'audio_only'].includes(s),
        ),
        (unknownType) => {
          const indicator = shareIndicatorForType(unknownType);
          expect(indicator.char).toBe('▲');
          expect(indicator.label).toBe('sharing screen');
        },
      ),
      { numRuns: 100 },
    );
  });

  it('valid shareType round-trips through JSON serialization', () => {
    fc.assert(
      fc.property(arbShareMode, (mode) => {
        const serialized = JSON.stringify({ shareType: mode });
        const deserialized = JSON.parse(serialized) as { shareType: string };
        expect(deserialized.shareType).toBe(mode);
        const indicator = shareIndicatorForType(deserialized.shareType);
        const directIndicator = shareIndicatorForType(mode);
        expect(indicator).toEqual(directIndicator);
      }),
      { numRuns: 100 },
    );
  });

  it('missing shareType in deserialized message produces default', () => {
    fc.assert(
      fc.property(fc.string({ minLength: 1, maxLength: 32 }), (type) => {
        const msg = JSON.parse(JSON.stringify({ type })) as { type: string; shareType?: string };
        const indicator = shareIndicatorForType(msg.shareType);
        expect(indicator.char).toBe('▲');
        expect(indicator.label).toBe('sharing screen');
      }),
      { numRuns: 100 },
    );
  });
});

/* ═══ Property 17: Contextual share indicator per participant ═══════ */

describe('Property 17: Contextual share indicator per participant', () => {
  it('audio_only always shows music note indicator', () => {
    const indicator = shareIndicatorForType('audio_only');
    expect(indicator.char).toBe('🎵');
    expect(indicator.label).toContain('audio');
  });

  it('screen_audio and window show triangle indicator', () => {
    fc.assert(
      fc.property(
        fc.constantFrom('screen_audio' as const, 'window' as const),
        (mode) => {
          const indicator = shareIndicatorForType(mode);
          expect(indicator.char).toBe('▲');
        },
      ),
      { numRuns: 100 },
    );
  });

  it('indicator type is deterministic for any given shareType', () => {
    fc.assert(
      fc.property(
        fc.oneof(arbShareMode, fc.constant(undefined as string | undefined)),
        (shareType) => {
          const a = shareIndicatorForType(shareType);
          const b = shareIndicatorForType(shareType);
          expect(a).toEqual(b);
        },
      ),
      { numRuns: 100 },
    );
  });

  it('audio_only is the only mode that produces a non-triangle indicator', () => {
    fc.assert(
      fc.property(arbShareMode, (mode) => {
        const indicator = shareIndicatorForType(mode);
        if (mode === 'audio_only') {
          expect(indicator.char).not.toBe('▲');
        } else {
          expect(indicator.char).toBe('▲');
        }
      }),
      { numRuns: 100 },
    );
  });
});
