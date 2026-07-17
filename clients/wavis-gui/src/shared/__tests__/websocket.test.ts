/**
 * Tests for SignalingClient (websocket.ts).
 *
 * Mocks the global WebSocket constructor so no real network connections are made.
 * Uses vi.useFakeTimers() to control reconnect/keepalive timers deterministically.
 */

import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';
import { SignalingClient } from '../websocket';

// ─── Mock auth module ──────────────────────────────────────────────

vi.mock('@features/auth/auth', () => ({
  getAccessToken: vi.fn().mockResolvedValue('test-access-token'),
  isTokenExpired: vi.fn().mockResolvedValue(false),
  refreshTokens: vi.fn().mockResolvedValue({ status: 'success' }),
}));

// ─── Mock ws-message-buffer ────────────────────────────────────────

vi.mock('../ws-message-buffer', () => ({
  wsMessageBuffer: { record: vi.fn() },
}));

// ─── WebSocket mock factory ────────────────────────────────────────

type WsEventName = 'open' | 'message' | 'close' | 'error';

interface MockWsInstance {
  readyState: number;
  onopen: (() => void) | null;
  onmessage: ((e: { data: string }) => void) | null;
  onclose: (() => void) | null;
  onerror: (() => void) | null;
  send: ReturnType<typeof vi.fn>;
  close: ReturnType<typeof vi.fn>;
  /** Simulate an event from the server side. */
  simulate(event: WsEventName, data?: unknown): void;
}

let lastWsInstance: MockWsInstance | null = null;
let wsConstructorCalls: string[] = [];

function createMockWs(): typeof WebSocket {
  // Must be a real class so `new WebSocket(url)` works
  class MockWsClass {
    readyState: number;
    onopen: (() => void) | null = null;
    onmessage: ((e: { data: string }) => void) | null = null;
    onclose: (() => void) | null = null;
    onerror: (() => void) | null = null;
    send = vi.fn();
    close = vi.fn().mockImplementation(() => {
      this.readyState = MockWsClass.CLOSED;
      this.onclose?.();
    });

    static CONNECTING = 0;
    static OPEN = 1;
    static CLOSING = 2;
    static CLOSED = 3;

    constructor(url: string) {
      wsConstructorCalls.push(url);
      this.readyState = MockWsClass.CONNECTING;
      lastWsInstance = this;
    }

    simulate(event: WsEventName, data?: unknown) {
      if (event === 'open') {
        this.readyState = MockWsClass.OPEN;
        this.onopen?.();
      } else if (event === 'message') {
        this.onmessage?.({ data: JSON.stringify(data) });
      } else if (event === 'close') {
        this.readyState = MockWsClass.CLOSED;
        this.onclose?.();
      } else if (event === 'error') {
        this.onerror?.();
      }
    }
  }

  return MockWsClass as unknown as typeof WebSocket;
}

// ─── Test setup ───────────────────────────────────────────────────

beforeEach(() => {
  vi.useFakeTimers();
  lastWsInstance = null;
  wsConstructorCalls = [];
  vi.stubGlobal('WebSocket', createMockWs());
});

afterEach(() => {
  vi.useRealTimers();
  vi.unstubAllGlobals();
  vi.restoreAllMocks();
});

// ─── connect() ────────────────────────────────────────────────────

