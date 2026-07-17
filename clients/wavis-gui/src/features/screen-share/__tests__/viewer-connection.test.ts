import { describe, it, expect, vi, beforeEach } from 'vitest';
import { Track, RoomEvent, VideoQuality } from 'livekit-client';
import {
  computeViewerBackoffMs,
  makeViewerWindowId,
  pickScreenSharePublication,
  ViewerRoomConnection,
  type ScreenSharePublicationLike,
} from '../viewer-connection';

/* ─── livekit-client mock ───────────────────────────────────────────
 * A real constructable Room class (not vi.fn().mockImplementation(), which
 * cannot be `new`ed) with a minimal on/off/emit event bus. `roomState` is
 * declared via vi.hoisted so the mock factory below — which vitest hoists
 * above these imports — can close over it safely. */

const roomState = vi.hoisted<{ current: unknown }>(() => ({ current: null }));

vi.mock('livekit-client', () => {
  class MockRoom {
    state = 'disconnected';
    remoteParticipants = new Map<string, unknown>();
    private handlers = new Map<string, Array<(...args: unknown[]) => void>>();
    connect = vi.fn(() => {
      this.state = 'connected';
      return Promise.resolve();
    });
    disconnect = vi.fn(() => {
      this.state = 'disconnected';
      return Promise.resolve();
    });
    options: Record<string, unknown> | undefined;
    constructor(options?: Record<string, unknown>) {
      this.options = options;
      roomState.current = this;
    }
    on(event: string, handler: (...args: unknown[]) => void) {
      const arr = this.handlers.get(event) ?? [];
      arr.push(handler);
      this.handlers.set(event, arr);
      return this;
    }
    off(event: string, handler: (...args: unknown[]) => void) {
      const arr = this.handlers.get(event);
      if (arr)
        this.handlers.set(
          event,
          arr.filter((h) => h !== handler),
        );
      return this;
    }
    emit(event: string, ...args: unknown[]) {
      for (const h of this.handlers.get(event) ?? []) h(...args);
    }
  }

  return {
    Room: MockRoom,
    RoomEvent: {
      TrackSubscribed: 'trackSubscribed',
      TrackUnsubscribed: 'trackUnsubscribed',
      TrackMuted: 'trackMuted',
      TrackUnmuted: 'trackUnmuted',
      TrackPublished: 'trackPublished',
      ParticipantConnected: 'participantConnected',
      ParticipantDisconnected: 'participantDisconnected',
      Reconnected: 'reconnected',
      Disconnected: 'disconnected',
    },
    Track: {
      Kind: { Audio: 'audio', Video: 'video', Unknown: 'unknown' },
      Source: {
        Camera: 'camera',
        Microphone: 'microphone',
        ScreenShare: 'screen_share',
        ScreenShareAudio: 'screen_share_audio',
        Unknown: 'unknown',
      },
    },
    ConnectionState: {
      Disconnected: 'disconnected',
      Connecting: 'connecting',
      Connected: 'connected',
      Reconnecting: 'reconnecting',
      SignalReconnecting: 'signalReconnecting',
    },
    VideoQuality: { LOW: 0, MEDIUM: 1, HIGH: 2 },
  };
});

/* ─── @tauri-apps/api/event mock ─────────────────────────────────────
 * Drives the viewer-token handshake: emitTo('main', 'viewer-token:request', …)
 * is answered synchronously by echoing the windowId back through the
 * registered 'viewer-token:response' listener, standing in for the main
 * window's broker round-trip. */

vi.mock('@tauri-apps/api/event', () => {
  const listeners = new Map<string, Array<(event: { payload: unknown }) => void>>();
  return {
    listen: vi.fn((event: string, cb: (event: { payload: unknown }) => void) => {
      const arr = listeners.get(event) ?? [];
      arr.push(cb);
      listeners.set(event, arr);
      return Promise.resolve(() => {
        const remaining = listeners.get(event);
        if (remaining)
          listeners.set(
            event,
            remaining.filter((h) => h !== cb),
          );
      });
    }),
    emitTo: vi.fn((_target: string, event: string, payload: unknown) => {
      if (event !== 'viewer-token:request') return Promise.resolve();
      const { windowId } = payload as { windowId: string };
      for (const cb of listeners.get('viewer-token:response') ?? []) {
        cb({
          payload: { windowId, token: 'mock-token', sfuUrl: 'wss://mock-sfu', identity: 'viewer' },
        });
      }
      return Promise.resolve();
    }),
  };
});

