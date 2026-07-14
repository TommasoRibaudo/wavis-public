/**
 * Linux Desktop Capability Normalizers
 *
 * Pure normalization of raw Linux desktop-environment / session-type strings
 * into the keys used by the generated capability matrix, plus the fallback
 * row for unknown combinations.
 *
 * Lives beside linux-capability-matrix.ts (which is generated — do not add
 * hand-written code there). Extracted from voice-room.ts.
 */

import type { LinuxCapabilityRow } from './linux-capability-matrix';

export function normalizeLinuxDesktopEnv(desktopEnv: string): string {
  const tokens = desktopEnv
    .split(':')
    .map((token) => token.trim().toLowerCase())
    .filter((token) => token.length > 0);

  for (const token of tokens) {
    if (token.includes('gnome')) return 'gnome';
    if (token.includes('kde') || token.includes('plasma')) return 'kde';
    if (token.includes('sway')) return 'sway';
    if (token.includes('xfce')) return 'xfce';
  }

  return tokens[0] ?? 'unknown';
}

export function normalizeLinuxCompositor(sessionType: string): 'wayland' | 'x11' | null {
  const normalized = sessionType.trim().toLowerCase();
  if (normalized === 'wayland' || normalized === 'x11') {
    return normalized;
  }
  return null;
}

export function buildUnknownLinuxCapabilityRow(reason: string): LinuxCapabilityRow {
  return {
    desktopEnv: 'unknown',
    compositor: 'x11',
    videoCapture: 'unsupported',
    audioCapture: 'unsupported',
    combinedStatus: 'degraded',
    userMessage: reason,
  };
}