describe('connect()', () => {
  it('sets status to connecting immediately', () => {
    const client = new SignalingClient();
    const statuses: string[] = [];
    client.onStatusChange((s) => statuses.push(s));
    client.connect('ws://localhost/ws');
    expect(statuses).toContain('connecting');
  });

  it('sets status to connected on open', () => {
    const client = new SignalingClient();
    const statuses: string[] = [];
    client.onStatusChange((s) => statuses.push(s));
    client.connect('ws://localhost/ws');
    lastWsInstance!.simulate('open');
    expect(statuses).toContain('connected');
    expect(client.status).toBe('connected');
  });

  it('dispatches parsed messages to registered handlers', () => {
    const client = new SignalingClient();
    const received: unknown[] = [];
    client.onMessage((m) => received.push(m));
    client.connect('ws://localhost/ws');
    lastWsInstance!.simulate('open');
    lastWsInstance!.simulate('message', { type: 'joined', peerId: 'p1' });
    expect(received).toHaveLength(1);
    expect((received[0] as Record<string, unknown>).type).toBe('joined');
  });

  it('dispatches to multiple handlers', () => {
    const client = new SignalingClient();
    const a: unknown[] = [];
    const b: unknown[] = [];
    client.onMessage((m) => a.push(m));
    client.onMessage((m) => b.push(m));
    client.connect('ws://localhost/ws');
    lastWsInstance!.simulate('open');
    lastWsInstance!.simulate('message', { type: 'ping' });
    expect(a).toHaveLength(1);
    expect(b).toHaveLength(1);
  });

  it('unsubscribe removes handler', () => {
    const client = new SignalingClient();
    const received: unknown[] = [];
    const unsub = client.onMessage((m) => received.push(m));
    client.connect('ws://localhost/ws');
    lastWsInstance!.simulate('open');
    unsub();
    lastWsInstance!.simulate('message', { type: 'ping' });
    expect(received).toHaveLength(0);
  });

  it('ignores malformed JSON without throwing', () => {
    const client = new SignalingClient();
    client.connect('ws://localhost/ws');
    lastWsInstance!.simulate('open');
    // Simulate raw malformed message
    lastWsInstance!.onmessage?.({ data: 'not-json{{' });
    // No throw — test passes if we reach here
  });
});

// ─── disconnect() ─────────────────────────────────────────────────

describe('disconnect()', () => {
  it('sets status to disconnected', () => {
    const client = new SignalingClient();
    client.connect('ws://localhost/ws');
    lastWsInstance!.simulate('open');
    client.disconnect();
    expect(client.status).toBe('disconnected');
  });

  it('does not schedule reconnect after intentional disconnect', () => {
    const client = new SignalingClient();
    client.connect('ws://localhost/ws');
    lastWsInstance!.simulate('open');
    const prevCallCount = wsConstructorCalls.length;
    client.disconnect();
    vi.advanceTimersByTime(60_000);
    expect(wsConstructorCalls.length).toBe(prevCallCount);
  });

  it('calling disconnect when not connected is safe', () => {
    const client = new SignalingClient();
    expect(() => client.disconnect()).not.toThrow();
  });
});

// ─── send() ───────────────────────────────────────────────────────

describe('send()', () => {
  it('sends JSON-serialized message when connected', () => {
    const client = new SignalingClient();
    client.connect('ws://localhost/ws');
    lastWsInstance!.simulate('open');
    client.send({ type: 'ping' });
    expect(lastWsInstance!.send).toHaveBeenCalledWith(JSON.stringify({ type: 'ping' }));
  });

  it('does not send when not connected', () => {
    const client = new SignalingClient();
    client.connect('ws://localhost/ws');
    // Not yet open — readyState is CONNECTING
    client.send({ type: 'ping' });
    expect(lastWsInstance!.send).not.toHaveBeenCalled();
  });

  it('does not throw when ws is null', () => {
    const client = new SignalingClient();
    expect(() => client.send({ type: 'ping' })).not.toThrow();
  });
});

// ─── onStatusChange() ─────────────────────────────────────────────

