import { useState, useEffect, useCallback, useRef, type MouseEvent } from 'react';

import type { CapturedContext, BugReportPayload } from './bug-report';
import { captureAllContext, isScreenshotTooLarge, submitBugReport } from './bug-report';
import {
  analyzeBugReport,
  generateIssueBody,
  buildOfflineIssueBody,
} from './llm-client';
import type { LlmAnalysis, QaPair } from './llm-client';
import ScreenshotRedactor from './ScreenshotRedactor';

/* ─── Types ─────────────────────────────────────────────────────── */

type FlowStep =
  | 'capture'
  | 'redact'
  | 'describe'
  | 'questionnaire'
  | 'preview'
  | 'submitting'
  | 'success'
  | 'error';

interface OfflineFormData {
  category: string;
  stepsToReproduce: string;
  expectedBehavior: string;
  actualBehavior: string;
}

interface BugReportFlowProps {
  onClose: () => void;
  /** Screenshot captured before the panel opened, so the panel itself is not in it. */
  preScreenshot?: Uint8Array | null;
}

/* ─── Constants ─────────────────────────────────────────────────── */

const LOG_PREFIX = '[wavis:bug-report]';
const MIN_DESCRIPTION_LENGTH = 10;
const OFFLINE_CATEGORIES = ['audio', 'ui', 'connectivity', 'crash', 'performance', 'other'];
const MAX_ISSUE_BODY_CHARS = 900_000;
const MAX_SUBMISSION_JSON_CHARS = 120_000;
const MAX_SCREENSHOT_BASE64_CHARS = 48_000;
const DEFAULT_TRUNCATION_NOTICE = '\n\n_[diagnostics truncated]_';
const TRANSPORT_TRUNCATION_NOTICE = '\n\n_[diagnostics truncated for submission transport]_';

/* ─── Pure Functions (exported for testing) ──────────────────────── */

export function validateDescription(text: string): boolean {
  return text.trim().length >= MIN_DESCRIPTION_LENGTH;
}

export function truncateIssueBody(
  body: string,
): string {
  return body.length > MAX_ISSUE_BODY_CHARS
    ? body.slice(0, MAX_ISSUE_BODY_CHARS) + DEFAULT_TRUNCATION_NOTICE
    : body;
}

export async function openExternalLink(
  url: string,
  opener?: (url: string) => Promise<unknown>,
): Promise<boolean> {
  if (!url) {
    return false;
  }

  const openUrl = opener ?? (await import('@tauri-apps/plugin-shell')).open;
  await openUrl(url);
  return true;
}

/* ─── Helpers ───────────────────────────────────────────────────── */

function isLinux(): boolean {
  return navigator.platform?.toLowerCase().includes('linux') ?? false;
}

function measureBugReportPayloadChars(payload: BugReportPayload): number {
  return JSON.stringify(payload).length;
}

function truncateTextToBudget(body: string, maxChars: number, notice: string): string {
  if (body.length <= maxChars) return body;
  if (maxChars <= notice.length) return body.slice(0, maxChars);
  return body.slice(0, maxChars - notice.length) + notice;
}

async function blobToBase64(blob: Blob): Promise<string> {
  const arrayBuffer = await blob.arrayBuffer();
  const bytes = new Uint8Array(arrayBuffer);
  const chunkSize = 0x8000;
  let binary = '';
  for (let i = 0; i < bytes.length; i += chunkSize) {
    const chunk = bytes.subarray(i, i + chunkSize);
    binary += String.fromCharCode(...chunk);
  }
  return btoa(binary);
}

async function loadImageFromBlob(blob: Blob): Promise<HTMLImageElement> {
  return new Promise((resolve, reject) => {
    const url = URL.createObjectURL(blob);
    const image = new Image();
    image.onload = () => {
      URL.revokeObjectURL(url);
      resolve(image);
    };
    image.onerror = () => {
      URL.revokeObjectURL(url);
      reject(new Error('Failed to decode screenshot blob'));
    };
    image.src = url;
  });
}

