/**
 * Wavis WS Message Buffer
 *
 * Fixed-size circular buffer for formatted WebSocket message logs.
 * Stores sent and received message summaries with local timestamps.
 */

import { RingBuffer } from './ring-buffer';

// ─── Types ───────────────────────────────────────────────────────────────────

export type WsMessageDirection = 'sent' | 'received';

// ─── Constants ───────────────────────────────────────────────────────────────

const DEFAULT_WS_MESSAGE_BUFFER_CAPACITY = 50;
const SENT_ARROW = '→';
const RECEIVED_ARROW = '←';
const UNKNOWN_MESSAGE_TYPE = 'unknown';

// ─── WsMessageBuffer ─────────────────────────────────────────────────────────

export class WsMessageBuffer {
  private readonly buffer: RingBuffer<string>;

  constructor(capacity: number = DEFAULT_WS_MESSAGE_BUFFER_CAPACITY) {
    this.buffer = new RingBuffer<string>(capacity);
  }

  record(direction: WsMessageDirection, message: unknown): void {
    this.buffer.push(this.formatMessage(direction, message));
  }

  snapshot(): string[] {
    return this.buffer.snapshot();
  }

  drain(): string[] {
    return this.buffer.drain();
  }

  private formatMessage(direction: WsMessageDirection, message: unknown): string {
    const timestamp = new Date(Date.now());
    const hours = timestamp.getHours().toString().padStart(2, '0');
    const minutes = timestamp.getMinutes().toString().padStart(2, '0');
    const seconds = timestamp.getSeconds().toString().padStart(2, '0');
    const arrow = direction === 'sent' ? SENT_ARROW : RECEIVED_ARROW;
    const type = this.extractMessageType(message);

    return `[${hours}:${minutes}:${seconds}] ${arrow} {${type}}`;
  }

  private extractMessageType(message: unknown): string {
    if (message === null || typeof message !== 'object') {
      return UNKNOWN_MESSAGE_TYPE;
    }

    const type = (message as { type?: unknown }).type;
    return typeof type === 'string' && type.length > 0
      ? type
      : UNKNOWN_MESSAGE_TYPE;
  }
}

// ─── Singleton ───────────────────────────────────────────────────────────────

export const wsMessageBuffer = new WsMessageBuffer();
