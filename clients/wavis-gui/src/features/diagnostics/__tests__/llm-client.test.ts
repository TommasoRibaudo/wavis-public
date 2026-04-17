import { beforeEach, describe, expect, it, vi } from 'vitest';
import fc from 'fast-check';

const { apiFetchMock, apiPublicFetchMock } = vi.hoisted(() => ({
  apiFetchMock: vi.fn(),
  apiPublicFetchMock: vi.fn(),
}));

vi.mock('@shared/api', () => ({
  apiFetch: apiFetchMock,
  apiPublicFetch: apiPublicFetchMock,
}));

import type { CapturedContext, AppStateSnapshot } from '../bug-report';
import type { QaPair } from '../llm-client';
import { analyzeBugReport, buildOfflineIssueBody, generateIssueBody } from '../llm-client';

function makeMockContext(overrides?: Partial<CapturedContext>): CapturedContext {
  return {
    jsConsoleLogs: ['[2026-03-16T00:00:00Z] [error] test error'],
    rustLogs: ['2026-03-16 00:00:00 ERROR test rust log'],
    wsMessages: ['[00:00:00] -> {ping}'],
    screenshot: null,
    appState: {
      route: '/room',
      wsStatus: 'connected',
      voiceRoomState: 'active',
      audioDevices: { input: null, output: null },
      platform: 'Win32',
      appVersion: 'unknown',
    },
    capturedAt: '2026-03-16T00:00:00.000Z',
    ...overrides,
  };
}

const safeChars = [...'ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789 '];

const safeStringArb = fc
  .array(fc.constantFrom(...safeChars), { minLength: 1, maxLength: 100 })
  .map((chars) => chars.join(''));

const categoryArb = fc.constantFrom('audio', 'ui', 'connectivity', 'crash', 'performance', 'other');

const qaPairArb: fc.Arbitrary<QaPair> = fc.record({
  question: safeStringArb,
  answer: safeStringArb,
});

const qaRoundArb = fc.array(qaPairArb, { minLength: 1, maxLength: 5 });

const appStateArb: fc.Arbitrary<AppStateSnapshot> = fc.record({
  route: safeStringArb,
  wsStatus: fc.constantFrom('connected', 'disconnected', 'connecting'),
  voiceRoomState: fc.oneof(safeStringArb, fc.constant(null)),
  audioDevices: fc.record({
    input: fc.oneof(safeStringArb, fc.constant(null)),
    output: fc.oneof(safeStringArb, fc.constant(null)),
  }),
  platform: safeStringArb,
  appVersion: safeStringArb,
});

const capturedContextArb: fc.Arbitrary<CapturedContext> = fc.record({
  jsConsoleLogs: fc.array(safeStringArb, { minLength: 0, maxLength: 10 }),
  rustLogs: fc.array(safeStringArb, { minLength: 0, maxLength: 10 }),
  wsMessages: fc.array(safeStringArb, { minLength: 0, maxLength: 5 }),
  screenshot: fc.constant(null),
  appState: appStateArb,
  capturedAt: fc.constant('2026-03-16T00:00:00.000Z'),
});

