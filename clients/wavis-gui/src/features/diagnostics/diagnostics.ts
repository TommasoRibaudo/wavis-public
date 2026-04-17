/**
 * Wavis Diagnostics Service
 *
 * Env-gated polling service for the diagnostics window. Collects:
 *   - Process tree RSS + CPU % (Rust IPC via sysinfo)
 *   - JS heap size (performance.memory, Chromium/WebView2 only)
 *   - DOM node count (main window, O(n) — acceptable at 1s intervals)
 *   - Network stats + MOS estimate (pushed from main window via Tauri events)
 *   - Audio levels (pushed from main window via Tauri events)
 *   - Screen share capture stats (pushed from main window via Tauri events)
 *   - Rolling 5-minute history (RingBuffer<DiagnosticsSnapshot>, 300 samples)
 *
 * Voice-room state (network, share, audio) is received via 'diagnostics:voice-stats'
 * events emitted by App.tsx in the main window. The diagnostics window runs in a
 * separate webview with its own JS context, so it cannot read voice-room.ts state
 * directly — direct module-level reads always return the initial empty defaults.
 *
 * Only active while the diagnostics window is open. Zero overhead otherwise.
 */

import { invoke } from '@tauri-apps/api/core';
import { listen } from '@tauri-apps/api/event';
import type { UnlistenFn } from '@tauri-apps/api/event';
import { isPermissionGranted, requestPermission, sendNotification } from '@tauri-apps/plugin-notification';
import { RingBuffer } from '@shared/ring-buffer';
import type { NetworkStats } from '@features/voice/voice-room';
import type { ShareStats, VideoReceiveStats } from '@features/voice/livekit-media';

const LOG = '[wavis:diagnostics]';

/* ─── Types ─────────────────────────────────────────────────────── */

export interface DiagnosticsConfig {
  enabled: boolean;
  notificationsEnabled: boolean;
  pollMs: number;
  memoryWarnMb: number;
  networkWarnMbps: number;
  renderWarnMs: number;
}

export interface DiagnosticsSnapshot {
  timestamp: number;
  rss: { mb: number; childCount: number } | null;
  /** JS heap size. Null on macOS (WKWebView does not expose performance.memory). */
  jsHeap: { usedMb: number; totalMb: number } | null;
  /** DOM node count from document.querySelectorAll('*').length.
   *  O(n) per poll — fine at ≥1s intervals. If the DOM bloats (the exact leak
   *  scenario), this call gets slower as the problem worsens. Phase 2 can replace
   *  with a MutationObserver counter for constant-time measurement. */
  domNodes: number;
  /** CPU % across all logical cores. Null on first call (sysinfo needs two samples). */
  cpuPercent: number | null;
  network: {
    rttMs: number;
    packetLossPercent: number;
    jitterMs: number;
    /** E-model MOS estimate (1.0–4.5). */
    mos: number;
    /** Current jitter buffer target delay in ms (0 = unavailable). */
    jitterBufferDelayMs: number;
    /** Concealment events (PLC) since the previous stats poll. */
    concealmentEventsPerInterval: number;
    /** ICE candidate type of the active connection. */
    candidateType: 'host' | 'srflx' | 'relay' | 'unknown';
    /** Estimated available outgoing bandwidth in kbps (0 = unavailable). */
    availableBandwidthKbps: number;
  } | null;
  /** Audio levels for local mic and remote participants. Null when not in a session. */
  audio: {
    localRms: number;
    localSpeaking: boolean;
    remoteSpeakingCount: number;
    participantCount: number;
  } | null;
  /** Updated at the share stats polling cadence (~5s), not the diagnostics cadence. */
  share: {
    bitrateKbps: number;
    fps: number;
    qualityLimitationReason: string;
    packetLossPercent: number;
    frameWidth: number;
    frameHeight: number;
    /** Delta PLIs since last poll. */
    pliCount: number;
    /** Delta NACKs since last poll. */
    nackCount: number;
    availableBandwidthKbps: number;
  } | null;
  /** Wall-clock HH:MM:SS when share last started. */
  shareStartedAt: string | null;
  /** Wall-clock HH:MM:SS when share last stopped. */
  shareStoppedAt: string | null;
  /** Live stats for an incoming screen share (viewer perspective). Null when not watching. */
  videoReceive: {
    fps: number;
    frameWidth: number;
    frameHeight: number;
    framesDropped: number;
    packetLossPercent: number;
    jitterBufferDelayMs: number;
    freezeCount: number;
    freezeDurationMs: number;
    pliCount: number;
    nackCount: number;
    avgDecodeTimeMs: number;
  } | null;
}

