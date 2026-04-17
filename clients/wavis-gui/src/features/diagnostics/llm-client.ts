/**
 * Wavis LLM Client — Bug Report Analysis (Server-Proxied)
 *
 * All LLM calls are proxied through the backend. The developer's API key
 * lives server-side as BUG_REPORT_LLM_API_KEY — users don't configure anything.
 * Falls back to offline mode when the backend LLM service is unavailable.
 */

import { apiFetch, apiPublicFetch } from '@shared/api';
import type { CapturedContext } from './bug-report';

// ─── Types ─────────────────────────────────────────────────────────

export interface LlmQuestion {
  text: string;
  options?: string[];
}

export interface LlmAnalysis {
  category: string;
  questions: LlmQuestion[];
  needsFollowUp: boolean;
}

export interface QaPair {
  question: string;
  answer: string;
}

// ─── Constants ─────────────────────────────────────────────────────

const LOG_PREFIX = '[wavis:llm]';
const MAX_LOG_ENTRY_CHARS = 500;

// ─── Helpers (private) ─────────────────────────────────────────────

/** Convert client CapturedContext to the backend's expected shape. */
function buildServerContext(context: CapturedContext) {
  return {
    js_console_logs: context.jsConsoleLogs,
    rust_logs: context.rustLogs,
    ws_messages: context.wsMessages,
    ...(context.shareLeakSummary ? { share_leak_summary: context.shareLeakSummary } : {}),
    app_state: {
      route: context.appState.route,
      ws_status: context.appState.wsStatus,
      voice_room_state: context.appState.voiceRoomState ?? null,
      platform: context.appState.platform,
    },
  };
}

// ─── API Functions (exported) ──────────────────────────────────────

/**
 * Analyze a bug report via the backend LLM proxy.
 * Returns category, follow-up questions, and whether another round is needed.
 * Throws on failure (caller should fall back to offline mode).
 */
export async function analyzeBugReport(
  description: string,
  context: CapturedContext,
  previousAnswers?: QaPair[],
): Promise<LlmAnalysis> {
  console.info(LOG_PREFIX, 'Requesting server-side analysis...');

  const body = {
    description,
    context: buildServerContext(context),
    previous_answers: previousAnswers?.map((qa) => ({
      question: qa.question,
      answer: qa.answer,
    })) ?? null,
  };

  // Try authenticated first, fall back to public (anonymous users)
  let result: { category: string; questions: LlmQuestion[]; needs_follow_up: boolean };
  try {
    result = await apiFetch<typeof result>('/bug-report/analyze', {
      method: 'POST',
      body: JSON.stringify(body),
    });
  } catch {
    // If auth fails (e.g. not logged in), try public endpoint
    result = await apiPublicFetch<typeof result>('/bug-report/analyze', {
      method: 'POST',
      body: JSON.stringify(body),
    });
  }

  return {
    category: result.category,
    questions: result.questions,
    needsFollowUp: result.needs_follow_up,
  };
}

/**
 * Generate a structured GitHub issue body via the backend LLM proxy.
 * Returns { title, body } for the issue preview.
 * Throws on failure (caller should use offline fallback body).
 */
export async function generateIssueBody(
  description: string,
  context: CapturedContext,
  qaRounds: QaPair[][],
  category: string,
): Promise<{ title: string; body: string }> {
  console.info(LOG_PREFIX, 'Requesting server-side issue body generation...');

  const body = {
    description,
    context: buildServerContext(context),
    qa_rounds: qaRounds.map((round) =>
      round.map((qa) => ({ question: qa.question, answer: qa.answer })),
    ),
    category,
  };

  let result: { title: string; body: string };
  try {
    result = await apiFetch<typeof result>('/bug-report/generate-body', {
      method: 'POST',
      body: JSON.stringify(body),
    });
  } catch {
    result = await apiPublicFetch<typeof result>('/bug-report/generate-body', {
      method: 'POST',
      body: JSON.stringify(body),
    });
  }

  return result;
}

/**
 * Generate an issue body without LLM assistance (offline mode).
 * Used when the backend LLM service is unavailable.
 */
export function buildOfflineIssueBody(
  description: string,
  context: CapturedContext,
  qaRounds: QaPair[][],
  category: string,
): string {
  const sections: string[] = [
    '## Bug Report',
    '',
    `### Category`,
    category,
    '',
    '### Description',
    description,
  ];

  if (qaRounds.length > 0) {
    sections.push('', '### Follow-Up Answers');
    for (const [i, round] of qaRounds.entries()) {
      for (const qa of round) {
        sections.push(`**Round ${i + 1} — Q:** ${qa.question}`, `**A:** ${qa.answer}`, '');
      }
    }
  }

  const truncateEntry = (s: string) =>
    s.length > MAX_LOG_ENTRY_CHARS ? s.slice(0, MAX_LOG_ENTRY_CHARS) + '…' : s;

  sections.push(
    '',
    '<details><summary>Diagnostics</summary>',
    '',
    '### Console Logs',
    '```',
    context.jsConsoleLogs.slice(-30).map(truncateEntry).join('\n'),
    '```',
    '',
    '### Rust Logs',
    '```',
    context.rustLogs.slice(-30).map(truncateEntry).join('\n'),
    '```',
    '',
    '### WebSocket Messages',
    '```',
    context.wsMessages.slice(-15).map(truncateEntry).join('\n'),
    '```',
    '',
    '### App State',
    `- Route: ${context.appState.route}`,
    `- WS Status: ${context.appState.wsStatus}`,
    `- Voice Room: ${context.appState.voiceRoomState ?? 'none'}`,
    `- Platform: ${context.appState.platform}`,
    `- Captured At: ${context.capturedAt}`,
    ...(context.shareLeakSummary
      ? [
          '',
          '### Share Leak Summary',
          '```json',
          JSON.stringify(context.shareLeakSummary, null, 2),
          '```',
        ]
      : []),
    '',
    '</details>',
  );

  return sections.join('\n');
}
