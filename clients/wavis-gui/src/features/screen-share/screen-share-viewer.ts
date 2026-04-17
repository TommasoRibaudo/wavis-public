/**
 * Wavis Screen Share Bridge
 *
 * Pipes MediaStreams from the main window to child Tauri windows
 * using local WebRTC loopback (RTCPeerConnection pairs). Each
 * sender/receiver pair is keyed by a composite key of
 * `participantId::windowLabel`, allowing the same participant's
 * stream to be piped to multiple windows simultaneously (e.g.
 * a pop-out ScreenShareWindow and the Watch All window).
 *
 * SDP/ICE signaling happens via Tauri's cross-window event system
 * with composite-key-scoped event names.
 *
 * The `::` separator is safe because participantIds are UUIDs
 * (hex + hyphens only) and window labels are hardcoded strings
 * (`watch-all`, `screen-share-{uuid}`). Neither can contain `::`.
 */

import { emit, listen } from '@tauri-apps/api/event';

const LOG = '[wavis:screen-share-bridge]';
const DEBUG_SHARE_VIEW = import.meta.env.VITE_DEBUG_SCREEN_SHARE_VIEW === 'true';

/* ─── Composite Key ─────────────────────────────────────────────── */

/** Build a composite key from participantId and windowLabel using `::` separator. */
export function compositeKey(participantId: string, windowLabel: string): string {
  return `${participantId}::${windowLabel}`;
}

/* ─── Track Health ──────────────────────────────────────────────── */

/**
 * Returns true if the stream has at least one video track whose underlying
 * MediaStreamTrack is still live. We intentionally do not treat `muted` as a
 * dead-track signal here: remote WebRTC video can be temporarily muted during
 * renegotiation or adaptive-stream pauses (for example when share audio is
 * added on top of an active screen share). Those cases should stay on the
 * lighter renderer recovery path instead of tearing down the whole loopback
 * bridge.
 *
 * A stream whose audio track is still alive but whose video track has ended
 * still returns false — `stream.active` alone returns true in that case,
 * causing the stall detector to no-op re-attach srcObject instead of
 * triggering a full bridge reconnect.
 */
export function isVideoTrackAlive(stream: MediaStream): boolean {
  return stream.getVideoTracks().some(
    (t) => t.readyState === 'live',
  );
}

/* ─── Sender (main window) ──────────────────────────────────────── */

interface SenderEntry {
  pc: RTCPeerConnection;
  cleanups: Array<() => void>;
  offerSdp: string | null;
}

const senders = new Map<string, SenderEntry>();

/** Expose the senders Map for property-based tests. */
export function _getSendersForTest(): Map<string, SenderEntry> {
  return senders;
}

/**
 * Start sending a MediaStream to a child window for a specific
 * participant + window combination. Call from the main window after
 * creating the WebviewWindow.
 */