export interface DiagnosticsBaseline {
  snapshot: DiagnosticsSnapshot;
  capturedAt: number;
}

export interface WarningEntry {
  key: string;
  message: string;
  since: number;
  lastNotifiedAt: number;
}

/* ─── Rust IPC types (matching diagnostics.rs) ──────────────────── */

interface RustDiagnosticsConfig {
  enabled: boolean;
  notificationsEnabled: boolean;
  pollMs: number;
  memoryWarnMb: number;
  networkWarnMbps: number;
  renderWarnMs: number;
}

interface RustDiagnosticsSnapshot {
  rssMb: number;
  childCount: number;
  timestampMs: number;
  cpuUsagePercent: number;
}

/* ─── Voice stats payload (emitted by App.tsx in the main window) ── */

interface DiagnosticsVoiceStatsPayload {
  networkStats: NetworkStats;
  shareStats: ShareStats | null;
  videoReceiveStats: VideoReceiveStats | null;
  participants: Array<{ id: string; rmsLevel: number; isSpeaking: boolean }>;
  selfParticipantId: string | null;
}

/* ─── Module state ──────────────────────────────────────────────── */

let config: DiagnosticsConfig | null = null;
let baseline: DiagnosticsBaseline | null = null;
let pollInterval: ReturnType<typeof setInterval> | null = null;
let onUpdate: ((snap: DiagnosticsSnapshot, warnings: WarningEntry[], history: DiagnosticsSnapshot[] | null) => void) | null = null;
const warnings = new Map<string, WarningEntry>();

/** Rolling 5-minute history at 1s cadence (300 samples). */
const historyBuffer = new RingBuffer<DiagnosticsSnapshot>(300);

/** Tracks whether the first Rust poll has completed (CPU needs two samples for a delta). */
let isFirstCpuPoll = true;

/** Poll counter — used to throttle history state pushes to the UI (every 5th poll). */
let pollCount = 0;

// Share lifecycle event markers (updated by voice-room event observation)
let shareStartedAt: string | null = null;
let shareStoppedAt: string | null = null;
let prevWasSharing = false;

/** Latest voice-room stats received from the main window via 'diagnostics:voice-stats' event. */
let cachedVoiceStats: DiagnosticsVoiceStatsPayload | null = null;

/** Unlisten function for the 'diagnostics:voice-stats' event listener. */
let unlistenVoiceStats: UnlistenFn | null = null;

const WARN_SUSTAIN_MS = 8_000;
const WARN_COOLDOWN_MS = 60_000;

/* ─── Helpers ───────────────────────────────────────────────────── */

function formatTime(date: Date): string {
  return date.toTimeString().slice(0, 8); // HH:MM:SS
}

function readJsHeap(): DiagnosticsSnapshot['jsHeap'] {
  // performance.memory is Chromium-specific (available on Windows WebView2).
  // WKWebView (macOS) does not expose it — return null rather than undefined.
  const mem = (performance as { memory?: { usedJSHeapSize: number; totalJSHeapSize: number } }).memory;
  if (!mem) return null;
  return {
    usedMb: mem.usedJSHeapSize / 1024 / 1024,
    totalMb: mem.totalJSHeapSize / 1024 / 1024,
  };
}

/**
 * Simplified E-model MOS estimate.
 * Returns a score in the range [1.0, 4.5].
 * Formula: R-value → MOS via ITU-T G.107 approximation.
 */