describe('onStatusChange()', () => {
  it('fires on connecting → connected transition', () => {
    const client = new SignalingClient();
    const statuses: string[] = [];
    client.onStatusChange((s) => statuses.push(s));
    client.connect('ws://localhost/ws');
    lastWsInstance!.simulate('open');
    expect(statuses).toEqual(['connecting', 'connected']);
  });

  it('fires disconnected on close', () => {
    const client = new SignalingClient();
    const statuses: string[] = [];
    client.onStatusChange((s) => statuses.push(s));
    client.connect('ws://localhost/ws');
    lastWsInstance!.simulate('open');
    client.disconnect();
    expect(statuses).toContain('disconnected');
  });

  it('unsubscribe stops status callbacks', () => {
    const client = new SignalingClient();
    const statuses: string[] = [];
    const unsub = client.onStatusChange((s) => statuses.push(s));
    client.connect('ws://localhost/ws');
    unsub();
    lastWsInstance!.simulate('open');
    // 'connecting' was fired before unsub; 'connected' should not be
    expect(statuses).not.toContain('connected');
  });

  it('second onStatusChange call replaces the first handler', () => {
    const client = new SignalingClient();
    const first: string[] = [];
    const second: string[] = [];
    client.onStatusChange((s) => first.push(s));
    client.onStatusChange((s) => second.push(s));
    client.connect('ws://localhost/ws');
    lastWsInstance!.simulate('open');
    // Only the second handler should receive 'connected'
    expect(second).toContain('connected');
    expect(first).not.toContain('connected');
  });
});

// ─── keepalive ────────────────────────────────────────────────────

describe('keepalive', () => {
  it('sends ping after 30s when connected', () => {
    const client = new SignalingClient();
    client.connect('ws://localhost/ws');
    lastWsInstance!.simulate('open');
    vi.advanceTimersByTime(30_000);
    const calls = lastWsInstance!.send.mock.calls.map((c) => JSON.parse(c[0] as string));
    expect(calls.some((m) => m.type === 'ping')).toBe(true);
  });

  it('stops sending pings after disconnect', () => {
    const client = new SignalingClient();
    client.connect('ws://localhost/ws');
    lastWsInstance!.simulate('open');
    client.disconnect();
    const callsBefore = lastWsInstance!.send.mock.calls.length;
    vi.advanceTimersByTime(60_000);
    expect(lastWsInstance!.send.mock.calls.length).toBe(callsBefore);
  });
});

// ─── reconnect (exponential backoff) ──────────────────────────────