export async function startSending(
  participantId: string,
  windowLabel: string,
  stream: MediaStream,
): Promise<void> {
  const key = compositeKey(participantId, windowLabel);
  if (DEBUG_SHARE_VIEW) console.log(LOG, `sender[${key}] startSending — tracks: ${stream.getTracks().length}`);
  stopSending(participantId, windowLabel);

  const pc = new RTCPeerConnection();
  const cleanups: Array<() => void> = [];

  if (DEBUG_SHARE_VIEW) {
    pc.oniceconnectionstatechange = () => {
      console.log(LOG, `sender[${key}] iceConnectionState → ${pc.iceConnectionState}`);
    };
    pc.onicegatheringstatechange = () => {
      console.log(LOG, `sender[${key}] iceGatheringState → ${pc.iceGatheringState}`);
    };
  }

  for (const track of stream.getTracks()) {
    pc.addTrack(track, stream);
  }

  pc.onicecandidate = (e) => {
    if (e.candidate) {
      emit(`ss-bridge:ice-sender:${key}`, { candidate: JSON.stringify(e.candidate) });
    }
  };

  // Store entry early so callbacks can find it. offerSdp starts null
  // and is set once the offer is created.
  const entry: SenderEntry = { pc, cleanups, offerSdp: null };
  senders.set(key, entry);

  // Helper: send the offer if it's ready
  const sendOffer = () => {
    const e = senders.get(key);
    if (e?.offerSdp) {
      emit(`ss-bridge:offer:${key}`, { sdp: e.offerSdp });
    }
  };

  // Duplicate receiver-ready events can cause duplicate offers, which in turn
  // can race duplicate answers back to the sender. Serialize answer handling so
  // only one remote answer is applied per local offer cycle.
  let processingAnswer = false;

  // Register ALL listeners BEFORE creating the offer so we never miss
  // a receiver-ready or answer event due to async registration gaps.
  const [unlistenIce, unlistenAnswer, unlistenReady] = await Promise.all([
    listen<{ candidate: string }>(`ss-bridge:ice-receiver:${key}`, (event) => {
      const e = senders.get(key);
      if (!e) return;
      const candidate = JSON.parse(event.payload.candidate);
      e.pc.addIceCandidate(candidate).catch((err) => {
        console.warn(LOG, `sender[${key}] addIceCandidate failed:`, err);
      });
    }),
    listen<{ sdp: string }>(`ss-bridge:answer:${key}`, async (event) => {
      const e = senders.get(key);
      if (!e) return;
      // Guard: only accept an answer when we have a local offer pending.
      // Duplicate offers can cause duplicate answers — ignore if already stable.
      if (processingAnswer || e.pc.signalingState !== 'have-local-offer') {
        console.warn(LOG, `sender[${key}] ignoring duplicate answer, signalingState=${e.pc.signalingState}`);
        return;
      }
      processingAnswer = true;
      try {
        await e.pc.setRemoteDescription({ type: 'answer', sdp: event.payload.sdp });
        console.log(LOG, `sender[${key}] got answer, connection established`);
      } catch (err) {
        console.warn(LOG, `sender[${key}] setRemoteDescription(answer) failed:`, err);
      } finally {
        processingAnswer = false;
      }
    }),
    listen(`ss-bridge:receiver-ready:${key}`, () => {
      console.log(LOG, `sender[${key}] got receiver-ready`);
      sendOffer();
    }),
  ]);
  cleanups.push(unlistenIce, unlistenAnswer, unlistenReady);

  // Guard: if a concurrent startSending call replaced our entry and closed
  // this PC (e.g. watch-all:ready fired twice), bail out silently.
  if (senders.get(key)?.pc !== pc || pc.signalingState === 'closed') {
    if (DEBUG_SHARE_VIEW) console.log(LOG, `sender[${key}] PC replaced or closed before createOffer — bailing`);
    return;
  }

  // Create offer and store it, then emit
  const offer = await pc.createOffer();
  if (DEBUG_SHARE_VIEW) console.log(LOG, `sender[${key}] offer created, sdp length: ${offer.sdp?.length}`);
  await pc.setLocalDescription(offer);
  entry.offerSdp = offer.sdp ?? null;
  sendOffer();
  console.log(LOG, `sender[${key}] started, offer sent`);
}

/** Stop sending for a specific (participantId, windowLabel) pair. */
export function stopSending(participantId: string, windowLabel: string): void {
  const key = compositeKey(participantId, windowLabel);
  const entry = senders.get(key);
  if (!entry) return;
  for (const cleanup of entry.cleanups) cleanup();
  entry.pc.close();
  senders.delete(key);
}

/** Stop all senders targeting a specific window label (e.g. 'watch-all'). */
export function stopSendingForWindow(windowLabel: string): void {
  for (const [key, entry] of [...senders.entries()]) {
    if (key.endsWith(`::${windowLabel}`)) {
      for (const cleanup of entry.cleanups) cleanup();
      entry.pc.close();
      senders.delete(key);
    }
  }
}

/** Stop all senders across all windows. Call on room leave / unmount. */
export function stopAllSending(): void {
  for (const [, entry] of senders) {
    for (const cleanup of entry.cleanups) cleanup();
    entry.pc.close();
  }
  senders.clear();
}

/**
 * Re-send a (possibly updated) MediaStream for a participant + window
 * that already has an open viewer.
 *
 * Fast path: if an existing connected sender is found, uses
 * RTCRtpSender.replaceTrack() to swap the video track in-place with zero
 * frame gap. This avoids tearing down the RTCPeerConnection for the common
 * case of a LiveKit-internal track replacement (codec renegotiation, ICE
 * restart, adaptive-stream recovery) where no real source switch occurred.
 *
 * Slow path (full rebuild): used when no sender exists yet, the connection
 * is not in a usable state, or replaceTrack() rejects (e.g. codec mismatch).
 * Safe to call even if no sender exists yet (acts like startSending).
 */
