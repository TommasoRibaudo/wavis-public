/**
 * Wavis WebSocket Signaling Client (Tauri)
 *
 * Connects to the backend WS endpoint for real-time signaling.
 * Standard WebSocket API works in Tauri webview.
 */

import { fetchWsTicket, isTokenExpired, refreshTokens } from '@features/auth/auth';
import { wsMessageBuffer } from './ws-message-buffer';

// ─── Types ─────────────────────────────────────────────────────────

export type WsStatus = 'disconnected' | 'connecting' | 'connected';

export type WsMessageHandler = (message: unknown) => void;

// ─── Constants ─────────────────────────────────────────────────────

const LOG_PREFIX = '[wavis:ws]';

/** Keepalive ping interval (ms). CloudFront VPC origin_read_timeout is 60s;
 *  ping every 30s to stay safely within that window. */
const KEEPALIVE_INTERVAL_MS = 30_000;

/** Slow periodic retry interval (ms) after fast reconnect attempts are exhausted. */
const PERIODIC_RETRY_INTERVAL_MS = 30_000;

// ═══ SignalingClient ═══════════════════════════════════════════════

export class SignalingClient {
  private ws: WebSocket | null = null;
  private reconnectTimer: ReturnType<typeof setTimeout> | null = null;
  private keepaliveTimer: ReturnType<typeof setInterval> | null = null;
  private periodicRetryTimer: ReturnType<typeof setInterval> | null = null;
  private reconnectAttempt = 0;
  private maxReconnectAttempt = 10;
  private handlers: WsMessageHandler[] = [];
  private statusChangeHandler: ((status: WsStatus) => void) | null = null;
  private intentionalDisconnect = false;
  status: WsStatus = 'disconnected';
  /**
   * True once the fast exponential-backoff reconnect budget
   * (maxReconnectAttempt) has been spent and periodic retry has taken over.
   * Callers (voice-room.ts) use this — not "is a reconnect timer currently
   * pending" — to decide the session is truly lost: a timer handle is
   * transiently null between one attempt ending and the next being
   * scheduled, so checking timer nullness alone declares defeat after a
   * single failed attempt instead of after the real retry budget is spent.
   */
  reconnectExhausted = false;

  /** Legacy connect — no auth message sent. Preserved for backward compatibility. */
  connect(url: string): void {
    if (this.ws) this.disconnect();

    this.intentionalDisconnect = false;
    this.setStatus('connecting');
    this.ws = new WebSocket(url);

    this.ws.onopen = () => {
      this.setStatus('connected');
      this.reconnectAttempt = 0;
      this.reconnectExhausted = false;
      this.stopPeriodicRetry();
    };

    this.ws.onmessage = (event) => {
      try {
        const msg = JSON.parse(event.data);
        wsMessageBuffer.record('received', msg);
        this.handlers.forEach((h) => h(msg));
      } catch {
        console.warn(LOG_PREFIX, 'failed to parse message');
      }
    };

    this.ws.onclose = () => {
      this.setStatus('disconnected');
      this.ws = null;
      if (!this.intentionalDisconnect) {
        this.scheduleReconnect(url);
      }
    };

    this.ws.onerror = () => {
      this.ws?.close();
    };
  }

  /**
   * Connect with pre-auth ticket bootstrap:
   * 1. If token expired, refreshTokens() first
   * 2. Fetch a short-lived, one-use ws-ticket via POST /auth/ws-ticket
   * 3. Open the WebSocket with ?ticket=... — the ticket authenticates the
   *    connection at upgrade time, so no post-connect signaling auth
   *    message is sent (the backend rejects one as "already authenticated").
   */
  async connectWithAuth(wsUrl: string): Promise<void> {
    if (await isTokenExpired()) {
      const result = await refreshTokens();
      if (result.status !== 'success') {
        this.setStatus('disconnected');
        throw new Error(`Token refresh failed — cannot connect (${result.status})`);
      }
    }

    let ticket: string;
    try {
      ticket = await fetchWsTicket();
    } catch (err) {
      this.setStatus('disconnected');
      const message = err instanceof Error ? err.message : String(err);
      throw new Error(`WS ticket fetch failed — cannot connect (${message})`, { cause: err });
    }

    if (this.ws) this.disconnect();

    this.intentionalDisconnect = false;
    this.setStatus('connecting');
    this.ws = new WebSocket(`${wsUrl}?ticket=${encodeURIComponent(ticket)}`);

    this.ws.onopen = () => {
      this.setStatus('connected');
      this.reconnectAttempt = 0;
      this.reconnectExhausted = false;
      this.stopPeriodicRetry();
    };

    this.ws.onmessage = (event) => {
      try {
        const msg = JSON.parse(event.data);
        wsMessageBuffer.record('received', msg);
        this.handlers.forEach((h) => h(msg));
      } catch {
        console.warn(LOG_PREFIX, 'failed to parse message');
      }
    };

    this.ws.onclose = () => {
      this.setStatus('disconnected');
      this.ws = null;
      if (!this.intentionalDisconnect) {
        this.scheduleReconnect(wsUrl);
      }
    };

    this.ws.onerror = () => {
      this.ws?.close();
    };
  }