describe('llm-client', () => {
  beforeEach(() => {
    apiFetchMock.mockReset();
    apiPublicFetchMock.mockReset();
  });

  it('buildOfflineIssueBody produces valid markdown with all sections', () => {
    const context = makeMockContext();
    const body = buildOfflineIssueBody('App crashes on join', context, [], 'crash');

    expect(body).toContain('## Bug Report');
    expect(body).toContain('### Category');
    expect(body).toContain('crash');
    expect(body).toContain('### Description');
    expect(body).toContain('App crashes on join');
    expect(body).toContain('<details><summary>Diagnostics</summary>');
    expect(body).toContain('</details>');
  });

  it('buildOfflineIssueBody includes QA rounds when provided', () => {
    const context = makeMockContext();
    const qaRounds: QaPair[][] = [
      [{ question: 'What happened?', answer: 'It crashed' }],
    ];
    const body = buildOfflineIssueBody('Bug', context, qaRounds, 'audio');

    expect(body).toContain('### Follow-Up Answers');
    expect(body).toContain('What happened?');
    expect(body).toContain('It crashed');
  });

  it('buildOfflineIssueBody omits QA section when no rounds', () => {
    const context = makeMockContext();
    const body = buildOfflineIssueBody('Bug', context, [], 'ui');

    expect(body).not.toContain('### Follow-Up Answers');
  });

  it('analyzeBugReport uses the authenticated backend endpoint first', async () => {
    const context = makeMockContext();
    apiFetchMock.mockResolvedValue({
      category: 'audio',
      questions: [{ text: 'What happened first?', options: ['Immediately', 'After a few seconds', 'On reconnect'] }],
      needs_follow_up: true,
    });

    const result = await analyzeBugReport('Audio broke on join', context);

    expect(apiFetchMock).toHaveBeenCalledWith('/bug-report/analyze', {
      method: 'POST',
      body: JSON.stringify({
        description: 'Audio broke on join',
        context: {
          js_console_logs: context.jsConsoleLogs,
          rust_logs: context.rustLogs,
          ws_messages: context.wsMessages,
          app_state: {
            route: context.appState.route,
            ws_status: context.appState.wsStatus,
            voice_room_state: context.appState.voiceRoomState,
            platform: context.appState.platform,
          },
        },
        previous_answers: null,
      }),
    });
    expect(result).toEqual({
      category: 'audio',
      questions: [{ text: 'What happened first?', options: ['Immediately', 'After a few seconds', 'On reconnect'] }],
      needsFollowUp: true,
    });
  });

  it('analyzeBugReport falls back to the public endpoint when the authenticated call fails', async () => {
    const context = makeMockContext();
    const previousAnswers = [{ question: 'What did you click?', answer: 'Join' }];
    apiFetchMock.mockRejectedValue(new Error('unauthorized'));
    apiPublicFetchMock.mockResolvedValue({
      category: 'ui',
      questions: [{ text: 'Did the dialog close?', options: ['Yes', 'No', 'Partially'] }],
      needs_follow_up: false,
    });

    const result = await analyzeBugReport('The dialog froze', context, previousAnswers);

    expect(apiPublicFetchMock).toHaveBeenCalledWith('/bug-report/analyze', {
      method: 'POST',
      body: JSON.stringify({
        description: 'The dialog froze',
        context: {
          js_console_logs: context.jsConsoleLogs,
          rust_logs: context.rustLogs,
          ws_messages: context.wsMessages,
          app_state: {
            route: context.appState.route,
            ws_status: context.appState.wsStatus,
            voice_room_state: context.appState.voiceRoomState,
            platform: context.appState.platform,
          },
        },
        previous_answers: previousAnswers,
      }),
    });
    expect(result).toEqual({
      category: 'ui',
      questions: [{ text: 'Did the dialog close?', options: ['Yes', 'No', 'Partially'] }],
      needsFollowUp: false,
    });
  });

  it('generateIssueBody uses the backend generate-body endpoint', async () => {
    const context = makeMockContext();
    const qaRounds: QaPair[][] = [[{ question: 'What happened?', answer: 'It crashed' }]];
    apiFetchMock.mockResolvedValue({
      title: 'Bug Report: crash on join',
      body: '## Bug Report\n\n### Description\nIt crashed',
    });

    const result = await generateIssueBody('Crash on join', context, qaRounds, 'crash');

    expect(apiFetchMock).toHaveBeenCalledWith('/bug-report/generate-body', {
      method: 'POST',
      body: JSON.stringify({
        description: 'Crash on join',
        context: {
          js_console_logs: context.jsConsoleLogs,
          rust_logs: context.rustLogs,
          ws_messages: context.wsMessages,
          app_state: {
            route: context.appState.route,
            ws_status: context.appState.wsStatus,
            voice_room_state: context.appState.voiceRoomState,
            platform: context.appState.platform,
          },
        },
        qa_rounds: qaRounds,
        category: 'crash',
      }),
    });
    expect(result).toEqual({
      title: 'Bug Report: crash on join',
      body: '## Bug Report\n\n### Description\nIt crashed',
    });
  });
});

