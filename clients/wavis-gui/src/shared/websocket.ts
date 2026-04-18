/**
 * Wavis WebSocket Signaling Client (Tauri)
 *
 * Connects to the backend WS endpoint for real-time signaling.
 * Standard WebSocket API works in Tauri webview.
 */

import { getAccessToken, isTokenExpired, refreshTokens } from '@features/auth/auth';
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

  /** Legacy connect — no auth message sent. Preserved for backward compatibility. */
  connect(url: string): void {
    if (this.ws) this.disconnect();

    this.intentionalDisconnect = false;
    this.setStatus('connecting');
    this.ws = new WebSocket(url);

    this.ws.onopen = () => {
      this.setStatus('connected');
      this.reconnectAttempt = 0;
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
   * Connect with auth bootstrap:
   * 1. If token expired, refreshTokens() first
   * 2. Open WebSocket connection
   * 3. On open, send Auth message with current access token
   */
  async connectWithAuth(wsUrl: string): Promise<void> {
    if (await isTokenExpired()) {
      const ok = await refreshTokens();
      if (!ok) {
        this.setStatus('disconnected');
        throw new Error('Token refresh failed — cannot connect');
      }
    }

    const token = await getAccessToken();
    if (this.ws) this.disconnect();

    this.intentionalDisconnect = false;
    this.setStatus('connecting');
    this.ws = new WebSocket(wsUrl);

    this.ws.onopen = () => {
      this.setStatus('connected');
      this.reconnectAttempt = 0;
      this.stopPeriodicRetry();
      // Send Auth message as first message
      this.send({ type: 'auth', accessToken: token });
      console.log(LOG_PREFIX, 'Auth message sent');
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
      this.startPeriodicRetry(url);
      return;
    }
    const delay = Math.min(1000 * 2 ** this.reconnectAttempt, 30_000);
    this.reconnectAttempt++;
    this.reconnectTimer = setTimeout(() => {
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
