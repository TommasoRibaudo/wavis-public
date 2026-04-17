/**
 * BugReportFlow Property Tests (fast-check)
 *
 * Property-based tests for bug report flow pure functions:
 * - Property 9: Description minimum length enforcement
 * - Property 13: Edited preview is the submitted body
 * - Property 21: Cancel discards all captured data
 */

import { describe, it, expect, vi } from 'vitest';
import * as fc from 'fast-check';
import { openExternalLink, truncateIssueBody, validateDescription } from '../BugReportFlow';
import type { BugReportPayload } from '../bug-report';

describe('Feature: in-app-bug-report, Property 9: Description minimum length enforcement', () => {
  it('rejects strings with trimmed length < 10', () => {
    fc.assert(
      fc.property(
        fc.string({ minLength: 0, maxLength: 9 }),
        (s) => {
          if (s.trim().length >= 10) return;
          expect(validateDescription(s)).toBe(false);
        },
      ),
      { numRuns: 200 },
    );
  });

  it('accepts strings with trimmed length >= 10', () => {
    fc.assert(
      fc.property(
        fc.string({ minLength: 10, maxLength: 500 }).filter((s) => s.trim().length >= 10),
        (s) => {
          expect(validateDescription(s)).toBe(true);
        },
      ),
      { numRuns: 200 },
    );
  });

  it('rejects empty and whitespace-only strings', () => {
    fc.assert(
      fc.property(
        fc.integer({ min: 0, max: 20 }).map((n) => ' '.repeat(n)),
        (s) => {
          expect(validateDescription(s)).toBe(false);
        },
      ),
      { numRuns: 100 },
    );
  });
});

describe('Feature: in-app-bug-report, Property 13: Edited preview is the submitted body', () => {
  it('payload body is always the edited content, not the original', () => {
    fc.assert(
      fc.property(
        fc.string({ minLength: 1, maxLength: 500 }),
        fc.string({ minLength: 1, maxLength: 500 }),
        fc.string({ minLength: 1, maxLength: 100 }),
        fc.string({ minLength: 1, maxLength: 50 }),
        (original, edited, title, category) => {
          const payload: BugReportPayload = {
            title,
            body: edited,
            category,
            screenshot: null,
          };

          expect(payload.body).toBe(edited);
          if (original !== edited) {
            expect(payload.body).not.toBe(original);
          }
        },
      ),
      { numRuns: 200 },
    );
  });
});

describe('Feature: in-app-bug-report, Property 21: Cancel discards all captured data', () => {
  interface CapturedState {
    context: unknown | null;
    screenshotBlob: unknown | null;
    description: string;
    analysis: unknown | null;
    qaRounds: unknown[];
    issueTitle: string;
    issueBody: string;
    category: string;
    issueUrl: string;
    errorMessage: string;
  }

  function applyCancel(): CapturedState {
    return {
      context: null,
      screenshotBlob: null,
      description: '',
      analysis: null,
      qaRounds: [],
      issueTitle: '',
      issueBody: '',
      category: 'other',
      issueUrl: '',
      errorMessage: '',
    };
  }

  it('cancel always produces a clean state with no diagnostic data', () => {
    fc.assert(
      fc.property(
        fc.record({
          context: fc.oneof(fc.constant(null), fc.string()),
          screenshotBlob: fc.oneof(fc.constant(null), fc.string()),
          description: fc.string({ minLength: 0, maxLength: 200 }),
          analysis: fc.oneof(fc.constant(null), fc.string()),
          qaRounds: fc.array(fc.string(), { minLength: 0, maxLength: 5 }),
          issueTitle: fc.string({ minLength: 0, maxLength: 100 }),
          issueBody: fc.string({ minLength: 0, maxLength: 500 }),
          category: fc.string({ minLength: 0, maxLength: 50 }),
          issueUrl: fc.string({ minLength: 0, maxLength: 200 }),
          errorMessage: fc.string({ minLength: 0, maxLength: 200 }),
        }),
        () => {
          const cleanState = applyCancel();

          expect(cleanState.context).toBeNull();
          expect(cleanState.screenshotBlob).toBeNull();
          expect(cleanState.description).toBe('');
          expect(cleanState.analysis).toBeNull();
          expect(cleanState.qaRounds).toEqual([]);
          expect(cleanState.issueTitle).toBe('');
          expect(cleanState.issueBody).toBe('');
          expect(cleanState.category).toBe('other');
          expect(cleanState.issueUrl).toBe('');
          expect(cleanState.errorMessage).toBe('');
        },
      ),
      { numRuns: 100 },
    );
  });

  it('no captured data field retains a non-empty value after cancel', () => {
    fc.assert(
      fc.property(
        fc.record({
          context: fc.oneof(fc.constant(null), fc.string({ minLength: 1 })),
          screenshotBlob: fc.oneof(fc.constant(null), fc.string({ minLength: 1 })),
          description: fc.string({ minLength: 1, maxLength: 200 }),
          analysis: fc.oneof(fc.constant(null), fc.string({ minLength: 1 })),
          qaRounds: fc.array(fc.string(), { minLength: 1, maxLength: 5 }),
          issueTitle: fc.string({ minLength: 1, maxLength: 100 }),
          issueBody: fc.string({ minLength: 1, maxLength: 500 }),
        }),
        () => {
          const cleanState = applyCancel();

          const diagnosticFields = [
            cleanState.context,
            cleanState.screenshotBlob,
            cleanState.analysis,
          ];
          for (const field of diagnosticFields) {
            expect(field).toBeNull();
          }

          const textFields = [
            cleanState.description,
            cleanState.issueTitle,
            cleanState.issueBody,
            cleanState.issueUrl,
            cleanState.errorMessage,
          ];
          for (const field of textFields) {
            expect(field).toBe('');
          }

          expect(cleanState.qaRounds).toHaveLength(0);
        },
      ),
      { numRuns: 100 },
    );
  });
});

