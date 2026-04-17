/**
 * Wavis Ring Buffer
 *
 * Generic fixed-size circular buffer for in-memory log collection.
 * Module-level singleton intercepts console.log/error/warn/info to
 * buffer log lines while selectively forwarding Wavis debug output.
 */

// ─── Types ─────────────────────────────────────────────────────────

export interface ConsoleLogEntry {
  level: 'log' | 'error' | 'warn' | 'info';
  message: string;
  timestamp: number;
}

// ─── RingBuffer ────────────────────────────────────────────────────

export class RingBuffer<T> {
  private buffer: (T | undefined)[];
  private head: number;
  private count: number;
  private readonly capacity: number;

  constructor(capacity: number) {
    if (capacity < 1) {
      throw new Error('RingBuffer capacity must be at least 1');
    }
    this.capacity = capacity;
    this.buffer = new Array<T | undefined>(capacity);
    this.head = 0;
    this.count = 0;
  }

  push(item: T): void {
    const index = (this.head + this.count) % this.capacity;
    if (this.count < this.capacity) {
      this.buffer[index] = item;
      this.count++;
    } else {
      // Buffer full — overwrite oldest entry
      this.buffer[this.head] = item;
      this.head = (this.head + 1) % this.capacity;
    }
  }

  snapshot(): T[] {
    const result: T[] = [];
    for (let i = 0; i < this.count; i++) {
      result.push(this.buffer[(this.head + i) % this.capacity] as T);
    }
    return result;
  }

  drain(): T[] {
    const result = this.snapshot();
    this.clear();
    return result;
  }

  get size(): number {
    return this.count;
  }

  clear(): void {
    this.buffer = new Array<T | undefined>(this.capacity);
    this.head = 0;
    this.count = 0;
  }
}

// ─── Constants ─────────────────────────────────────────────────────

const DEFAULT_CONSOLE_BUFFER_CAPACITY = 200;
const DEBUG_LOGS = import.meta.env.VITE_DEBUG_LOGS === 'true';
const DEBUG_CAPTURE = import.meta.env.VITE_DEBUG_SCREEN_CAPTURE === 'true';
const DEBUG_AUDIO_OUTPUT = import.meta.env.VITE_DEBUG_AUDIO_OUTPUT === 'true';
const DEBUG_NOISE_SUPPRESSION = import.meta.env.VITE_DEBUG_NOISE_SUPPRESSION === 'true';

// ─── Console Log Buffer Singleton ──────────────────────────────────

export const consoleLogBuffer = new RingBuffer<ConsoleLogEntry>(
  DEFAULT_CONSOLE_BUFFER_CAPACITY,
);

// ─── Console Monkey-Patching ───────────────────────────────────────

const originalConsoleLog = console.log;
const originalConsoleError = console.error;
const originalConsoleWarn = console.warn;
const originalConsoleInfo = console.info;

/** Exported for testing — access to original console methods. */
export const _originals = {
  log: originalConsoleLog,
  error: originalConsoleError,
  warn: originalConsoleWarn,
  info: originalConsoleInfo,
} as const;

function formatArgs(args: unknown[]): string {
  return args
    .map((a) => (typeof a === 'string' ? a : JSON.stringify(a)))
    .join(' ');
}

export function _shouldForwardForTest(level: ConsoleLogEntry['level'], args: unknown[]): boolean {
  if (level === 'error') return true;
  if (DEBUG_LOGS) return true;

  const firstArg = args[0];
  if (typeof firstArg !== 'string' || !firstArg.startsWith('[wavis:')) {
    return true;
  }

  const message = formatArgs(args);
  if (message.includes('[audio-output]')) return DEBUG_AUDIO_OUTPUT;
  if (message.includes('native capture:')) return DEBUG_CAPTURE;
  if (message.includes('[wavis:ns]')) return DEBUG_NOISE_SUPPRESSION;
  // Leak triage depends on these summaries even when the older capture debug
  // flag is off. Keep them always forwarded so Windows/browser-path repros do
  // not disappear behind the ring-buffer filter again.
  if (message.includes('[share-leak]')) return true;
  if (message.includes('[wasapi')) return true;

  return false;
}

console.log = (...args: unknown[]): void => {
  consoleLogBuffer.push({
    level: 'log',
    message: formatArgs(args),
    timestamp: Date.now(),
  });
  if (_shouldForwardForTest('log', args)) {
    originalConsoleLog.apply(console, args);
  }
};

console.error = (...args: unknown[]): void => {
  consoleLogBuffer.push({
    level: 'error',
    message: formatArgs(args),
    timestamp: Date.now(),
  });
  if (_shouldForwardForTest('error', args)) {
    originalConsoleError.apply(console, args);
  }
};

console.warn = (...args: unknown[]): void => {
  consoleLogBuffer.push({
    level: 'warn',
    message: formatArgs(args),
    timestamp: Date.now(),
  });
  if (_shouldForwardForTest('warn', args)) {
    originalConsoleWarn.apply(console, args);
  }
};

console.info = (...args: unknown[]): void => {
  consoleLogBuffer.push({
    level: 'info',
    message: formatArgs(args),
    timestamp: Date.now(),
  });
  if (_shouldForwardForTest('info', args)) {
    originalConsoleInfo.apply(console, args);
  }
};
