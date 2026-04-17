/**
 * Unit tests for the screen share bridge composite key and caller updates.
 *
 * Requirements: 12.1, 12.2
 */

import { describe, it, expect, vi, beforeEach } from 'vitest';

const { eventListeners, emitMock, peerConnections } = vi.hoisted(() => ({
  eventListeners: new Map<string, Array<(event: unknown) => unknown>>(),
  emitMock: vi.fn(),
  peerConnections: [] as MockRTCPeerConnection[],
}));

/* ─── Mocks ─────────────────────────────────────────────────────── */

vi.mock('@tauri-apps/api/event', () => ({
  emit: emitMock,
  listen: vi.fn(async (event: string, callback: (event: unknown) => unknown) => {
    const listeners = eventListeners.get(event) ?? [];
    listeners.push(callback);
    eventListeners.set(event, listeners);
    return () => {
      const current = eventListeners.get(event) ?? [];
      eventListeners.set(event, current.filter((entry) => entry !== callback));
    };
  }),
}));

class MockRTCRtpSender {
  track: MediaStreamTrack | null;
  replaceTrack = vi.fn().mockResolvedValue(undefined);
  constructor(track: MediaStreamTrack | null) {
    this.track = track;
  }
}

class MockRTCPeerConnection {
  onicecandidate: ((e: unknown) => void) | null = null;
  ontrack: ((e: unknown) => void) | null = null;
  onconnectionstatechange: (() => void) | null = null;
  connectionState = 'new';
  signalingState = 'stable';
  private _senders: MockRTCRtpSender[] = [];
  addTrack = vi.fn().mockImplementation((track: MediaStreamTrack) => {
    const sender = new MockRTCRtpSender(track);
    this._senders.push(sender);
    return sender;
  });
  getSenders = vi.fn().mockImplementation(() => this._senders);
  createOffer = vi.fn().mockResolvedValue({ sdp: 'mock-offer', type: 'offer' });
  createAnswer = vi.fn().mockResolvedValue({ sdp: 'mock-answer', type: 'answer' });
  setLocalDescription = vi.fn().mockImplementation(async (description?: { type?: string }) => {
    this.signalingState = description?.type === 'answer' ? 'stable' : 'have-local-offer';
  });
  setRemoteDescription = vi.fn().mockResolvedValue(undefined);
  addIceCandidate = vi.fn().mockResolvedValue(undefined);
  close = vi.fn();

  constructor() {
    peerConnections.push(this);
  }

  /** Test helper: simulate a connection state transition. */
  _simulateConnectionState(state: string): void {
    this.connectionState = state;
    this.onconnectionstatechange?.();
  }
}
globalThis.RTCPeerConnection = MockRTCPeerConnection as unknown as typeof RTCPeerConnection;

class MockMediaStream {
  private tracks: MediaStreamTrack[] = [];
  id = Math.random().toString(36).slice(2);
  getTracks() { return this.tracks; }
  addTrack(t: MediaStreamTrack) { this.tracks.push(t); }
  getVideoTracks() { return this.tracks.filter((t) => t.kind === 'video'); }
  getAudioTracks() { return this.tracks.filter((t) => t.kind === 'audio'); }
}
globalThis.MediaStream = MockMediaStream as unknown as typeof MediaStream;

import {
  compositeKey,
  startSending,
  stopSendingForWindow,
  stopAllSending,
  resendStream,
  _getSendersForTest,
  isVideoTrackAlive,
  StreamReceiver,
} from '../screen-share-viewer';

/* ═══ Unit Tests ════════════════════════════════════════════════════ */

beforeEach(() => {
  stopAllSending();
  eventListeners.clear();
  emitMock.mockClear();
  peerConnections.length = 0;
});

describe('compositeKey', () => {
  it("compositeKey('user1', 'watch-all') → 'user1::watch-all'", () => {
    expect(compositeKey('user1', 'watch-all')).toBe('user1::watch-all');
  });

  it("compositeKey('user1', 'screen-share-user1') → 'user1::screen-share-user1'", () => {
    expect(compositeKey('user1', 'screen-share-user1')).toBe('user1::screen-share-user1');
  });
});