describe('Feature: in-app-bug-report, Property 12: Generated issue body contains all required sections', () => {
  it('offline issue body contains all required sections for any input', () => {
    fc.assert(
      fc.property(
        safeStringArb,
        capturedContextArb,
        fc.array(qaRoundArb, { minLength: 0, maxLength: 2 }),
        categoryArb,
        (description, context, qaRounds, category) => {
          const body = buildOfflineIssueBody(description, context, qaRounds, category);

          expect(body).toContain('## Bug Report');
          expect(body).toContain('### Category');
          expect(body).toContain(category);
          expect(body).toContain('### Description');
          expect(body).toContain(description);
          expect(body).toContain('<details><summary>Diagnostics</summary>');
          expect(body).toContain('</details>');

          if (qaRounds.length > 0 && qaRounds.some((round) => round.length > 0)) {
            expect(body).toContain('### Follow-Up Answers');
            for (const round of qaRounds) {
              for (const qa of round) {
                expect(body).toContain(qa.question);
                expect(body).toContain(qa.answer);
              }
            }
          }
        },
      ),
      { numRuns: 200 },
    );
  });
});

// ─── Log entry truncation ────────────────────────────────────────────

describe('Feature: in-app-bug-report, log entry truncation in buildOfflineIssueBody', () => {
  it('truncates a log entry longer than 500 chars to 500 chars + ellipsis', () => {
    const longEntry = 'x'.repeat(600);
    const context = makeMockContext({ jsConsoleLogs: [longEntry] });
    const body = buildOfflineIssueBody('Bug', context, [], 'other');

    expect(body).not.toContain(longEntry);
    expect(body).toContain('x'.repeat(500) + '…');
  });

  it('does not truncate a log entry of exactly 500 chars', () => {
    const exactEntry = 'y'.repeat(500);
    const context = makeMockContext({ jsConsoleLogs: [exactEntry] });
    const body = buildOfflineIssueBody('Bug', context, [], 'other');

    expect(body).toContain(exactEntry);
    expect(body).not.toContain(exactEntry + '…');
  });

  it('does not truncate a log entry shorter than 500 chars', () => {
    const shortEntry = 'z'.repeat(100);
    const context = makeMockContext({ rustLogs: [shortEntry] });
    const body = buildOfflineIssueBody('Bug', context, [], 'other');

    expect(body).toContain(shortEntry);
  });

  it('truncates entries in all three log sections (JS, Rust, WS)', () => {
    const long = 'a'.repeat(600);
    const context = makeMockContext({
      jsConsoleLogs: [long],
      rustLogs: [long],
      wsMessages: [long],
    });
    const body = buildOfflineIssueBody('Bug', context, [], 'other');

    // Original 600-char entry must not appear anywhere
    expect(body).not.toContain(long);
    // All three sections should contain the truncated form
    const truncated = 'a'.repeat(500) + '…';
    const occurrences = body.split(truncated).length - 1;
    expect(occurrences).toBe(3);
  });

  it('truncates a 501-char entry but not a 500-char entry', () => {
    const at500 = 'b'.repeat(500);
    const at501 = 'c'.repeat(501);
    const context = makeMockContext({ jsConsoleLogs: [at500, at501] });
    const body = buildOfflineIssueBody('Bug', context, [], 'other');

    expect(body).toContain(at500);
    expect(body).not.toContain(at501);
    expect(body).toContain('c'.repeat(500) + '…');
  });

  it('Property: no log entry in the rendered body exceeds 501 chars on any line', () => {
    fc.assert(
      fc.property(
        fc.array(fc.string({ minLength: 0, maxLength: 1000 }), { minLength: 0, maxLength: 10 }),
        fc.array(fc.string({ minLength: 0, maxLength: 1000 }), { minLength: 0, maxLength: 10 }),
        fc.array(fc.string({ minLength: 0, maxLength: 1000 }), { minLength: 0, maxLength: 5 }),
        (jsLogs, rustLogs, wsMessages) => {
          const context = makeMockContext({ jsConsoleLogs: jsLogs, rustLogs, wsMessages });
          const body = buildOfflineIssueBody('Bug', context, [], 'other');

          // Each line in the log sections must be at most 501 chars (500 + '…').
          // Lines outside the log blocks (markdown headers, fences) are short by design.
          for (const line of body.split('\n')) {
            expect(line.length).toBeLessThanOrEqual(501);
          }
        },
      ),
      { numRuns: 200 },
    );
  });
});