/* ─── MediaStream stub — vitest runs this file under environment: 'node' ── */

class MockMediaStream {
  private tracks: MediaStreamTrack[];
  constructor(tracks: MediaStreamTrack[] = []) {
    this.tracks = tracks;
  }
  getVideoTracks() {
    return this.tracks;
  }
}
vi.stubGlobal('MediaStream', MockMediaStream);

describe('computeViewerBackoffMs', () => {
  it('starts at 1s and doubles per attempt', () => {
    expect(computeViewerBackoffMs(0)).toBe(1000);
    expect(computeViewerBackoffMs(1)).toBe(2000);
    expect(computeViewerBackoffMs(2)).toBe(4000);
    expect(computeViewerBackoffMs(3)).toBe(8000);
  });

  it('caps at 15s and never decreases', () => {
    let prev = 0;
    for (let attempt = 0; attempt < 40; attempt++) {
      const delay = computeViewerBackoffMs(attempt);
      expect(delay).toBeGreaterThanOrEqual(prev);
      expect(delay).toBeLessThanOrEqual(15000);
      prev = delay;
    }
    expect(computeViewerBackoffMs(10)).toBe(15000);
  });

  it('clamps negative attempts to the base delay', () => {
    expect(computeViewerBackoffMs(-1)).toBe(1000);
  });
});

describe('makeViewerWindowId', () => {
  it('produces ids within the backend identifier charset and length bound', () => {
    for (let i = 0; i < 50; i++) {
      const id = makeViewerWindowId();
      expect(id).toMatch(/^w[0-9a-f]{8}$/);
      expect(id.length).toBeLessThanOrEqual(32);
    }
  });

  it('produces distinct ids across instances', () => {
    const ids = new Set(Array.from({ length: 100 }, () => makeViewerWindowId()));
    expect(ids.size).toBeGreaterThan(90); // 2^32 space — collisions in 100 draws are a bug
  });
});

describe('pickScreenSharePublication', () => {
  function participantWith(pubs: Partial<Record<Track.Source, ScreenSharePublicationLike>>) {
    return {
      getTrackPublication: (source: Track.Source) => pubs[source],
    };
  }

  it('returns the screen-share video publication', () => {
    const sharePub = { kind: Track.Kind.Video };
    const participant = participantWith({
      [Track.Source.ScreenShare]: sharePub,
      [Track.Source.Camera]: { kind: Track.Kind.Video },
    });
    expect(pickScreenSharePublication(participant)).toBe(sharePub);
  });

  it('ignores a screen-share AUDIO publication surfaced under the video source', () => {
    const participant = participantWith({
      [Track.Source.ScreenShare]: { kind: Track.Kind.Audio },
    });
    expect(pickScreenSharePublication(participant)).toBeNull();
  });

  it('returns null when the participant has no screen-share publication', () => {
    const participant = participantWith({
      [Track.Source.Camera]: { kind: Track.Kind.Video },
    });
    expect(pickScreenSharePublication(participant)).toBeNull();
  });
});

/* ─── ViewerRoomConnection Room construction options — issue #117 ──
 * Our self-hosted LiveKit server (pinned to v1.9.3) doesn't serve the SDK's
 * default /rtc/v1 join path, so every viewer-window connect (Watch All,
 * screen-share pop-outs) would otherwise pay a guaranteed-fail round trip
 * before falling back to the legacy path it ends up using anyway. */

interface TestRoom {
  state: string;
  remoteParticipants: Map<string, unknown>;
  disconnect: ReturnType<typeof vi.fn>;
  emit: (event: string, ...args: unknown[]) => void;
  options?: Record<string, unknown>;
}

describe('ViewerRoomConnection Room construction', () => {
  beforeEach(() => {
    roomState.current = null;
  });

  it('constructs Room with singlePeerConnection:false', async () => {
    const conn = new ViewerRoomConnection({ windowLabel: 'watch-all' });
    conn.watch('alice', () => {});
    await tick();
    await tick();

    const room = roomState.current as TestRoom;
    expect(room).not.toBeNull();
    expect(room.options?.singlePeerConnection).toBe(false);
  });
});

/* ─── ViewerRoomConnection.refreshIdentity (Watch All dead-track recovery) ──
 * Regression coverage for #283: a single dead track used to call
 * forceReconnect(), tearing down the whole Room and blacking out every
 * Watch All tile. refreshIdentity resubscribes one identity in place. */

function tick(): Promise<void> {
  return new Promise((resolve) => setTimeout(resolve, 0));
}

