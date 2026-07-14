/**
 * Wavis Direct LiveKit Viewer Connection
 *
 * Lets a child viewer window (screen-share pop-out, Watch All) open its own
 * subscribe-only LiveKit Room connection instead of receiving video through
 * the main window's RTCPeerConnection loopback bridge. The loopback bridge
 * re-encoded every watched stream in software; a direct subscription costs
 * one decode in the displaying window and nothing in the main window.
 *
 * Token flow: the child has no signaling WebSocket, so it asks the main
 * window to broker a token — `emitTo('main', 'viewer-token:request')` →
 * main sends `request_viewer_token` over its WS → backend returns a hidden,
 * subscribe-only token → main relays it back via `viewer-token:response`
 * scoped to this window's label. A fresh token is requested before every
 * (re)connect attempt, so token TTL never needs client-side refresh.
 *
 * One ViewerRoomConnection per window. Watch All watches N identities over
 * a single Room; each pop-out window has its own Room.
 */

import { Room, RoomEvent, Track, VideoQuality, ConnectionState } from 'livekit-client';
import type { RemoteParticipant, RemoteTrack, RemoteTrackPublication } from 'livekit-client';
import { emitTo, listen } from '@tauri-apps/api/event';

const DEBUG_VIEWER_CONNECTION = import.meta.env.VITE_DEBUG_VIEWER_CONNECTION === 'true';
const LOG = '[wavis:viewer-connection]';

const TOKEN_REQUEST_TIMEOUT_MS = 8000;
const BACKOFF_BASE_MS = 1000;
const BACKOFF_CAP_MS = 15000;

/* ─── Pure helpers (unit-tested) ────────────────────────────────── */

/** Exponential reconnect backoff: 1s, 2s, 4s, ... capped at 15s. */
export function computeViewerBackoffMs(attempt: number): number {
  const exponent = Math.max(0, Math.min(attempt, 31));
  return Math.min(BACKOFF_BASE_MS * 2 ** exponent, BACKOFF_CAP_MS);
}

/**
 * Per-window-instance correlation id, unique across fast close/reopen cycles
 * so two window instances never collide on the same LiveKit viewer identity.
 * Charset stays within the backend's identifier rules ([A-Za-z0-9_-]).
 */
export function makeViewerWindowId(): string {
  const bytes = new Uint8Array(4);
  crypto.getRandomValues(bytes);
  return 'w' + Array.from(bytes, (b) => b.toString(16).padStart(2, '0')).join('');
}

export interface ScreenSharePublicationLike {
  kind: Track.Kind;
  setSubscribed?: (subscribed: boolean) => void;
  setEnabled?: (enabled: boolean) => void;
  setVideoQuality?: (quality: VideoQuality) => void;
}

/** Pick a participant's screen-share VIDEO publication (audio/camera ignored). */
export function pickScreenSharePublication<T extends ScreenSharePublicationLike>(participant: {
  getTrackPublication(source: Track.Source): T | undefined;
}): T | null {
  const publication = participant.getTrackPublication(Track.Source.ScreenShare);
  if (!publication || publication.kind !== Track.Kind.Video) return null;
  return publication;
}

/* ─── Viewer connection ─────────────────────────────────────────── */

interface ViewerTokenResponse {
  windowId: string;
  token: string;
  sfuUrl: string;
  identity: string;
}

type WatchCallback = (stream: MediaStream | null) => void;

export interface ViewerRoomConnectionOptions {
  windowLabel: string;
  /** Invoked after each failed connect attempt (already scheduled to retry). */
  onConnectionError?: (error: unknown) => void;
}

export class ViewerRoomConnection {
  private readonly windowLabel: string;
  private readonly windowId: string;
  private readonly onConnectionError?: (error: unknown) => void;

  private room: Room | null = null;
  private connectPromise: Promise<void> | null = null;
  private generation = 0;
  private attempt = 0;
  private reconnectTimer: ReturnType<typeof setTimeout> | null = null;
  private disposed = false;