describe('reconnect backoff', () => {
  it('reconnects after connection drop (attempt 1 = 1s delay)', async () => {
    const client = new SignalingClient();
    client.connect('ws://localhost/ws');
    lastWsInstance!.simulate('open');
    const prevCount = wsConstructorCalls.length;

    // Simulate unexpected close (not intentional)
    lastWsInstance!.readyState = WebSocket.CLOSED;
    lastWsInstance!.onclose?.();

    // Advance past the first reconnect delay (2^0 * 1000 = 1000ms)
    await vi.advanceTimersByTimeAsync(1100);
    expect(wsConstructorCalls.length).toBeGreaterThan(prevCount);
  });

  it('does not reconnect after intentional disconnect', async () => {
    const client = new SignalingClient();
    client.connect('ws://localhost/ws');
    lastWsInstance!.simulate('open');
    const prevCount = wsConstructorCalls.length;
    client.disconnect();
    await vi.advanceTimersByTimeAsync(60_000);
    expect(wsConstructorCalls.length).toBe(prevCount);
  });

  it('resets reconnect attempt counter on successful connect', async () => {
    const client = new SignalingClient();
    client.connect('ws://localhost/ws');
    lastWsInstance!.simulate('open');

    // Drop and reconnect
    lastWsInstance!.readyState = WebSocket.CLOSED;
    lastWsInstance!.onclose?.();
    await vi.advanceTimersByTimeAsync(1100);

    // New WS opened — simulate it connecting
    lastWsInstance!.simulate('open');

    // reconnectAttempt should be reset to 0 — next drop uses 1s delay again
    const prevCount = wsConstructorCalls.length;
    lastWsInstance!.readyState = WebSocket.CLOSED;
    lastWsInstance!.onclose?.();
    await vi.advanceTimersByTimeAsync(1100);
    expect(wsConstructorCalls.length).toBeGreaterThan(prevCount);
  });

  it('clears reconnectTimer once the scheduled reconnect attempt actually fires', async () => {
    const client = new SignalingClient();
    client.connect('ws://localhost/ws');
    lastWsInstance!.simulate('open');

    lastWsInstance!.readyState = WebSocket.CLOSED;
    lastWsInstance!.onclose?.();
    expect(client['reconnectTimer']).not.toBeNull();

    // Once the scheduled callback fires, the stale handle must be cleared —
    // otherwise it stays truthy forever and callers checking "is a reconnect
    // still pending" can never see it go falsy.
    await vi.advanceTimersByTimeAsync(1100);
    expect(client['reconnectTimer']).toBeNull();
  });

  it('reconnectExhausted stays false after a single failed reconnect attempt', async () => {
    // Regression guard: an earlier version of this fix read
    // `!reconnectTimer && !periodicRetryTimer` as "give up", but a timer
    // handle is transiently null between one failed attempt ending and the
    // next being scheduled — that condition is briefly true after the very
    // FIRST failed attempt too, which would make voice-room.ts announce
    // "connection lost" after ~1s instead of after the real ~181s+ budget.
    const client = new SignalingClient();
    client.connect('ws://localhost/ws');
    lastWsInstance!.simulate('open');

    // First disconnect — schedules the first fast-retry attempt.
    lastWsInstance!.readyState = WebSocket.CLOSED;
    lastWsInstance!.onclose?.();
    expect(client.reconnectExhausted).toBe(false);

    // Let that attempt fire and fail (mock WS never opens on its own).
    await vi.advanceTimersByTimeAsync(1100);
    lastWsInstance!.readyState = WebSocket.CLOSED;
    lastWsInstance!.onclose?.();

    expect(client.reconnectExhausted).toBe(false);
  });

  it('reconnectExhausted becomes true once the fast-retry budget (maxReconnectAttempt) is spent', async () => {
    const client = new SignalingClient();
    client.connect('ws://localhost/ws');
    lastWsInstance!.simulate('open');

    // Drive through all 10 fast-retry attempts (maxReconnectAttempt).
    for (let i = 0; i < 10; i++) {
      lastWsInstance!.readyState = WebSocket.CLOSED;
      lastWsInstance!.onclose?.();
      expect(client.reconnectExhausted).toBe(false);
      await vi.advanceTimersByTimeAsync(31_000);
    }

    // The 11th close exhausts the fast-retry budget and hands off to periodic
    // retry — this is the "give up" transition voice-room.ts relies on.
    lastWsInstance!.readyState = WebSocket.CLOSED;
    lastWsInstance!.onclose?.();

    expect(client.reconnectExhausted).toBe(true);
  });

  it('reconnectExhausted resets to false on a successful reconnect', async () => {
    const client = new SignalingClient();
    client.connect('ws://localhost/ws');
    lastWsInstance!.simulate('open');

    for (let i = 0; i < 10; i++) {
      lastWsInstance!.readyState = WebSocket.CLOSED;
      lastWsInstance!.onclose?.();
      await vi.advanceTimersByTimeAsync(31_000);
    }
    lastWsInstance!.readyState = WebSocket.CLOSED;
    lastWsInstance!.onclose?.();
    expect(client.reconnectExhausted).toBe(true);

    // Periodic retry succeeds.
    await vi.advanceTimersByTimeAsync(30_000);
    lastWsInstance!.simulate('open');

    expect(client.reconnectExhausted).toBe(false);
  });
});

// ─── connectWithAuth() ────────────────────────────────────────────