export async function resendStream(
  participantId: string,
  windowLabel: string,
  stream: MediaStream,
): Promise<void> {
  const key = compositeKey(participantId, windowLabel);
  const existing = senders.get(key);

  if (existing && existing.pc.connectionState === 'connected') {
    const newVideoTrack = stream.getVideoTracks()[0];
    const videoSender = existing.pc.getSenders().find((s) => s.track?.kind === 'video');
    if (newVideoTrack && videoSender) {
      try {
        await videoSender.replaceTrack(newVideoTrack);
        if (DEBUG_SHARE_VIEW) console.log(LOG, `resendStream(${key}) — replaceTrack succeeded (no bridge rebuild)`);
        return;
      } catch (e) {
        if (DEBUG_SHARE_VIEW) console.log(LOG, `resendStream(${key}) — replaceTrack failed, falling back to full rebuild`, e);
      }
    }
  }

  if (DEBUG_SHARE_VIEW) console.log(LOG, `resendStream(${key}) — full rebuild (stopSending + startSending)`);
  stopSending(participantId, windowLabel);
  await startSending(participantId, windowLabel, stream);
}


/* ─── Receiver (child window) ───────────────────────────────────── */

/**
 * Per-instance stream receiver. Each instance owns its own
 * RTCPeerConnection and event listeners, allowing multiple
 * tiles (e.g. Watch All) to receive concurrently.
 */
export class StreamReceiver {
  readonly participantId: string;
  readonly windowLabel: string;
  pc: RTCPeerConnection | null = null;
  private cleanups: Array<() => void> = [];
  private startTimeout: ReturnType<typeof setTimeout> | null = null;
  private startGeneration = 0;

  constructor(participantId: string, windowLabel: string) {
    this.participantId = participantId;
    this.windowLabel = windowLabel;
  }