async function scaleBlobAsPng(blob: Blob, scale: number): Promise<Blob | null> {
  const image = await loadImageFromBlob(blob);
  const width = Math.max(1, Math.floor(image.naturalWidth * scale));
  const height = Math.max(1, Math.floor(image.naturalHeight * scale));
  const canvas = document.createElement('canvas');
  canvas.width = width;
  canvas.height = height;
  const ctx = canvas.getContext('2d');
  if (!ctx) return null;
  ctx.imageSmoothingEnabled = true;
  ctx.imageSmoothingQuality = 'high';
  ctx.drawImage(image, 0, 0, width, height);
  return await new Promise((resolve) => {
    canvas.toBlob((nextBlob) => resolve(nextBlob), 'image/png');
  });
}

async function fitScreenshotToTransportBudget(
  screenshotBlob: Blob,
  maxBase64Chars: number,
): Promise<string | null> {
  if (maxBase64Chars <= 0) return null;

  let currentBlob = screenshotBlob;
  let currentBase64 = await blobToBase64(currentBlob);
  if (currentBase64.length <= maxBase64Chars) return currentBase64;

  for (let attempt = 0; attempt < 6; attempt++) {
    const scale = Math.min(0.85, Math.sqrt(maxBase64Chars / currentBase64.length) * 0.95);
    if (!Number.isFinite(scale) || scale <= 0 || scale >= 1) break;
    const scaledBlob = await scaleBlobAsPng(currentBlob, scale);
    if (!scaledBlob) break;
    currentBlob = scaledBlob;
    currentBase64 = await blobToBase64(currentBlob);
    if (currentBase64.length <= maxBase64Chars) return currentBase64;
  }

  return null;
}

function buildOfflineFormBody(
  description: string,
  form: OfflineFormData,
  context: CapturedContext,
): string {
  return buildOfflineIssueBody(
    description,
    context,
    form.stepsToReproduce
      ? [[
          { question: 'Steps to reproduce', answer: form.stepsToReproduce },
          { question: 'Expected behavior', answer: form.expectedBehavior },
          { question: 'Actual behavior', answer: form.actualBehavior },
        ]]
      : [],
    form.category || 'other',
  );
}

/* ═══ Component ═════════════════════════════════════════════════════ */