describe('connectWithAuth()', () => {
  it('sends Auth message on open', async () => {
    const client = new SignalingClient();
    // connectWithAuth is async — it awaits isTokenExpired then opens WS
    const connectPromise = client.connectWithAuth('ws://localhost/ws');
    // Flush the isTokenExpired + getAccessToken microtasks so the WS is created
    await Promise.resolve();
    await Promise.resolve();
    lastWsInstance!.simulate('open');
    await connectPromise;
    const sentMessages = lastWsInstance!.send.mock.calls.map((c) => JSON.parse(c[0] as string));
    expect(sentMessages.some((m) => m.type === 'auth')).toBe(true);
    expect(sentMessages.find((m) => m.type === 'auth')?.accessToken).toBe('test-access-token');
  });

  it('refreshes token when expired before connecting', async () => {
    const { isTokenExpired, refreshTokens } = await import('@features/auth/auth');
    vi.mocked(isTokenExpired).mockResolvedValueOnce(true);
    vi.mocked(refreshTokens).mockResolvedValueOnce({ status: 'success' });

    const client = new SignalingClient();
    const connectPromise = client.connectWithAuth('ws://localhost/ws');
    await Promise.resolve();
    await Promise.resolve();
    await Promise.resolve();
    lastWsInstance?.simulate('open');
    await connectPromise;

    expect(refreshTokens).toHaveBeenCalled();
  });

  it('sets status to disconnected and throws when refresh returns a 500 server_error', async () => {
    // refreshTokens() always resolves to a truthy RefreshResult discriminated union —
    // it never returns a bare falsy value in production (see auth.ts refreshTokens()).
    // A 500 from POST /auth/refresh surfaces as { status: 'server_error', httpStatus: 500 }.
    const { isTokenExpired, refreshTokens } = await import('@features/auth/auth');
    vi.mocked(isTokenExpired).mockResolvedValueOnce(true);
    vi.mocked(refreshTokens).mockResolvedValueOnce({ status: 'server_error', httpStatus: 500 });

    const client = new SignalingClient();
    let threw = false;
    try {
      await client.connectWithAuth('ws://localhost/ws');
    } catch {
      threw = true;
    }
    expect(threw).toBe(true);
    expect(client.status).toBe('disconnected');
  });

  it('sets status to disconnected and throws when refresh returns a 400 bad_request', async () => {
    const { isTokenExpired, refreshTokens } = await import('@features/auth/auth');
    vi.mocked(isTokenExpired).mockResolvedValueOnce(true);
    vi.mocked(refreshTokens).mockResolvedValueOnce({ status: 'bad_request' });

    const client = new SignalingClient();
    let threw = false;
    try {
      await client.connectWithAuth('ws://localhost/ws');
    } catch {
      threw = true;
    }
    expect(threw).toBe(true);
    expect(client.status).toBe('disconnected');
  });
});

// ─── reconnectWithNewToken() ──────────────────────────────────────

describe('reconnectWithNewToken()', () => {
  it('closes old connection and opens a new one', async () => {
    const client = new SignalingClient();
    client.connect('ws://localhost/ws');
    lastWsInstance!.simulate('open');
    const firstWs = lastWsInstance!;

    const reconnectPromise = client.reconnectWithNewToken('ws://localhost/ws');
    await Promise.resolve();
    await Promise.resolve();
    lastWsInstance!.simulate('open');
    await reconnectPromise;

    expect(firstWs.close).toHaveBeenCalled();
    expect(wsConstructorCalls.length).toBe(2);
  });

  it('sends Auth message on the new connection', async () => {
    const client = new SignalingClient();
    client.connect('ws://localhost/ws');
    lastWsInstance!.simulate('open');

    const reconnectPromise = client.reconnectWithNewToken('ws://localhost/ws');
    await Promise.resolve();
    await Promise.resolve();
    lastWsInstance!.simulate('open');
    await reconnectPromise;

    const sentMessages = lastWsInstance!.send.mock.calls.map((c) => JSON.parse(c[0] as string));
    expect(sentMessages.some((m) => m.type === 'auth')).toBe(true);
  });
});

// ─── error handling ───────────────────────────────────────────────

describe('error handling', () => {
  it('closes the socket on error event', () => {
    const client = new SignalingClient();
    client.connect('ws://localhost/ws');
    lastWsInstance!.simulate('open');
    lastWsInstance!.simulate('error');
    expect(lastWsInstance!.close).toHaveBeenCalled();
  });
});