  /**
   * Create a per-instance RTCPeerConnection and subscribe to scoped events.
   * @param onConnectionFailed Optional callback invoked when the RTCPeerConnection
   *   transitions to the 'failed' state. Use this to trigger a full bridge
   *   reconnect from the consumer (e.g. ScreenSharePage's retryCount mechanism).
   *   Not fired for 'disconnected' — that state may recover on its own; the
   *   stall detector handles it if video frames stop arriving.
   */
  async start(onConnectionFailed?: () => void): Promise<MediaStream> {
    this.stop();
    const key = compositeKey(this.participantId, this.windowLabel);
    const startGeneration = ++this.startGeneration;
    if (DEBUG_SHARE_VIEW) console.log(LOG, `receiver[${key}] start()`);

    return new Promise<MediaStream>(async (resolve, reject) => {
      let settled = false;
      const clearStartTimeout = () => {
        if (this.startTimeout !== null) {
          clearTimeout(this.startTimeout);
          this.startTimeout = null;
        }
      };
      const resolveOnce = (stream: MediaStream) => {
        if (settled || this.startGeneration !== startGeneration) return;
        settled = true;
        clearStartTimeout();
        resolve(stream);
      };
      const rejectOnce = (err: unknown) => {
        if (settled || this.startGeneration !== startGeneration) return;
        settled = true;
        clearStartTimeout();
        reject(err);
      };
      try {
        this.pc = new RTCPeerConnection();

        if (DEBUG_SHARE_VIEW) {
          this.pc.oniceconnectionstatechange = () => {
            console.log(LOG, `receiver[${key}] iceConnectionState → ${this.pc?.iceConnectionState}`);
          };
          this.pc.onicegatheringstatechange = () => {
            console.log(LOG, `receiver[${key}] iceGatheringState → ${this.pc?.iceGatheringState}`);
          };
        }

        // Detect permanent bridge failure early so the consumer can reconnect
        // without waiting for the 3-second stall detector cycle.
        this.pc.onconnectionstatechange = () => {
          if (this.startGeneration !== startGeneration) return;
          if (this.pc?.connectionState === 'failed') {
            console.warn(LOG, `receiver[${key}] connection failed`);
            onConnectionFailed?.();
          }
        };

        const remoteStream = new MediaStream();
        this.pc.ontrack = (e: RTCTrackEvent) => {
          if (this.startGeneration !== startGeneration) return;
          if (DEBUG_SHARE_VIEW) console.log(LOG, `receiver[${key}] ontrack — kind: ${e.track.kind}, readyState: ${e.track.readyState}, muted: ${e.track.muted}`);
          if (e.streams[0]) {
            for (const t of e.streams[0].getTracks()) {
              remoteStream.addTrack(t);
            }
          } else {
            remoteStream.addTrack(e.track);
          }
          if (remoteStream.getTracks().length > 0) {
            resolveOnce(remoteStream);
          }
        };

        this.pc.onicecandidate = (e) => {
          if (e.candidate) {
            emit(`ss-bridge:ice-receiver:${key}`, { candidate: JSON.stringify(e.candidate) });
          }
        };

        // Capture pc reference for use in closures (avoid `this` binding issues)
        const pc = this.pc;

        // Serialise offer processing: two offers arriving simultaneously both
        // see signalingState==='stable' before either calls setRemoteDescription,
        // so the plain signalingState guard is insufficient — use an explicit flag.
        let processingOffer = false;

        // Register both listeners in parallel before emitting receiver-ready
        const [unlistenIce, unlistenOffer] = await Promise.all([
          listen<{ candidate: string }>(`ss-bridge:ice-sender:${key}`, (event) => {
            if (this.startGeneration !== startGeneration) return;
            if (!pc || pc.connectionState === 'closed') return;
            const candidate = JSON.parse(event.payload.candidate);
            pc.addIceCandidate(candidate).catch((err) => {
              console.warn(LOG, `receiver[${key}] addIceCandidate failed:`, err);
            });
          }),
          listen<{ sdp: string }>(`ss-bridge:offer:${key}`, async (event) => {
            if (this.startGeneration !== startGeneration) return;
            if (!pc || pc.connectionState === 'closed') return;
            // Guard: only accept an offer when in 'stable' state and not already
            // processing one. The sender fires the offer both on creation and on
            // receiver-ready, so two offers can arrive before either async handler
            // has had time to change signalingState — the processingOffer flag
            // closes that TOCTOU window.
            if (processingOffer || pc.signalingState !== 'stable') {
              console.warn(LOG, `receiver[${key}] ignoring duplicate offer, signalingState=${pc.signalingState}, processing=${processingOffer}`);
              return;
            }
            processingOffer = true;
            try {
              await pc.setRemoteDescription({ type: 'offer', sdp: event.payload.sdp });
              const answer = await pc.createAnswer();
              await pc.setLocalDescription(answer);
              emit(`ss-bridge:answer:${key}`, { sdp: answer.sdp });
              if (DEBUG_SHARE_VIEW) console.log(LOG, `receiver[${key}] answer sent, sdp length: ${answer.sdp?.length}`);
              console.log(LOG, `receiver[${key}] got offer, answer sent`);
            } finally {
              processingOffer = false;
            }
          }),
        ]);
        if (this.startGeneration !== startGeneration || !this.pc || this.pc !== pc || pc.connectionState === 'closed') {
          unlistenIce();
          unlistenOffer();
          return;
        }
        this.cleanups.push(unlistenIce, unlistenOffer);

        // Signal readiness — sender will (re-)send the offer
        emit(`ss-bridge:receiver-ready:${key}`, {});
        console.log(LOG, `receiver[${key}] started, waiting for offer`);

        this.startTimeout = setTimeout(() => {
          if (!settled && this.startGeneration === startGeneration) {
            if (DEBUG_SHARE_VIEW) console.warn(LOG, `receiver[${key}] TIMEOUT — stream never resolved after 15s`);
            rejectOnce(new Error('Timed out waiting for screen share stream'));
          }
        }, 15000);
      } catch (err) {
        rejectOnce(err);
      }
    });
  }

  /** Clean up the RTCPeerConnection and event listeners. */
  stop(): void {
    this.startGeneration += 1;
    if (this.startTimeout !== null) {
      clearTimeout(this.startTimeout);
      this.startTimeout = null;
    }
    for (const cleanup of this.cleanups) cleanup();
    this.cleanups = [];
    if (this.pc) {
      this.pc.close();
      this.pc = null;
    }
  }
}

/* ─── Global receiver wrappers (backward compat for ScreenSharePage) ── */

let _moduleReceiver: StreamReceiver | null = null;

/**
 * Start receiving a MediaStream from the main window.
 * Call from the screen share child window on mount.
 * Thin wrapper around a module-scoped StreamReceiver instance.
 */
export async function startReceiving(
  participantId: string,
  windowLabel: string,
  onConnectionFailed?: () => void,
): Promise<MediaStream> {
  stopReceiving();
  _moduleReceiver = new StreamReceiver(participantId, windowLabel);
  return _moduleReceiver.start(onConnectionFailed);
}

/** Stop receiving. Call when the screen share window unmounts. */
export function stopReceiving(): void {
  if (_moduleReceiver) {
    _moduleReceiver.stop();
    _moduleReceiver = null;
  }
}