export default function BugReportFlow({ onClose, preScreenshot }: BugReportFlowProps) {
  /* ── State ──────────────────────────────────────────────────────── */
  const [step, setStep] = useState<FlowStep>('capture');
  const [context, setContext] = useState<CapturedContext | null>(null);
  const [screenshotBlob, setScreenshotBlob] = useState<Blob | null>(null);
  const [description, setDescription] = useState('');
  const [descriptionError, setDescriptionError] = useState<string | null>(null);
  const [reportMode, setReportMode] = useState<'quick' | 'ai' | null>(null);

  // LLM state
  const [isOfflineMode, setIsOfflineMode] = useState(false);
  const [analysis, setAnalysis] = useState<LlmAnalysis | null>(null);
  const [qaRounds, setQaRounds] = useState<QaPair[][]>([]);
  const [currentAnswers, setCurrentAnswers] = useState<Record<number, string>>({});
  const [llmRound, setLlmRound] = useState(0);
  const [llmLoading, setLlmLoading] = useState(false);

  // Offline form
  const [offlineForm, setOfflineForm] = useState<OfflineFormData>({
    category: 'other',
    stepsToReproduce: '',
    expectedBehavior: '',
    actualBehavior: '',
  });

  // Preview / submit
  const [issueTitle, setIssueTitle] = useState('');
  const [issueBody, setIssueBody] = useState('');
  const [category, setCategory] = useState('other');
  const [issueUrl, setIssueUrl] = useState('');
  const [errorMessage, setErrorMessage] = useState('');

  const descriptionRef = useRef<HTMLTextAreaElement>(null);

  /* ── Cancel / discard all data (Requirement 17.5) ──────────────── */
  const handleCancel = useCallback(() => {
    setContext(null);
    setScreenshotBlob(null);
    setDescription('');
    setDescriptionError(null);
    setReportMode(null);
    setIsOfflineMode(false);
    setAnalysis(null);
    setQaRounds([]);
    setCurrentAnswers({});
    setLlmRound(0);
    setLlmLoading(false);
    setOfflineForm({ category: 'other', stepsToReproduce: '', expectedBehavior: '', actualBehavior: '' });
    setIssueTitle('');
    setIssueBody('');
    setCategory('other');
    setIssueUrl('');
    setErrorMessage('');
    onClose();
  }, [onClose]);

  /* ── Step 1: Capture ───────────────────────────────────────────── */
  useEffect(() => {
    if (step !== 'capture') return;
    let cancelled = false;

    (async () => {
      try {
        const captured = await captureAllContext(preScreenshot);
        if (cancelled) return;
        setContext(captured);

        // Skip redaction step on Linux or if no screenshot
        if (isLinux() || !captured.screenshot) {
          setStep('describe');
        } else {
          setStep('redact');
        }
      } catch (err) {
        console.warn(LOG_PREFIX, 'Context capture failed:', err);
        if (cancelled) return;
        // Proceed with empty context
        setContext({
          jsConsoleLogs: [],
          rustLogs: [],
          wsMessages: [],
          screenshot: null,
          appState: {
            route: window.location.hash || window.location.pathname,
            wsStatus: 'unknown',
            voiceRoomState: null,
            audioDevices: { input: null, output: null },
            platform: navigator.platform ?? 'unknown',
            appVersion: 'unknown',
          },
          capturedAt: new Date().toISOString(),
        });
        setStep('describe');
      }
    })();

    return () => { cancelled = true; };
  }, [step, preScreenshot]);

  /* ── Focus description input when entering describe step ────────── */
  useEffect(() => {
    if (step === 'describe') {
      setTimeout(() => descriptionRef.current?.focus(), 100);
    }
  }, [step]);

  /* ── Step 2: Screenshot redaction handlers ─────────────────────── */
  const handleScreenshotConfirm = useCallback((blob: Blob) => {
    // 4 MB client-side check (Requirement 12.8)
    if (blob.size > 4 * 1024 * 1024) {
      console.warn(LOG_PREFIX, 'Redacted screenshot exceeds 4 MB, skipping');
      setScreenshotBlob(null);
    } else {
      setScreenshotBlob(blob);
    }
    setStep('describe');
  }, []);

  const handleScreenshotSkip = useCallback(() => {
    setScreenshotBlob(null);
    setStep('describe');
  }, []);

  /* ── Step 3: Description — shared validation ───────────────────── */
  function validateAndClearError(): boolean {
    if (!validateDescription(description)) {
      setDescriptionError('Description must be at least 10 characters');
      return false;
    }
    setDescriptionError(null);
    return true;
  }

  /* ── Step 3a: Quick Send — skip AI, go straight to preview ─────── */
  const handleQuickSend = useCallback(() => {
    if (!validateAndClearError()) return;
    if (!context) return;
    setReportMode('quick');
    const body = buildOfflineIssueBody(description, context, [], 'other');
    setIssueTitle(`Bug Report: ${description.slice(0, 80)}`);
    setIssueBody(body);
    setCategory('other');
    setStep('preview');
  }, [description, context]); // eslint-disable-line react-hooks/exhaustive-deps

  /* ── Step 3b: AI-Assisted — run LLM analysis ───────────────────── */
  const handleAiAssisted = useCallback(async () => {
    if (!validateAndClearError()) return;
    setReportMode('ai');
    setLlmLoading(true);
    setStep('questionnaire');

    try {
      const result = await analyzeBugReport(description, context!);
      setAnalysis(result);
      setCategory(result.category);
      setLlmRound(1);
      const answers: Record<number, string> = {};
      result.questions.forEach((_, i) => { answers[i] = ''; });
      setCurrentAnswers(answers);
    } catch (err) {
      console.warn(LOG_PREFIX, 'Server LLM analysis unavailable, falling back to offline mode:', err);
      setIsOfflineMode(true);
      setAnalysis(null);
    } finally {
      setLlmLoading(false);
    }
  }, [description, context]); // eslint-disable-line react-hooks/exhaustive-deps

  /* ── Step 4: Questionnaire submit ───────────────────────────────── */
  const handleQuestionnaireSubmit = useCallback(async () => {
    if (isOfflineMode) {
      // Offline mode: go straight to preview
      if (!context) return;
      const body = buildOfflineFormBody(description, offlineForm, context);
      const title = `Bug Report: ${description.slice(0, 80)}`;
      setIssueTitle(title);
      setIssueBody(body);
      setCategory(offlineForm.category || 'other');
      setStep('preview');
      return;
    }

    // LLM mode: collect answers
    const answers: QaPair[] = (analysis?.questions ?? []).map((q, i) => ({
      question: q.text,
      answer: currentAnswers[i] ?? '',
    }));

    const newRounds = [...qaRounds, answers];
    setQaRounds(newRounds);

    // Check if we need a second round
    if (analysis?.needsFollowUp && llmRound < 2 && context) {
      setLlmLoading(true);
      try {
        const result = await analyzeBugReport(description, context, answers);
        setAnalysis(result);
        setLlmRound(2);
        const newAnswers: Record<number, string> = {};
        result.questions.forEach((_, i) => { newAnswers[i] = ''; });
        setCurrentAnswers(newAnswers);
      } catch (err) {
        console.warn(LOG_PREFIX, 'Second LLM round failed:', err);
        // Proceed to preview with what we have
        await generatePreview(newRounds);
      } finally {
        setLlmLoading(false);
      }
    } else {
      // No more rounds — generate preview
      await generatePreview(newRounds);
    }
  }, [isOfflineMode, context, description, offlineForm, analysis, currentAnswers, qaRounds, llmRound]);

  const generatePreview = useCallback(async (rounds: QaPair[][]) => {
    if (!context) return;

    if (!isOfflineMode) {
      setLlmLoading(true);
      try {
        const result = await generateIssueBody(description, context, rounds, category);
        setIssueTitle(result.title);
        setIssueBody(result.body);
      } catch (err) {
        console.warn(LOG_PREFIX, 'Issue body generation failed, using offline format:', err);
        const body = buildOfflineIssueBody(description, context, rounds, category);
        setIssueTitle(`Bug Report: ${description.slice(0, 80)}`);
        setIssueBody(body);
      } finally {
        setLlmLoading(false);
      }
    } else {
      const body = buildOfflineIssueBody(description, context, rounds, category);
      setIssueTitle(`Bug Report: ${description.slice(0, 80)}`);
      setIssueBody(body);
    }

    setStep('preview');
  }, [context, description, category, isOfflineMode]);

  /* ── Step 5: Submit ────────────────────────────────────────────── */
  const handleSubmit = useCallback(async () => {
    setStep('submitting');

    try {
      let submissionBody = truncateIssueBody(issueBody);
      let basePayload: BugReportPayload = {
        title: issueTitle,
        body: submissionBody,
        category,
        screenshot: null,
      };

      if (measureBugReportPayloadChars(basePayload) > MAX_SUBMISSION_JSON_CHARS) {
        const fixedChars = measureBugReportPayloadChars({ ...basePayload, body: '' });
        const maxBodyChars = Math.max(
          0,
          MAX_SUBMISSION_JSON_CHARS - fixedChars,
        );
        submissionBody = truncateTextToBudget(
          submissionBody,
          maxBodyChars,
          TRANSPORT_TRUNCATION_NOTICE,
        );
        basePayload = {
          ...basePayload,
          body: submissionBody,
        };
        console.warn(
          LOG_PREFIX,
          'Issue body exceeded transport budget and was truncated before submission',
          { bodyChars: issueBody.length, submittedBodyChars: submissionBody.length },
        );
      }

      // Convert screenshot blob to base64 if present and it fits the transport budget.
      let screenshotBase64: string | null = null;
      if (screenshotBlob) {
        const bytes = new Uint8Array(await screenshotBlob.arrayBuffer());
        if (isScreenshotTooLarge(bytes)) {
          console.warn(LOG_PREFIX, 'Screenshot too large at submit time, skipping');
        } else {
          const reservedChars = measureBugReportPayloadChars({ ...basePayload, screenshot: '' });
          const screenshotBudget = Math.min(
            MAX_SCREENSHOT_BASE64_CHARS,
            MAX_SUBMISSION_JSON_CHARS - reservedChars,
          );
          screenshotBase64 = await fitScreenshotToTransportBudget(screenshotBlob, screenshotBudget);
          if (!screenshotBase64) {
            console.warn(
              LOG_PREFIX,
              'Screenshot omitted because it could not fit within the Tauri HTTP transport budget',
              { screenshotBytes: bytes.byteLength, screenshotBudget },
            );
          }
        }
      }

      const payload: BugReportPayload = {
        title: issueTitle,
        body: submissionBody,
        category,
        screenshot: screenshotBase64,
      };

      const payloadChars = measureBugReportPayloadChars(payload);
      console.info(LOG_PREFIX, 'Submitting bug report payload', {
        payloadChars,
        bodyChars: payload.body.length,
        screenshotChars: payload.screenshot?.length ?? 0,
      });
      if (payloadChars > MAX_SUBMISSION_JSON_CHARS) {
        console.warn(
          LOG_PREFIX,
          'Final payload still exceeded transport budget, dropping screenshot',
          { payloadChars, screenshotChars: screenshotBase64?.length ?? 0 },
        );
        payload.screenshot = null;
      }

      const response = await submitBugReport(payload);
      setIssueUrl(response.issue_url);
      setStep('success');
    } catch (err) {
      // Log full error details for debugging — the sanitized message alone is useless
      const isApiError = err && typeof err === 'object' && 'status' in err && 'kind' in err;
      if (isApiError) {
        const { status, kind, message } = err as { status: number; kind: string; message: string };
        console.error(LOG_PREFIX, `Submission failed: HTTP ${status} (${kind}) — ${message}`);
      } else {
        console.error(LOG_PREFIX, 'Submission failed:', err);
      }

      const msg = err instanceof Error ? err.message : 'Failed to submit bug report';
      // Check for rate limit (429)
      if (msg.includes('429') || msg.toLowerCase().includes('too many')) {
        setErrorMessage('Too many reports — please wait before submitting again.');
      } else if (isApiError) {
        const { status, kind } = err as { status: number; kind: string };
        setErrorMessage(`${msg} (${status} ${kind})`);
      } else {
        setErrorMessage(msg);
      }
      setStep('error');
    }
  }, [screenshotBlob, issueTitle, issueBody, category]);

  const handleRetry = useCallback(() => {
    setErrorMessage('');
    setStep('preview');
  }, []);

  const handleIssueLinkClick = useCallback(async (event: MouseEvent<HTMLAnchorElement>) => {
    event.preventDefault();

    try {
      await openExternalLink(issueUrl);
    } catch (err) {
      console.error(LOG_PREFIX, 'Failed to open issue URL:', err);
    }
  }, [issueUrl]);

  /* ── Render ──────────────────────────────────────────────────────── */
  return (
    <div data-bug-report-modal className="fixed inset-0 z-50 flex items-center justify-center bg-wavis-overlay-base/80 font-mono text-wavis-text">
      <div
        className="bg-wavis-panel border border-wavis-text-secondary w-full max-w-2xl max-h-[90vh] overflow-y-auto p-6"
        onClick={(e) => e.stopPropagation()}
      >
        {/* Header */}
        <div className="flex items-center justify-between mb-4">
          <p className="text-wavis-accent text-sm">&gt; Bug Report</p>
          {step !== 'submitting' && (
            <button
              className="text-wavis-text-secondary hover:text-wavis-danger transition-colors text-xs"
              onClick={handleCancel}
            >
              [cancel]
            </button>
          )}
        </div>

        <div className="text-wavis-text-secondary mb-4">{'─'.repeat(48)}</div>

        {/* Step: Capture */}
        {step === 'capture' && (
          <div className="flex items-center gap-2 text-wavis-text-secondary text-sm">
            <span className="animate-pulse">●</span>
            <span>Capturing diagnostic context...</span>
          </div>
        )}

        {/* Step: Redact */}
        {step === 'redact' && context?.screenshot && (
          <div>
            <p className="text-sm text-wavis-text-secondary mb-3">
              Step 1 of 4 — Screenshot Redaction
            </p>
            <ScreenshotRedactor
              screenshotData={context.screenshot}
              onConfirm={handleScreenshotConfirm}
              onSkip={handleScreenshotSkip}
            />
          </div>
        )}

        {/* Step: Describe */}
        {step === 'describe' && (
          <div>
            <p className="text-sm text-wavis-text-secondary mb-3">Describe the Bug</p>
            <div className="flex items-start gap-2 mb-2">
              <span className="text-wavis-accent mt-1">&gt;</span>
              <textarea
                ref={descriptionRef}
                className="flex-1 bg-transparent border border-wavis-text-secondary outline-none px-2 py-1 font-mono text-wavis-text resize-y min-h-[80px]"
                placeholder="Describe what happened (min 10 characters)..."
                value={description}
                onChange={(e) => {
                  setDescription(e.target.value);
                  if (descriptionError) setDescriptionError(null);
                }}
                onKeyDown={(e) => {
                  if (e.key === 'Escape') handleCancel();
                }}
              />
            </div>
            {descriptionError && (
              <p className="text-wavis-danger text-xs mb-2 ml-5">{descriptionError}</p>
            )}
            <p className="text-xs text-wavis-text-secondary mb-1 ml-5">
              {description.trim().length} / {MIN_DESCRIPTION_LENGTH} min chars
            </p>

            <div className="text-wavis-text-secondary mt-4 mb-3">{'─'.repeat(48)}</div>
            <p className="text-xs text-wavis-text-secondary mb-3">How would you like to submit?</p>

            <div className="flex gap-3">
              <button
                className="flex-1 text-left border border-wavis-text-secondary p-3 hover:border-wavis-text hover:bg-wavis-text-secondary/10 transition-colors disabled:opacity-40 disabled:cursor-not-allowed"
                onClick={handleQuickSend}
                disabled={!validateDescription(description)}
              >
                <p className="text-sm text-wavis-text font-bold mb-1">Quick Send</p>
                <p className="text-xs text-wavis-text-secondary leading-relaxed">
                  Submits your description immediately with diagnostic logs attached. No extra steps.
                </p>
              </button>
              <button
                className="flex-1 text-left border border-wavis-accent p-3 hover:bg-wavis-accent/10 transition-colors disabled:opacity-40 disabled:cursor-not-allowed"
                onClick={handleAiAssisted}
                disabled={!validateDescription(description)}
              >
                <p className="text-sm text-wavis-accent font-bold mb-1">AI-Assisted</p>
                <p className="text-xs text-wavis-text-secondary leading-relaxed">
                  An AI analyzes your report, asks targeted follow-up questions, and generates a structured GitHub issue.
                </p>
              </button>
            </div>
          </div>
        )}

        {/* Step: Questionnaire (AI-Assisted path only) */}
        {step === 'questionnaire' && (
          <div>
            <p className="text-sm text-wavis-text-secondary mb-3">
              {context?.screenshot ? 'Step 3 of 4' : 'Step 2 of 3'} — AI Follow-Up Questions
            </p>

            {llmLoading && (
              <div className="flex items-center gap-2 text-wavis-text-secondary text-sm mb-3">
                <span className="animate-pulse">●</span>
                <span>Analyzing your report...</span>
              </div>
            )}

            {!llmLoading && isOfflineMode && (
              <OfflineForm
                form={offlineForm}
                onChange={setOfflineForm}
                onSubmit={handleQuestionnaireSubmit}
              />
            )}

            {!llmLoading && !isOfflineMode && analysis && (
              <div className="space-y-3">
                <p className="text-xs text-wavis-text-secondary">
                  Category: <span className="text-wavis-accent">{analysis.category}</span>
                  {llmRound > 1 && ' — Round 2'}
                </p>
                {analysis.questions.map((q, i) => (
                  <div key={i} className="space-y-1">
                    <p className="text-sm text-wavis-text">{q.text}</p>
                    <div className="flex items-start gap-2">
                      <span className="text-wavis-accent mt-1">&gt;</span>
                      {q.options && q.options.length > 0 ? (
                        <select
                          className="flex-1 bg-wavis-bg border border-wavis-text-secondary outline-none px-2 py-1 font-mono text-wavis-text"
                          value={currentAnswers[i] ?? ''}
                          onChange={(e) =>
                            setCurrentAnswers((prev) => ({ ...prev, [i]: e.target.value }))
                          }
                        >
                          <option value="" disabled>Select an option...</option>
                          {q.options.map((opt) => (
                            <option key={opt} value={opt}>{opt}</option>
                          ))}
                        </select>
                      ) : (
                        <input
                          className="flex-1 bg-transparent border-b border-wavis-text-secondary outline-none px-2 py-1 font-mono text-wavis-text"
                          value={currentAnswers[i] ?? ''}
                          onChange={(e) =>
                            setCurrentAnswers((prev) => ({ ...prev, [i]: e.target.value }))
                          }
                          placeholder="Your answer..."
                        />
                      )}
                    </div>
                  </div>
                ))}
                <div className="flex justify-end mt-3">
                  <button
                    className="border border-wavis-accent text-wavis-accent hover:bg-wavis-accent hover:text-wavis-bg transition-colors px-4 py-1"
                    onClick={handleQuestionnaireSubmit}
                  >
                    Next
                  </button>
                </div>
              </div>
            )}
          </div>
        )}

        {/* Step: Preview */}
        {step === 'preview' && (
          <div>
            <p className="text-sm text-wavis-text-secondary mb-3">
              {reportMode === 'quick'
                ? (context?.screenshot ? 'Step 3 of 3' : 'Step 2 of 2')
                : (context?.screenshot ? 'Step 4 of 4' : 'Step 3 of 3')
              } — Review &amp; Submit
            </p>

            {llmLoading ? (
              <div className="flex items-center gap-2 text-wavis-text-secondary text-sm">
                <span className="animate-pulse">●</span>
                <span>Generating issue preview...</span>
              </div>
            ) : (
              <>
                <div className="mb-3">
                  <label className="text-xs text-wavis-text-secondary block mb-1">Title</label>
                  <input
                    className="w-full bg-transparent border border-wavis-text-secondary outline-none px-2 py-1 font-mono text-wavis-text"
                    value={issueTitle}
                    onChange={(e) => setIssueTitle(e.target.value)}
                  />
                </div>
                <div className="mb-3">
                  <label className="text-xs text-wavis-text-secondary block mb-1">Issue Body</label>
                  <textarea
                    className="w-full bg-transparent border border-wavis-text-secondary outline-none px-2 py-1 font-mono text-wavis-text resize-y min-h-[200px] text-xs"
                    value={issueBody}
                    onChange={(e) => setIssueBody(e.target.value)}
                  />
                </div>
                {screenshotBlob && (
                  <p className="text-xs text-wavis-text-secondary mb-3">
                    📎 Screenshot attached ({(screenshotBlob.size / 1024).toFixed(1)} KB)
                  </p>
                )}
                <div className="flex justify-end gap-3">
                  <button
                    className="border border-wavis-text-secondary text-wavis-text-secondary hover:bg-wavis-text-secondary hover:text-wavis-bg transition-colors px-4 py-1"
                    onClick={handleCancel}
                  >
                    Cancel
                  </button>
                  <button
                    className="border border-wavis-accent text-wavis-accent hover:bg-wavis-accent hover:text-wavis-bg transition-colors px-4 py-1"
                    onClick={handleSubmit}
                  >
                    Submit
                  </button>
                </div>
              </>
            )}
          </div>
        )}

        {/* Step: Submitting */}
        {step === 'submitting' && (
          <div className="flex items-center gap-2 text-wavis-text-secondary text-sm">
            <span className="animate-pulse">●</span>
            <span>Submitting bug report...</span>
          </div>
        )}

        {/* Step: Success */}
        {step === 'success' && (
          <div>
            <p className="text-wavis-accent text-sm mb-3">✓ Bug report submitted</p>
            {issueUrl && (
              <p className="text-sm mb-3">
                <span className="text-wavis-text-secondary">Issue: </span>
                <a
                  href={issueUrl}
                  target="_blank"
                  rel="noopener noreferrer"
                  className="text-wavis-accent hover:underline break-all"
                  onClick={handleIssueLinkClick}
                >
                  {issueUrl}
                </a>
              </p>
            )}
            <div className="flex justify-end">
              <button
                className="border border-wavis-accent text-wavis-accent hover:bg-wavis-accent hover:text-wavis-bg transition-colors px-4 py-1"
                onClick={handleCancel}
              >
                Close
              </button>
            </div>
          </div>
        )}

        {/* Step: Error */}
        {step === 'error' && (
          <div>
            <p className="text-wavis-danger text-sm mb-3">✗ Submission failed</p>
            <p className="text-sm text-wavis-text-secondary mb-3">{errorMessage}</p>
            <div className="flex justify-end gap-3">
              <button
                className="border border-wavis-text-secondary text-wavis-text-secondary hover:bg-wavis-text-secondary hover:text-wavis-bg transition-colors px-4 py-1"
                onClick={handleCancel}
              >
                Cancel
              </button>
              <button
                className="border border-wavis-accent text-wavis-accent hover:bg-wavis-accent hover:text-wavis-bg transition-colors px-4 py-1"
                onClick={handleRetry}
              >
                Retry
              </button>
            </div>
          </div>
        )}
      </div>
    </div>
  );
}

