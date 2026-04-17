import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';

import { WsMessageBuffer } from '../ws-message-buffer';

function freezeTime(hours: number, minutes: number, seconds: number): void {
  const d = new Date();
  d.setHours(hours, minutes, seconds, 0);
  vi.spyOn(Date, 'now').mockReturnValue(d.getTime());
}

describe('WsMessageBuffer', () => {
  beforeEach(() => {
    vi.restoreAllMocks();
  });

  afterEach(() => {
    vi.restoreAllMocks();
  });

  it('returns an empty snapshot when no messages were recorded', () => {
    const buffer = new WsMessageBuffer(10);

    expect(buffer.snapshot()).toEqual([]);
  });

  it('formats sent messages as [HH:MM:SS] -> {type}', () => {
    const timestamp = new Date(2026, 2, 16, 9, 5, 7).getTime();
    vi.spyOn(Date, 'now').mockReturnValue(timestamp);

    const buffer = new WsMessageBuffer(10);
    buffer.record('sent', { type: 'offer' });

    expect(buffer.snapshot()).toEqual(['[09:05:07] → {offer}']);
  });

  it('formats received messages as [HH:MM:SS] <- {type}', () => {
    const timestamp = new Date(2026, 2, 16, 9, 5, 7).getTime();
    vi.spyOn(Date, 'now').mockReturnValue(timestamp);

    const buffer = new WsMessageBuffer(10);
    buffer.record('received', { type: 'answer' });

    expect(buffer.snapshot()).toEqual(['[09:05:07] ← {answer}']);
  });

  it('extracts the type field from message objects', () => {
    freezeTime(12, 0, 0);

    const buffer = new WsMessageBuffer(10);
    buffer.record('sent', { type: 'ice_candidate', candidate: 'abc' });

    expect(buffer.snapshot()[0]).toContain('{ice_candidate}');
  });

  it('uses unknown when the message type is missing or invalid', () => {
    freezeTime(12, 0, 0);

    const buffer = new WsMessageBuffer(10);
    buffer.record('received', { data: 'raw' });
    buffer.record('received', null);
    buffer.record('received', 42);

    expect(buffer.snapshot()).toEqual([
      '[12:00:00] ← {unknown}',
      '[12:00:00] ← {unknown}',
      '[12:00:00] ← {unknown}',
    ]);
  });

  it('retains only the most recent messages up to capacity', () => {
    freezeTime(12, 0, 0);

    const buffer = new WsMessageBuffer(3);
    buffer.record('sent', { type: 'a' });
    buffer.record('sent', { type: 'b' });
    buffer.record('sent', { type: 'c' });
    buffer.record('sent', { type: 'd' });

    expect(buffer.snapshot()).toEqual([
      '[12:00:00] → {b}',
      '[12:00:00] → {c}',
      '[12:00:00] → {d}',
    ]);
  });

  it('snapshot does not clear buffered messages', () => {
    freezeTime(12, 0, 0);

    const buffer = new WsMessageBuffer(10);
    buffer.record('sent', { type: 'ping' });

    expect(buffer.snapshot()).toEqual(['[12:00:00] → {ping}']);
    expect(buffer.snapshot()).toEqual(['[12:00:00] → {ping}']);
  });

  it('drain returns buffered messages and clears the buffer', () => {
    freezeTime(12, 0, 0);

    const buffer = new WsMessageBuffer(10);
    buffer.record('sent', { type: 'ping' });
    buffer.record('received', { type: 'pong' });

    expect(buffer.drain()).toEqual([
      '[12:00:00] → {ping}',
      '[12:00:00] ← {pong}',
    ]);
    expect(buffer.snapshot()).toEqual([]);
  });

  it('uses a default capacity of 50 messages', () => {
    freezeTime(12, 0, 0);

    const buffer = new WsMessageBuffer();

    for (let index = 0; index < 55; index++) {
      buffer.record('sent', { type: `msg${index}` });
    }

    const snapshot = buffer.snapshot();
    expect(snapshot).toHaveLength(50);
    expect(snapshot[0]).toBe('[12:00:00] → {msg5}');
    expect(snapshot[49]).toBe('[12:00:00] → {msg54}');
  });
});