function estimateMos(rttMs: number, lossPercent: number, jitterMs: number): number {
  const effectiveLatency = rttMs / 2 + jitterMs * 2 + 10;
  const r = 93.2 - effectiveLatency / 40 - lossPercent * 2.5;
  const clamped = Math.max(0, Math.min(100, r));
  const mos = 1 + 0.035 * clamped + 7e-6 * clamped * (clamped - 60) * (100 - clamped);
  return Math.round(Math.max(1, Math.min(4.5, mos)) * 10) / 10;
}

export function mosLabel(mos: number): string {
  if (mos >= 4.3) return 'Excellent';
  if (mos >= 4.0) return 'Good';
  if (mos >= 3.6) return 'Fair';
  if (mos >= 3.1) return 'Poor';
  return 'Bad';
}

function checkWarning(
  key: string,
  message: string,
  condition: boolean,
  notificationsEnabled: boolean,
): void {
  if (condition) {
    if (!warnings.has(key)) {
      warnings.set(key, { key, message, since: Date.now(), lastNotifiedAt: 0 });
    } else {
      const entry = warnings.get(key)!;
      const sustainedMs = Date.now() - entry.since;
      if (
        notificationsEnabled &&
        sustainedMs >= WARN_SUSTAIN_MS &&
        Date.now() - entry.lastNotifiedAt >= WARN_COOLDOWN_MS
      ) {
        entry.lastNotifiedAt = Date.now();
        fireNotification(message).catch(() => {});
      }
    }
  } else {
    warnings.delete(key);
  }
}

async function fireNotification(body: string): Promise<void> {
  try {
    let permitted = await isPermissionGranted();
    if (!permitted) {
      const result = await requestPermission();
      permitted = result === 'granted';
    }
    if (!permitted) return;
    sendNotification({ title: 'Wavis Diagnostics', body });
  } catch {
    // Silent failure on platforms where notifications are unavailable
  }
}

/* ─── Public API ────────────────────────────────────────────────── */

/**
 * Initialise the diagnostics polling loop.
 * Reads env-based config via IPC, then polls at config.pollMs.
 * Returns the resolved config (with enabled=false if the env var is unset).
 */
export async function initDiagnostics(
  cb: (snap: DiagnosticsSnapshot, warnings: WarningEntry[], history: DiagnosticsSnapshot[] | null) => void,
): Promise<DiagnosticsConfig> {
  destroyDiagnostics();

  const raw = await invoke<RustDiagnosticsConfig>('get_diagnostics_config');
  config = {
    enabled: raw.enabled,
    notificationsEnabled: raw.notificationsEnabled,
    pollMs: raw.pollMs,
    memoryWarnMb: raw.memoryWarnMb,
    networkWarnMbps: raw.networkWarnMbps,
    renderWarnMs: raw.renderWarnMs,
  };
  onUpdate = cb;

  // Subscribe to voice-room stats pushed by the main window.
  // The diagnostics window is a separate webview — direct voice-room module state
  // is always empty here. App.tsx emits this event at 1s from the main window.
  unlistenVoiceStats = await listen<DiagnosticsVoiceStatsPayload>(
    'diagnostics:voice-stats',
    (event) => { cachedVoiceStats = event.payload; },
  );

  // VITE_DIAGNOSTICS=true is the build-time gate; the Rust `enabled` flag is
  // unreliable in release builds (dotenvy only loads .env in debug mode).
  pollInterval = setInterval(poll, config.pollMs);
  // Run one poll immediately so the UI isn't blank for the first pollMs
  poll();

  return config;
}

/** Stop the polling loop and reset all module state. */
export function destroyDiagnostics(): void {
  if (pollInterval !== null) {
    clearInterval(pollInterval);
    pollInterval = null;
  }
  onUpdate = null;
  warnings.clear();
  historyBuffer.clear();
  isFirstCpuPoll = true;
  pollCount = 0;
  prevWasSharing = false;
  shareStartedAt = null;
  shareStoppedAt = null;
  unlistenVoiceStats?.();
  unlistenVoiceStats = null;
  cachedVoiceStats = null;
}

/** Store a baseline snapshot for delta display. */
export function setBaseline(snap: DiagnosticsSnapshot): void {
  baseline = { snapshot: snap, capturedAt: Date.now() };
}

/** Clear the stored baseline. */
export function clearBaseline(): void {
  baseline = null;
}