  /** identity → callbacks wanting that identity's screen-share stream. */
  private watches = new Map<string, Set<WatchCallback>>();
  /** identity → currently delivered MediaStream. */
  private streams = new Map<string, MediaStream>();

  constructor(opts: ViewerRoomConnectionOptions) {
    this.windowLabel = opts.windowLabel;
    this.onConnectionError = opts.onConnectionError;
    this.windowId = makeViewerWindowId();
  }

  /**
   * Watch a participant identity's screen-share video. `cb` fires with a
   * MediaStream when the track is (re)subscribed and `null` when it goes
   * away (sharer stopped / disconnected); the watch survives either — the
   * sharer may restart. Returns an unwatch function.
   */
  watch(identity: string, cb: WatchCallback): () => void {
    if (this.disposed) return () => {};
    let set = this.watches.get(identity);
    if (!set) {
      set = new Set();
      this.watches.set(identity, set);
    }
    set.add(cb);

    const current = this.streams.get(identity);
    if (current) cb(current);

    this.subscribeIdentity(identity);
    void this.ensureConnected();

    return () => {
      const callbacks = this.watches.get(identity);
      if (!callbacks) return;
      callbacks.delete(cb);
      if (callbacks.size === 0) {
        this.watches.delete(identity);
        this.streams.delete(identity);
        this.unsubscribeIdentity(identity);
      }
    };
  }

  /** Tear down the Room and reconnect immediately (dead-track escape hatch). */
  forceReconnect(): void {
    if (this.disposed) return;
    if (DEBUG_VIEWER_CONNECTION) console.log(LOG, `[${this.windowLabel}] forceReconnect`);
    this.generation += 1;
    if (this.reconnectTimer !== null) {
      clearTimeout(this.reconnectTimer);
      this.reconnectTimer = null;
    }
    this.attempt = 0;
    const room = this.room;
    this.room = null;
    if (room) void room.disconnect();
    this.clearStreams();
    void this.ensureConnected();
  }

  dispose(): void {
    if (this.disposed) return;
    this.disposed = true;
    this.generation += 1;
    if (this.reconnectTimer !== null) {
      clearTimeout(this.reconnectTimer);
      this.reconnectTimer = null;
    }
    const room = this.room;
    this.room = null;
    if (room) void room.disconnect();
    this.watches.clear();
    this.streams.clear();
  }

  /* ── Connection lifecycle ── */

  private async ensureConnected(): Promise<void> {
    if (this.disposed) return;
    if (this.room && this.room.state === ConnectionState.Connected) return;
    if (this.connectPromise) return this.connectPromise;
    this.connectPromise = this.connectOnce().finally(() => {
      this.connectPromise = null;
    });
    return this.connectPromise;
  }

  private async connectOnce(): Promise<void> {
    const generation = ++this.generation;
    try {
      const { token, sfuUrl, identity } = await this.requestToken();
      if (this.disposed || generation !== this.generation) return;
      if (DEBUG_VIEWER_CONNECTION) {
        console.log(LOG, `[${this.windowLabel}] connecting to ${sfuUrl} as ${identity}`);
      }

      // adaptiveStream off: tiles attach via srcObject, invisible to LiveKit's
      // element observer, and we pin quality explicitly per subscription.
      const room = new Room({ adaptiveStream: false, dynacast: false });
      this.wireRoomEvents(room);
      this.room = room;

      // autoSubscribe:false is the core of the design — this connection pulls
      // exactly the watched ScreenShare video publications, never room audio
      // (that stays in the main window's mixer) and never cameras.
      await room.connect(sfuUrl, token, { autoSubscribe: false });
      // The guard above narrowed `this.disposed` to false and TS carries that
      // narrowing across the await — but dispose() flips it from a separate call
      // path (window closed) while this connect is in flight. Dropping the check
      // would leak a connected Room after disposal.
      // eslint-disable-next-line @typescript-eslint/no-unnecessary-condition
      if (this.disposed || generation !== this.generation) {
        void room.disconnect();
        return;
      }

      this.attempt = 0;
      if (DEBUG_VIEWER_CONNECTION) console.log(LOG, `[${this.windowLabel}] connected`);
      this.subscribeAll();
    } catch (err) {
      if (this.disposed || generation !== this.generation) return;
      if (DEBUG_VIEWER_CONNECTION) {
        console.warn(LOG, `[${this.windowLabel}] connect attempt failed:`, err);
      }
      this.room = null;
      this.onConnectionError?.(err);
      this.scheduleReconnect();
    }
  }