describe('startSending with two different window labels', () => {
  it('creates two separate entries for the same participant', async () => {
    const stream = new MediaStream() as unknown as MediaStream;

    await startSending('user1', 'watch-all', stream);
    await startSending('user1', 'screen-share-user1', stream);

    const senders = _getSendersForTest();
    expect(senders.size).toBe(2);
    expect(senders.has('user1::watch-all')).toBe(true);
    expect(senders.has('user1::screen-share-user1')).toBe(true);
  });

  it('ignores a duplicate answer while the first answer is still being applied', async () => {
    const stream = new MediaStream() as unknown as MediaStream;

    await startSending('user1', 'watch-all', stream);

    const sender = _getSendersForTest().get('user1::watch-all');
    expect(sender).toBeDefined();

    const pc = sender?.pc as unknown as MockRTCPeerConnection;
    let resolveRemoteDescription: VoidFunction | undefined;
    pc.setRemoteDescription = vi.fn().mockImplementation(() => new Promise<void>((resolve) => {
      resolveRemoteDescription = resolve;
    }));

    const answerListeners = eventListeners.get('ss-bridge:answer:user1::watch-all') as
      | Array<(event: { payload: { sdp: string } }) => unknown>
      | undefined;
    expect(answerListeners).toHaveLength(1);

    const onAnswer = answerListeners?.[0] as
      | ((event: { payload: { sdp: string } }) => Promise<void> | void)
      | undefined;
    expect(onAnswer).toBeDefined();

    const firstAnswer = Promise.resolve(onAnswer?.({ payload: { sdp: 'answer-1' } }));
    const duplicateAnswer = Promise.resolve(onAnswer?.({ payload: { sdp: 'answer-2' } }));

    expect(pc.setRemoteDescription).toHaveBeenCalledTimes(1);
    expect(pc.setRemoteDescription).toHaveBeenCalledWith({ type: 'answer', sdp: 'answer-1' });

    if (resolveRemoteDescription) {
      resolveRemoteDescription();
    }
    await Promise.all([firstAnswer, duplicateAnswer]);
  });
});

describe('stopSendingForWindow', () => {
  it('removes only entries for the target window label', async () => {
    const stream = new MediaStream() as unknown as MediaStream;

    await startSending('user1', 'watch-all', stream);
    await startSending('user2', 'watch-all', stream);
    await startSending('user1', 'screen-share-user1', stream);

    const senders = _getSendersForTest();
    expect(senders.size).toBe(3);

    stopSendingForWindow('watch-all');

    expect(senders.size).toBe(1);
    expect(senders.has('user1::watch-all')).toBe(false);
    expect(senders.has('user2::watch-all')).toBe(false);
    expect(senders.has('user1::screen-share-user1')).toBe(true);
  });
});

/* ─── isVideoTrackAlive ─────────────────────────────────────────── */

function makeStream(videoTracks: Partial<MediaStreamTrack>[] = []): MediaStream {
  return {
    getVideoTracks: () => videoTracks as MediaStreamTrack[],
  } as unknown as MediaStream;
}

describe('isVideoTrackAlive', () => {
  it('returns false when stream has no video tracks', () => {
    expect(isVideoTrackAlive(makeStream([]))).toBe(false);
  });

  it('returns false when video track readyState is "ended"', () => {
    expect(isVideoTrackAlive(makeStream([{ readyState: 'ended', muted: false }]))).toBe(false);
  });

  it('returns true when video track is live but muted (transient renegotiation/adaptive pause)', () => {
    expect(isVideoTrackAlive(makeStream([{ readyState: 'live', muted: true }]))).toBe(true);
  });

  // Regression: stream.active is true when audio is alive but video is ended.
  // The old stall detector used stream.active, causing infinite no-op re-attach.
  it('returns false when audio track is live but video track is ended (the bug scenario)', () => {
    const stream = {
      active: true, // stream.active is true because audio track is still alive
      getVideoTracks: () => [{ readyState: 'ended', muted: false }] as unknown as MediaStreamTrack[],
    } as unknown as MediaStream;
    expect(isVideoTrackAlive(stream)).toBe(false);
  });

  it('returns true when video track is live and not muted', () => {
    expect(isVideoTrackAlive(makeStream([{ readyState: 'live', muted: false }]))).toBe(true);
  });

  it('returns true when multiple tracks exist and at least one is live and unmuted', () => {
    const tracks: Partial<MediaStreamTrack>[] = [
      { readyState: 'ended', muted: false },
      { readyState: 'live', muted: false },
    ];
    expect(isVideoTrackAlive(makeStream(tracks))).toBe(true);
  });
});

/* ─── StreamReceiver onConnectionFailed ─────────────────────────── */