/** Return the current baseline, or null if none is set. */
export function getBaseline(): DiagnosticsBaseline | null {
  return baseline;
}

/**
 * Serialise a snapshot + history into human-readable plaintext for clipboard export.
 * Includes baseline deltas when available. History is downsampled to every 5th sample
 * (matching the chart display resolution) to keep the export concise.
 */
export function exportSnapshot(snap: DiagnosticsSnapshot): string {
  const now = new Date();
  const lines: string[] = [];

  const pad = (label: string, value: string): string =>
    `  ${label.padEnd(18)} ${value}`;

  const deltaStr = (current: number, base: number | undefined, unit = ''): string => {
    if (base === undefined) return `${current.toFixed(1)}${unit}`;
    const d = current - base;
    const sign = d >= 0 ? '+' : '';
    return `${current.toFixed(1)}${unit}  (${sign}${d.toFixed(1)}${unit} from baseline)`;
  };

  lines.push('=== WAVIS DIAGNOSTICS SNAPSHOT ===');
  lines.push(`Captured: ${now.toISOString()}`);
  lines.push('');

  // Memory
  lines.push('[MEMORY]');
  if (snap.rss) {
    lines.push(pad('Process RSS:', deltaStr(snap.rss.mb, baseline?.snapshot.rss?.mb, ' MB')));
    lines.push(pad('Child processes:', String(snap.rss.childCount)));
  } else {
    lines.push(pad('Process RSS:', 'Unavailable'));
  }
  if (snap.jsHeap) {
    lines.push(pad('JS Heap:', `${snap.jsHeap.usedMb.toFixed(1)} MB used / ${snap.jsHeap.totalMb.toFixed(1)} MB total`));
  } else {
    lines.push(pad('JS Heap:', 'N/A (macOS)'));
  }
  lines.push(pad('DOM Nodes:', (() => {
    const base = baseline?.snapshot.domNodes;
    const d = base !== undefined ? ` (${snap.domNodes - base >= 0 ? '+' : ''}${snap.domNodes - base} from baseline)` : '';
    return `${snap.domNodes.toLocaleString()}${d}`;
  })()));
  if (snap.cpuPercent !== null) {
    lines.push(pad('CPU Usage:', `${snap.cpuPercent.toFixed(1)}%`));
  }
  lines.push('');

  // Network
  lines.push('[NETWORK]');
  if (snap.network) {
    lines.push(pad('RTT:', `${Math.round(snap.network.rttMs)} ms`));
    lines.push(pad('Packet Loss:', `${snap.network.packetLossPercent.toFixed(1)}%`));
    lines.push(pad('Jitter:', `${Math.round(snap.network.jitterMs)} ms`));
    lines.push(pad('MOS (est.):', `${snap.network.mos.toFixed(1)}  (${mosLabel(snap.network.mos)})`));
    lines.push(pad('Jitter Buffer:', snap.network.jitterBufferDelayMs > 0 ? `${snap.network.jitterBufferDelayMs} ms` : 'N/A'));
    lines.push(pad('Concealment:', `${snap.network.concealmentEventsPerInterval} events/interval`));
    lines.push(pad('Bandwidth:', snap.network.availableBandwidthKbps > 0 ? `${(snap.network.availableBandwidthKbps / 1000).toFixed(1)} Mbps avail.` : 'N/A'));
    lines.push(pad('Candidate:', snap.network.candidateType));
  } else {
    lines.push(pad('Status:', 'No active session'));
  }
  lines.push('');

  // Audio
  lines.push('[AUDIO]');
  if (snap.audio) {
    lines.push(pad('Local RMS:', `${snap.audio.localRms.toFixed(3)}  (${snap.audio.localSpeaking ? 'speaking' : 'silent'})`));
    lines.push(pad('Remote Speaking:', `${snap.audio.remoteSpeakingCount}/${snap.audio.participantCount - 1}`));
  } else {
    lines.push(pad('Status:', 'No active session'));
  }
  lines.push('');

  // Screen Share
  lines.push('[SCREEN SHARE]');
  if (snap.share) {
    lines.push(pad('Bitrate:', `${(snap.share.bitrateKbps / 1000).toFixed(1)} Mbps`));
    lines.push(pad('FPS:', snap.share.fps.toFixed(1)));
    lines.push(pad('Resolution:', snap.share.frameWidth > 0 ? `${snap.share.frameWidth}×${snap.share.frameHeight}` : 'N/A'));
    lines.push(pad('Quality Limit:', snap.share.qualityLimitationReason || 'none'));
    lines.push(pad('Outbound Loss:', `${snap.share.packetLossPercent.toFixed(1)}%`));
    lines.push(pad('PLIs/interval:', String(snap.share.pliCount)));
    lines.push(pad('NACKs/interval:', String(snap.share.nackCount)));
    lines.push(pad('Bandwidth:', snap.share.availableBandwidthKbps > 0 ? `${(snap.share.availableBandwidthKbps / 1000).toFixed(1)} Mbps avail.` : 'N/A'));
    if (snap.shareStartedAt) {
      lines.push(pad('Started:', snap.shareStartedAt));
    }
    if (snap.shareStoppedAt) {
      lines.push(pad('Stopped:', snap.shareStoppedAt));
    }
  } else {
    lines.push(pad('Status:', 'Not sharing'));
  }
  lines.push('');

  // Video Receive
  lines.push('[SCREEN SHARE RECEIVED]');
  if (snap.videoReceive) {
    lines.push(pad('FPS:', snap.videoReceive.fps.toFixed(1)));
    lines.push(pad('Resolution:', snap.videoReceive.frameWidth > 0 ? `${snap.videoReceive.frameWidth}×${snap.videoReceive.frameHeight}` : 'N/A'));
    lines.push(pad('Inbound loss:', `${snap.videoReceive.packetLossPercent.toFixed(1)}%`));
    lines.push(pad('Jitter buffer:', snap.videoReceive.jitterBufferDelayMs > 0 ? `${snap.videoReceive.jitterBufferDelayMs} ms` : 'N/A'));
    lines.push(pad('Frames dropped:', String(snap.videoReceive.framesDropped)));
    lines.push(pad('Freeze events:', String(snap.videoReceive.freezeCount)));
    lines.push(pad('Freeze time:', `${snap.videoReceive.freezeDurationMs} ms`));
    lines.push(pad('PLIs sent:', String(snap.videoReceive.pliCount)));
    lines.push(pad('NACKs sent:', String(snap.videoReceive.nackCount)));
    lines.push(pad('Avg decode:', snap.videoReceive.avgDecodeTimeMs > 0 ? `${snap.videoReceive.avgDecodeTimeMs.toFixed(1)} ms` : 'N/A'));
  } else {
    lines.push(pad('Status:', 'Not watching a share'));
  }
  lines.push('');

  // History CSV (every 5th sample ≈ 5s resolution, 60 rows for 5 min)
  const history = historyBuffer.snapshot().filter((_, i) => i % 5 === 0);
  if (history.length > 0) {
    lines.push(`[HISTORY — ${history.length} samples, ~5s interval]`);
    lines.push('timestamp,rtt_ms,loss_pct,jitter_ms,mos,jitter_buf_ms,concealment,bw_kbps,rss_mb,cpu_pct,share_fps,share_res,share_pli,share_nack');
    for (const h of history) {
      const t = new Date(h.timestamp).toTimeString().slice(0, 8);
      const rtt = h.network ? Math.round(h.network.rttMs) : '';
      const loss = h.network ? h.network.packetLossPercent.toFixed(1) : '';
      const jitter = h.network ? Math.round(h.network.jitterMs) : '';
      const mos = h.network ? h.network.mos.toFixed(1) : '';
      const jb = h.network ? h.network.jitterBufferDelayMs : '';
      const conc = h.network ? h.network.concealmentEventsPerInterval : '';
      const bw = h.network ? h.network.availableBandwidthKbps : '';
      const rss = h.rss ? h.rss.mb.toFixed(1) : '';
      const cpu = h.cpuPercent !== null ? h.cpuPercent.toFixed(1) : '';
      const fps = h.share ? h.share.fps.toFixed(1) : '';
      const res = h.share && h.share.frameWidth > 0 ? `${h.share.frameWidth}x${h.share.frameHeight}` : '';
      const pli = h.share ? h.share.pliCount : '';
      const nack = h.share ? h.share.nackCount : '';
      lines.push(`${t},${rtt},${loss},${jitter},${mos},${jb},${conc},${bw},${rss},${cpu},${fps},${res},${pli},${nack}`);
    }
  }

  return lines.join('\n');
}