  private scheduleReconnect(): void {
    if (this.disposed || this.reconnectTimer !== null) return;
    if (this.watches.size === 0) return; // nothing to reconnect for
    const delay = computeViewerBackoffMs(this.attempt);
    this.attempt += 1;
    if (DEBUG_VIEWER_CONNECTION) {
      console.log(LOG, `[${this.windowLabel}] reconnect in ${delay}ms (attempt ${this.attempt})`);
    }
    this.reconnectTimer = setTimeout(() => {
      this.reconnectTimer = null;
      void this.ensureConnected();
    }, delay);
  }

  /** Ask the main window to broker a fresh subscribe-only token. */
  private requestToken(): Promise<ViewerTokenResponse> {
    return new Promise((resolve, reject) => {
      let settled = false;
      let unlisten: (() => void) | null = null;
      const timer = setTimeout(() => {
        if (settled) return;
        settled = true;
        unlisten?.();
        reject(new Error('viewer token request timed out'));
      }, TOKEN_REQUEST_TIMEOUT_MS);

      listen<ViewerTokenResponse>('viewer-token:response', ({ payload }) => {
        if (settled || payload.windowId !== this.windowId) return;
        settled = true;
        clearTimeout(timer);
        unlisten?.();
        resolve(payload);
      })
        .then((fn) => {
          if (settled) {
            fn();
            return;
          }
          unlisten = fn;
          void emitTo('main', 'viewer-token:request', {
            windowId: this.windowId,
            windowLabel: this.windowLabel,
          });
        })
        .catch((err) => {
          if (settled) return;
          settled = true;
          clearTimeout(timer);
          reject(err instanceof Error ? err : new Error(String(err)));
        });
    });
  }

  /* ── Room events ── */

  private wireRoomEvents(room: Room): void {
    room.on(RoomEvent.TrackSubscribed, (track, publication, participant) => {
      if (room !== this.room) return;
      this.handleTrackSubscribed(track, publication, participant);
    });

    room.on(RoomEvent.TrackUnsubscribed, (_track, publication, participant) => {
      if (room !== this.room) return;
      if (publication.source !== Track.Source.ScreenShare) return;
      this.deliverNull(participant.identity);
    });

    // The SFU can deliver a track muted and unmute it later (e.g. sharer-side
    // source switch). Re-deliver a fresh MediaStream so video elements that
    // froze on the muted track re-attach — same recovery the main window does.
    room.on(RoomEvent.TrackUnmuted, (publication, participant) => {
      if (room !== this.room) return;
      if (publication.source !== Track.Source.ScreenShare) return;
      const track = publication.track;
      if (!track || track.kind !== Track.Kind.Video) return;
      if (!this.watches.has(participant.identity)) return;
      if (DEBUG_VIEWER_CONNECTION) {
        console.log(
          LOG,
          `[${this.windowLabel}] screen share unmuted for ${participant.identity} — re-delivering stream`,
        );
      }
      this.deliverStream(participant.identity, (track as RemoteTrack).mediaStreamTrack);
    });

    room.on(RoomEvent.TrackPublished, (publication, participant) => {
      if (room !== this.room) return;
      if (publication.source !== Track.Source.ScreenShare) return;
      if (!this.watches.has(participant.identity)) return;
      this.pinPublication(publication);
    });

    room.on(RoomEvent.ParticipantConnected, (participant) => {
      if (room !== this.room) return;
      if (!this.watches.has(participant.identity)) return;
      this.subscribeIdentity(participant.identity);
    });

    room.on(RoomEvent.ParticipantDisconnected, (participant) => {
      if (room !== this.room) return;
      this.deliverNull(participant.identity);
    });

    room.on(RoomEvent.Reconnected, () => {
      if (room !== this.room) return;
      if (DEBUG_VIEWER_CONNECTION)
        console.log(LOG, `[${this.windowLabel}] reconnected — re-pinning subscriptions`);
      this.subscribeAll();
    });

    // Terminal disconnect (transient drops are handled internally by
    // livekit-client's Reconnecting/Reconnected). Also fires when the room is
    // destroyed server-side (voice session ended) — retries then fail quietly
    // until the window is closed by the existing voice-session:ended listener.
    room.on(RoomEvent.Disconnected, (reason) => {
      if (this.disposed || room !== this.room) return;
      if (DEBUG_VIEWER_CONNECTION)
        console.log(LOG, `[${this.windowLabel}] disconnected (${reason ?? 'unknown'})`);
      this.room = null;
      this.clearStreams();
      this.scheduleReconnect();
    });
  }