function createMockPublication() {
  return {
    kind: Track.Kind.Video,
    setSubscribed: vi.fn(),
    setEnabled: vi.fn(),
    setVideoQuality: vi.fn(),
  };
}

function createMockParticipant(identity: string, publication: ScreenSharePublicationLike) {
  return {
    identity,
    getTrackPublication: (source: Track.Source) =>
      source === Track.Source.ScreenShare ? publication : undefined,
  };
}

describe('ViewerRoomConnection.refreshIdentity', () => {
  beforeEach(() => {
    roomState.current = null;
  });

  it('resubscribes only the target identity and leaves the Room untouched', async () => {
    const conn = new ViewerRoomConnection({ windowLabel: 'watch-all' });
    const aliceEvents: Array<MediaStream | null> = [];
    const bobEvents: Array<MediaStream | null> = [];
    conn.watch('alice', (s) => aliceEvents.push(s));
    conn.watch('bob', (s) => bobEvents.push(s));
    await tick();
    await tick();

    const room = roomState.current as TestRoom;
    expect(room).not.toBeNull();

    const alicePub = createMockPublication();
    const bobPub = createMockPublication();
    room.remoteParticipants.set('alice', createMockParticipant('alice', alicePub));
    room.remoteParticipants.set('bob', createMockParticipant('bob', bobPub));

    // Both participants join and get pinned via the normal subscribe path.
    room.emit(RoomEvent.ParticipantConnected, { identity: 'alice' });
    room.emit(RoomEvent.ParticipantConnected, { identity: 'bob' });

    // SFU delivers a live track for each.
    const aliceTrack = { kind: Track.Kind.Video, mediaStreamTrack: {} as MediaStreamTrack };
    const bobTrack = { kind: Track.Kind.Video, mediaStreamTrack: {} as MediaStreamTrack };
    room.emit(
      RoomEvent.TrackSubscribed,
      aliceTrack,
      { source: Track.Source.ScreenShare },
      { identity: 'alice' },
    );
    room.emit(
      RoomEvent.TrackSubscribed,
      bobTrack,
      { source: Track.Source.ScreenShare },
      { identity: 'bob' },
    );

    expect(aliceEvents.at(-1)).toBeInstanceOf(MockMediaStream);
    expect(bobEvents.at(-1)).toBeInstanceOf(MockMediaStream);

    alicePub.setSubscribed.mockClear();
    alicePub.setEnabled.mockClear();
    alicePub.setVideoQuality.mockClear();
    bobPub.setSubscribed.mockClear();
    bobPub.setEnabled.mockClear();
    bobPub.setVideoQuality.mockClear();

    conn.refreshIdentity('alice');

    // Alice: unsubscribe, then re-pin (subscribe, enable, quality).
    expect(alicePub.setSubscribed.mock.calls.map((c: unknown[]) => c[0])).toEqual([false, true]);
    expect(alicePub.setEnabled).toHaveBeenCalledWith(true);
    expect(alicePub.setVideoQuality).toHaveBeenCalledWith(VideoQuality.HIGH);

    // Bob's publication and the shared Room are untouched — no whole-room teardown.
    expect(bobPub.setSubscribed).not.toHaveBeenCalled();
    expect(bobPub.setEnabled).not.toHaveBeenCalled();
    expect(bobPub.setVideoQuality).not.toHaveBeenCalled();
    expect(room.disconnect).not.toHaveBeenCalled();

    // Alice's tile is told to drop the dead stream; Bob's last delivery stands.
    expect(aliceEvents.at(-1)).toBeNull();
    expect(bobEvents).toHaveLength(1);

    conn.dispose();
  });

  it('is a no-op after dispose()', async () => {
    const conn = new ViewerRoomConnection({ windowLabel: 'watch-all' });
    const events: Array<MediaStream | null> = [];
    conn.watch('alice', (s) => events.push(s));
    await tick();
    await tick();

    const room = roomState.current as TestRoom;
    const alicePub = createMockPublication();
    room.remoteParticipants.set('alice', createMockParticipant('alice', alicePub));
    room.emit(RoomEvent.ParticipantConnected, { identity: 'alice' });

    conn.dispose(); // dispose() itself calls room.disconnect() once — clear before asserting
    alicePub.setSubscribed.mockClear();
    room.disconnect.mockClear();
    const eventsBefore = events.length;

    conn.refreshIdentity('alice');

    expect(alicePub.setSubscribed).not.toHaveBeenCalled();
    expect(room.disconnect).not.toHaveBeenCalled();
    expect(events).toHaveLength(eventsBefore);
  });
});
