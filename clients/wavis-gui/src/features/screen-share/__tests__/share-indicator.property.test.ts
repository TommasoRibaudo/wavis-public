import { describe, it, expect } from 'vitest';
import fc from 'fast-check';
import type { ShareMode } from '../share-types';
import { shareLabel } from '../ShareIndicator';

/* ─── Arbitraries ───────────────────────────────────────────────── */

const arbShareMode: fc.Arbitrary<ShareMode> = fc.constantFrom(
  'screen_audio' as const,
  'window' as const,
  'audio_only' as const,
);

/* ═══ Property 11: Share indicator displays correct info ════════════ */
// Feature: custom-share-picker, Property 11: Share indicator displays correct info
// **Validates: Requirements 6.2**

describe('Property 11: Share indicator displays correct info', () => {
  it('label contains expected share type text', () => {
    fc.assert(
      fc.property(arbShareMode, (mode) => {
        const label = shareLabel(mode);
        if (mode === 'screen_audio') expect(label).toContain('Screen');
        else if (mode === 'window') expect(label).toContain('Window');
        else if (mode === 'audio_only') expect(label).toContain('Audio');
      }),
      { numRuns: 100 },
    );
  });

  it('label is never empty', () => {
    fc.assert(
      fc.property(arbShareMode, (mode) => {
        expect(shareLabel(mode).length).toBeGreaterThan(0);
      }),
      { numRuns: 100 },
    );
  });

  it('label contains a visual indicator character', () => {
    fc.assert(
      fc.property(arbShareMode, (mode) => {
        const label = shareLabel(mode);
        expect(label.includes('▲') || label.includes('♪')).toBe(true);
      }),
      { numRuns: 100 },
    );
  });
});