describe('StreamReceiver onConnectionFailed', () => {
  it('calls the callback when RTCPeerConnection connectionState becomes "failed"', () => {
    const receiver = new StreamReceiver('user1', 'screen-share-user1');
    const onFailed = vi.fn();

    // start() sets this.pc synchronously before the first await in the executor
    const startPromise = receiver.start(onFailed);

    const pc = receiver.pc as unknown as MockRTCPeerConnection;
    expect(pc).not.toBeNull();

    pc._simulateConnectionState('failed');

    expect(onFailed).toHaveBeenCalledOnce();

    receiver.stop();
    startPromise.catch(() => {}); // suppress 15 s timeout rejection
  });

  it('does NOT call the callback when connectionState becomes "disconnected" (may recover)', () => {
    const receiver = new StreamReceiver('user1', 'screen-share-user1');
    const onFailed = vi.fn();

    const startPromise = receiver.start(onFailed);

    const pc = receiver.pc as unknown as MockRTCPeerConnection;
    pc._simulateConnectionState('disconnected');

    expect(onFailed).not.toHaveBeenCalled();

    receiver.stop();
    startPromise.catch(() => {});
  });

  it('does NOT call the callback when connectionState becomes "connected"', () => {
    const receiver = new StreamReceiver('user1', 'screen-share-user1');
    const onFailed = vi.fn();

    const startPromise = receiver.start(onFailed);

    const pc = receiver.pc as unknown as MockRTCPeerConnection;
    pc._simulateConnectionState('connected');

    expect(onFailed).not.toHaveBeenCalled();

    receiver.stop();
    startPromise.catch(() => {});
  });

  it('does NOT call the callback after receiver.stop() cleans up the connection', () => {
    const receiver = new StreamReceiver('user1', 'screen-share-user1');
    const onFailed = vi.fn();

    const startPromise = receiver.start(onFailed);
    const pc = receiver.pc as unknown as MockRTCPeerConnection;

    receiver.stop(); // clears this.pc

    // Simulate state change after stop — callback must NOT fire
    pc._simulateConnectionState('failed');

    expect(onFailed).not.toHaveBeenCalled();
    startPromise.catch(() => {});
  });

  it('clears the pending start timeout when receiver.stop() is called', async () => {
    vi.useFakeTimers();
    try {
      const receiver = new StreamReceiver('user1', 'screen-share-user1');
      const onRejected = vi.fn();

      receiver.start().catch(onRejected);
      receiver.stop();

      await vi.advanceTimersByTimeAsync(15_000);

      expect(onRejected).not.toHaveBeenCalled();
    } finally {
      vi.useRealTimers();
    }
  });

  it('only resolves the latest start generation', async () => {
    const receiver = new StreamReceiver('user1', 'screen-share-user1');
    const firstResolved = vi.fn();
    const secondResolved = vi.fn();

    receiver.start().then(firstResolved);
    const firstPc = peerConnections[0];

    const secondStart = receiver.start().then(secondResolved);
    const secondPc = peerConnections[1];

    expect(firstPc).toBeDefined();
    expect(secondPc).toBeDefined();

    firstPc.ontrack?.({
      track: { kind: 'video' },
      streams: [],
    } as unknown as RTCTrackEvent);
    await Promise.resolve();

    expect(firstResolved).not.toHaveBeenCalled();
    expect(secondResolved).not.toHaveBeenCalled();

    secondPc.ontrack?.({
      track: { kind: 'video' },
      streams: [],
    } as unknown as RTCTrackEvent);
    await secondStart;

    expect(firstResolved).not.toHaveBeenCalled();
    expect(secondResolved).toHaveBeenCalledTimes(1);

    receiver.stop();
  });

  it('stale start promises do not fire viewer-ready side effects', async () => {
    const receiver = new StreamReceiver('user1', 'screen-share-user1');
    const readySpy = vi.fn();

    receiver.start().then(() => {
      readySpy('stale');
    });
    const firstPc = peerConnections[0];

    const latestStart = receiver.start().then(() => {
      readySpy('latest');
    });
    const secondPc = peerConnections[1];

    firstPc.ontrack?.({
      track: { kind: 'video' },
      streams: [],
    } as unknown as RTCTrackEvent);
    await Promise.resolve();

    expect(readySpy).not.toHaveBeenCalled();

    secondPc.ontrack?.({
      track: { kind: 'video' },
      streams: [],
    } as unknown as RTCTrackEvent);
    await latestStart;

    expect(readySpy).toHaveBeenCalledTimes(1);
    expect(readySpy).toHaveBeenCalledWith('latest');

    receiver.stop();
  });
});

