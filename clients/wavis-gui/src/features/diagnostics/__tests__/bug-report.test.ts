import { describe, expect, it } from 'vitest';
import fc from 'fast-check';

import { isScreenshotTooLarge } from '../bug-report';

// ─── Constants ─────────────────────────────────────────────────────

const FOUR_MB = 4 * 1024 * 1024;

// ─── Unit Tests ────────────────────────────────────────────────────

describe('bug-report', () => {
  it('isScreenshotTooLarge returns false for exactly 4 MB', () => {
    const screenshot = new Uint8Array(FOUR_MB);
    expect(isScreenshotTooLarge(screenshot)).toBe(false);
  });

  it('isScreenshotTooLarge returns true for 4 MB + 1 byte', () => {
    const screenshot = new Uint8Array(FOUR_MB + 1);
    expect(isScreenshotTooLarge(screenshot)).toBe(true);
  });

  it('isScreenshotTooLarge returns false for empty screenshot', () => {
    const screenshot = new Uint8Array(0);
    expect(isScreenshotTooLarge(screenshot)).toBe(false);
  });
});

// ─── Property 15 (client portion): Screenshot raw PNG > 4 MB refused ───

describe('Feature: in-app-bug-report, Property 15: Screenshot raw PNG > 4 MB refused', () => {
  /**
   * For any screenshot whose raw bytes exceed 4 MB, the client must
   * refuse to include it in the payload. For any screenshot at or
   * below 4 MB, the client must accept it.
   */
  it('rejects screenshots larger than 4 MB, accepts those at or below', () => {
    fc.assert(
      fc.property(
        fc.integer({ min: 0, max: 8 * 1024 * 1024 }),
        (size) => {
          const screenshot = new Uint8Array(size);
          const tooLarge = isScreenshotTooLarge(screenshot);

          if (size > FOUR_MB) {
            expect(tooLarge).toBe(true);
          } else {
            expect(tooLarge).toBe(false);
          }
        },
      ),
      { numRuns: 200 },
    );
  });
});