  /**
   * Close current connection and reconnect with a fresh token.
   * Suppresses the intermediate 'disconnected' status change so callers
   * (e.g. voice-room) don't see a spurious disconnect during the swap.
   */
  async reconnectWithNewToken(wsUrl: string): Promise<void> {
    // Tear down old connection without firing status change
    this.intentionalDisconnect = true;
    if (this.reconnectTimer) clearTimeout(this.reconnectTimer);
    this.reconnectTimer = null;
    this.stopPeriodicRetry();
    this.ws?.close();
    this.ws = null;
    // Don't call setStatus('disconnected') — keep current status until new connection resolves
    await this.connectWithAuth(wsUrl);
  }

  disconnect(): void {
    this.intentionalDisconnect = true;
    if (this.reconnectTimer) clearTimeout(this.reconnectTimer);
    this.reconnectTimer = null;
    this.reconnectExhausted = false;
    this.stopPeriodicRetry();
    this.ws?.close();
    this.ws = null;
    this.setStatus('disconnected');
  }

  send(message: unknown): void {
    if (this.ws?.readyState === WebSocket.OPEN) {
      this.ws.send(JSON.stringify(message));
      wsMessageBuffer.record('sent', message);
    }
  }

  onMessage(handler: WsMessageHandler): () => void {
    this.handlers.push(handler);
    return () => {
      this.handlers = this.handlers.filter((h) => h !== handler);
    };
  }

  /**
   * Register a callback for WebSocket status changes.
   * Single callback slot — calling again replaces the previous handler.
   * Returns an unsubscribe function.
   */
  onStatusChange(handler: (status: WsStatus) => void): () => void {
    this.statusChangeHandler = handler;
    return () => {
      if (this.statusChangeHandler === handler) {
        this.statusChangeHandler = null;
      }
    };
  }

  private setStatus(next: WsStatus): void {
    this.status = next;
    if (next === 'connected') {
      this.startKeepalive();
    } else {
      this.stopKeepalive();
    }
    this.statusChangeHandler?.(next);
  }

  /** Send periodic pings to prevent proxy idle-timeout disconnects. */
  private startKeepalive(): void {
    this.stopKeepalive();
    this.keepaliveTimer = setInterval(() => {
      if (this.ws?.readyState === WebSocket.OPEN) {
        this.send({ type: 'ping' });
      }
    }, KEEPALIVE_INTERVAL_MS);
  }

  private stopKeepalive(): void {
    if (this.keepaliveTimer) {
      clearInterval(this.keepaliveTimer);
      this.keepaliveTimer = null;
    }
  }

  private scheduleReconnect(url: string): void {
    if (this.reconnectAttempt >= this.maxReconnectAttempt) {
      console.log(LOG_PREFIX, 'fast reconnect exhausted — starting periodic retry');
      this.reconnectExhausted = true;
      this.startPeriodicRetry(url);
      return;
    }
    const delay = Math.min(1000 * 2 ** this.reconnectAttempt, 30_000);
    this.reconnectAttempt++;
    this.reconnectTimer = setTimeout(() => {
      this.reconnectTimer = null;
      this.connectWithAuth(url).catch((err) => {
        console.error(LOG_PREFIX, 'Reconnect auth failed:', err);
      });
    }, delay);
  }

  /** Slow periodic retry every 30s after fast reconnect attempts are exhausted. */
  private startPeriodicRetry(url: string): void {
    this.stopPeriodicRetry();
    this.periodicRetryTimer = setInterval(() => {
      if (this.status === 'connected' || this.intentionalDisconnect) {
        this.stopPeriodicRetry();
        return;
      }
      console.log(LOG_PREFIX, 'periodic retry attempt');
      this.reconnectAttempt = 0;
      this.connectWithAuth(url).catch((err) => {
        console.error(LOG_PREFIX, 'Periodic retry auth failed:', err);
      });
    }, PERIODIC_RETRY_INTERVAL_MS);
  }

  private stopPeriodicRetry(): void {
    if (this.periodicRetryTimer) {
      clearInterval(this.periodicRetryTimer);
      this.periodicRetryTimer = null;
    }
  }
}
