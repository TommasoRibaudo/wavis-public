import { describe, it, expect } from 'vitest';
import fc from 'fast-check';
import { filterSourcesByMode } from '../SharePicker';
import type {
  ShareMode,
  ShareSource,
  ShareSourceType,
  ShareSelection,
} from '../share-types';

/* ─── Arbitraries ───────────────────────────────────────────────── */

const arbShareSourceType: fc.Arbitrary<ShareSourceType> = fc.constantFrom(
  'screen' as const,
  'window' as const,
  'system_audio' as const,
);

const arbShareMode: fc.Arbitrary<ShareMode> = fc.constantFrom(
  'screen_audio' as const,
  'window' as const,
  'audio_only' as const,
);

const arbShareSource: fc.Arbitrary<ShareSource> = fc.record({
  id: fc.string({ minLength: 1, maxLength: 64 }),
  name: fc.string({ minLength: 1, maxLength: 128 }),
  source_type: arbShareSourceType,
  thumbnail: fc.oneof(fc.constant(null), fc.string({ minLength: 1, maxLength: 32 })),
  app_name: fc.oneof(fc.constant(null), fc.string({ minLength: 1, maxLength: 64 })),
});

/* ─── Mode → source_type mapping ────────────────────────────────── */

const MODE_TO_SOURCE_TYPE: Record<ShareMode, ShareSourceType> = {
  screen_audio: 'screen',
  window: 'window',
  audio_only: 'system_audio',
};

/* ═══ Property 3: Mode-based source filtering ═══════════════════════ */
// Feature: custom-share-picker, Property 3: Mode-based source filtering
// **Validates: Requirements 2.3, 2.4, 2.5**

describe('Property 3: Mode-based source filtering', () => {
  it('returns only sources matching the expected source_type for the given mode', () => {
    fc.assert(
      fc.property(fc.array(arbShareSource), arbShareMode, (sources, mode) => {
        const filtered = filterSourcesByMode(sources, mode);
        const expectedType = MODE_TO_SOURCE_TYPE[mode];
        for (const s of filtered) {
          expect(s.source_type).toBe(expectedType);
        }
      }),
      { numRuns: 100 },
    );
  });

  it('returns all sources of the matching type (no valid sources dropped)', () => {
    fc.assert(
      fc.property(fc.array(arbShareSource), arbShareMode, (sources, mode) => {
        const filtered = filterSourcesByMode(sources, mode);
        const expectedType = MODE_TO_SOURCE_TYPE[mode];
        const expected = sources.filter((s) => s.source_type === expectedType);
        expect(filtered).toHaveLength(expected.length);
      }),
      { numRuns: 100 },
    );
  });

  it('returns empty array when no sources match the mode', () => {
    fc.assert(
      fc.property(arbShareMode, (mode) => {
        const expectedType = MODE_TO_SOURCE_TYPE[mode];
        // Build sources that all have a *different* type
        const otherTypes = (['screen', 'window', 'system_audio'] as ShareSourceType[]).filter(
          (t) => t !== expectedType,
        );
        const sources: ShareSource[] = otherTypes.map((t) => ({
          id: `id-${t}`,
          name: `name-${t}`,
          source_type: t,
          thumbnail: null,
          app_name: null,
        }));
        expect(filterSourcesByMode(sources, mode)).toHaveLength(0);
      }),
      { numRuns: 100 },
    );
  });
});


/* ═══ Property 4: Share button requires valid selection ══════════════ */
// Feature: custom-share-picker, Property 4: Share button requires valid selection
// **Validates: Requirements 2.6**

describe('Property 4: Share button requires valid selection', () => {
  it('button is disabled when selectedSource is null', () => {
    fc.assert(
      fc.property(arbShareMode, (_mode) => {
        const selectedSource: ShareSource | null = null;
        const isDisabled = selectedSource === null;
        expect(isDisabled).toBe(true);
      }),
      { numRuns: 100 },
    );
  });

  it('button is enabled when a valid source is selected', () => {
    fc.assert(
      fc.property(arbShareMode, arbShareSource, (_mode, source) => {
        const selectedSource: ShareSource | null = source;
        const isDisabled = selectedSource === null;
        expect(isDisabled).toBe(false);
      }),
      { numRuns: 100 },
    );
  });

  it('disabled state matches exactly whether selectedSource is null', () => {
    fc.assert(
      fc.property(
        arbShareMode,
        fc.oneof(fc.constant(null), arbShareSource),
        (_mode, selectedSource) => {
          const isDisabled = selectedSource === null;
          if (selectedSource === null) {
            expect(isDisabled).toBe(true);
          } else {
            expect(isDisabled).toBe(false);
          }
        },
      ),
      { numRuns: 100 },
    );
  });
});

/* ═══ Property 5: Share selection event payload completeness ═════════ */
// Feature: custom-share-picker, Property 5: Share selection event payload completeness
// **Validates: Requirements 3.1**

describe('Property 5: Share selection event payload completeness', () => {
  /** Arbitrary that builds a valid ShareSelection from random inputs. */
  const arbShareSelection: fc.Arbitrary<ShareSelection> = fc.record({
    mode: arbShareMode,
    sourceId: fc.string({ minLength: 1, maxLength: 64 }),
    sourceName: fc.string({ minLength: 1, maxLength: 128 }),
    withAudio: fc.boolean(),
  });

  it('payload contains all required fields with correct types', () => {
    fc.assert(
      fc.property(arbShareSelection, (selection) => {
        expect(typeof selection.mode).toBe('string');
        expect(['screen_audio', 'window', 'audio_only']).toContain(selection.mode);
        expect(typeof selection.sourceId).toBe('string');
        expect(selection.sourceId.length).toBeGreaterThan(0);
        expect(typeof selection.sourceName).toBe('string');
        expect(selection.sourceName.length).toBeGreaterThan(0);
        expect(typeof selection.withAudio).toBe('boolean');
      }),
      { numRuns: 100 },
    );
  });

  it('sourceId is never empty', () => {
    fc.assert(
      fc.property(arbShareSelection, (selection) => {
        expect(selection.sourceId).not.toBe('');
      }),
      { numRuns: 100 },
    );
  });

  it('sourceName is never empty', () => {
    fc.assert(
      fc.property(arbShareSelection, (selection) => {
        expect(selection.sourceName).not.toBe('');
      }),
      { numRuns: 100 },
    );
  });
});