/* ─── resendStream fast path (replaceTrack) ─────────────────────── */

function makeVideoTrack(readyState: string = 'live'): MediaStreamTrack {
  return { kind: 'video', readyState } as unknown as MediaStreamTrack;
}

describe('resendStream', () => {
  it('uses replaceTrack() when an existing connected sender is found — no stopSending/startSending', async () => {
    const track1 = makeVideoTrack();
    const stream1 = new MediaStream() as unknown as MediaStream;
    (stream1 as unknown as MockMediaStream).addTrack(track1);

    await startSending('user1', 'watch-all', stream1);

    const senders = _getSendersForTest();
    const entry = senders.get('user1::watch-all');
    expect(entry).toBeDefined();

    // Simulate the connection reaching the connected state
    const pc = entry!.pc as unknown as MockRTCPeerConnection;
    pc._simulateConnectionState('connected');

    const track2 = makeVideoTrack();
    const stream2 = new MediaStream() as unknown as MediaStream;
    (stream2 as unknown as MockMediaStream).addTrack(track2);

    await resendStream('user1', 'watch-all', stream2);

    // The peer connection must NOT have been closed (no full rebuild)
    expect(pc.close).not.toHaveBeenCalled();
    // replaceTrack must have been called on the video sender
    const videoSender = pc.getSenders().find(
      (s: MockRTCRtpSender) => s.track?.kind === 'video',
    ) as MockRTCRtpSender | undefined;
    expect(videoSender?.replaceTrack).toHaveBeenCalledWith(track2);
    // The entry must still be in the senders map (not torn down)
    expect(senders.has('user1::watch-all')).toBe(true);
  });

  it('falls back to full rebuild when no existing sender is found', async () => {
    const track = makeVideoTrack();
    const stream = new MediaStream() as unknown as MediaStream;
    (stream as unknown as MockMediaStream).addTrack(track);

    // No startSending called first — resendStream acts like startSending
    await resendStream('user1', 'watch-all', stream);

    const senders = _getSendersForTest();
    expect(senders.has('user1::watch-all')).toBe(true);
    // addTrack must have been called (startSending path)
    const entry = senders.get('user1::watch-all');
    expect((entry!.pc as unknown as MockRTCPeerConnection).addTrack).toHaveBeenCalled();
  });

  it('falls back to full rebuild when connection is not yet connected', async () => {
    const stream1 = new MediaStream() as unknown as MediaStream;
    (stream1 as unknown as MockMediaStream).addTrack(makeVideoTrack());

    await startSending('user1', 'watch-all', stream1);

    const senders = _getSendersForTest();
    const entry = senders.get('user1::watch-all');
    const pc = entry!.pc as unknown as MockRTCPeerConnection;
    // connectionState remains 'new' — not 'connected'

    const stream2 = new MediaStream() as unknown as MediaStream;
    (stream2 as unknown as MockMediaStream).addTrack(makeVideoTrack());

    await resendStream('user1', 'watch-all', stream2);

    // Full rebuild: old pc must be closed, a new entry created
    expect(pc.close).toHaveBeenCalled();
    // A new PC should have been created (peerConnections has 2 entries after beforeEach reset)
    expect(peerConnections.length).toBeGreaterThanOrEqual(2);
  });

  it('falls back to full rebuild when replaceTrack() rejects', async () => {
    const stream1 = new MediaStream() as unknown as MediaStream;
    (stream1 as unknown as MockMediaStream).addTrack(makeVideoTrack());

    await startSending('user1', 'watch-all', stream1);

    const senders = _getSendersForTest();
    const entry = senders.get('user1::watch-all');
    const pc = entry!.pc as unknown as MockRTCPeerConnection;
    pc._simulateConnectionState('connected');

    // Make replaceTrack reject to simulate codec mismatch
    const videoSender = pc.getSenders()[0] as MockRTCRtpSender;
    videoSender.replaceTrack.mockRejectedValueOnce(new Error('codec mismatch'));

    const stream2 = new MediaStream() as unknown as MediaStream;
    (stream2 as unknown as MockMediaStream).addTrack(makeVideoTrack());

    await resendStream('user1', 'watch-all', stream2);

    // Full rebuild must have occurred
    expect(pc.close).toHaveBeenCalled();
  });
});