/* ─── Sub-components ────────────────────────────────────────────── */

function OfflineForm({
  form,
  onChange,
  onSubmit,
}: {
  form: OfflineFormData;
  onChange: (form: OfflineFormData) => void;
  onSubmit: () => void;
}) {
  return (
    <div className="space-y-3">
      <p className="text-xs text-wavis-text-secondary">
        Offline mode — LLM analysis unavailable
      </p>

      <div>
        <label className="text-xs text-wavis-text-secondary block mb-1">Category</label>
        <select
          className="w-full bg-wavis-bg border border-wavis-text-secondary outline-none px-2 py-1 font-mono text-wavis-text"
          value={form.category}
          onChange={(e) => onChange({ ...form, category: e.target.value })}
        >
          {OFFLINE_CATEGORIES.map((cat) => (
            <option key={cat} value={cat}>{cat}</option>
          ))}
        </select>
      </div>

      <div>
        <label className="text-xs text-wavis-text-secondary block mb-1">Steps to Reproduce</label>
        <textarea
          className="w-full bg-transparent border border-wavis-text-secondary outline-none px-2 py-1 font-mono text-wavis-text resize-y min-h-[60px]"
          value={form.stepsToReproduce}
          onChange={(e) => onChange({ ...form, stepsToReproduce: e.target.value })}
          placeholder="1. Open the app&#10;2. Click on...&#10;3. ..."
        />
      </div>

      <div>
        <label className="text-xs text-wavis-text-secondary block mb-1">Expected Behavior</label>
        <textarea
          className="w-full bg-transparent border border-wavis-text-secondary outline-none px-2 py-1 font-mono text-wavis-text resize-y min-h-[40px]"
          value={form.expectedBehavior}
          onChange={(e) => onChange({ ...form, expectedBehavior: e.target.value })}
          placeholder="What should have happened..."
        />
      </div>

      <div>
        <label className="text-xs text-wavis-text-secondary block mb-1">Actual Behavior</label>
        <textarea
          className="w-full bg-transparent border border-wavis-text-secondary outline-none px-2 py-1 font-mono text-wavis-text resize-y min-h-[40px]"
          value={form.actualBehavior}
          onChange={(e) => onChange({ ...form, actualBehavior: e.target.value })}
          placeholder="What actually happened..."
        />
      </div>

      <div className="flex justify-end mt-3">
        <button
          className="border border-wavis-accent text-wavis-accent hover:bg-wavis-accent hover:text-wavis-bg transition-colors px-4 py-1"
          onClick={onSubmit}
        >
          Next
        </button>
      </div>
    </div>
  );
}