/* ─── Poll loop ─────────────────────────────────────────────────── */

async function poll(): Promise<void> {
  if (!config || !onUpdate) return;

  // 1. RSS + CPU from Rust (process tree: main + webview children)
  let rss: DiagnosticsSnapshot['rss'] = null;
  let cpuPercent: number | null = null;
  try {
    const raw = await invoke<RustDiagnosticsSnapshot>('get_diagnostics_snapshot');
    rss = { mb: raw.rssMb, childCount: raw.childCount };
    // First Rust call returns 0.0 for CPU (sysinfo needs two samples for a delta).
    // Use a flag rather than checking > 0 — a genuinely idle system can report ~0.0.
    if (isFirstCpuPoll) {
      isFirstCpuPoll = false;
      cpuPercent = null;
    } else {
      cpuPercent = raw.cpuUsagePercent;
    }
  } catch (err) {
    console.warn(LOG, 'get_diagnostics_snapshot failed:', err);
  }

  // 2. JS heap (Chromium/WebView2 only — null on macOS)
  const jsHeap = readJsHeap();

  // 3. DOM node count (O(n); tracks zombie DOM trees and unreleased video/canvas elements)
  const domNodes = document.querySelectorAll('*').length;

  // 4. Network + share + audio stats received from main window via event.
  // cachedVoiceStats is null until the first 'diagnostics:voice-stats' event arrives
  // (~1s after the diagnostics window opens).
  const voiceStats = cachedVoiceStats;
  const networkStats = voiceStats?.networkStats ?? {
    rttMs: 0,
    packetLossPercent: 0,
    jitterMs: 0,
    jitterBufferDelayMs: 0,
    concealmentEventsPerInterval: 0,
    candidateType: 'unknown' as const,
    availableBandwidthKbps: 0,
  };
  const shareStats = voiceStats?.shareStats ?? null;
  const videoReceiveStats = voiceStats?.videoReceiveStats ?? null;
  const participants = voiceStats?.participants ?? [];
  const selfParticipantId = voiceStats?.selfParticipantId ?? null;

  // Show network block whenever we're in a session (selfParticipantId is non-null).
  // Avoid gating on rttMs > 0: the subscriber PC often returns RTT=0 on cycles where
  // the publisher hasn't been polled yet, causing the section to flicker every 10s.
  const network =
    selfParticipantId !== null
      ? {
          rttMs: networkStats.rttMs,
          packetLossPercent: networkStats.packetLossPercent,
          jitterMs: networkStats.jitterMs,
          mos: estimateMos(networkStats.rttMs, networkStats.packetLossPercent, networkStats.jitterMs),
          jitterBufferDelayMs: networkStats.jitterBufferDelayMs,
          concealmentEventsPerInterval: networkStats.concealmentEventsPerInterval,
          candidateType: networkStats.candidateType,
          availableBandwidthKbps: networkStats.availableBandwidthKbps,
        }
      : null;

  const share = shareStats
    ? {
        bitrateKbps: shareStats.bitrateKbps,
        fps: shareStats.fps,
        qualityLimitationReason: shareStats.qualityLimitationReason,
        packetLossPercent: shareStats.packetLossPercent,
        frameWidth: shareStats.frameWidth,
        frameHeight: shareStats.frameHeight,
        pliCount: shareStats.pliCount,
        nackCount: shareStats.nackCount,
        availableBandwidthKbps: shareStats.availableBandwidthKbps,
      }
    : null;

  // 5. Audio levels: use participant state from main window
  let audio: DiagnosticsSnapshot['audio'] = null;
  if (participants.length > 0) {
    const selfP = participants.find((p) => p.id === selfParticipantId);
    const remoteSpeaking = participants.filter(
      (p) => p.id !== selfParticipantId && p.isSpeaking,
    ).length;
    audio = {
      localRms: selfP?.rmsLevel ?? 0,
      localSpeaking: selfP?.isSpeaking ?? false,
      remoteSpeakingCount: remoteSpeaking,
      participantCount: participants.length,
    };
  }

  // 6. Track share lifecycle events for correlation with RSS deltas
  const isSharing = shareStats !== null;
  if (isSharing && !prevWasSharing) {
    shareStartedAt = formatTime(new Date());
    shareStoppedAt = null;
  } else if (!isSharing && prevWasSharing) {
    shareStoppedAt = formatTime(new Date());
  }
  prevWasSharing = isSharing;

  const videoReceive = videoReceiveStats
    ? {
        fps: videoReceiveStats.fps,
        frameWidth: videoReceiveStats.frameWidth,
        frameHeight: videoReceiveStats.frameHeight,
        framesDropped: videoReceiveStats.framesDropped,
        packetLossPercent: videoReceiveStats.packetLossPercent,
        jitterBufferDelayMs: videoReceiveStats.jitterBufferDelayMs,
        freezeCount: videoReceiveStats.freezeCount,
        freezeDurationMs: videoReceiveStats.freezeDurationMs,
        pliCount: videoReceiveStats.pliCount,
        nackCount: videoReceiveStats.nackCount,
        avgDecodeTimeMs: videoReceiveStats.avgDecodeTimeMs,
      }
    : null;

  const snap: DiagnosticsSnapshot = {
    timestamp: Date.now(),
    rss,
    jsHeap,
    domNodes,
    cpuPercent,
    network,
    audio,
    share,
    shareStartedAt,
    shareStoppedAt,
    videoReceive,
  };

  // 7. Push to rolling history buffer
  historyBuffer.push(snap);
  pollCount++;

  // 8. Warnings state machine
  const notif = config.notificationsEnabled;
  checkWarning(
    'rss_high',
    `Process memory high (${Math.round(rss?.mb ?? 0)} MB > ${config.memoryWarnMb} MB)`,
    rss !== null && rss.mb > config.memoryWarnMb,
    notif,
  );
  checkWarning(
    'network_loss_high',
    `Packet loss high (${network?.packetLossPercent.toFixed(1) ?? '?'}%)`,
    network !== null && network.packetLossPercent > 5,
    notif,
  );
  checkWarning(
    'share_bw_limited',
    'Screen share is bandwidth-limited',
    share !== null && share.qualityLimitationReason === 'bandwidth',
    notif,
  );
  checkWarning(
    'jitter_buffer_high',
    `Voice lag high — jitter buffer delay ${network?.jitterBufferDelayMs ?? 0} ms`,
    network !== null && network.jitterBufferDelayMs > 150,
    notif,
  );
  checkWarning(
    'concealment_high',
    `Audio quality degraded — ${network?.concealmentEventsPerInterval ?? 0} concealment events`,
    network !== null && network.concealmentEventsPerInterval > 10,
    notif,
  );
  checkWarning(
    'share_resolution_low',
    'Screen share quality reduced — resolution downgraded',
    share !== null && share.frameHeight > 0 && share.frameHeight < 720,
    notif,
  );
  checkWarning(
    'video_recv_frozen',
    `Received video is freezing — ${snap.videoReceive?.freezeCount ?? 0} freeze events`,
    snap.videoReceive !== null && snap.videoReceive.freezeCount > 0,
    notif,
  );
  checkWarning(
    'video_recv_loss_high',
    `Received video packet loss high (${snap.videoReceive?.packetLossPercent.toFixed(1) ?? '?'}%)`,
    snap.videoReceive !== null && snap.videoReceive.packetLossPercent > 5,
    notif,
  );

  // Pass history snapshot to UI every 5th poll (~5s, matching chart resolution)
  // to avoid 300-element array copies into React state at 1Hz.
  const historySnapshot = pollCount % 5 === 0 ? historyBuffer.snapshot() : null;
  onUpdate(snap, [...warnings.values()], historySnapshot);
}
