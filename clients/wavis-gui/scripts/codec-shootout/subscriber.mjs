/**
 * W4 shootout synthetic subscriber.
 *
 * Joins a LiveKit room as a silent subscriber to fill the participant slot
 * for a given cell (p2 = 1 subscriber, p4 = 3 subscribers, p6 = 5 subscribers).
 *
 * Uses the LiveKit signaling WebSocket directly (no WebRTC media) — enough to
 * register as a participant and keep the slot filled for the measurement window.
 *
 * Env vars (set by run-matrix.mjs):
 *   WAVIS_LIVEKIT_URL           wss://... LiveKit server
 *   WAVIS_LIVEKIT_API_KEY       LiveKit API key
 *   WAVIS_LIVEKIT_API_SECRET    LiveKit API secret
 *   WAVIS_SHOOTOUT_ROOM_NAME    room to join
 *   WAVIS_SHOOTOUT_SUBSCRIBER_INDEX  1-based index (used for identity)
 *   WAVIS_SHOOTOUT_CELL_ID      for logging
 */

import { createHmac } from 'node:crypto';
import { WebSocket } from 'node:events'; // node 22+ has WebSocket globally; older nodes need 'ws'

const ROOM   = process.env.WAVIS_SHOOTOUT_ROOM_NAME ?? 'shootout-room';
const INDEX  = process.env.WAVIS_SHOOTOUT_SUBSCRIBER_INDEX ?? '1';
const CELL   = process.env.WAVIS_SHOOTOUT_CELL_ID ?? 'unknown';
const LK_URL = process.env.WAVIS_LIVEKIT_URL ?? '';
const API_KEY    = process.env.WAVIS_LIVEKIT_API_KEY ?? '';
const API_SECRET = process.env.WAVIS_LIVEKIT_API_SECRET ?? '';
const IDENTITY = `shootout-sub-${INDEX}`;

if (!LK_URL || !API_KEY || !API_SECRET) {
  console.error(`[sub-${INDEX}] Missing WAVIS_LIVEKIT_URL / API_KEY / API_SECRET`);
  process.exit(1);
}

function buildAccessToken(apiKey, apiSecret, roomName, identity) {
  const header = Buffer.from(JSON.stringify({ alg: 'HS256', typ: 'JWT' })).toString('base64url');
  const now = Math.floor(Date.now() / 1000);
  const payload = Buffer.from(JSON.stringify({
    iss: apiKey,
    sub: identity,
    exp: now + 3600,
    nbf: now,
    video: { roomJoin: true, room: roomName, canSubscribe: true, canPublish: false },
  })).toString('base64url');
  const sig = createHmac('sha256', apiSecret)
    .update(`${header}.${payload}`)
    .digest('base64url');
  return `${header}.${payload}.${sig}`;
}

const token = buildAccessToken(API_KEY, API_SECRET, ROOM, IDENTITY);
const wsUrl = `${LK_URL.replace(/\/$/, '')}/rtc?access_token=${token}&auto_subscribe=1&sdk=js&version=2.0.0&protocol=15`;

let ws;
let pingInterval;

function connect() {
  const NodeWebSocket = globalThis.WebSocket ?? (await import('ws').then(m => m.default).catch(() => null));
  if (!NodeWebSocket) {
    console.error(`[sub-${INDEX}] No WebSocket implementation found. Install 'ws': npm install ws -g`);
    process.exit(1);
  }

  ws = new NodeWebSocket(wsUrl, { rejectUnauthorized: false });

  ws.on('open', () => {
    console.log(`[sub-${INDEX}] connected to room ${ROOM} (cell ${CELL})`);
    // send ping every 10s to keep connection alive
    pingInterval = setInterval(() => {
      if (ws.readyState === 1) ws.send(JSON.stringify({ type: 'ping' }));
    }, 10_000);
  });

  ws.on('message', () => { /* ignore incoming signals — subscriber only */ });

  ws.on('close', (code) => {
    console.log(`[sub-${INDEX}] disconnected (code ${code})`);
    clearInterval(pingInterval);
  });

  ws.on('error', (err) => {
    console.error(`[sub-${INDEX}] WS error: ${err.message}`);
  });
}

// Graceful shutdown on SIGTERM from the runner
process.on('SIGTERM', () => {
  console.log(`[sub-${INDEX}] SIGTERM — closing`);
  clearInterval(pingInterval);
  if (ws) ws.close(1000, 'cell complete');
  setTimeout(() => process.exit(0), 500);
});

connect().catch((err) => {
  console.error(`[sub-${INDEX}] fatal: ${err.message}`);
  process.exit(1);
});
