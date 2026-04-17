import type { AuthLogEntry } from './auth';

const LOG_COLOR_MAP: Record<AuthLogEntry['type'], string> = {
  info: 'var(--wavis-text-secondary)',
  success: 'var(--wavis-accent)',
  warning: 'var(--wavis-warn)',
  error: 'var(--wavis-danger)',
};

export function logEntryColor(type: AuthLogEntry['type']): string {
  return LOG_COLOR_MAP[type];
}
