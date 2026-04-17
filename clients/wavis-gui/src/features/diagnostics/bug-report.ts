/**
 * Wavis Bug Report — Context Capture + Submission
 *
 * Gathers diagnostic data (JS console logs, Rust logs, WS messages,
 * screenshot, app state) in parallel, applies client-side redaction,
 * and submits the finalized report to POST /bug-report.
 */

import { invoke } from '@tauri-apps/api/core';

import { apiFetch, apiPublicFetch } from '@shared/api';
import { getAccessToken, getServerUrl } from '@features/auth/auth';
import { redactAll, redactText } from '@shared/redaction';
import { consoleLogBuffer } from '@shared/ring-buffer';
import { getState as getVoiceRoomState } from '@features/voice/voice-room';
import type { ShareSessionLeakSummary } from '@features/voice/share-leak-diagnostics';
import { wsMessageBuffer } from '@shared/ws-message-buffer';

// ─── Types ─────────────────────────────────────────────────────────

export interface AppStateSnapshot {
  route: string;
  wsStatus: string;
  voiceRoomState: string | null;
  audioDevices: { input: string | null; output: string | null };
  platform: string;
  appVersion: string;
}

export interface CapturedContext {
  jsConsoleLogs: string[];
  rustLogs: string[];
  wsMessages: string[];
  screenshot: Uint8Array | null;
  appState: AppStateSnapshot;
  shareLeakSummary?: ShareSessionLeakSummary | null;
  capturedAt: string;
}

export interface BugReportPayload {
  title: string;
  body: string;
  category: string;
  screenshot: string | null;
}

export interface BugReportResponse {
  issue_url: string;
}

// ─── Constants ─────────────────────────────────────────────────────

const LOG_PREFIX = '[wavis:bug-report]';
const MAX_SCREENSHOT_BYTES = 4 * 1024 * 1024; // 4 MB

// ─── Helpers (private) ─────────────────────────────────────────────

function captureAppState(): AppStateSnapshot {
  const voiceState = getVoiceRoomState();
  return {
    route: window.location.hash || window.location.pathname,
    wsStatus: voiceState.machineState === 'idle' ? 'disconnected' : 'connected',
    voiceRoomState: voiceState.machineState !== 'idle' ? voiceState.machineState : null,
    audioDevices: { input: null, output: null },
    platform: navigator.platform ?? 'unknown',
    appVersion: 'unknown',
  };
}

async function captureRustLogs(): Promise<string[]> {
  try {
    return await invoke<string[]>('get_rust_log_buffer');
  } catch (err) {
    console.warn(LOG_PREFIX, 'Failed to capture Rust logs:', err);
    return [];
  }
}

async function captureScreenshot(): Promise<Uint8Array | null> {
  try {
    const bytes = await invoke<number[]>('capture_window_screenshot');
    return new Uint8Array(bytes);
  } catch (err) {
    console.warn(LOG_PREFIX, 'Screenshot capture failed:', err);
    return null;
  }
}

function formatConsoleLogEntries(): string[] {
  return consoleLogBuffer.snapshot().map(
    (entry) => `[${new Date(entry.timestamp).toISOString()}] [${entry.level}] ${entry.message}`,
  );
}

// ─── API Functions (exported) ──────────────────────────────────────

/**
 * Capture all diagnostic context in parallel.
 * Applies redaction to all text fields before returning.
 * Uses snapshot() (not drain()) to preserve buffer contents.
 */
export async function captureAllContext(preScreenshot?: Uint8Array | null): Promise<CapturedContext> {
  const [jsLogs, rustLogs, wsMessages, screenshot] = await Promise.all([
    Promise.resolve(formatConsoleLogEntries()),
    captureRustLogs(),
    Promise.resolve(wsMessageBuffer.snapshot()),
    // Skip IPC capture if a screenshot was taken before the panel opened.
    preScreenshot !== undefined ? Promise.resolve(preScreenshot) : captureScreenshot(),
  ]);

  const voiceState = getVoiceRoomState();
  const appState = captureAppState();

  // Redact all text fields
  const redactedJsLogs = redactAll(jsLogs);
  const redactedRustLogs = redactAll(rustLogs);
  const redactedWsMessages = redactAll(wsMessages);
  const redactedAppState: AppStateSnapshot = {
    ...appState,
    route: redactText(appState.route),
    wsStatus: appState.wsStatus,
    voiceRoomState: appState.voiceRoomState ? redactText(appState.voiceRoomState) : null,
  };

  return {
    jsConsoleLogs: redactedJsLogs,
    rustLogs: redactedRustLogs,
    wsMessages: redactedWsMessages,
    screenshot,
    appState: redactedAppState,
    shareLeakSummary: voiceState.latestClosedShareLeakSummary,
    capturedAt: new Date().toISOString(),
  };
}

/**
 * Check whether a screenshot exceeds the 4 MB client-side limit.
 * Returns true if the screenshot is too large.
 */
export function isScreenshotTooLarge(screenshot: Uint8Array): boolean {
  return screenshot.byteLength > MAX_SCREENSHOT_BYTES;
}

/**
 * Submit a bug report to POST /bug-report.
 * Uses apiFetch() for authenticated users, apiPublicFetch() for anonymous.
 */
export async function submitBugReport(
  payload: BugReportPayload,
): Promise<BugReportResponse> {
  const serverUrl = await getServerUrl();
  if (!serverUrl) {
    throw new Error('Server not configured — please complete setup first');
  }

  const token = await getAccessToken();
  const body = JSON.stringify(payload);

  if (token) {
    return apiFetch<BugReportResponse>('/bug-report', {
      method: 'POST',
      body,
    });
  }

  return apiPublicFetch<BugReportResponse>('/bug-report', {
    method: 'POST',
    body,
  });
}