  private handleTrackSubscribed(
    track: RemoteTrack,
    publication: RemoteTrackPublication,
    participant: RemoteParticipant,
  ): void {
    if (publication.source !== Track.Source.ScreenShare) return;
    if (track.kind !== Track.Kind.Video) return;
    if (!this.watches.has(participant.identity)) return;
    if (DEBUG_VIEWER_CONNECTION) {
      console.log(
        LOG,
        `[${this.windowLabel}] screen share subscribed for ${participant.identity} (sid ${track.sid})`,
      );
    }
    this.deliverStream(participant.identity, track.mediaStreamTrack);
  }

  /* ── Subscription management ── */

  private subscribeAll(): void {
    for (const identity of this.watches.keys()) {
      this.subscribeIdentity(identity);
    }
  }

  private subscribeIdentity(identity: string): void {
    const participant = this.room?.remoteParticipants.get(identity);
    if (!participant) return; // will subscribe on ParticipantConnected/TrackPublished
    const publication = pickScreenSharePublication(participant);
    if (!publication) return;
    this.pinPublication(publication);

    // Track may already be subscribed (e.g. re-watch after tile retry).
    const track = publication.track;
    if (track && track.kind === Track.Kind.Video) {
      this.deliverStream(identity, track.mediaStreamTrack);
    }
  }

  private unsubscribeIdentity(identity: string): void {
    const participant = this.room?.remoteParticipants.get(identity);
    if (!participant) return;
    const publication = pickScreenSharePublication(participant);
    publication?.setSubscribed(false);
  }

  private pinPublication(publication: ScreenSharePublicationLike): void {
    publication.setSubscribed?.(true);
    publication.setEnabled?.(true);
    publication.setVideoQuality?.(VideoQuality.HIGH);
  }

  /* ── Stream delivery ── */

  private deliverStream(identity: string, mediaStreamTrack: MediaStreamTrack): void {
    const existing = this.streams.get(identity);
    if (existing && existing.getVideoTracks()[0] === mediaStreamTrack) return; // already delivered
    const stream = new MediaStream([mediaStreamTrack]);
    this.streams.set(identity, stream);
    for (const cb of this.watches.get(identity) ?? []) cb(stream);
  }

  private deliverNull(identity: string): void {
    if (!this.streams.delete(identity)) return;
    for (const cb of this.watches.get(identity) ?? []) cb(null);
  }

  private clearStreams(): void {
    for (const identity of [...this.streams.keys()]) {
      this.deliverNull(identity);
    }
  }
}