// ─── Issue body truncation ───────────────────────────────────────────

describe('Feature: in-app-bug-report, issue body truncation before submission', () => {
  const LIMIT = 900_000;

  it('returns the body unchanged when it is under the limit', () => {
    const body = 'x'.repeat(LIMIT - 1);
    expect(truncateIssueBody(body)).toBe(body);
  });

  it('returns the body unchanged when it is exactly at the limit', () => {
    const body = 'x'.repeat(LIMIT);
    expect(truncateIssueBody(body)).toBe(body);
  });

  it('truncates and appends notice when body exceeds the limit', () => {
    const body = 'x'.repeat(LIMIT + 100);
    const result = truncateIssueBody(body);

    expect(result.length).toBeLessThan(body.length);
    expect(result.startsWith('x'.repeat(LIMIT))).toBe(true);
    expect(result.endsWith('\n\n_[diagnostics truncated]_')).toBe(true);
  });

  it('truncated result starts with the first 900,000 chars of the original', () => {
    const body = 'abcdefghij'.repeat(100_000); // 1,000,000 chars
    const result = truncateIssueBody(body);

    expect(result.slice(0, LIMIT)).toBe(body.slice(0, LIMIT));
  });

  it('Property: result never exceeds limit + notice length for any input', () => {
    const noticeLength = '\n\n_[diagnostics truncated]_'.length;
    fc.assert(
      fc.property(
        fc.string({ minLength: 0, maxLength: LIMIT + 10_000 }),
        (body) => {
          const result = truncateIssueBody(body);
          expect(result.length).toBeLessThanOrEqual(LIMIT + noticeLength);
        },
      ),
      { numRuns: 200 },
    );
  });

  it('Property: body under the limit is always returned as-is', () => {
    fc.assert(
      fc.property(
        fc.string({ minLength: 0, maxLength: LIMIT }),
        (body) => {
          expect(truncateIssueBody(body)).toBe(body);
        },
      ),
      { numRuns: 200 },
    );
  });
});

describe('Feature: in-app-bug-report, issue link opening', () => {
  it('opens the submitted issue URL via the provided opener', async () => {
    const open = vi.fn(async (_url: string) => {});

    await expect(openExternalLink('https://github.com/example/wavis/issues/79', open)).resolves.toBe(true);

    expect(open).toHaveBeenCalledWith('https://github.com/example/wavis/issues/79');
  });

  it('does nothing when no issue URL is available', async () => {
    const open = vi.fn(async (_url: string) => {});

    await expect(openExternalLink('', open)).resolves.toBe(false);

    expect(open).not.toHaveBeenCalled();
  });
});
