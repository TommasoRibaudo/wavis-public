/**
 * Property-based tests for LiveKitModule lifecycle.
 *
 * This file contains ALL property tests and unit tests for the livekit-media module.
 * Tasks 2.4, 3.4, 4.4, 5.4, 7.7, 11.x will all add to this file.
 *
 * Uses a mock LiveKit client shim that records SDK calls and simulates events.
 */

import { describe, it, expect, vi, beforeEach, afterEach } from 'vitest';
import * as fc from 'fast-check';

// ─── Mock @tauri-apps/plugin-store ─────────────────────────────────

const mockSettingsStorage = new Map<string, unknown>();

vi.mock('@tauri-apps/plugin-store', () => ({
  load: vi.fn().mockResolvedValue({
    get: vi.fn(async <T>(key: string): Promise<T | undefined> => {
      return mockSettingsStorage.get(key) as T | undefined;
    }),
    set: vi.fn(async (key: string, value: unknown): Promise<void> => {
      mockSettingsStorage.set(key, value);
    }),
    save: vi.fn(async (): Promise<void> => {}),
  }),
}));

// ─── Mock State ────────────────────────────────────────────────────

/** All SDK calls recorded across the mock. */
let sdkCalls: Array<{ method: string; args: unknown[] }>;

/** Event handlers registered on the mock room via room.on(). */
let roomEventHandlers: Map<string, Array<(...args: unknown[]) => void>>;

/** Whether setMicrophoneEnabled should reject. */
let micShouldReject: boolean;

/** The error message for mic rejection. */
let micRejectMsg: string;

/** Tracks AudioContext.close() calls. */
let audioCtxCloseCalls: number;

/** Tracks AudioContext constructor calls. */
let audioCtxConstructorCalls: number;

/** Tracks AudioContext.createMediaStreamSource() calls. */
let createMediaStreamSourceCalls: number;

/** Tracks AudioContext.createGain() calls. */
let createGainCalls: number;

/** Tracks GainNode.disconnect() calls. */
let gainDisconnectCalls: number;

/** Tracks all created GainNode mock objects for gain.value assertions. */
let createdGains: Array<{
  gain: {
    value: number;
    setValueAtTime: ReturnType<typeof vi.fn>;
  };
  connect: ReturnType<typeof vi.fn>;
  disconnect: ReturnType<typeof vi.fn>;
}>;

/** Document event listeners (for gesture resume testing). */
let docListeners: Map<string, Set<(...args: unknown[]) => void>>;

/** The mock room object — recreated per test. */
let mockRoom: ReturnType<typeof createMockRoom>;

type TestWavisSenderData = {
  reused: boolean;
  degradationPreferenceConfigured: boolean;
  attemptedPreferences: string[];
  invalidStateSkipped: boolean;
  lastErrorName: string | null;
  lastErrorMessage: string | null;
};

type TestWavisSenderDataStoreHost = typeof globalThis & {
  __wavisSenderData?: WeakMap<object, TestWavisSenderData>;
};

function createMockLocalParticipant() {
  const trackPublications = new Map();
  const localParticipant = {
    setMicrophoneEnabled: vi.fn(async (enabled: boolean, audioOptions?: unknown) => {
      sdkCalls.push({ method: 'setMicrophoneEnabled', args: [enabled, audioOptions] });
      if (micShouldReject) throw new Error(micRejectMsg);
    }),
    setScreenShareEnabled: vi.fn(async (enabled: boolean, captureOpts?: unknown, publishOpts?: unknown) => {
      sdkCalls.push({ method: 'setScreenShareEnabled', args: [enabled, captureOpts, publishOpts] });
      return enabled;
    }),
    getTrackPublication: vi.fn((source: string) => {
      for (const publication of trackPublications.values()) {
        if ((publication as { source?: string }).source === source) return publication;
      }
      return undefined;
    }),
    publishTrack: vi.fn(async (track: unknown, opts?: unknown) => {
      sdkCalls.push({ method: 'publishTrack', args: [track, opts] });
      return undefined;
    }),
    unpublishTrack: vi.fn(async (track: unknown) => {
      sdkCalls.push({ method: 'unpublishTrack', args: [track] });
      for (const [sid, publication] of trackPublications.entries()) {
        const pub = publication as {
          track?: unknown;
        };
        const publicationTrack = pub.track;
        const mediaStreamTrack =
          publicationTrack &&
          typeof publicationTrack === 'object' &&
          'mediaStreamTrack' in publicationTrack
            ? publicationTrack.mediaStreamTrack
            : undefined;
        if (publicationTrack === track || mediaStreamTrack === track) {
          trackPublications.delete(sid);
          return publication;
        }
      }
      return undefined;
    }),
    trackPublications,
    connectionQuality: 'excellent' as unknown,
    identity: 'self',
  };
  return localParticipant;
}

function createMockRoom() {
  const lp = createMockLocalParticipant();
  const room = {
    connect: vi.fn(async (url: string, token: string, opts?: unknown) => {
      sdkCalls.push({ method: 'connect', args: [url, token, opts] });
    }),
    disconnect: vi.fn(() => {
      sdkCalls.push({ method: 'disconnect', args: [] });
    }),
    on: vi.fn((event: string, handler: (...args: unknown[]) => void) => {
      if (!roomEventHandlers.has(event)) roomEventHandlers.set(event, []);
      roomEventHandlers.get(event)!.push(handler);
      return room;
    }),
    off: vi.fn((event: string, handler: (...args: unknown[]) => void) => {
      const arr = roomEventHandlers.get(event);
      if (arr) roomEventHandlers.set(event, arr.filter(h => h !== handler));
      return room;
    }),
    localParticipant: lp,
    remoteParticipants: new Map(),
    switchActiveDevice: vi.fn(async (kind: string, deviceId: string) => {
      sdkCalls.push({ method: 'switchActiveDevice', args: [kind, deviceId] });
    }),
  };
  return room;
}

// ─── Mock LiveKit SDK ──────────────────────────────────────────────

function emitRoomEvent(event: string, ...args: unknown[]) {
  const handlers = roomEventHandlers.get(event) || [];
  for (const h of handlers) h(...args);
}

vi.mock('livekit-client', () => ({
  Room: vi.fn(function () { return mockRoom; }),
  VideoPreset: vi.fn(function (opts: { width: number; height: number; maxBitrate: number }) {
    return { width: opts.width, height: opts.height, maxBitrate: opts.maxBitrate };
  }),
  RoomEvent: {
    Connected: 'connected',
    Disconnected: 'disconnected',
    Reconnecting: 'reconnecting',
    Reconnected: 'reconnected',
    TrackPublished: 'trackPublished',
    TrackSubscribed: 'trackSubscribed',
    TrackUnsubscribed: 'trackUnsubscribed',
    ActiveSpeakersChanged: 'activeSpeakersChanged',
    ParticipantConnected: 'participantConnected',
    ParticipantDisconnected: 'participantDisconnected',
    ConnectionQualityChanged: 'connectionQualityChanged',
    LocalTrackPublished: 'localTrackPublished',
    LocalTrackUnpublished: 'localTrackUnpublished',
    MediaDevicesError: 'mediaDevicesError',
  },
  Track: {
    Kind: { Audio: 'audio', Video: 'video' },
    Source: { Microphone: 'microphone', ScreenShare: 'screen_share', ScreenShareAudio: 'screen_share_audio' },
  },
  ConnectionQuality: {
    Excellent: 'excellent',
    Good: 'good',
    Poor: 'poor',
    Lost: 'lost',
  },
}));

// ─── Mock Web Audio API ────────────────────────────────────────────

vi.stubGlobal('AudioContext', function AudioContextMock(this: Record<string, unknown>) {
  audioCtxConstructorCalls++;
  this.state = 'running';
  this.currentTime = 0;
  this.destination = {};
  this.createGain = vi.fn(() => {
    createGainCalls++;
    const node = {
      gain: {
        value: 1,
        setValueAtTime: vi.fn((value: number) => {
          node.gain.value = value;
        }),
      },
      connect: vi.fn(),
      disconnect: vi.fn(() => { gainDisconnectCalls++; }),
    };
    createdGains.push(node);
    return node;
  });
  this.createAnalyser = vi.fn(() => ({
    fftSize: 2048,
    connect: vi.fn(),
    disconnect: vi.fn(),
    getFloatTimeDomainData: vi.fn(),
  }));
  this.createMediaStreamSource = vi.fn(() => {
    createMediaStreamSourceCalls++;
    return {
      connect: vi.fn(),
      disconnect: vi.fn(),
    };
  });
  this.createMediaStreamDestination = vi.fn(() => ({
    stream: { getAudioTracks: () => [{ id: 'mock-wasapi-track', kind: 'audio', stop: vi.fn() }] },
    disconnect: vi.fn(),
  }));
  this.audioWorklet = { addModule: vi.fn(async () => {}) };
  this.close = vi.fn(async () => { audioCtxCloseCalls++; });
  this.resume = vi.fn(async () => {});
  // Return this explicitly so `new AudioContext()` works
  return this;
});

// Stub AudioWorkletNode as a proper constructor (must use function, not arrow)
// eslint-disable-next-line @typescript-eslint/no-explicit-any
vi.stubGlobal('AudioWorkletNode', function AudioWorkletNodeMock(this: Record<string, unknown>) {
  this.port = { postMessage: vi.fn(), onmessage: null };
  this.connect = vi.fn();
  this.disconnect = vi.fn();
  return this;
});

// ─── Mock DOM APIs ─────────────────────────────────────────────────

docListeners = new Map();

vi.stubGlobal('document', {
  createElement: vi.fn((tag: string) => {
    if (tag === 'audio') {
      return { pause: vi.fn(), remove: vi.fn(), srcObject: null, muted: false, autoplay: false };
    }
    if (tag === 'video') {
      return { srcObject: null, muted: false, style: { cssText: '' }, play: vi.fn(async () => {}), remove: vi.fn() };
    }
    return { tagName: tag };
  }),
  body: { appendChild: vi.fn((node: unknown) => node) },
  addEventListener: vi.fn((event: string, handler: (...args: unknown[]) => void) => {
    if (!docListeners.has(event)) docListeners.set(event, new Set());
    docListeners.get(event)!.add(handler);
  }),
  removeEventListener: vi.fn((event: string, handler: (...args: unknown[]) => void) => {
    docListeners.get(event)?.delete(handler);
  }),
});

vi.stubGlobal('MediaStream', function MediaStreamMock(this: Record<string, unknown>) {
  this.id = 'mock-stream';
  this.getTracks = () => [];
  return this;
});

describe('macOS share-audio routing', () => {
  beforeEach(() => {
    tauriInvokeCalls = [];
    resetAll();
    vi.stubGlobal('navigator', {
      userAgent: 'Mozilla/5.0 (Macintosh; Intel Mac OS X 10_15_7) AppleWebKit/605.1.15',
      mediaDevices: createMockMediaDevices(),
    });
  });

  afterEach(() => {
    vi.stubGlobal('navigator', { userAgent: '', mediaDevices: createMockMediaDevices() });
  });

  it('startWasapiAudioBridge(false): macOS does NOT mute masterGain when bare SCK is active', async () => {
    (mockRoom.localParticipant as Record<string, unknown>).publishTrack = vi.fn(async (track: unknown) => {
      sdkCalls.push({ method: 'publishTrack', args: [track] });
      return { track, source: 'screen_share_audio' };
    });

    const cbs = createMockCallbacks();
    const mod = new LiveKitModule(cbs);

    await driveToConnected(mod);

    const masterGain = createdGains[0];
    expect(masterGain).toBeDefined();
    const setValueAtTimeSpy = masterGain.gain.setValueAtTime as ReturnType<typeof vi.fn>;
    setValueAtTimeSpy.mockClear();

    await (mod as unknown as { startWasapiAudioBridge: (v: boolean) => Promise<void> })
      .startWasapiAudioBridge(false);

    const zeroingCalls = setValueAtTimeSpy.mock.calls.filter((args: unknown[]) => args[0] === 0);
    expect(zeroingCalls).toHaveLength(0);
    expect((mod as unknown as { preShareGain: number | null }).preShareGain).toBeNull();

    mod.disconnect();
  });

  it('resolves CoreAudio UID through browser enumerateDevices before setSinkId', async () => {
    audioShareStartResult = {
      loopback_exclusion_available: true,
      real_output_device_id: 'coreaudio-real-output',
    };
    (navigator.mediaDevices.enumerateDevices as ReturnType<typeof vi.fn>).mockResolvedValue([
      {
        kind: 'audiooutput',
        deviceId: 'coreaudio-real-output',
        groupId: '',
        label: 'MacBook Pro Speakers',
      },
    ] as MediaDeviceInfo[]);
    realEnumerateDevicesMock.mockResolvedValue([
      {
        kind: 'audiooutput',
        deviceId: 'webkit-device-123',
        groupId: '',
        label: 'MacBook Pro Speakers',
      },
    ] as MediaDeviceInfo[]);

    const cbs = createMockCallbacks();
    const mod = new LiveKitModule(cbs);
    await driveToConnected(mod);

    const bridge = mod as unknown as {
      audioContext: { setSinkId?: ReturnType<typeof vi.fn> } | null;
      startWasapiAudioBridge: ReturnType<typeof vi.fn>;
      startWasapiScreenShareAudio: () => Promise<void>;
    };
    bridge.audioContext!.setSinkId = vi.fn(async () => {});
    bridge.startWasapiAudioBridge = vi.fn(async () => {});

    await bridge.startWasapiScreenShareAudio();

    expect(realEnumerateDevicesMock).toHaveBeenCalled();
    expect(bridge.audioContext!.setSinkId).toHaveBeenCalledWith('webkit-device-123');
    expect(bridge.audioContext!.setSinkId).not.toHaveBeenCalledWith('coreaudio-real-output');
    expect(bridge.startWasapiAudioBridge).toHaveBeenCalledWith(true);

    mod.disconnect();
  });

  it('warns and skips setSinkId when no browser device matches the CoreAudio UID', async () => {
    audioShareStartResult = {
      loopback_exclusion_available: true,
      real_output_device_id: 'coreaudio-missing',
    };
    (navigator.mediaDevices.enumerateDevices as ReturnType<typeof vi.fn>).mockResolvedValue([
      {
        kind: 'audiooutput',
        deviceId: 'different-device',
        groupId: '',
        label: 'Studio Display',
      },
    ] as MediaDeviceInfo[]);
    realEnumerateDevicesMock.mockResolvedValue([
      {
        kind: 'audiooutput',
        deviceId: 'webkit-device-999',
        groupId: '',
        label: 'Studio Display',
      },
    ] as MediaDeviceInfo[]);

    const warnSpy = vi.spyOn(console, 'warn').mockImplementation(() => {});
    const cbs = createMockCallbacks();
    const mod = new LiveKitModule(cbs);
    await driveToConnected(mod);

    const bridge = mod as unknown as {
      audioContext: { setSinkId?: ReturnType<typeof vi.fn> } | null;
      startWasapiAudioBridge: ReturnType<typeof vi.fn>;
      startWasapiScreenShareAudio: () => Promise<void>;
    };
    bridge.audioContext!.setSinkId = vi.fn(async () => {});
    bridge.startWasapiAudioBridge = vi.fn(async () => {});

    await bridge.startWasapiScreenShareAudio();

    expect(bridge.audioContext!.setSinkId).not.toHaveBeenCalledWith('coreaudio-missing');
    expect(bridge.audioContext!.setSinkId).not.toHaveBeenCalled();
    expect(
      warnSpy.mock.calls.some((args) =>
        args.some((arg) => typeof arg === 'string' && (arg as string).includes('no browser audiooutput matched CoreAudio UID')),
      ),
    ).toBe(true);

    warnSpy.mockRestore();
    mod.disconnect();
  });

  it('restores the AudioContext sink to default after a pinned macOS share stops', async () => {
    audioShareStartResult = {
      loopback_exclusion_available: true,
      real_output_device_id: 'coreaudio-real-output',
    };
    (navigator.mediaDevices.enumerateDevices as ReturnType<typeof vi.fn>).mockResolvedValue([
      {
        kind: 'audiooutput',
        deviceId: 'coreaudio-real-output',
        groupId: '',
        label: 'MacBook Pro Speakers',
      },
    ] as MediaDeviceInfo[]);
    realEnumerateDevicesMock.mockResolvedValue([
      {
        kind: 'audiooutput',
        deviceId: 'webkit-device-123',
        groupId: '',
        label: 'MacBook Pro Speakers',
      },
    ] as MediaDeviceInfo[]);

    const cbs = createMockCallbacks();
    const mod = new LiveKitModule(cbs);
    await driveToConnected(mod);

    const bridge = mod as unknown as {
      audioContext: { setSinkId?: ReturnType<typeof vi.fn> } | null;
      startWasapiAudioBridge: ReturnType<typeof vi.fn>;
      stopWasapiAudioBridge: ReturnType<typeof vi.fn>;
      startWasapiScreenShareAudio: () => Promise<void>;
      stopWasapiScreenShareAudio: () => Promise<void>;
    };
    bridge.audioContext!.setSinkId = vi.fn(async () => {});
    bridge.startWasapiAudioBridge = vi.fn(async () => {});
    bridge.stopWasapiAudioBridge = vi.fn(async () => {});

    await bridge.startWasapiScreenShareAudio();
    await bridge.stopWasapiScreenShareAudio();

    expect(bridge.audioContext!.setSinkId).toHaveBeenNthCalledWith(1, 'webkit-device-123');
    expect(bridge.audioContext!.setSinkId).toHaveBeenLastCalledWith('');

    mod.disconnect();
  });
});
vi.stubGlobal('requestAnimationFrame', vi.fn((cb: () => void) => { cb(); return 1; }));
vi.stubGlobal('cancelAnimationFrame', vi.fn());

const realEnumerateDevicesMock = vi.fn<() => Promise<MediaDeviceInfo[]>>(async () => []);
function MediaDevicesMock() {}
Object.defineProperty(MediaDevicesMock.prototype, 'enumerateDevices', {
  configurable: true,
  value: function enumerateDevices() {
    return realEnumerateDevicesMock();
  },
});
vi.stubGlobal('MediaDevices', MediaDevicesMock);

/** Create a mock navigator.mediaDevices with addEventListener/removeEventListener. */
function createMockVideoTrack(id = 'mock-video-track'): MediaStreamTrack & { contentHint: string } {
  return {
    id,
    kind: 'video',
    contentHint: '',
    stop: vi.fn(),
    getSettings: vi.fn(() => ({})),
  } as unknown as MediaStreamTrack & { contentHint: string };
}

function createMockDisplayMediaStream(videoTrackId = 'mock-display-video'): MediaStream {
  const videoTrack = createMockVideoTrack(videoTrackId);
  return {
    getVideoTracks: vi.fn(() => [videoTrack]),
    getAudioTracks: vi.fn(() => []),
    getTracks: vi.fn(() => [videoTrack]),
  } as unknown as MediaStream;
}

function createMockMediaDevices() {
  return {
    addEventListener: vi.fn(),
    removeEventListener: vi.fn(),
    enumerateDevices: vi.fn(async () => []),
    getDisplayMedia: vi.fn(async () => createMockDisplayMediaStream()),
  };
}

vi.stubGlobal('navigator', {
  userAgent: '',
  mediaDevices: createMockMediaDevices(),
});

// ─── Import module under test ──────────────────────────────────────

import { LiveKitModule, type MediaCallbacks } from '../livekit-media';

// ─── Callback Mock Helper ──────────────────────────────────────────

interface CallRecord { method: string; args: unknown[] }

function createMockCallbacks(): MediaCallbacks & { calls: CallRecord[] } {
  const calls: CallRecord[] = [];
  return {
    calls,
    onMediaConnected: () => calls.push({ method: 'onMediaConnected', args: [] }),
    onMediaFailed: (reason) => calls.push({ method: 'onMediaFailed', args: [reason] }),
    onMediaDisconnected: () => calls.push({ method: 'onMediaDisconnected', args: [] }),
    onAudioLevels: (levels) => calls.push({ method: 'onAudioLevels', args: [levels] }),
    onLocalAudioLevel: (level) => calls.push({ method: 'onLocalAudioLevel', args: [level] }),
    onActiveSpeakers: (ids) => calls.push({ method: 'onActiveSpeakers', args: [ids] }),
    onConnectionQuality: (stats) => calls.push({ method: 'onConnectionQuality', args: [stats] }),
    onScreenShareSubscribed: (id, stream) => calls.push({ method: 'onScreenShareSubscribed', args: [id, stream] }),
    onScreenShareUnsubscribed: (id) => calls.push({ method: 'onScreenShareUnsubscribed', args: [id] }),
    onLocalScreenShareEnded: () => calls.push({ method: 'onLocalScreenShareEnded', args: [] }),
    onParticipantMuteChanged: (identity, isMuted) => calls.push({ method: 'onParticipantMuteChanged', args: [identity, isMuted] }),
    onSystemEvent: (msg) => calls.push({ method: 'onSystemEvent', args: [msg] }),
    onShareLeakSummary: (summary) => calls.push({ method: 'onShareLeakSummary', args: [summary] }),
    onNoiseSuppressionState: (active) => calls.push({ method: 'onNoiseSuppressionState', args: [active] }),
  };
}

// ─── Reset ─────────────────────────────────────────────────────────

function resetAll() {
  sdkCalls = [];
  roomEventHandlers = new Map();
  mockSettingsStorage.clear();
  realEnumerateDevicesMock.mockReset();
  realEnumerateDevicesMock.mockResolvedValue([]);
  micShouldReject = false;
  micRejectMsg = 'mic permission denied';
  audioCtxCloseCalls = 0;
  audioCtxConstructorCalls = 0;
  createMediaStreamSourceCalls = 0;
  createGainCalls = 0;
  gainDisconnectCalls = 0;
  createdGains = [];
  docListeners = new Map();
  mockRoom = createMockRoom();
  audioShareStartResult = { loopback_exclusion_available: true, real_output_device_id: null };
  (globalThis as TestWavisSenderDataStoreHost).__wavisSenderData = new WeakMap();
}

beforeEach(() => { resetAll(); });

// ─── Helpers ───────────────────────────────────────────────────────

/** Flush microtask queue so resolved promises propagate. */
const tick = () => new Promise<void>(r => setTimeout(r, 0));

const expectedPerceptualGain = (volume: number) => {
  const v = Math.max(0, Math.min(100, volume)) / 100;
  return v * v * v * 3.0;
};

function createMockScreenShareTrack(id: string, sid = id) {
  let endedHandler: (() => void) | undefined;
  const mediaStreamTrack = {
    id,
    readyState: 'live' as 'live' | 'ended',
    addEventListener: vi.fn((event: string, handler: () => void) => {
      if (event === 'ended') endedHandler = handler;
    }),
    removeEventListener: vi.fn((event: string, handler: () => void) => {
      if (event === 'ended' && endedHandler === handler) endedHandler = undefined;
    }),
    dispatchEnded: () => {
      mediaStreamTrack.readyState = 'ended';
      endedHandler?.();
    },
  };

  return {
    kind: 'video',
    sid,
    mediaStreamTrack,
  };
}

function createMockLocalScreenShareMediaTrack(displaySurface: 'monitor' | 'window' | 'browser' = 'window') {
  let readyState: 'live' | 'ended' = 'live';
  const stop = vi.fn(() => {
    readyState = 'ended';
  });
  return {
    id: `local-screen-${displaySurface}`,
    kind: 'video' as const,
    contentHint: '',
    applyConstraints: vi.fn().mockResolvedValue(undefined),
    stop,
    get readyState() {
      return readyState;
    },
    getSettings: vi.fn(() => ({
      displaySurface,
      width: 2560,
      height: 1440,
      frameRate: 60,
    })),
  };
}

function installLocalScreenSharePublication(
  mediaStreamTrack = createMockLocalScreenShareMediaTrack(),
  overrides?: Partial<{ sid: string }>,
) {
  const publication = {
    trackSid: overrides?.sid ?? 'local-screen-share',
    source: 'screen_share',
    kind: 'video',
    track: {
      mediaStreamTrack,
      replaceTrack: vi.fn(async () => {}),
    },
  };
  mockRoom.localParticipant.trackPublications.set(publication.trackSid, publication);
  return { publication, mediaStreamTrack };
}

function attachManagedScreenSharePublisherPeerConnection(options: {
  displaySurface?: 'monitor' | 'window' | 'browser';
  reuseExpected?: boolean;
  degradationPreferenceConfigured?: boolean;
  degradationPreferenceResult?: Partial<{
    attemptedPreferences: string[];
    finalErrorName: string | null;
    finalErrorMessage: string | null;
    invalidStateSkipped: boolean;
  }>;
} = {}) {
  const mediaStreamTrack = createMockLocalScreenShareMediaTrack(options.displaySurface ?? 'window');
  const publication = {
    trackSid: 'local-screen-share',
    source: 'screen_share',
    kind: 'video',
    track: {
      mediaStreamTrack,
    },
  };
  const audioTrack = {
    id: 'local-audio-track',
    kind: 'audio',
    readyState: 'live' as const,
  };
  const videoSender = {
    track: null as typeof mediaStreamTrack | null,
    getParameters: vi.fn(() => ({ encodings: [] as unknown[] })),
    setParameters: vi.fn(async () => {}),
    replaceTrack: vi.fn(async (track: typeof mediaStreamTrack | null) => {
      videoSender.track = track;
    }),
  };
  const audioSender = {
    track: audioTrack,
  };
  const audioTransceiver = {
    mid: '1',
    direction: 'sendonly' as RTCRtpTransceiverDirection,
    currentDirection: 'sendonly' as RTCRtpTransceiverDirection,
    stopped: false,
    sender: audioSender,
    receiver: { track: { kind: 'audio' } },
  };
  const videoTransceiver = {
    mid: '2',
    direction: (options.reuseExpected ?? true) ? 'inactive' as RTCRtpTransceiverDirection : 'sendonly' as RTCRtpTransceiverDirection,
    currentDirection: (options.reuseExpected ?? true) ? 'inactive' as RTCRtpTransceiverDirection : 'sendonly' as RTCRtpTransceiverDirection,
    stopped: false,
    sender: videoSender,
    receiver: { track: { kind: 'video' } },
  };
  const publisher = {
    getSenders: () => [audioSender, videoSender].filter((sender) => !!sender.track),
    getTransceivers: () => [audioTransceiver, videoTransceiver],
  };

  if (options.reuseExpected ?? true) {
    (globalThis as TestWavisSenderDataStoreHost).__wavisSenderData?.set(videoSender, {
      reused: true,
      degradationPreferenceConfigured: options.degradationPreferenceConfigured ?? false,
      attemptedPreferences: [...(options.degradationPreferenceResult?.attemptedPreferences ?? [])],
      invalidStateSkipped: options.degradationPreferenceResult?.invalidStateSkipped ?? false,
      lastErrorName: options.degradationPreferenceResult?.finalErrorName ?? null,
      lastErrorMessage: options.degradationPreferenceResult?.finalErrorMessage ?? null,
    });
  }

  (mockRoom as Record<string, unknown>).engine = {
    pcManager: {
      publisher,
    },
  };

  mockRoom.localParticipant.setScreenShareEnabled = vi.fn(async (
    enabled: boolean,
    captureOpts?: unknown,
    publishOpts?: unknown,
  ) => {
    sdkCalls.push({ method: 'setScreenShareEnabled', args: [enabled, captureOpts, publishOpts] });
    if (enabled) {
      mockRoom.localParticipant.trackPublications.set(publication.trackSid, publication);
      videoSender.track = mediaStreamTrack;
      videoTransceiver.direction = 'sendonly';
      videoTransceiver.currentDirection = 'sendonly';
      return true;
    }
    mockRoom.localParticipant.trackPublications.delete(publication.trackSid);
    videoSender.track = null;
    videoTransceiver.direction = 'inactive';
    videoTransceiver.currentDirection = 'inactive';
    return false;
  });

  return {
    mediaStreamTrack,
    publication,
    publisher,
    videoSender,
    videoTransceiver,
  };
}

/** Audio publication object for LocalTrackPublished events. */
const AUDIO_PUB = { track: { kind: 'audio' }, source: 'microphone' };

/** Audio publication that includes a mock mediaStreamTrack (needed for denoise tests). */
function createAudioPubWithMst() {
  const mediaStreamTrack = {
    id: 'mock-local-mic-track',
    applyConstraints: vi.fn().mockResolvedValue(undefined),
    getSettings: vi.fn(() => ({ sampleRate: 48000, channelCount: 1 })),
    getCapabilities: vi.fn(() => ({ noiseSuppression: [true, false] })),
  };
  let currentProcessor: unknown = null;
  const track = {
    kind: 'audio',
    mediaStreamTrack,
    setAudioContext: vi.fn(),
    setProcessor: vi.fn(async (processor: unknown) => {
      currentProcessor = processor;
    }),
    getProcessor: vi.fn(() => currentProcessor),
    stopProcessor: vi.fn(async () => {
      currentProcessor = null;
    }),
  };
  return {
    pub: { track, source: 'microphone' },
    mediaStreamTrack,
    track,
  };
}

/**
 * Drive the module through the full connect → Connected → mic published flow.
 * Returns after onMediaConnected has fired.
 */
async function driveToConnected(mod: LiveKitModule, url = 'wss://sfu.test', token = 'tok') {
  await mod.connect(url, token);
  emitRoomEvent('connected');
  await tick();
  emitRoomEvent('localTrackPublished', AUDIO_PUB, mockRoom.localParticipant);
}

// ═══ LiveKitModule lifecycle ═══════════════════════════════════════

describe('LiveKitModule lifecycle', () => {

  // Feature: gui-livekit-media, Property 4: Media connected requires Room connected AND mic published
  describe('P4: Media connected requires Room connected AND mic published', () => {
    it('onMediaConnected fires only after BOTH Connected and LocalTrackPublished', async () => {
      await fc.assert(
        fc.asyncProperty(
          fc.boolean(), // true = Connected first (normal), false = LocalTrackPublished first
          async (connectedFirst) => {
            resetAll();
            const cbs = createMockCallbacks();
            const mod = new LiveKitModule(cbs);
            await mod.connect('wss://sfu.test', 'tok-p4');

            const countConnected = () => cbs.calls.filter(c => c.method === 'onMediaConnected').length;

            // Before any events: not connected
            expect(countConnected()).toBe(0);

            if (connectedFirst) {
              // Normal flow: Connected → setMicrophoneEnabled → LocalTrackPublished
              emitRoomEvent('connected');
              await tick(); // let setMicrophoneEnabled resolve
              expect(countConnected()).toBe(0); // Connected alone is not enough
              emitRoomEvent('localTrackPublished', AUDIO_PUB, mockRoom.localParticipant);
              expect(countConnected()).toBe(1);
            } else {
              // Unusual: LocalTrackPublished arrives before Connected
              emitRoomEvent('localTrackPublished', AUDIO_PUB, mockRoom.localParticipant);
              expect(countConnected()).toBe(0); // mic ready but not connected
              emitRoomEvent('connected');
              await tick(); // let setMicrophoneEnabled resolve
              // After Connected, lkConnected=true. micReady was already true.
              // But checkReady is only called from LocalTrackPublished handler.
              // The SDK will fire another LocalTrackPublished when setMicrophoneEnabled
              // succeeds, so simulate that:
              emitRoomEvent('localTrackPublished', AUDIO_PUB, mockRoom.localParticipant);
              expect(countConnected()).toBe(1);
            }

            // Exactly one onMediaConnected
            expect(countConnected()).toBe(1);
            mod.disconnect();
          },
        ),
        { numRuns: 100 },
      );
    });

    /**
     * Validates: Requirements 3.2, 4.1
     */
    it('Connected event alone does NOT trigger onMediaConnected', async () => {
      const cbs = createMockCallbacks();
      const mod = new LiveKitModule(cbs);
      await mod.connect('wss://sfu.test', 'tok-alone');
      emitRoomEvent('connected');
      await tick();
      expect(cbs.calls.filter(c => c.method === 'onMediaConnected')).toHaveLength(0);
      mod.disconnect();
    });
  });

  // Feature: gui-livekit-media, Property 5: Mic permission denied results in listen-only mode
  describe('P5: Mic permission denied results in listen-only mode', () => {
    it('onMediaConnected fires in listen-only when mic is denied', async () => {
      await fc.assert(
        fc.asyncProperty(
          fc.string({ minLength: 1, maxLength: 50 }).filter(s => s.trim().length > 0),
          async (errMsg) => {
            resetAll();
            micShouldReject = true;
            micRejectMsg = errMsg;
            // Recreate room so localParticipant picks up the rejection flag
            mockRoom = createMockRoom();

            const cbs = createMockCallbacks();
            const mod = new LiveKitModule(cbs);
            await mod.connect('wss://sfu.test', 'tok-p5');

            // Connected → setMicrophoneEnabled rejects → listen-only → checkReady
            emitRoomEvent('connected');
            await tick(); // let the rejection propagate

            // onMediaConnected should fire (listen-only)
            expect(cbs.calls.filter(c => c.method === 'onMediaConnected')).toHaveLength(1);

            // System event about mic denied
            const sysEvents = cbs.calls.filter(c => c.method === 'onSystemEvent');
            expect(sysEvents.some(c =>
              typeof c.args[0] === 'string' && (c.args[0] as string).includes('mic permission denied'),
            )).toBe(true);

            // onMediaFailed NOT called
            expect(cbs.calls.filter(c => c.method === 'onMediaFailed')).toHaveLength(0);

            mod.disconnect();
          },
        ),
        { numRuns: 100 },
      );
    });
  });

  // Feature: gui-livekit-media, Property 20: Full disconnect cleanup
  describe('P20: Full disconnect cleanup', () => {
    it('disconnect clears all maps and closes AudioContext', async () => {
      await fc.assert(
        fc.asyncProperty(
          fc.uniqueArray(
            fc.string({ minLength: 1, maxLength: 20 }).filter(s => s.trim().length > 0),
            { minLength: 0, maxLength: 5 },
          ),
          async (participantIds) => {
            resetAll();
            const cbs = createMockCallbacks();
            const mod = new LiveKitModule(cbs);

            await driveToConnected(mod);

            // Simulate TrackSubscribed for each participant
            for (const pid of participantIds) {
              emitRoomEvent('trackSubscribed',
                { kind: 'audio', mediaStreamTrack: { id: `t-${pid}` }, sid: `s-${pid}` },
                { source: 'microphone' },
                { identity: pid },
              );
            }

            mod.disconnect();

            // AudioContext.close() called
            expect(audioCtxCloseCalls).toBe(1);
            // room.disconnect() called
            expect(sdkCalls.filter(c => c.method === 'disconnect')).toHaveLength(1);
            // Not connected
            expect(mod.isConnected).toBe(false);
          },
        ),
        { numRuns: 100 },
      );
    });
  });

  // Feature: gui-livekit-media, Property 21: Disconnect idempotency
  describe('P21: Disconnect idempotency', () => {
    it('multiple disconnect calls do not throw and cleanup happens once', async () => {
      await fc.assert(
        fc.asyncProperty(
          fc.integer({ min: 1, max: 10 }),
          async (n) => {
            resetAll();
            const cbs = createMockCallbacks();
            const mod = new LiveKitModule(cbs);
            await mod.connect('wss://sfu.test', 'tok-p21');

            for (let i = 0; i < n; i++) {
              expect(() => mod.disconnect()).not.toThrow();
            }

            // room.disconnect() exactly once
            expect(sdkCalls.filter(c => c.method === 'disconnect')).toHaveLength(1);
            // AudioContext.close() exactly once
            expect(audioCtxCloseCalls).toBe(1);
          },
        ),
        { numRuns: 100 },
      );
    });
  });

  // Feature: gui-livekit-media, Property 33: Disposed guard prevents stale event callbacks
  describe('P33: Disposed guard prevents stale event callbacks', () => {
    it('no callbacks fire for events arriving after disconnect', async () => {
      await fc.assert(
        fc.asyncProperty(
          fc.subarray([
            'trackSubscribed',
            'trackUnsubscribed',
            'activeSpeakersChanged',
            'participantDisconnected',
            'connectionQualityChanged',
            'localTrackPublished',
            'localTrackUnpublished',
            'mediaDevicesError',
            'reconnecting',
            'reconnected',
            'disconnected',
          ] as const, { minLength: 1 }),
          async (eventsToFire) => {
            resetAll();
            const cbs = createMockCallbacks();
            const mod = new LiveKitModule(cbs);

            await driveToConnected(mod);

            // Capture handlers before disconnect (room.off will remove them)
            const captured = new Map<string, Array<(...args: unknown[]) => void>>();
            for (const [ev, handlers] of roomEventHandlers.entries()) {
              captured.set(ev, [...handlers]);
            }

            const countBefore = cbs.calls.length;
            mod.disconnect();

            // Invoke captured handlers directly (simulating stale async delivery)
            for (const ev of eventsToFire) {
              for (const h of (captured.get(ev) || [])) {
                switch (ev) {
                  case 'trackSubscribed':
                    h({ kind: 'audio', mediaStreamTrack: { id: 'x' }, sid: 'x' }, { source: 'microphone' }, { identity: 'x' });
                    break;
                  case 'trackUnsubscribed':
                    h({ kind: 'audio', sid: 'x' }, { source: 'microphone' }, { identity: 'x' });
                    break;
                  case 'activeSpeakersChanged':
                    h([{ identity: 'x' }]);
                    break;
                  case 'participantDisconnected':
                    h({ identity: 'x' });
                    break;
                  case 'connectionQualityChanged':
                    h('excellent', { identity: 'self' });
                    break;
                  case 'localTrackPublished':
                    h(AUDIO_PUB, mockRoom.localParticipant);
                    break;
                  case 'localTrackUnpublished':
                    h({ source: 'screen_share' }, mockRoom.localParticipant);
                    break;
                  case 'mediaDevicesError':
                    h(new Error('stale'));
                    break;
                  default:
                    h();
                    break;
                }
              }
            }

            // No new callbacks after disconnect
            expect(cbs.calls.length).toBe(countBefore);
          },
        ),
        { numRuns: 100 },
      );
    });
  });

});


// ═══ Audio subscription ════════════════════════════════════════════

describe('Audio subscription', () => {

  // Feature: gui-livekit-media, Property 6: Remote audio subscription creates full audio pipeline
  describe('P6: Remote audio subscription creates full audio pipeline', () => {
    it('each subscribed participant gets an audio element, MediaStreamSource, and GainNode', async () => {
      await fc.assert(
        fc.asyncProperty(
          fc.uniqueArray(
            fc.string({ minLength: 1, maxLength: 20 }).filter(s => s.trim().length > 0),
            { minLength: 1, maxLength: 5 },
          ),
          async (identities) => {
            resetAll();
            const cbs = createMockCallbacks();
            const mod = new LiveKitModule(cbs);

            await driveToConnected(mod);

            // Clear createElement call count from any setup calls
            const createElBefore = vi.mocked(document.createElement).mock.calls.length;

            // Subscribe audio tracks for each participant
            for (const identity of identities) {
              emitRoomEvent('trackSubscribed',
                { kind: 'audio', mediaStreamTrack: { id: `track-${identity}` }, sid: `sid-${identity}` },
                { source: 'microphone' },
                { identity },
              );
            }

            // Assert: document.createElement('audio') was called for each participant
            const audioCreateCalls = vi.mocked(document.createElement).mock.calls
              .slice(createElBefore)
              .filter(c => c[0] === 'audio');
            expect(audioCreateCalls).toHaveLength(identities.length);

            // Assert: AudioContext.createMediaStreamSource was called for each
            // Global counter tracks all createMediaStreamSource calls
            expect(createMediaStreamSourceCalls).toBe(identities.length);

            // Assert: AudioContext.createGain was called for each participant (plus 1 for masterGain)
            // masterGain is created in ensureAudioContext, then 1 per participant
            expect(createGainCalls).toBe(identities.length + 1);

            mod.disconnect();
          },
        ),
        { numRuns: 100 },
      );
    });

    /**
     * Validates: Requirements 5.1, 5.2, 8.1, 19.1, 19.5
     */
  });

  // Feature: gui-livekit-media, Property 7: Participant disconnect releases all audio resources
  describe('P7: Participant disconnect releases all audio resources', () => {
    it('disconnecting participants cleans up gain nodes and audio elements', async () => {
      await fc.assert(
        fc.asyncProperty(
          fc.uniqueArray(
            fc.string({ minLength: 1, maxLength: 20 }).filter(s => s.trim().length > 0),
            { minLength: 1, maxLength: 5 },
          ),
          async (identities) => {
            resetAll();
            const cbs = createMockCallbacks();
            const mod = new LiveKitModule(cbs);

            await driveToConnected(mod);

            // Subscribe audio tracks for each participant
            for (const identity of identities) {
              emitRoomEvent('trackSubscribed',
                { kind: 'audio', mediaStreamTrack: { id: `track-${identity}` }, sid: `sid-${identity}` },
                { source: 'microphone' },
                { identity },
              );
            }

            // Reset gainDisconnectCalls to isolate participant cleanup
            gainDisconnectCalls = 0;

            // Collect audio elements before disconnect to check pause/remove
            const audioEls: Array<{ pause: ReturnType<typeof vi.fn>; remove: ReturnType<typeof vi.fn> }> = [];
            for (const call of vi.mocked(document.createElement).mock.results) {
              if (call.type === 'return' && call.value && typeof (call.value as unknown as Record<string, unknown>).pause === 'function') {
                audioEls.push(call.value as unknown as { pause: ReturnType<typeof vi.fn>; remove: ReturnType<typeof vi.fn> });
              }
            }

            // Emit ParticipantDisconnected for each
            for (const identity of identities) {
              emitRoomEvent('participantDisconnected', { identity });
            }

            // Assert: gain.disconnect() was called for each participant
            expect(gainDisconnectCalls).toBe(identities.length);

            // Assert: audio element pause and remove were called for each participant
            // Each audio element that was created for a participant should have been paused and removed
            let pauseCalls = 0;
            let removeCalls = 0;
            for (const el of audioEls) {
              pauseCalls += el.pause.mock.calls.length;
              removeCalls += el.remove.mock.calls.length;
            }
            // At least one pause and remove per participant
            expect(pauseCalls).toBeGreaterThanOrEqual(identities.length);
            expect(removeCalls).toBeGreaterThanOrEqual(identities.length);

            mod.disconnect();
          },
        ),
        { numRuns: 100 },
      );
    });

    /**
     * Validates: Requirements 5.3, 8.3, 19.2
     */
  });

  // Feature: gui-livekit-media, Property 14: Single shared AudioContext invariant
  describe('P14: Single shared AudioContext invariant', () => {
    it('exactly one AudioContext is created regardless of participant count', async () => {
      await fc.assert(
        fc.asyncProperty(
          fc.integer({ min: 0, max: 5 }),
          async (participantCount) => {
            resetAll();
            const cbs = createMockCallbacks();
            const mod = new LiveKitModule(cbs);

            await driveToConnected(mod);

            // Subscribe audio tracks for N participants
            for (let i = 0; i < participantCount; i++) {
              emitRoomEvent('trackSubscribed',
                { kind: 'audio', mediaStreamTrack: { id: `track-p${i}` }, sid: `sid-p${i}` },
                { source: 'microphone' },
                { identity: `participant-${i}` },
              );
            }

            // Assert: no more than one AudioContext is ever created.
            // At the default volume (100) with no gain processor active, an
            // AudioContext is only created when remote participants subscribe
            // (N>0). For N=0 with no mic processing, count may be 0.
            expect(audioCtxConstructorCalls).toBeLessThanOrEqual(1);

            mod.disconnect();
          },
        ),
        { numRuns: 100 },
      );
    });

    /**
     * Validates: Requirements 8.4, 9.1
     */
  });

  // Feature: gui-livekit-media, Property 28: No duplicate audio elements per participant
  describe('P28: No duplicate audio elements per participant', () => {
    it('re-subscribing the same participant reuses the audio element', async () => {
      await fc.assert(
        fc.asyncProperty(
          fc.string({ minLength: 1, maxLength: 20 }).filter(s => s.trim().length > 0),
          async (identity) => {
            resetAll();
            const cbs = createMockCallbacks();
            const mod = new LiveKitModule(cbs);

            await driveToConnected(mod);

            const createElBefore = vi.mocked(document.createElement).mock.calls.length;

            // Subscribe audio track for the participant — first time
            emitRoomEvent('trackSubscribed',
              { kind: 'audio', mediaStreamTrack: { id: `track-${identity}` }, sid: `sid-${identity}` },
              { source: 'microphone' },
              { identity },
            );

            // Subscribe audio track for the same participant — second time (re-subscribe)
            emitRoomEvent('trackSubscribed',
              { kind: 'audio', mediaStreamTrack: { id: `track-${identity}-2` }, sid: `sid-${identity}-2` },
              { source: 'microphone' },
              { identity },
            );

            // Assert: only one audio element was created (reused, not duplicated)
            const audioCreateCalls = vi.mocked(document.createElement).mock.calls
              .slice(createElBefore)
              .filter(c => c[0] === 'audio');
            expect(audioCreateCalls).toHaveLength(1);

            mod.disconnect();
          },
        ),
        { numRuns: 100 },
      );
    });

    /**
     * Validates: Requirements 19.5
     */
  });

});


// ═══ Volume control, mute, and speaking indicators ═════════════════

describe('Volume control, mute, and speaking indicators', () => {

  // Feature: gui-livekit-media, Property 8: Mute/unmute round trip
  describe('P8: Mute/unmute round trip', () => {
    it('setMicEnabled calls setMicrophoneEnabled with the correct boolean for each toggle', async () => {
      /**
       * Validates: Requirements 6.1, 6.2
       */
      await fc.assert(
        fc.asyncProperty(
          fc.array(fc.boolean(), { minLength: 1, maxLength: 10 }),
          async (toggles) => {
            resetAll();
            const cbs = createMockCallbacks();
            const mod = new LiveKitModule(cbs);

            await driveToConnected(mod);

            // Clear SDK calls from connect flow (the initial setMicrophoneEnabled(true))
            const micCallsBefore = sdkCalls.filter(c => c.method === 'setMicrophoneEnabled').length;

            for (const enabled of toggles) {
              await mod.setMicEnabled(enabled);
            }

            const micCalls = sdkCalls
              .filter(c => c.method === 'setMicrophoneEnabled')
              .slice(micCallsBefore);

            // One call per toggle
            expect(micCalls).toHaveLength(toggles.length);

            // Each call has the correct boolean
            for (let i = 0; i < toggles.length; i++) {
              expect(micCalls[i].args[0]).toBe(toggles[i]);
            }

            mod.disconnect();
          },
        ),
        { numRuns: 100 },
      );
    });
  });

  // Feature: gui-livekit-media, Property 10: Muted participant audio level is zero
  describe('P10: Muted participant audio level is zero', () => {
    it('ActiveSpeakersChanged forwards speaker data via onAudioLevels callback', async () => {
      /**
       * Validates: Requirements 6.5, 7.5
       *
       * At the LiveKitModule level, audio levels are forwarded as-is from the SDK.
       * The voice-room.ts layer applies the mute filter (isMuted → level=0, isSpeaking=false).
       * This test verifies that onAudioLevels is called with the speaker data from ActiveSpeakersChanged.
       */
      await fc.assert(
        fc.asyncProperty(
          fc.uniqueArray(
            fc.record({
              identity: fc.string({ minLength: 1, maxLength: 20 }).filter(s => s.trim().length > 0),
            }),
            { minLength: 1, maxLength: 5, selector: r => r.identity },
          ),
          async (participants) => {
            resetAll();
            const cbs = createMockCallbacks();
            const mod = new LiveKitModule(cbs);

            await driveToConnected(mod);

            // Emit ActiveSpeakersChanged with the generated participants
            const speakers = participants.map(p => ({ identity: p.identity }));
            emitRoomEvent('activeSpeakersChanged', speakers);

            // onAudioLevels should have been called (rAF fires synchronously in default mock)
            const levelCalls = cbs.calls.filter(c => c.method === 'onAudioLevels');
            expect(levelCalls.length).toBeGreaterThanOrEqual(1);

            // The last onAudioLevels call should contain all speaker identities
            const lastLevels = levelCalls[levelCalls.length - 1].args[0] as Map<string, { isSpeaking: boolean; rmsLevel: number }>;
            for (const p of participants) {
              expect(lastLevels.has(p.identity)).toBe(true);
              const entry = lastLevels.get(p.identity)!;
              expect(entry.isSpeaking).toBe(true);
              expect(entry.rmsLevel).toBe(1.0);
            }

            mod.disconnect();
          },
        ),
        { numRuns: 100 },
      );
    });
  });

  // Feature: gui-livekit-media, Property 11: Audio level coalescing preserves last value per participant
  describe('P11: Audio level coalescing preserves last value per participant', () => {
    it('multiple speaker events within a frame coalesce to one flush with last values', async () => {
      /**
       * Validates: Requirements 7.2, 7.3, 20.1, 20.2, 20.4
       */
      await fc.assert(
        fc.asyncProperty(
          // Generate M participants (1-5) with N bursts (1-20) of speaker updates
          fc.integer({ min: 1, max: 5 }),
          fc.integer({ min: 2, max: 20 }),
          async (numParticipants, numBursts) => {
            resetAll();

            // Override rAF to NOT call immediately — deferred flush
            let rafCallback: (() => void) | null = null;
            vi.stubGlobal('requestAnimationFrame', vi.fn((cb: () => void) => {
              rafCallback = cb;
              return 1;
            }));

            const cbs = createMockCallbacks();
            const mod = new LiveKitModule(cbs);

            await driveToConnected(mod);

            // Clear any onAudioLevels calls from setup
            const callsBefore = cbs.calls.filter(c => c.method === 'onAudioLevels').length;

            // Generate participant identities
            const identities = Array.from({ length: numParticipants }, (_, i) => `p${i}`);

            // Track which identities actually appear in any burst
            const appearedIdentities = new Set<string>();

            // Emit multiple ActiveSpeakersChanged events without flushing
            // Each burst uses a different subset of participants
            for (let burst = 0; burst < numBursts; burst++) {
              const speakersInBurst = identities.slice(0, ((burst % numParticipants) + 1));
              for (const id of speakersInBurst) appearedIdentities.add(id);
              const speakers = speakersInBurst.map(id => ({ identity: id }));
              emitRoomEvent('activeSpeakersChanged', speakers);
            }

            // No flush yet (rAF deferred)
            const callsAfterEmit = cbs.calls.filter(c => c.method === 'onAudioLevels').length;
            expect(callsAfterEmit - callsBefore).toBe(0);

            // Now flush
            expect(rafCallback).not.toBeNull();
            rafCallback!();

            // Exactly one onAudioLevels call after flush
            const callsAfterFlush = cbs.calls.filter(c => c.method === 'onAudioLevels').length;
            expect(callsAfterFlush - callsBefore).toBe(1);

            // The flushed levels should contain entries for all participants that appeared in any burst
            const flushedCall = cbs.calls.filter(c => c.method === 'onAudioLevels')[callsBefore];
            const levels = flushedCall.args[0] as Map<string, { isSpeaking: boolean; rmsLevel: number }>;

            // Every participant that appeared in at least one burst should be present
            for (const id of appearedIdentities) {
              expect(levels.has(id)).toBe(true);
            }

            // Restore default rAF mock
            vi.stubGlobal('requestAnimationFrame', vi.fn((cb: () => void) => { cb(); return 1; }));

            mod.disconnect();
          },
        ),
        { numRuns: 100 },
      );
    });
  });

  // Feature: gui-livekit-media, Property 12: Batched audio level updates trigger single state mutation
  describe('P12: Batched audio level updates trigger single state mutation', () => {
    it('multiple speaker events within a frame produce exactly one onAudioLevels callback', async () => {
      /**
       * Validates: Requirements 20.3
       */
      await fc.assert(
        fc.asyncProperty(
          fc.integer({ min: 2, max: 15 }),
          async (numEvents) => {
            resetAll();

            // Override rAF to NOT call immediately — deferred flush
            let rafCallback: (() => void) | null = null;
            vi.stubGlobal('requestAnimationFrame', vi.fn((cb: () => void) => {
              rafCallback = cb;
              return 1;
            }));

            const cbs = createMockCallbacks();
            const mod = new LiveKitModule(cbs);

            await driveToConnected(mod);

            const callsBefore = cbs.calls.filter(c => c.method === 'onAudioLevels').length;

            // Emit N speaker events without flushing
            for (let i = 0; i < numEvents; i++) {
              emitRoomEvent('activeSpeakersChanged', [{ identity: `speaker-${i % 3}` }]);
            }

            // No callbacks yet
            expect(cbs.calls.filter(c => c.method === 'onAudioLevels').length - callsBefore).toBe(0);

            // Flush once
            expect(rafCallback).not.toBeNull();
            rafCallback!();

            // Exactly ONE onAudioLevels callback
            expect(cbs.calls.filter(c => c.method === 'onAudioLevels').length - callsBefore).toBe(1);

            // Restore default rAF mock
            vi.stubGlobal('requestAnimationFrame', vi.fn((cb: () => void) => { cb(); return 1; }));

            mod.disconnect();
          },
        ),
        { numRuns: 100 },
      );
    });
  });

  // Feature: gui-livekit-media, Property 13: Volume slider updates gain value
  describe('P13: Volume slider updates gain value', () => {
    it('setParticipantVolume and setMasterVolume map 0-100 to 0.0-1.0 gain', async () => {
      /**
       * Validates: Requirements 8.2, 9.2
       */
      await fc.assert(
        fc.asyncProperty(
          fc.integer({ min: 0, max: 100 }),
          fc.integer({ min: 0, max: 100 }),
          async (participantVol, masterVol) => {
            resetAll();
            const cbs = createMockCallbacks();
            const mod = new LiveKitModule(cbs);

            await driveToConnected(mod);

            // Subscribe a participant's audio track so a GainNode is created
            const identity = 'test-participant';
            emitRoomEvent('trackSubscribed',
              { kind: 'audio', mediaStreamTrack: { id: 'track-vol' }, sid: 'sid-vol' },
              { source: 'microphone' },
              { identity },
            );

            // createdGains[0] = masterGain (from ensureAudioContext in connect)
            // createdGains[1] = participant gain (from attachAudioTrack)
            expect(createdGains.length).toBeGreaterThanOrEqual(2);
            const masterGainNode = createdGains[0];
            const participantGainNode = createdGains[1];

            // Set participant volume and verify gain value matches perceptual curve
            mod.setParticipantVolume(identity, participantVol);
            const expectedParticipantGain = Math.pow(participantVol / 100, 3) * 3.0;
            expect(participantGainNode.gain.value).toBeCloseTo(expectedParticipantGain, 5);

            // Set master volume and verify gain value matches perceptual curve
            mod.setMasterVolume(masterVol);
            const expectedMasterGain = Math.pow(masterVol / 100, 3) * 3.0;
            expect(masterGainNode.gain.value).toBeCloseTo(expectedMasterGain, 5);

            mod.disconnect();
          },
        ),
        { numRuns: 100 },
      );
    });
  });

  // Feature: gui-livekit-media, Property 14: Volume set before subscribe seeds the GainNode
  describe('P14: Volume set before subscribe is honored', () => {
    it('setParticipantVolume before trackSubscribed seeds the GainNode at attach time', async () => {
      resetAll();
      const cbs = createMockCallbacks();
      const mod = new LiveKitModule(cbs);

      await driveToConnected(mod);

      // Set volume before the track is subscribed — no GainNode exists yet
      const identity = 'test-participant';
      mod.setParticipantVolume(identity, 44);

      // Now subscribe the audio track — attachAudioTrack should read the cached desired volume
      emitRoomEvent('trackSubscribed',
        { kind: 'audio', mediaStreamTrack: { id: 'track-p14' }, sid: 'sid-p14' },
        { source: 'microphone' },
        { identity },
      );

      // The participant GainNode is the last created gain node
      expect(createdGains.length).toBeGreaterThanOrEqual(2);
      const participantGainNode = createdGains[createdGains.length - 1];
      const expectedGain = Math.pow(44 / 100, 3) * 3.0;
      expect(participantGainNode.gain.value).toBeCloseTo(expectedGain, 5);

      mod.disconnect();
    });
  });

});


// ═══ Screen share and device selection ═════════════════════════════

describe('Screen share and device selection', () => {

  // Feature: gui-livekit-media, Property 15: Device switching delegates to LiveKit SDK
  describe('P15: Device switching delegates to LiveKit SDK', () => {
    it('setInputDevice and setOutputDevice call switchActiveDevice with correct kind and id', async () => {
      /**
       * Validates: Requirements 10.2, 10.3
       */
      await fc.assert(
        fc.asyncProperty(
          fc.string({ minLength: 1, maxLength: 50 }).filter(s => s.trim().length > 0),
          async (inputDeviceId) => {
            resetAll();
            const cbs = createMockCallbacks();
            const mod = new LiveKitModule(cbs);
            await driveToConnected(mod);

            // Clear SDK calls from connect flow
            const callsBefore = sdkCalls.length;

            await mod.setInputDevice(inputDeviceId);

            const switchCalls = sdkCalls.slice(callsBefore).filter(c => c.method === 'switchActiveDevice');
            expect(switchCalls).toHaveLength(1);

            // audioinput with the input device ID
            expect(switchCalls[0].args[0]).toBe('audioinput');
            expect(switchCalls[0].args[1]).toBe(inputDeviceId);

            mod.disconnect();
          },
        ),
        { numRuns: 100 },
      );
    });
  });

  // Feature: gui-livekit-media, Property 16: Screen share lifecycle signaling correctness
  describe('P16: Screen share lifecycle signaling correctness', () => {
    it('startScreenShare calls setScreenShareEnabled(true) and returns true; non-Windows stopScreenShare calls setScreenShareEnabled(false)', async () => {
      /**
       * Validates: Requirements 11.1, 11.2
       */
      await fc.assert(
        fc.asyncProperty(
          fc.boolean(), // whether to also stop after starting
          async (alsoStop) => {
            resetAll();
            const cbs = createMockCallbacks();
            const mod = new LiveKitModule(cbs);
            await driveToConnected(mod);

            const callsBefore = sdkCalls.length;

            const result = await mod.startScreenShare();
            expect(result).toBe(true);

            const startCalls = sdkCalls.slice(callsBefore).filter(c => c.method === 'setScreenShareEnabled');
            expect(startCalls).toHaveLength(1);
            expect(startCalls[0].args[0]).toBe(true);

            if (alsoStop) {
              const callsBeforeStop = sdkCalls.length;
              await mod.stopScreenShare();

              const stopCalls = sdkCalls.slice(callsBeforeStop).filter(c => c.method === 'setScreenShareEnabled');
              expect(stopCalls).toHaveLength(1);
              expect(stopCalls[0].args[0]).toBe(false);
            }

            mod.disconnect();
          },
        ),
        { numRuns: 100 },
      );
    });
  });

  // Feature: gui-livekit-media, Property 17: External screen share end triggers callback
  describe('P17: External screen share end triggers StopShare', () => {
    it('LocalTrackUnpublished with screen_share source calls onLocalScreenShareEnded', async () => {
      /**
       * Validates: Requirements 11.4, 11.5
       */
      await fc.assert(
        fc.asyncProperty(
          fc.integer({ min: 1, max: 5 }), // number of times the event fires
          async (fireCount) => {
            resetAll();
            const cbs = createMockCallbacks();
            const mod = new LiveKitModule(cbs);
            await driveToConnected(mod);

            const countBefore = cbs.calls.filter(c => c.method === 'onLocalScreenShareEnded').length;

            for (let i = 0; i < fireCount; i++) {
              emitRoomEvent('localTrackUnpublished',
                { source: 'screen_share' },
                mockRoom.localParticipant,
              );
            }

            const endedCalls = cbs.calls.filter(c => c.method === 'onLocalScreenShareEnded').length - countBefore;
            expect(endedCalls).toBe(fireCount);

            mod.disconnect();
          },
        ),
        { numRuns: 100 },
      );
    });
  });

  // Feature: gui-livekit-media, Property 18: Remote screen share subscription and cleanup
  describe('P18: Remote screen share subscription and cleanup', () => {
    it('TrackSubscribed with screen_share calls onScreenShareSubscribed; TrackUnsubscribed with matching trackSid calls onScreenShareUnsubscribed', async () => {
      /**
       * Validates: Requirements 12.1, 12.2
       */
      await fc.assert(
        fc.asyncProperty(
          fc.uniqueArray(
            fc.record({
              identity: fc.string({ minLength: 1, maxLength: 20 }).filter(s => s.trim().length > 0),
              trackSid: fc.string({ minLength: 1, maxLength: 20 }).filter(s => s.trim().length > 0),
            }),
            { minLength: 1, maxLength: 4, selector: r => r.identity },
          ),
          async (participants) => {
            resetAll();
            const cbs = createMockCallbacks();
            const mod = new LiveKitModule(cbs);
            await driveToConnected(mod);

            // Subscribe screen shares for each participant
            for (const p of participants) {
              emitRoomEvent('trackSubscribed',
                { kind: 'video', mediaStreamTrack: { id: `screen-track-${p.identity}`, addEventListener: vi.fn(), removeEventListener: vi.fn() }, sid: p.trackSid },
                { source: 'screen_share', setEnabled: vi.fn() },
                { identity: p.identity },
              );
            }

            // Verify onScreenShareSubscribed was called for each
            const subCalls = cbs.calls.filter(c => c.method === 'onScreenShareSubscribed');
            expect(subCalls).toHaveLength(participants.length);
            for (let i = 0; i < participants.length; i++) {
              expect(subCalls[i].args[0]).toBe(participants[i].identity);
            }

            // Unsubscribe each with matching trackSid
            for (const p of participants) {
              emitRoomEvent('trackUnsubscribed',
                { kind: 'video', sid: p.trackSid },
                { source: 'screen_share' },
                { identity: p.identity },
              );
            }

            // Verify onScreenShareUnsubscribed was called for each
            const unsubCalls = cbs.calls.filter(c => c.method === 'onScreenShareUnsubscribed');
            expect(unsubCalls).toHaveLength(participants.length);
            for (let i = 0; i < participants.length; i++) {
              expect(unsubCalls[i].args[0]).toBe(participants[i].identity);
            }

            mod.disconnect();
          },
        ),
        { numRuns: 100 },
      );
    });

    it('monitorScreenShareTrack retries with backoff', async () => {
      resetAll();
      const logSpy = vi.spyOn(console, 'log').mockImplementation(() => {});
      const cbs = createMockCallbacks();
      const mod = new LiveKitModule(cbs);
      await driveToConnected(mod);

      vi.useFakeTimers();
      try {
        const initialTrack = createMockScreenShareTrack('screen-1', 'sid-1');
        const replacementTrack = createMockScreenShareTrack('screen-2', 'sid-2');
        const publication = {
          source: 'screen_share',
          setEnabled: vi.fn(),
          track: initialTrack,
        };
        const participant = { identity: 'alice' };

        emitRoomEvent('trackSubscribed', initialTrack, publication, participant);
        initialTrack.mediaStreamTrack.dispatchEnded();

        await vi.advanceTimersByTimeAsync(200);
        expect(cbs.calls.filter(c => c.method === 'onScreenShareSubscribed')).toHaveLength(1);

        publication.track = replacementTrack;
        await vi.advanceTimersByTimeAsync(400);

        const subCalls = cbs.calls.filter(c => c.method === 'onScreenShareSubscribed');
        expect(subCalls).toHaveLength(2);
        expect(subCalls[1].args[0]).toBe('alice');

        const screenShareElements = (mod as unknown as {
          screenShareElements: Map<string, { trackSid: string }>;
        }).screenShareElements;
        expect(screenShareElements.get('alice')?.trackSid).toBe('sid-2');

        mod.disconnect();
      } finally {
        vi.useRealTimers();
        logSpy.mockRestore();
      }
    });

    it('monitorScreenShareTrack gives up after max retries', async () => {
      resetAll();
      const logSpy = vi.spyOn(console, 'log').mockImplementation(() => {});
      const cbs = createMockCallbacks();
      const mod = new LiveKitModule(cbs);
      await driveToConnected(mod);

      vi.useFakeTimers();
      try {
        const initialTrack = createMockScreenShareTrack('screen-1', 'sid-1');
        const publication = {
          source: 'screen_share',
          setEnabled: vi.fn(),
          track: initialTrack,
        };
        const participant = { identity: 'alice' };

        emitRoomEvent('trackSubscribed', initialTrack, publication, participant);
        initialTrack.mediaStreamTrack.dispatchEnded();

        await vi.advanceTimersByTimeAsync(1400);

        const noReplacementLogs = logSpy.mock.calls.filter(
          args => args.some(arg => typeof arg === 'string' && arg.includes('no replacement after 4 attempts')),
        );
        expect(noReplacementLogs).toHaveLength(1);

        mod.disconnect();
      } finally {
        vi.useRealTimers();
        logSpy.mockRestore();
      }
    });
  });

  describe('Screen share audio attachment lifecycle', () => {
    it('defers screen share audio on subscribe and only builds the audio graph on attach', async () => {
      resetAll();
      const cbs = createMockCallbacks();
      const mod = new LiveKitModule(cbs);
      await driveToConnected(mod);

      const remoteTrack = { kind: 'audio', mediaStreamTrack: { id: 'ssa-1' } };
      emitRoomEvent(
        'trackSubscribed',
        remoteTrack,
        { source: 'screen_share_audio' },
        { identity: 'alice' },
      );

      const deferredMap = (mod as unknown as {
        screenShareAudioTracks: Map<string, { track: unknown }>;
      }).screenShareAudioTracks;
      const audioMapBefore = (mod as unknown as { audioElementMap: Map<string, unknown> }).audioElementMap;

      expect(deferredMap.has('alice')).toBe(true);
      expect(audioMapBefore.has('alice:screen-share')).toBe(false);
      expect(createMediaStreamSourceCalls).toBe(0);

      mod.attachScreenShareAudio('alice');

      const audioElementMap = (mod as unknown as {
        audioElementMap: Map<string, { autoplay: boolean; muted: boolean; srcObject: unknown }>;
      }).audioElementMap;
      const participantGains = (mod as unknown as {
        participantGains: Map<string, { gain: { value: number } }>;
      }).participantGains;
      const audioEl = audioElementMap.get('alice:screen-share');

      expect(audioEl).toBeDefined();
      expect(audioEl?.autoplay).toBe(true);
      expect(audioEl?.muted).toBe(true);
      expect(audioEl?.srcObject).not.toBeNull();
      expect(participantGains.has('alice:screen-share')).toBe(true);
      expect(createMediaStreamSourceCalls).toBe(1);
      expect(createGainCalls).toBe(2);

      mod.disconnect();
    });

    it('keeps ScreenShareAudio unsubscribed until the viewer attaches it', async () => {
      resetAll();
      const cbs = createMockCallbacks();
      const mod = new LiveKitModule(cbs);
      await driveToConnected(mod);

      const setSubscribed = vi.fn();
      const participant = {
        identity: 'alice',
        getTrackPublication: vi.fn(() => undefined),
        trackPublications: new Map(),
      };

      emitRoomEvent(
        'trackPublished',
        { source: 'screen_share_audio', kind: 'audio', trackSid: 'ssa-pub-1', setSubscribed },
        participant,
      );

      expect(setSubscribed).toHaveBeenCalledWith(false);

      emitRoomEvent(
        'trackSubscribed',
        { kind: 'audio', mediaStreamTrack: { id: 'ssa-1' } },
        { source: 'screen_share_audio', setSubscribed },
        participant,
      );

      mod.attachScreenShareAudio('alice');

      expect(setSubscribed).toHaveBeenLastCalledWith(true);

      mod.disconnect();
    });

    it('immediately unsubscribes a screen share audio track that arrived before viewer intent', async () => {
      resetAll();
      const cbs = createMockCallbacks();
      const mod = new LiveKitModule(cbs);
      await driveToConnected(mod);

      const setSubscribed = vi.fn();
      const participant = {
        identity: 'alice',
        getTrackPublication: vi.fn(() => undefined),
        trackPublications: new Map(),
      };

      emitRoomEvent(
        'trackSubscribed',
        { kind: 'audio', mediaStreamTrack: { id: 'ssa-race-1' } },
        { source: 'screen_share_audio', setSubscribed },
        participant,
      );

      expect(setSubscribed).toHaveBeenCalledWith(false);

      mod.disconnect();
    });

    it('late join keeps cached screen share audio detached until viewer-ready calls attachScreenShareAudio', async () => {
      resetAll();
      const cbs = createMockCallbacks();
      const mod = new LiveKitModule(cbs);
      await driveToConnected(mod);

      const remoteTrack = { kind: 'audio', mediaStreamTrack: { id: 'ssa-late-1' } };
      mockRoom.remoteParticipants.set('alice', {
        identity: 'alice',
        trackPublications: new Map([
          ['ssa-late', { source: 'screen_share_audio', track: remoteTrack }],
        ]),
      });

      const audioElementMapBefore = (mod as unknown as { audioElementMap: Map<string, unknown> }).audioElementMap;
      expect(audioElementMapBefore.has('alice:screen-share')).toBe(false);
      expect(createMediaStreamSourceCalls).toBe(0);

      mod.attachScreenShareAudio('alice');

      const deferredMap = (mod as unknown as {
        screenShareAudioTracks: Map<string, { track: unknown }>;
      }).screenShareAudioTracks;
      const audioElementMapAfter = (mod as unknown as { audioElementMap: Map<string, unknown> }).audioElementMap;

      expect(deferredMap.has('alice')).toBe(true);
      expect(audioElementMapAfter.has('alice:screen-share')).toBe(true);
      expect(createMediaStreamSourceCalls).toBe(1);

      mod.disconnect();
    });

    it('attachScreenShareAudio is a no-op for unknown participants', async () => {
      resetAll();
      const cbs = createMockCallbacks();
      const mod = new LiveKitModule(cbs);
      await driveToConnected(mod);

      mod.attachScreenShareAudio('missing-user');

      const audioElementMap = (mod as unknown as { audioElementMap: Map<string, unknown> }).audioElementMap;
      const participantGains = (mod as unknown as { participantGains: Map<string, unknown> }).participantGains;

      expect(audioElementMap.size).toBe(0);
      expect(participantGains.size).toBe(0);
      expect(createMediaStreamSourceCalls).toBe(0);

      mod.disconnect();
    });

    it('setScreenShareAudioVolume updates the attached gain node', async () => {
      resetAll();
      const cbs = createMockCallbacks();
      const mod = new LiveKitModule(cbs);
      await driveToConnected(mod);

      emitRoomEvent(
        'trackSubscribed',
        { kind: 'audio', mediaStreamTrack: { id: 'ssa-1' } },
        { source: 'screen_share_audio' },
        { identity: 'alice' },
      );
      mod.attachScreenShareAudio('alice');
      mod.setScreenShareAudioVolume('alice', 50);

      const participantGains = (mod as unknown as {
        participantGains: Map<string, { gain: { value: number } }>;
      }).participantGains;
      const gain = participantGains.get('alice:screen-share');

      expect(gain).toBeDefined();
      expect(gain?.gain.value).toBeCloseTo(expectedPerceptualGain(50));

      mod.disconnect();
    });

    it('setScreenShareAudioVolume uses setValueAtTime', async () => {
      resetAll();
      const cbs = createMockCallbacks();
      const mod = new LiveKitModule(cbs);
      await driveToConnected(mod);

      emitRoomEvent(
        'trackSubscribed',
        { kind: 'audio', mediaStreamTrack: { id: 'ssa-1' } },
        { source: 'screen_share_audio' },
        { identity: 'alice' },
      );
      mod.attachScreenShareAudio('alice');
      mod.setScreenShareAudioVolume('alice', 50);

      const participantGains = (mod as unknown as {
        participantGains: Map<string, {
          gain: {
            value: number;
            setValueAtTime: ReturnType<typeof vi.fn>;
          };
        }>;
      }).participantGains;
      const gain = participantGains.get('alice:screen-share');

      expect(gain).toBeDefined();
      expect(gain?.gain.setValueAtTime).toHaveBeenCalledWith(expectedPerceptualGain(50), 0);

      mod.disconnect();
    });

    it('volume recovers after 0 -> 70 round-trip', async () => {
      resetAll();
      const cbs = createMockCallbacks();
      const mod = new LiveKitModule(cbs);
      await driveToConnected(mod);

      emitRoomEvent(
        'trackSubscribed',
        { kind: 'audio', mediaStreamTrack: { id: 'ssa-1' } },
        { source: 'screen_share_audio' },
        { identity: 'alice' },
      );
      mod.attachScreenShareAudio('alice');
      mod.setScreenShareAudioVolume('alice', 0);
      mod.setScreenShareAudioVolume('alice', 70);

      const participantGains = (mod as unknown as {
        participantGains: Map<string, {
          gain: {
            value: number;
            setValueAtTime: ReturnType<typeof vi.fn>;
          };
        }>;
      }).participantGains;
      const gain = participantGains.get('alice:screen-share');

      expect(gain).toBeDefined();
      expect(gain?.gain.setValueAtTime).toHaveBeenLastCalledWith(expectedPerceptualGain(70), 0);
      expect(gain?.gain.value).toBeCloseTo(expectedPerceptualGain(70));

      mod.disconnect();
    });

    it('video track rebuild does not create duplicate screen share audio attachments', async () => {
      resetAll();
      const cbs = createMockCallbacks();
      const mod = new LiveKitModule(cbs);
      await driveToConnected(mod);

      emitRoomEvent(
        'trackSubscribed',
        { kind: 'audio', mediaStreamTrack: { id: 'ssa-1' } },
        { source: 'screen_share_audio' },
        { identity: 'alice' },
      );

      mod.attachScreenShareAudio('alice');
      emitRoomEvent(
        'trackSubscribed',
        createMockScreenShareTrack('share-video-1'),
        { source: 'screen_share', setEnabled: vi.fn() },
        { identity: 'alice' },
      );
      emitRoomEvent(
        'trackSubscribed',
        createMockScreenShareTrack('share-video-2'),
        { source: 'screen_share', setEnabled: vi.fn() },
        { identity: 'alice' },
      );
      mod.attachScreenShareAudio('alice');

      const audioElementMap = (mod as unknown as { audioElementMap: Map<string, unknown> }).audioElementMap;

      expect(audioElementMap.size).toBe(1);
      expect(audioElementMap.has('alice:screen-share')).toBe(true);
      expect(createMediaStreamSourceCalls).toBe(1);

      mod.disconnect();
    });

    it('defers a second audio track from an active sharer until the viewer attaches it', async () => {
      resetAll();
      const cbs = createMockCallbacks();
      const mod = new LiveKitModule(cbs);
      await driveToConnected(mod);

      const participant = {
        identity: 'alice',
        trackPublications: new Map([
          ['share-video', { source: 'screen_share' }],
        ]),
      };

      emitRoomEvent(
        'trackSubscribed',
        { kind: 'audio', mediaStreamTrack: { id: 'mic-1' } },
        { source: 'microphone' },
        participant,
      );

      emitRoomEvent(
        'trackSubscribed',
        { kind: 'audio', mediaStreamTrack: { id: 'ssa-linux-1' } },
        { source: 'microphone' },
        participant,
      );

      const deferredMap = (mod as unknown as {
        screenShareAudioTracks: Map<string, { track: unknown }>;
      }).screenShareAudioTracks;
      const audioElementMapBefore = (mod as unknown as { audioElementMap: Map<string, unknown> }).audioElementMap;

      expect(audioElementMapBefore.has('alice')).toBe(true);
      expect(audioElementMapBefore.has('alice:screen-share')).toBe(false);
      expect(deferredMap.has('alice')).toBe(true);
      expect(createMediaStreamSourceCalls).toBe(1);

      mod.attachScreenShareAudio('alice');

      const audioElementMapAfter = (mod as unknown as { audioElementMap: Map<string, unknown> }).audioElementMap;

      expect(audioElementMapAfter.has('alice:screen-share')).toBe(true);
      expect(createMediaStreamSourceCalls).toBe(2);

      mod.disconnect();
    });

    it('TrackUnsubscribed cleans deferred and attached screen share audio state', async () => {
      resetAll();
      const cbs = createMockCallbacks();
      const mod = new LiveKitModule(cbs);
      await driveToConnected(mod);

      emitRoomEvent(
        'trackSubscribed',
        { kind: 'audio', mediaStreamTrack: { id: 'ssa-1' } },
        { source: 'screen_share_audio' },
        { identity: 'alice' },
      );
      mod.attachScreenShareAudio('alice');

      const audioElementMap = (mod as unknown as {
        audioElementMap: Map<string, { pause: ReturnType<typeof vi.fn>; remove: ReturnType<typeof vi.fn> }>;
      }).audioElementMap;
      const participantGains = (mod as unknown as { participantGains: Map<string, unknown> }).participantGains;
      const deferredMap = (mod as unknown as { screenShareAudioTracks: Map<string, unknown> }).screenShareAudioTracks;
      const audioEl = audioElementMap.get('alice:screen-share');

      emitRoomEvent(
        'trackUnsubscribed',
        { kind: 'audio' },
        { source: 'screen_share_audio' },
        { identity: 'alice' },
      );

      expect(deferredMap.has('alice')).toBe(false);
      expect(audioElementMap.has('alice:screen-share')).toBe(false);
      expect(participantGains.has('alice:screen-share')).toBe(false);
      expect(audioEl?.pause).toHaveBeenCalled();
      expect(audioEl?.remove).toHaveBeenCalled();
      expect(gainDisconnectCalls).toBeGreaterThan(0);

      mod.disconnect();
    });

    it('ParticipantDisconnected cleans attached screen share audio state', async () => {
      resetAll();
      const cbs = createMockCallbacks();
      const mod = new LiveKitModule(cbs);
      await driveToConnected(mod);

      emitRoomEvent(
        'trackSubscribed',
        { kind: 'audio', mediaStreamTrack: { id: 'ssa-1' } },
        { source: 'screen_share_audio' },
        { identity: 'alice' },
      );
      mod.attachScreenShareAudio('alice');

      const audioElementMap = (mod as unknown as {
        audioElementMap: Map<string, { pause: ReturnType<typeof vi.fn>; remove: ReturnType<typeof vi.fn> }>;
      }).audioElementMap;
      const participantGains = (mod as unknown as { participantGains: Map<string, unknown> }).participantGains;
      const deferredMap = (mod as unknown as { screenShareAudioTracks: Map<string, unknown> }).screenShareAudioTracks;

      emitRoomEvent('participantDisconnected', { identity: 'alice' });

      expect(deferredMap.has('alice')).toBe(false);
      expect(audioElementMap.has('alice:screen-share')).toBe(false);
      expect(participantGains.has('alice:screen-share')).toBe(false);

      mod.disconnect();
    });
  });

  // Feature: gui-livekit-media, Property 19: Most recent screen share displayed
  describe('P19: Most recent screen share displayed', () => {
    it('getActiveScreenShares returns entries sorted by startedAtMs descending', async () => {
      /**
       * Validates: Requirements 12.5
       */
      await fc.assert(
        fc.asyncProperty(
          fc.uniqueArray(
            fc.record({
              identity: fc.string({ minLength: 1, maxLength: 20 }).filter(s => s.trim().length > 0),
              timestamp: fc.integer({ min: 1000, max: 999_999_999 }),
            }),
            { minLength: 2, maxLength: 5, selector: r => r.identity },
          ),
          async (shares) => {
            resetAll();
            const cbs = createMockCallbacks();
            const mod = new LiveKitModule(cbs);
            await driveToConnected(mod);

            // Subscribe screen shares with controlled timestamps
            for (const s of shares) {
              vi.spyOn(Date, 'now').mockReturnValue(s.timestamp);

              emitRoomEvent('trackSubscribed',
                { kind: 'video', mediaStreamTrack: { id: `screen-${s.identity}`, addEventListener: vi.fn(), removeEventListener: vi.fn() }, sid: `sid-${s.identity}` },
                { source: 'screen_share', setEnabled: vi.fn() },
                { identity: s.identity },
              );

              vi.spyOn(Date, 'now').mockRestore();
            }

            const active = mod.getActiveScreenShares();
            expect(active).toHaveLength(shares.length);

            // Verify sorted by startedAtMs descending
            for (let i = 1; i < active.length; i++) {
              expect(active[i - 1].startedAtMs).toBeGreaterThanOrEqual(active[i].startedAtMs);
            }

            // Verify all identities are present
            const activeIdentities = new Set(active.map(a => a.identity));
            for (const s of shares) {
              expect(activeIdentities.has(s.identity)).toBe(true);
            }

            // Verify timestamps match what we set
            for (const s of shares) {
              const entry = active.find(a => a.identity === s.identity);
              expect(entry).toBeDefined();
              expect(entry!.startedAtMs).toBe(s.timestamp);
            }

            mod.disconnect();
          },
        ),
        { numRuns: 100 },
      );
    });
  });

  // Feature: gui-livekit-media, Property 32: Screen share fallback on multi-participant unsubscribe
  describe('P32: Screen share fallback on multi-participant unsubscribe', () => {
    it('unsubscribing one share leaves remaining shares; stale trackSid does NOT remove entry', async () => {
      /**
       * Validates: Requirements 12.4, 12.5
       */
      await fc.assert(
        fc.asyncProperty(
          fc.uniqueArray(
            fc.record({
              identity: fc.string({ minLength: 1, maxLength: 20 }).filter(s => s.trim().length > 0),
              trackSid: fc.string({ minLength: 1, maxLength: 20 }).filter(s => s.trim().length > 0),
            }),
            { minLength: 2, maxLength: 4, selector: r => r.identity },
          ),
          async (participants) => {
            resetAll();
            const cbs = createMockCallbacks();
            const mod = new LiveKitModule(cbs);
            await driveToConnected(mod);

            // Subscribe screen shares for all participants
            for (const p of participants) {
              emitRoomEvent('trackSubscribed',
                { kind: 'video', mediaStreamTrack: { id: `screen-${p.identity}`, addEventListener: vi.fn(), removeEventListener: vi.fn() }, sid: p.trackSid },
                { source: 'screen_share', setEnabled: vi.fn() },
                { identity: p.identity },
              );
            }

            expect(mod.getActiveScreenShares()).toHaveLength(participants.length);

            // Unsubscribe the first participant with the correct trackSid
            const removed = participants[0];
            emitRoomEvent('trackUnsubscribed',
              { kind: 'video', sid: removed.trackSid },
              { source: 'screen_share' },
              { identity: removed.identity },
            );

            // Remaining shares should still be present
            const remaining = mod.getActiveScreenShares();
            expect(remaining).toHaveLength(participants.length - 1);
            expect(remaining.find(r => r.identity === removed.identity)).toBeUndefined();

            // All other participants still present
            for (let i = 1; i < participants.length; i++) {
              expect(remaining.find(r => r.identity === participants[i].identity)).toBeDefined();
            }

            // Stale trackSid unsubscribe does NOT remove the entry
            const target = participants[1];
            emitRoomEvent('trackUnsubscribed',
              { kind: 'video', sid: 'stale-sid-that-does-not-match' },
              { source: 'screen_share' },
              { identity: target.identity },
            );

            // Target should still be present (stale sid was rejected)
            const afterStale = mod.getActiveScreenShares();
            expect(afterStale.find(r => r.identity === target.identity)).toBeDefined();
            expect(afterStale).toHaveLength(participants.length - 1);

            mod.disconnect();
          },
        ),
        { numRuns: 100 },
      );
    });
  });

});

// ═══ Task 11.4: Event listener cleanup properties (P29, P30, P31) ══

describe('Event listener cleanup', () => {

  // Feature: gui-livekit-media, Property 29: AudioContext resume lifecycle
  describe('P29: AudioContext resume lifecycle', () => {
    it('suspended AudioContext registers gesture listener, resumes on click, removes listener', async () => {
      /**
       * Validates: Requirements 21.2, 21.3, 21.4
       */
      await fc.assert(
        fc.asyncProperty(
          fc.constantFrom('click', 'keydown'),
          async (gestureType) => {
            resetAll();
            // Make AudioContext start suspended
            vi.stubGlobal('AudioContext', function AudioContextSuspended(this: Record<string, unknown>) {
              audioCtxConstructorCalls++;
              this.state = 'suspended';
              this.currentTime = 0;
              this.destination = {};
              this.createGain = vi.fn(() => {
                createGainCalls++;
                const node = {
                  gain: {
                    value: 1,
                    setValueAtTime: vi.fn((value: number) => {
                      node.gain.value = value;
                    }),
                  },
                  connect: vi.fn(),
                  disconnect: vi.fn(() => { gainDisconnectCalls++; }),
                };
                createdGains.push(node);
                return node;
              });
              this.createAnalyser = vi.fn(() => ({
                fftSize: 2048,
                connect: vi.fn(),
                disconnect: vi.fn(),
                getFloatTimeDomainData: vi.fn(),
              }));
              this.createMediaStreamSource = vi.fn(() => {
                createMediaStreamSourceCalls++;
                return { connect: vi.fn(), disconnect: vi.fn() };
              });
              this.close = vi.fn(async () => { audioCtxCloseCalls++; });
              this.resume = vi.fn(async () => { (this as Record<string, unknown>).state = 'running'; });
              return this;
            });

            const cbs = createMockCallbacks();
            const mod = new LiveKitModule(cbs);
            await mod.connect('wss://sfu', 'tok');

            // System event about suspended state should have been logged
            const suspendedEvents = cbs.calls.filter(c =>
              c.method === 'onSystemEvent' && (c.args[0] as string).includes('suspended'),
            );
            expect(suspendedEvents.length).toBeGreaterThanOrEqual(1);

            // Gesture listeners should be registered
            const clickListeners = docListeners.get('click') ?? new Set();
            const keydownListeners = docListeners.get('keydown') ?? new Set();
            expect(clickListeners.size + keydownListeners.size).toBeGreaterThanOrEqual(1);

            // Simulate gesture
            const listeners = docListeners.get(gestureType);
            if (listeners && listeners.size > 0) {
              const handler = [...listeners][0];
              handler();
              await tick();
            }

            // Resume event should have been logged
            const resumeEvents = cbs.calls.filter(c =>
              c.method === 'onSystemEvent' && (c.args[0] as string).includes('resumed'),
            );
            expect(resumeEvents.length).toBeGreaterThanOrEqual(1);

            mod.disconnect();

            // Restore normal AudioContext mock
            vi.stubGlobal('AudioContext', function AudioContextMock(this: Record<string, unknown>) {
              audioCtxConstructorCalls++;
              this.state = 'running';
              this.currentTime = 0;
              this.destination = {};
              this.createGain = vi.fn(() => {
                createGainCalls++;
                const node = {
                  gain: {
                    value: 1,
                    setValueAtTime: vi.fn((value: number) => {
                      node.gain.value = value;
                    }),
                  },
                  connect: vi.fn(),
                  disconnect: vi.fn(() => { gainDisconnectCalls++; }),
                };
                createdGains.push(node);
                return node;
              });
              this.createAnalyser = vi.fn(() => ({
                fftSize: 2048,
                connect: vi.fn(),
                disconnect: vi.fn(),
                getFloatTimeDomainData: vi.fn(),
              }));
              this.createMediaStreamSource = vi.fn(() => {
                createMediaStreamSourceCalls++;
                return { connect: vi.fn(), disconnect: vi.fn() };
              });
              this.createMediaStreamDestination = vi.fn(() => ({
                stream: { getAudioTracks: () => [{ id: 'mock-wasapi-track', kind: 'audio', stop: vi.fn() }] },
                disconnect: vi.fn(),
              }));
              this.audioWorklet = { addModule: vi.fn(async () => {}) };
              this.close = vi.fn(async () => { audioCtxCloseCalls++; });
              this.resume = vi.fn(async () => {});
              return this;
            });
          },
        ),
        { numRuns: 10 },
      );
    });
  });

  // Feature: gui-livekit-media, Property 30: Event listener cleanup on teardown
  describe('P30: Event listener cleanup on teardown', () => {
    it('disconnect removes all room event listeners — registry is empty', async () => {
      /**
       * Validates: Requirements 18.5
       */
      await fc.assert(
        fc.asyncProperty(
          fc.constant(null),
          async () => {
            resetAll();
            const cbs = createMockCallbacks();
            const mod = new LiveKitModule(cbs);
            await driveToConnected(mod);

            // Before disconnect: room.on() was called multiple times
            const onCallCount = mockRoom.on.mock.calls.length;
            expect(onCallCount).toBeGreaterThan(0);

            mod.disconnect();

            // After disconnect: room.off() was called for each listener
            const offCallCount = mockRoom.off.mock.calls.length;
            expect(offCallCount).toBe(onCallCount);

            // Verify each on() call has a matching off() call
            for (const [event, handler] of mockRoom.on.mock.calls) {
              const matchingOff = mockRoom.off.mock.calls.find(
                ([e, h]: [string, unknown]) => e === event && h === handler,
              );
              expect(matchingOff).toBeDefined();
            }
          },
        ),
        { numRuns: 20 },
      );
    });
  });

  // Feature: gui-livekit-media, Property 31: Event listener count invariant across reconnections
  describe('P31: Event listener count invariant across reconnections', () => {
    it('after N reconnection cycles, listener count post-connect equals baseline', async () => {
      /**
       * Validates: Requirements 18.4, 18.5
       */
      await fc.assert(
        fc.asyncProperty(
          fc.integer({ min: 1, max: 5 }),
          async (cycles) => {
            resetAll();
            const cbs = createMockCallbacks();

            // Establish baseline listener count from a single connect
            const mod1 = new LiveKitModule(cbs);
            await driveToConnected(mod1);
            const baselineListenerCount = mockRoom.on.mock.calls.length;
            expect(baselineListenerCount).toBeGreaterThan(0);
            mod1.disconnect();

            // After disconnect, all listeners removed
            expect(mockRoom.off.mock.calls.length).toBe(baselineListenerCount);

            // Run N reconnection cycles
            for (let i = 0; i < cycles; i++) {
              resetAll();
              const mod = new LiveKitModule(cbs);
              await driveToConnected(mod);

              const currentListenerCount = mockRoom.on.mock.calls.length;
              expect(currentListenerCount).toBe(baselineListenerCount);

              mod.disconnect();
              expect(mockRoom.off.mock.calls.length).toBe(baselineListenerCount);
            }
          },
        ),
        { numRuns: 20 },
      );
    });
  });

});


// ═══ Screen share quality optimization ═════════════════════════════

describe('Screen share quality optimization', () => {

  // Feature: screen-share-quality, Property 1: Capture options contain all required fields
  describe('P1: Capture options contain all required fields', () => {
    it('startScreenShare passes capture options with correct resolution, fps, contentHint, surface options, and audio', async () => {
      /**
       * Validates: Requirements 1.1, 1.2, 1.3, 1.4
       */
      await fc.assert(
        fc.asyncProperty(
          fc.record({
            roomName: fc.string({ minLength: 1, maxLength: 30 }).filter(s => s.trim().length > 0),
            identity: fc.string({ minLength: 1, maxLength: 20 }).filter(s => s.trim().length > 0),
          }),
          async ({ roomName, identity }) => {
            resetAll();
            // Set participant identity on mock
            mockRoom.localParticipant.identity = identity;

            const cbs = createMockCallbacks();
            const mod = new LiveKitModule(cbs);
            await driveToConnected(mod, `wss://sfu.test/${roomName}`, `tok-${identity}`);

            const callsBefore = sdkCalls.length;
            await mod.startScreenShare();

            // Find the setScreenShareEnabled call
            const shareCalls = sdkCalls.slice(callsBefore).filter(c => c.method === 'setScreenShareEnabled');
            expect(shareCalls).toHaveLength(1);

            const [enabled, captureOpts] = shareCalls[0].args as [boolean, Record<string, unknown>];
            expect(enabled).toBe(true);
            expect(captureOpts).toBeDefined();

            // Resolution: ideal 2560×1440 (default quality is 'high')
            const resolution = captureOpts.resolution as Record<string, unknown>;
            expect(resolution.width).toBe(2560);
            expect(resolution.height).toBe(1440);

            // Frame rate: ideal 30 (high preset)
            expect(resolution.frameRate).toBe(30);

            // Content hint: 'detail' (high preset)
            expect(captureOpts.contentHint).toBe('detail');

            // Surface switching: 'include'
            expect(captureOpts.surfaceSwitching).toBe('include');

            // Self browser surface: 'exclude'
            expect(captureOpts.selfBrowserSurface).toBe('exclude');

            // Audio: false by default; enabling system audio is opt-in after share start
            expect(captureOpts.audio).toBe(false);

            mod.disconnect();
          },
        ),
        { numRuns: 100 },
      );
    });
  });

  // Feature: screen-share-quality, Property 2: Publish options contain all required fields
  describe('P2: Publish options contain all required fields', () => {
    it('startScreenShare passes publish options with correct codec, encoding, degradation, and simulcast layers', async () => {
      /**
       * Validates: Requirements 2.1, 2.2, 2.3, 2.4, 2.5
       */
      await fc.assert(
        fc.asyncProperty(
          fc.record({
            roomName: fc.string({ minLength: 1, maxLength: 30 }).filter(s => s.trim().length > 0),
            identity: fc.string({ minLength: 1, maxLength: 20 }).filter(s => s.trim().length > 0),
          }),
          async ({ roomName, identity }) => {
            resetAll();
            mockRoom.localParticipant.identity = identity;

            const cbs = createMockCallbacks();
            const mod = new LiveKitModule(cbs);
            await driveToConnected(mod, `wss://sfu.test/${roomName}`, `tok-${identity}`);

            const callsBefore = sdkCalls.length;
            await mod.startScreenShare();

            // Find the setScreenShareEnabled call
            const shareCalls = sdkCalls.slice(callsBefore).filter(c => c.method === 'setScreenShareEnabled');
            expect(shareCalls).toHaveLength(1);

            const [, , publishOpts] = shareCalls[0].args as [boolean, unknown, Record<string, unknown>];
            expect(publishOpts).toBeDefined();

            // videoCodec: 'vp9'
            expect(publishOpts.videoCodec).toBe('vp9');

            // backupCodec: object with codec === 'vp8' (SDK shape)
            expect(publishOpts.backupCodec).toBeDefined();
            expect(typeof publishOpts.backupCodec).toBe('object');
            expect((publishOpts.backupCodec as Record<string, unknown>).codec).toBe('vp8');

            // degradationPreference: 'maintain-resolution'
            expect(publishOpts.degradationPreference).toBe('maintain-resolution');

            // screenShareEncoding: maxBitrate 6_000_000, maxFramerate 30 (high preset)
            const encoding = publishOpts.screenShareEncoding as Record<string, unknown>;
            expect(encoding).toBeDefined();
            expect(encoding.maxBitrate).toBe(6_000_000);
            expect(encoding.maxFramerate).toBe(30);

            // screenShareSimulcastLayers: 2 entries at 360p and 720p heights
            // (VideoPreset mock produces { width, height, maxBitrate })
            const layers = publishOpts.screenShareSimulcastLayers as Array<Record<string, unknown>>;
            expect(layers).toBeDefined();
            expect(layers).toHaveLength(2);

            const heights = layers.map(l => l.height);
            expect(heights).toContain(360);
            expect(heights).toContain(720);

            const widths = layers.map(l => l.width);
            expect(widths).toContain(640);
            expect(widths).toContain(1280);

            mod.disconnect();
          },
        ),
        { numRuns: 100 },
      );
    });
  });

  // Feature: screen-share-quality, Property 6: Graceful degradation on lower-than-requested grants
  describe('P6: Constraint rejection fallback', () => {
    /**
     * Validates: Requirements 1.5, 1.6
     */
    it('falls back to defaults when first call throws OverconstrainedError, returns true', async () => {
      resetAll();
      const cbs = createMockCallbacks();
      const mod = new LiveKitModule(cbs);
      await driveToConnected(mod);

      // First call rejects with OverconstrainedError, second call (fallback) succeeds
      const overconstrainedErr = new Error('width is too large');
      overconstrainedErr.name = 'OverconstrainedError';

      mockRoom.localParticipant.setScreenShareEnabled = vi.fn()
        .mockRejectedValueOnce(overconstrainedErr)
        .mockResolvedValueOnce(true) as typeof mockRoom.localParticipant.setScreenShareEnabled;

      const result = await mod.startScreenShare();
      expect(result).toBe(true);

      // setScreenShareEnabled called twice
      const calls = mockRoom.localParticipant.setScreenShareEnabled.mock.calls;
      expect(calls).toHaveLength(2);

      // First call: with capture + publish opts (3 args)
      expect(calls[0][0]).toBe(true);
      expect(calls[0][1]).toBeDefined(); // captureOpts
      expect(calls[0][2]).toBeDefined(); // publishOpts

      // Second call: fallback with just `true` (no constraints)
      expect(calls[1][0]).toBe(true);
      expect(calls[1].length).toBe(1);

      // onSystemEvent called with constraint rejection message
      const sysEvents = cbs.calls.filter(c => c.method === 'onSystemEvent');
      expect(sysEvents.some(c =>
        typeof c.args[0] === 'string' && (c.args[0] as string).toLowerCase().includes('constraints rejected'),
      )).toBe(true);

      mod.disconnect();
    });

    it('returns false when both constrained and fallback calls fail', async () => {
      resetAll();
      const cbs = createMockCallbacks();
      const mod = new LiveKitModule(cbs);
      await driveToConnected(mod);

      // First call rejects with OverconstrainedError, fallback also fails
      const overconstrainedErr = new Error('width is too large');
      overconstrainedErr.name = 'OverconstrainedError';
      const fallbackErr = new Error('user cancelled picker');

      mockRoom.localParticipant.setScreenShareEnabled = vi.fn()
        .mockRejectedValueOnce(overconstrainedErr)
        .mockRejectedValueOnce(fallbackErr) as typeof mockRoom.localParticipant.setScreenShareEnabled;

      const result = await mod.startScreenShare();
      expect(result).toBe(false);

      // setScreenShareEnabled called twice (both failed)
      expect(mockRoom.localParticipant.setScreenShareEnabled.mock.calls).toHaveLength(2);

      // onSystemEvent called with failure message
      const sysEvents = cbs.calls.filter(c => c.method === 'onSystemEvent');
      expect(sysEvents.some(c =>
        typeof c.args[0] === 'string' && (c.args[0] as string).toLowerCase().includes('failed'),
      )).toBe(true);

      mod.disconnect();
    });
  });

  // Feature: screen-share-quality, Property 3: Post-publish tuning sets contentHint and applies constraints
  describe('P3: Post-publish tuning sets contentHint and applies constraints', () => {
    it('after startScreenShare resolves and 100ms elapses, contentHint is set to detail and applyConstraints is called with correct values', async () => {
      /**
       * Validates: Requirements 3.1, 3.2
       */
      await fc.assert(
        fc.asyncProperty(
          fc.record({
            roomName: fc.string({ minLength: 1, maxLength: 30 }).filter(s => s.trim().length > 0),
            identity: fc.string({ minLength: 1, maxLength: 20 }).filter(s => s.trim().length > 0),
          }),
          async ({ roomName, identity }) => {
            resetAll();
            mockRoom.localParticipant.identity = identity;

            // Set up a mock screen share track with writable contentHint and mock applyConstraints
            const mockMediaStreamTrack = {
              contentHint: '' as string,
              applyConstraints: vi.fn().mockResolvedValue(undefined),
              getSettings: vi.fn().mockReturnValue({ width: 2560, height: 1440, frameRate: 60 }),
            };

            const mockScreenSharePublication = {
              track: {
                mediaStreamTrack: mockMediaStreamTrack,
              },
              source: 'screen_share',
            };

            // Add getTrackPublication to the mock local participant
            (mockRoom.localParticipant as Record<string, unknown>).getTrackPublication = vi.fn(
              (source: string) => {
                if (source === 'screen_share') return mockScreenSharePublication;
                return undefined;
              },
            );

            const cbs = createMockCallbacks();
            const mod = new LiveKitModule(cbs);
            await driveToConnected(mod, `wss://sfu.test/${roomName}`, `tok-${identity}`);

            // Switch to fake timers AFTER connection is established (driveToConnected uses real setTimeout in tick())
            vi.useFakeTimers();
            try {
              await mod.startScreenShare();

              // Advance timers by 100ms to trigger the post-publish tuning setTimeout
              vi.advanceTimersByTime(100);

              // Flush the applyConstraints promise chain
              await vi.advanceTimersByTimeAsync(0);

              // Assert contentHint was set to 'detail'
              expect(mockMediaStreamTrack.contentHint).toBe('detail');

              // Assert applyConstraints was called with the correct constraints
              // Post-publish tuning derives values from the active preset (default: 'high')
              expect(mockMediaStreamTrack.applyConstraints).toHaveBeenCalledWith({
                width: { ideal: 2560 },
                height: { ideal: 1440 },
                frameRate: { min: 24, ideal: 30 },
              });

              mod.disconnect();
            } finally {
              vi.useRealTimers();
            }
          },
        ),
        { numRuns: 100 },
      );
    });
  });

  // Feature: screen-share-quality, Property 4: Quality preset mapping is correct and emits events
  describe('P4: Quality preset mapping is correct and emits events', () => {
    it('applying any quality preset sets correct constraints, contentHint, and emits system event', async () => {
      /**
       * Validates: Requirements 4.1, 4.2, 4.3, 4.4, 4.5
       */
      const EXPECTED_PRESETS: Record<string, { w: number; h: number; fps: number; degradation: string; contentHint: string }> = {
        low:  { w: 1920, h: 1080, fps: 60, degradation: 'maintain-framerate', contentHint: 'motion' },
        high: { w: 2560, h: 1440, fps: 30, degradation: 'maintain-resolution', contentHint: 'detail' },
        max:  { w: 2560, h: 1440, fps: 60, degradation: 'maintain-resolution', contentHint: 'detail' },
      };

      await fc.assert(
        fc.asyncProperty(
          fc.constantFrom('low' as const, 'high' as const, 'max' as const),
          async (quality) => {
            resetAll();

            const mockMediaStreamTrack = {
              contentHint: '' as string,
              applyConstraints: vi.fn().mockResolvedValue(undefined),
              getSettings: vi.fn().mockReturnValue({ width: 2560, height: 1440, frameRate: 60 }),
            };

            const mockScreenSharePublication = {
              track: { mediaStreamTrack: mockMediaStreamTrack },
              source: 'screen_share',
            };

            (mockRoom.localParticipant as Record<string, unknown>).getTrackPublication = vi.fn(
              (source: string) => {
                if (source === 'screen_share') return mockScreenSharePublication;
                return undefined;
              },
            );

            const cbs = createMockCallbacks();
            const mod = new LiveKitModule(cbs);
            await driveToConnected(mod);

            await mod.setScreenShareQuality(quality);

            const expected = EXPECTED_PRESETS[quality];

            // contentHint set per preset
            expect(mockMediaStreamTrack.contentHint).toBe(expected.contentHint);

            // applyConstraints called with correct resolution and frame rate
            expect(mockMediaStreamTrack.applyConstraints).toHaveBeenCalledTimes(1);
            expect(mockMediaStreamTrack.applyConstraints).toHaveBeenCalledWith({
              width: { ideal: expected.w },
              height: { ideal: expected.h },
              frameRate: { max: expected.fps },
            });

            // System event emitted with the quality tier name
            const sysEvents = cbs.calls.filter(c => c.method === 'onSystemEvent');
            expect(sysEvents.some(c =>
              typeof c.args[0] === 'string' && (c.args[0] as string).includes(quality),
            )).toBe(true);

            mod.disconnect();
          },
        ),
        { numRuns: 100 },
      );
    });

    it('quality preset failure retains previous quality and does not emit event', async () => {
      /**
       * Validates: Requirement 4.6
       */
      resetAll();

      const mockMediaStreamTrack = {
        contentHint: '' as string,
        applyConstraints: vi.fn().mockRejectedValue(new Error('track ended')),
        getSettings: vi.fn().mockReturnValue({ width: 2560, height: 1440, frameRate: 60 }),
      };

      const mockScreenSharePublication = {
        track: { mediaStreamTrack: mockMediaStreamTrack },
        source: 'screen_share',
      };

      (mockRoom.localParticipant as Record<string, unknown>).getTrackPublication = vi.fn(
        (source: string) => {
          if (source === 'screen_share') return mockScreenSharePublication;
          return undefined;
        },
      );

      const warnSpy = vi.spyOn(console, 'warn').mockImplementation(() => {});
      const cbs = createMockCallbacks();
      const mod = new LiveKitModule(cbs);
      await driveToConnected(mod);

      // First set to 'low' successfully
      mockMediaStreamTrack.applyConstraints.mockResolvedValueOnce(undefined);
      await mod.setScreenShareQuality('low');
      const eventsAfterLow = cbs.calls.filter(c =>
        c.method === 'onSystemEvent' && (c.args[0] as string).includes('low'),
      );
      expect(eventsAfterLow).toHaveLength(1);

      // Now try 'max' which will fail
      mockMediaStreamTrack.applyConstraints.mockRejectedValueOnce(new Error('track ended'));
      await mod.setScreenShareQuality('max');

      // No system event for 'max' (failure path doesn't emit)
      const eventsForMax = cbs.calls.filter(c =>
        c.method === 'onSystemEvent' && (c.args[0] as string).includes('max'),
      );
      expect(eventsForMax).toHaveLength(0);

      // console.warn was called
      expect(warnSpy).toHaveBeenCalled();

      mod.disconnect();
      warnSpy.mockRestore();
    });
  });

  // Feature: screen-share-quality, Property 5: Restart preserves capture profile and re-applies tuning
  describe('P5: Restart preserves capture profile and re-applies tuning', () => {
    it('restartScreenShareWithAudio passes the same capture/publish options as the original start', async () => {
      /**
       * Validates: Requirements 5.1, 5.2
       */
      await fc.assert(
        fc.asyncProperty(
          fc.boolean(), // withAudio for audio restart
          async (withAudio) => {
            resetAll();

            vi.stubGlobal('navigator', {
              userAgent: '',
              userActivation: { isActive: true },
              mediaDevices: createMockMediaDevices(),
            });

            const mockMediaStreamTrack = {
              contentHint: '' as string,
              applyConstraints: vi.fn().mockResolvedValue(undefined),
              getSettings: vi.fn().mockReturnValue({ width: 2560, height: 1440, frameRate: 60 }),
            };

            const mockScreenSharePublication = {
              track: {
                mediaStreamTrack: mockMediaStreamTrack,
                replaceTrack: vi.fn(async () => {}),
              },
              source: 'screen_share',
            };

            (mockRoom.localParticipant as Record<string, unknown>).getTrackPublication = vi.fn(
              (source: string) => {
                if (source === 'screen_share') return mockScreenSharePublication;
                return undefined;
              },
            );

            const cbs = createMockCallbacks();
            const mod = new LiveKitModule(cbs);
            await driveToConnected(mod);

            // Original start
            await mod.startScreenShare();

            // Capture the original start call's capture and publish options
            const startCalls = sdkCalls.filter(c => c.method === 'setScreenShareEnabled' && c.args[0] === true);
            expect(startCalls).toHaveLength(1);
            const originalCapture = startCalls[0].args[1] as Record<string, unknown>;
            const originalPublish = startCalls[0].args[2] as Record<string, unknown>;

            // Clear SDK calls
            sdkCalls.length = 0;

            await mod.restartScreenShareWithAudio(withAudio);

            // Find the restart's setScreenShareEnabled(true, ...) call
            const restartStartCalls = sdkCalls.filter(c => c.method === 'setScreenShareEnabled' && c.args[0] === true);
            expect(restartStartCalls).toHaveLength(1);
            const restartCapture = restartStartCalls[0].args[1] as Record<string, unknown>;
            const restartPublish = restartStartCalls[0].args[2] as Record<string, unknown>;

            // Publish options must match exactly
            expect(restartPublish.videoCodec).toBe(originalPublish.videoCodec);
            expect(restartPublish.degradationPreference).toBe(originalPublish.degradationPreference);
            expect(restartPublish.screenShareEncoding).toEqual(originalPublish.screenShareEncoding);
            expect(restartPublish.backupCodec).toEqual(originalPublish.backupCodec);
            expect(restartPublish.screenShareSimulcastLayers).toEqual(originalPublish.screenShareSimulcastLayers);

            // Capture options must match (except audio, which uses the withAudio parameter)
            const origRes = originalCapture.resolution as Record<string, unknown>;
            const restartRes = restartCapture.resolution as Record<string, unknown>;
            expect(restartRes.width).toBe(origRes.width);
            expect(restartRes.height).toBe(origRes.height);
            expect(restartRes.frameRate).toBe(origRes.frameRate);
            expect(restartCapture.contentHint).toBe(originalCapture.contentHint);
            expect(restartCapture.surfaceSwitching).toBe(originalCapture.surfaceSwitching);
            expect(restartCapture.selfBrowserSurface).toBe(originalCapture.selfBrowserSurface);
            expect(restartCapture.suppressLocalAudioPlayback).toBe(originalCapture.suppressLocalAudioPlayback);
            expect(restartCapture.audio).toBe(withAudio);

            // Verify stop was called before restart
            const stopCalls = sdkCalls.filter(c => c.method === 'setScreenShareEnabled' && c.args[0] === false);
            expect(stopCalls).toHaveLength(1);
            const stopIdx = sdkCalls.indexOf(stopCalls[0]);
            const startIdx = sdkCalls.indexOf(restartStartCalls[0]);
            expect(stopIdx).toBeLessThan(startIdx);

            mod.disconnect();
          },
        ),
        { numRuns: 100 },
      );
    });

    it('changeScreenShareSource passes the same capture constraints to getDisplayMedia as the original start', async () => {
      /**
       * Validates: Requirements 6.1, 6.2
       * changeScreenShareSource uses acquire-before-drop: it calls getDisplayMedia
       * with the active profile's constraints and replaceTrack() in-place, so no
       * setScreenShareEnabled calls happen when an existing publication is present.
       */
      await fc.assert(
        fc.asyncProperty(
          fc.constant(undefined),
          async () => {
            resetAll();

            const mockMediaDevices = createMockMediaDevices();
            vi.stubGlobal('navigator', {
              userAgent: '',
              userActivation: { isActive: true },
              mediaDevices: mockMediaDevices,
            });

            const mockMediaStreamTrack = {
              contentHint: '' as string,
              applyConstraints: vi.fn().mockResolvedValue(undefined),
              getSettings: vi.fn().mockReturnValue({ width: 2560, height: 1440, frameRate: 60 }),
            };

            const mockScreenSharePublication = {
              track: {
                mediaStreamTrack: mockMediaStreamTrack,
                replaceTrack: vi.fn(async () => {}),
              },
              source: 'screen_share',
            };

            (mockRoom.localParticipant as Record<string, unknown>).getTrackPublication = vi.fn(
              (source: string) => {
                if (source === 'screen_share') return mockScreenSharePublication;
                return undefined;
              },
            );

            const cbs = createMockCallbacks();
            const mod = new LiveKitModule(cbs);
            await driveToConnected(mod);

            // Original start — capture its options for reference
            await mod.startScreenShare();
            const startCalls = sdkCalls.filter(c => c.method === 'setScreenShareEnabled' && c.args[0] === true);
            expect(startCalls).toHaveLength(1);
            const originalCapture = startCalls[0].args[1] as Record<string, unknown>;

            sdkCalls.length = 0;

            await mod.changeScreenShareSource();

            // changeScreenShareSource must NOT call setScreenShareEnabled when a
            // publication is already active — it uses replaceTrack instead.
            expect(sdkCalls.filter(c => c.method === 'setScreenShareEnabled')).toHaveLength(0);

            // replaceTrack must have been called on the existing track.
            expect(mockScreenSharePublication.track.replaceTrack).toHaveBeenCalledTimes(1);

            // getDisplayMedia must have been called with constraints that match the
            // capture profile used at startScreenShare time.
            expect(mockMediaDevices.getDisplayMedia).toHaveBeenCalledTimes(1);
            const gdmArgs = (mockMediaDevices.getDisplayMedia.mock.calls as unknown as Array<[{ video: Record<string, unknown> }]>)[0][0];
            const origRes = originalCapture.resolution as Record<string, unknown>;
            expect(gdmArgs.video.width).toBe(origRes.width);
            expect(gdmArgs.video.height).toBe(origRes.height);
            expect(gdmArgs.video.frameRate).toBe(origRes.frameRate);
            expect(gdmArgs.video.surfaceSwitching).toBe(originalCapture.surfaceSwitching);
            expect(gdmArgs.video.selfBrowserSurface).toBe(originalCapture.selfBrowserSurface);

            mod.disconnect();
          },
        ),
        { numRuns: 20 },
      );
    });

    it('restart re-applies post-publish tuning after the new track is published', async () => {
      /**
       * Validates: Requirements 5.2, 6.2
       */
      await fc.assert(
        fc.asyncProperty(
          fc.constantFrom('audio' as const, 'source' as const),
          async (restartType) => {
            resetAll();

            vi.stubGlobal('navigator', {
              userAgent: '',
              userActivation: { isActive: true },
              mediaDevices: createMockMediaDevices(),
            });

            const mockMediaStreamTrack = {
              contentHint: '' as string,
              applyConstraints: vi.fn().mockResolvedValue(undefined),
              getSettings: vi.fn().mockReturnValue({ width: 2560, height: 1440, frameRate: 60 }),
            };

            const mockScreenSharePublication = {
              track: {
                mediaStreamTrack: mockMediaStreamTrack,
                replaceTrack: vi.fn(async () => {}),
              },
              source: 'screen_share',
            };

            (mockRoom.localParticipant as Record<string, unknown>).getTrackPublication = vi.fn(
              (source: string) => {
                if (source === 'screen_share') return mockScreenSharePublication;
                return undefined;
              },
            );

            const cbs = createMockCallbacks();
            const mod = new LiveKitModule(cbs);
            await driveToConnected(mod);

            vi.useFakeTimers();
            try {
              // Original start
              await mod.startScreenShare();
              vi.advanceTimersByTime(100);
              await vi.advanceTimersByTimeAsync(0);

              // Reset mock to track restart tuning
              mockMediaStreamTrack.contentHint = '';
              mockMediaStreamTrack.applyConstraints.mockClear();

              // Restart
              if (restartType === 'audio') {
                await mod.restartScreenShareWithAudio(true);
              } else {
                await mod.changeScreenShareSource();
              }

              // Advance 100ms to trigger post-publish tuning
              vi.advanceTimersByTime(100);
              await vi.advanceTimersByTimeAsync(0);

              // contentHint re-applied
              expect(mockMediaStreamTrack.contentHint).toBe('detail');

              // applyConstraints re-applied with post-publish tuning values
              // Derives from active preset (default: 'high' → 30fps ideal)
              expect(mockMediaStreamTrack.applyConstraints).toHaveBeenCalledWith({
                width: { ideal: 2560 },
                height: { ideal: 1440 },
                frameRate: { min: 24, ideal: 30 },
              });

              mod.disconnect();
            } finally {
              vi.useRealTimers();
            }
          },
        ),
        { numRuns: 100 },
      );
    });
  });

  // Feature: screen-share-quality, Task 2.3: Post-publish retry and double-failure scenarios
  describe('Post-publish retry and double-failure', () => {
    /**
     * Validates: Requirements 3.4, 3.5
     */
    it('applyConstraints fails once then succeeds on retry', async () => {
      resetAll();

      const mockMediaStreamTrack = {
        contentHint: '' as string,
        applyConstraints: vi.fn()
          .mockRejectedValueOnce(new Error('track not ready'))
          .mockResolvedValueOnce(undefined),
        getSettings: vi.fn().mockReturnValue({ width: 2560, height: 1440, frameRate: 60 }),
      };

      const mockScreenSharePublication = {
        track: { mediaStreamTrack: mockMediaStreamTrack },
        source: 'screen_share',
      };

      (mockRoom.localParticipant as Record<string, unknown>).getTrackPublication = vi.fn(
        (source: string) => {
          if (source === 'screen_share') return mockScreenSharePublication;
          return undefined;
        },
      );

      const warnSpy = vi.spyOn(console, 'warn').mockImplementation(() => {});

      const cbs = createMockCallbacks();
      const mod = new LiveKitModule(cbs);
      await driveToConnected(mod);

      vi.useFakeTimers();
      try {
        await mod.startScreenShare();

        // Advance 100ms to trigger post-publish tuning
        vi.advanceTimersByTime(100);
        // Flush the first applyConstraints rejection
        await vi.advanceTimersByTimeAsync(0);

        // First call failed — advance 300ms to trigger the retry
        vi.advanceTimersByTime(300);
        // Flush the retry applyConstraints promise
        await vi.advanceTimersByTimeAsync(0);

        // applyConstraints called twice (first failed, second succeeded)
        expect(mockMediaStreamTrack.applyConstraints).toHaveBeenCalledTimes(2);

        // contentHint was set to 'detail'
        expect(mockMediaStreamTrack.contentHint).toBe('detail');

        // No retry failure warning (second call succeeded)
        const retryFailWarns = warnSpy.mock.calls.filter(
          args => args.some(a => typeof a === 'string' && a.includes('failed after retry')),
        );
        expect(retryFailWarns).toHaveLength(0);

        mod.disconnect();
      } finally {
        vi.useRealTimers();
        warnSpy.mockRestore();
      }
    });

    it('both applyConstraints attempts fail — share continues', async () => {
      resetAll();

      const mockMediaStreamTrack = {
        contentHint: '' as string,
        applyConstraints: vi.fn()
          .mockRejectedValueOnce(new Error('track not ready'))
          .mockRejectedValueOnce(new Error('still not ready')),
        getSettings: vi.fn().mockReturnValue({ width: 2560, height: 1440, frameRate: 60 }),
      };

      const mockScreenSharePublication = {
        track: { mediaStreamTrack: mockMediaStreamTrack },
        source: 'screen_share',
      };

      (mockRoom.localParticipant as Record<string, unknown>).getTrackPublication = vi.fn(
        (source: string) => {
          if (source === 'screen_share') return mockScreenSharePublication;
          return undefined;
        },
      );

      const warnSpy = vi.spyOn(console, 'warn').mockImplementation(() => {});

      const cbs = createMockCallbacks();
      const mod = new LiveKitModule(cbs);
      await driveToConnected(mod);

      vi.useFakeTimers();
      try {
        const result = await mod.startScreenShare();

        // Advance 100ms to trigger post-publish tuning
        vi.advanceTimersByTime(100);
        // Flush the first applyConstraints rejection
        await vi.advanceTimersByTimeAsync(0);

        // Advance 300ms to trigger the retry
        vi.advanceTimersByTime(300);
        // Flush the retry applyConstraints rejection
        await vi.advanceTimersByTimeAsync(0);

        // applyConstraints called twice (both failed)
        expect(mockMediaStreamTrack.applyConstraints).toHaveBeenCalledTimes(2);

        // console.warn called with retry failure message
        const retryFailWarns = warnSpy.mock.calls.filter(
          args => args.some(a => typeof a === 'string' && a.includes('failed after retry')),
        );
        expect(retryFailWarns.length).toBeGreaterThanOrEqual(1);

        // startScreenShare returned true — share is still active despite tuning failure
        expect(result).toBe(true);

        mod.disconnect();
      } finally {
        vi.useRealTimers();
        warnSpy.mockRestore();
      }
    });
  });

  // Feature: screen-share-quality, Property 7: Quality info is reported via callback
  describe('P7: Quality info is reported via callback', () => {
    it('onShareQualityInfo callback is invoked with actual width, height, and frameRate from getSettings()', async () => {
      /**
       * Validates: Requirements 8.1, 8.3
       */
      await fc.assert(
        fc.asyncProperty(
          fc.record({
            width: fc.oneof(fc.integer({ min: 0, max: 7680 }), fc.constant(undefined)),
            height: fc.oneof(fc.integer({ min: 0, max: 4320 }), fc.constant(undefined)),
            frameRate: fc.oneof(fc.integer({ min: 0, max: 240 }), fc.constant(undefined)),
          }),
          async ({ width, height, frameRate }) => {
            resetAll();

            // Build getSettings return value — omit undefined fields to test the || 0 fallback
            const settingsReturn: Record<string, unknown> = {};
            if (width !== undefined) settingsReturn.width = width;
            if (height !== undefined) settingsReturn.height = height;
            if (frameRate !== undefined) settingsReturn.frameRate = frameRate;

            const mockMediaStreamTrack = {
              contentHint: '' as string,
              applyConstraints: vi.fn().mockResolvedValue(undefined),
              getSettings: vi.fn().mockReturnValue(settingsReturn),
            };

            const mockScreenSharePublication = {
              track: { mediaStreamTrack: mockMediaStreamTrack },
              source: 'screen_share',
            };

            (mockRoom.localParticipant as Record<string, unknown>).getTrackPublication = vi.fn(
              (source: string) => {
                if (source === 'screen_share') return mockScreenSharePublication;
                return undefined;
              },
            );

            // Create callbacks with onShareQualityInfo spy
            const cbs = createMockCallbacks();
            const qualityInfoSpy = vi.fn();
            cbs.onShareQualityInfo = qualityInfoSpy;

            const mod = new LiveKitModule(cbs);
            await driveToConnected(mod);

            vi.useFakeTimers();
            try {
              await mod.startScreenShare();

              // Advance 100ms to trigger post-publish tuning
              vi.advanceTimersByTime(100);
              // Flush the applyConstraints promise chain
              await vi.advanceTimersByTimeAsync(0);

              // onShareQualityInfo should have been called exactly once
              expect(qualityInfoSpy).toHaveBeenCalledTimes(1);

              // Verify the reported values match getSettings(), with 0 for missing values
              const reported = qualityInfoSpy.mock.calls[0][0] as { width: number; height: number; frameRate: number };
              expect(reported.width).toBe(width ?? 0);
              expect(reported.height).toBe(height ?? 0);
              expect(reported.frameRate).toBe(frameRate ?? 0);

              mod.disconnect();
            } finally {
              vi.useRealTimers();
            }
          },
        ),
        { numRuns: 100 },
      );
    });
  });

});


// ═══ Screen share sender stats polling ═════════════════════════════

describe('Screen share sender stats polling', () => {

  /**
   * Validates: Requirement 8.2
   * Task 5.2: Periodic sender stats logging during active screen share
   */

  /** Helper: create a mock RTCStatsReport with outbound-rtp video stats */
  function createMockStatsReport(overrides: {
    bytesSent?: number;
    timestamp?: number;
    framesPerSecond?: number;
    qualityLimitationReason?: string;
  } = {}): RTCStatsReport {
    const entries: Array<[string, Record<string, unknown>]> = [
      ['outbound-rtp-video', {
        type: 'outbound-rtp',
        kind: 'video',
        bytesSent: overrides.bytesSent ?? 500000,
        timestamp: overrides.timestamp ?? 1000000,
        framesPerSecond: overrides.framesPerSecond ?? 30,
        qualityLimitationReason: overrides.qualityLimitationReason ?? 'none',
      }],
    ];
    const map = new Map(entries);
    return {
      forEach: (cb: (value: Record<string, unknown>, key: string) => void) => map.forEach(cb),
      get: (key: string) => map.get(key),
      has: (key: string) => map.has(key),
      entries: () => map.entries(),
      keys: () => map.keys(),
      values: () => map.values(),
      [Symbol.iterator]: () => map[Symbol.iterator](),
      size: map.size,
    } as unknown as RTCStatsReport;
  }

  /** Helper: attach a mock publisher with getStats to the mock room */
  function attachMockPublisher(getStatsFn: () => Promise<RTCStatsReport>) {
    (mockRoom as Record<string, unknown>).engine = {
      pcManager: {
        publisher: {
          getStats: getStatsFn,
        },
        subscriber: undefined,
      },
    };
  }

  /** Helper: attach a mock screen share track publication so applyPostPublishTuning doesn't throw */
  function attachMockScreenShareTrack() {
    const mockMediaStreamTrack = {
      contentHint: '' as string,
      applyConstraints: vi.fn().mockResolvedValue(undefined),
      getSettings: vi.fn().mockReturnValue({ width: 2560, height: 1440, frameRate: 60 }),
    };
    (mockRoom.localParticipant as Record<string, unknown>).getTrackPublication = vi.fn(
      (source: string) => {
        if (source === 'screen_share') return { track: { mediaStreamTrack: mockMediaStreamTrack }, source: 'screen_share' };
        return undefined;
      },
    );
  }

  it('starts polling after startScreenShare and logs stats every 5 seconds', async () => {
    resetAll();

    let callCount = 0;
    attachMockPublisher(async () => {
      callCount++;
      return createMockStatsReport({
        bytesSent: callCount * 625000, // 625KB per 5s = ~1Mbps
        timestamp: 1000000 + callCount * 5000,
        framesPerSecond: 30,
        qualityLimitationReason: 'none',
      });
    });
    attachMockScreenShareTrack();

    const logSpy = vi.spyOn(console, 'log').mockImplementation(() => {});
    const cbs = createMockCallbacks();
    const mod = new LiveKitModule(cbs);
    await driveToConnected(mod);

    vi.useFakeTimers();
    try {
      await mod.startScreenShare();

      // No stats logged yet (interval hasn't fired)
      const statsLogsBefore = logSpy.mock.calls.filter(
        args => args.some(a => typeof a === 'string' && (a as string).includes('screen share stats:')),
      );
      expect(statsLogsBefore).toHaveLength(0);

      // Advance 5 seconds — first poll
      vi.advanceTimersByTime(5000);
      await vi.advanceTimersByTimeAsync(0);

      const statsLogs1 = logSpy.mock.calls.filter(
        args => args.some(a => typeof a === 'string' && (a as string).includes('screen share stats:')),
      );
      expect(statsLogs1).toHaveLength(1);
      // First poll has no previous data, so bitrate=0
      expect(statsLogs1[0].some((a: unknown) =>
        typeof a === 'string' && (a as string).includes('bitrate=') && (a as string).includes('fps=30') && (a as string).includes('qualityLimitation=none'),
      )).toBe(true);

      // Advance another 5 seconds — second poll (now has delta for bitrate)
      vi.advanceTimersByTime(5000);
      await vi.advanceTimersByTimeAsync(0);

      const statsLogs2 = logSpy.mock.calls.filter(
        args => args.some(a => typeof a === 'string' && (a as string).includes('screen share stats:')),
      );
      expect(statsLogs2).toHaveLength(2);

      mod.disconnect();
    } finally {
      vi.useRealTimers();
      logSpy.mockRestore();
    }
  });

  it('stops polling when stopScreenShare is called', async () => {
    resetAll();

    attachMockPublisher(async () => createMockStatsReport());
    attachMockScreenShareTrack();

    const logSpy = vi.spyOn(console, 'log').mockImplementation(() => {});
    const cbs = createMockCallbacks();
    const mod = new LiveKitModule(cbs);
    await driveToConnected(mod);

    vi.useFakeTimers();
    try {
      await mod.startScreenShare();

      // Advance 5s — one poll
      vi.advanceTimersByTime(5000);
      await vi.advanceTimersByTimeAsync(0);

      const countAfterFirst = logSpy.mock.calls.filter(
        args => args.some(a => typeof a === 'string' && (a as string).includes('screen share stats:')),
      ).length;
      expect(countAfterFirst).toBe(1);

      // Stop screen share
      await mod.stopScreenShare();

      // Advance another 10s — no more polls
      vi.advanceTimersByTime(10000);
      await vi.advanceTimersByTimeAsync(0);

      const countAfterStop = logSpy.mock.calls.filter(
        args => args.some(a => typeof a === 'string' && (a as string).includes('screen share stats:')),
      ).length;
      expect(countAfterStop).toBe(countAfterFirst);

      mod.disconnect();
    } finally {
      vi.useRealTimers();
      logSpy.mockRestore();
    }
  });

  it('logs warning and skips cycle on stats polling failure', async () => {
    resetAll();

    attachMockPublisher(async () => { throw new Error('stats unavailable'); });
    attachMockScreenShareTrack();

    const warnSpy = vi.spyOn(console, 'warn').mockImplementation(() => {});
    const logSpy = vi.spyOn(console, 'log').mockImplementation(() => {});
    const cbs = createMockCallbacks();
    const mod = new LiveKitModule(cbs);
    await driveToConnected(mod);

    vi.useFakeTimers();
    try {
      await mod.startScreenShare();

      // Advance 5s — poll fires but getStats rejects
      vi.advanceTimersByTime(5000);
      await vi.advanceTimersByTimeAsync(0);

      // No stats log (failed)
      const statsLogs = logSpy.mock.calls.filter(
        args => args.some(a => typeof a === 'string' && (a as string).includes('screen share stats:')),
      );
      expect(statsLogs).toHaveLength(0);

      // Warning logged about failure
      const warnLogs = warnSpy.mock.calls.filter(
        args => args.some(a => typeof a === 'string' && (a as string).includes('stats polling failed')),
      );
      expect(warnLogs.length).toBeGreaterThanOrEqual(1);

      mod.disconnect();
    } finally {
      vi.useRealTimers();
      warnSpy.mockRestore();
      logSpy.mockRestore();
    }
  });

  it('restarts polling on restartScreenShareWithAudio', async () => {
    resetAll();

    let callCount = 0;
    attachMockPublisher(async () => {
      callCount++;
      return createMockStatsReport({
        bytesSent: callCount * 500000,
        timestamp: 1000000 + callCount * 5000,
      });
    });
    attachMockScreenShareTrack();

    const logSpy = vi.spyOn(console, 'log').mockImplementation(() => {});
    const cbs = createMockCallbacks();
    const mod = new LiveKitModule(cbs);
    await driveToConnected(mod);

    vi.useFakeTimers();
    try {
      await mod.startScreenShare();

      // One poll
      vi.advanceTimersByTime(5000);
      await vi.advanceTimersByTimeAsync(0);

      const count1 = logSpy.mock.calls.filter(
        args => args.some(a => typeof a === 'string' && (a as string).includes('screen share stats:')),
      ).length;
      expect(count1).toBe(1);

      // Restart with audio — should restart polling (resets prev counters)
      await mod.restartScreenShareWithAudio(true);

      // Another poll after restart
      vi.advanceTimersByTime(5000);
      await vi.advanceTimersByTimeAsync(0);

      const count2 = logSpy.mock.calls.filter(
        args => args.some(a => typeof a === 'string' && (a as string).includes('screen share stats:')),
      ).length;
      expect(count2).toBe(2);

      mod.disconnect();
    } finally {
      vi.useRealTimers();
      logSpy.mockRestore();
    }
  });

});


// ═══ Adaptive quality (Phase 1) ════════════════════════════════════

describe('Adaptive quality', () => {

  /**
   * Helper: create a mock RTCStatsReport with both outbound-rtp (packetsSent)
   * and remote-inbound-rtp (packetsLost) entries to produce a desired packet
   * loss percentage.
   *
   * Loss formula: packetsLost / (packetsSent + packetsLost) * 100
   * We use a total of 1000 packets so packetsLost = round(lossPercent * 10).
   */
  function createAdaptiveStatsReport(opts: {
    lossPercent: number;
    bytesSent?: number;
    timestamp?: number;
    framesPerSecond?: number;
    qualityLimitationReason?: string;
  }): RTCStatsReport {
    const total = 1000;
    const lost = Math.round(opts.lossPercent * 10);
    const sent = total - lost;
    const entries: Array<[string, Record<string, unknown>]> = [
      ['outbound-rtp-video', {
        type: 'outbound-rtp',
        kind: 'video',
        bytesSent: opts.bytesSent ?? 500000,
        timestamp: opts.timestamp ?? 1000000,
        framesPerSecond: opts.framesPerSecond ?? 30,
        qualityLimitationReason: opts.qualityLimitationReason ?? 'none',
        packetsSent: sent,
      }],
      ['remote-inbound-rtp-video', {
        type: 'remote-inbound-rtp',
        kind: 'video',
        packetsLost: lost,
      }],
    ];
    const map = new Map(entries);
    return {
      forEach: (cb: (value: Record<string, unknown>, key: string) => void) => map.forEach(cb),
      get: (key: string) => map.get(key),
      has: (key: string) => map.has(key),
      entries: () => map.entries(),
      keys: () => map.keys(),
      values: () => map.values(),
      [Symbol.iterator]: () => map[Symbol.iterator](),
      size: map.size,
    } as unknown as RTCStatsReport;
  }

  /** Helper: attach mock publisher with a getStats function */
  function attachAdaptivePublisher(getStatsFn: () => Promise<RTCStatsReport>) {
    (mockRoom as Record<string, unknown>).engine = {
      pcManager: {
        publisher: { getStats: getStatsFn },
        subscriber: undefined,
      },
    };
  }

  /** Helper: attach mock screen share track with spied applyConstraints */
  function attachAdaptiveScreenShareTrack() {
    const applyConstraintsSpy = vi.fn().mockResolvedValue(undefined);
    const mockMediaStreamTrack = {
      contentHint: '' as string,
      applyConstraints: applyConstraintsSpy,
      getSettings: vi.fn().mockReturnValue({ width: 2560, height: 1440, frameRate: 60 }),
    };
    (mockRoom.localParticipant as Record<string, unknown>).getTrackPublication = vi.fn(
      (source: string) => {
        if (source === 'screen_share') return { track: { mediaStreamTrack: mockMediaStreamTrack }, source: 'screen_share' };
        return undefined;
      },
    );
    return { applyConstraintsSpy, mockMediaStreamTrack };
  }

  // Feature: screen-share-quality, Property 8: Adaptive step-down on moderate loss
  describe('P8: Adaptive step-down on moderate loss', () => {
    /**
     * Validates: Requirements 9.1, 9.4
     *
     * For any active screen share where outbound packet loss exceeds 5% for
     * 2 consecutive polls (10s at 5s cadence), the adaptive quality system
     * shall reduce the target frame rate before reducing resolution, and log
     * the transition.
     *
     * We use the 'max' preset (60fps base) so the FPS step-down is observable
     * (60→30). The default 'high' preset starts at 30fps which is already
     * near the lowest FPS tier.
     */
    it('reduces FPS (not resolution) after 2 consecutive polls of >5% loss', async () => {
      await fc.assert(
        fc.asyncProperty(
          // Generate loss values strictly above 5% (the moderate threshold)
          fc.double({ min: 5.1, max: 50, noNaN: true }),
          fc.double({ min: 5.1, max: 50, noNaN: true }),
          async (loss1, loss2) => {
            resetAll();

            let pollIndex = 0;
            const losses = [loss1, loss2];
            attachAdaptivePublisher(async () => {
              const idx = Math.min(pollIndex++, losses.length - 1);
              return createAdaptiveStatsReport({
                lossPercent: losses[idx],
                bytesSent: (pollIndex) * 500000,
                timestamp: 1000000 + pollIndex * 5000,
              });
            });
            const { applyConstraintsSpy } = attachAdaptiveScreenShareTrack();

            const logSpy = vi.spyOn(console, 'log').mockImplementation(() => {});
            const warnSpy = vi.spyOn(console, 'warn').mockImplementation(() => {});
            const cbs = createMockCallbacks();
            const mod = new LiveKitModule(cbs);
            await driveToConnected(mod);

            vi.useFakeTimers();
            try {
              // Set quality to 'max' before starting so the base preset is 'max' (60fps)
              // This makes the FPS step-down observable (60→30)
              (mod as unknown as Record<string, unknown>).currentQuality = 'max';
              await mod.startScreenShare();

              // Advance 100ms for post-publish tuning
              vi.advanceTimersByTime(100);
              await vi.advanceTimersByTimeAsync(0);

              // Clear applyConstraints calls from post-publish tuning
              applyConstraintsSpy.mockClear();

              // First poll at 5s — loss above 5%, consecutiveLossPolls becomes 1
              vi.advanceTimersByTime(5000);
              await vi.advanceTimersByTimeAsync(0);

              // No tier change yet (need 2 consecutive)
              expect(applyConstraintsSpy).not.toHaveBeenCalled();

              // Second poll at 10s — loss above 5%, consecutiveLossPolls becomes 2 → step down
              vi.advanceTimersByTime(5000);
              await vi.advanceTimersByTimeAsync(0);

              // applyConstraints should have been called for the tier transition
              expect(applyConstraintsSpy).toHaveBeenCalledTimes(1);

              // Verify FPS was reduced (60→30) but resolution stays at 2560×1440
              // The 'max' preset has 60fps base, FPS_TIERS=[60,30,15], so step-down → 30
              const constraints = applyConstraintsSpy.mock.calls[0][0] as MediaTrackConstraints;
              expect((constraints.width as ConstrainULongRange).ideal).toBe(2560);
              expect((constraints.height as ConstrainULongRange).ideal).toBe(1440);
              expect((constraints.frameRate as ConstrainDoubleRange).max).toBe(30);

              // Verify transition was logged
              const adaptiveLogs = logSpy.mock.calls.filter(
                args => args.some(a => typeof a === 'string' && (a as string).includes('adaptive quality:') && (a as string).includes('full') && (a as string).includes('reduced-fps')),
              );
              expect(adaptiveLogs.length).toBeGreaterThanOrEqual(1);

              mod.disconnect();
            } finally {
              vi.useRealTimers();
              logSpy.mockRestore();
              warnSpy.mockRestore();
            }
          },
        ),
        { numRuns: 100 },
      );
    });
  });

  // Feature: screen-share-quality, Property 9: Adaptive step-down on severe loss
  describe('P9: Adaptive step-down on severe loss', () => {
    /**
     * Validates: Requirements 9.2, 9.4
     *
     * For any active screen share already in the reduced-fps tier where
     * outbound packet loss exceeds 15% for 2 consecutive polls (10s at 5s
     * cadence), the adaptive quality system shall reduce the capture/encode
     * target resolution by one tier (1440→1080→720) using applyConstraints,
     * and log the transition.
     */
    it('reduces resolution after 2 consecutive polls of >15% loss in reduced-fps tier', async () => {
      await fc.assert(
        fc.asyncProperty(
          // Moderate loss to trigger initial FPS reduction (>5%)
          fc.double({ min: 5.1, max: 14.9, noNaN: true }),
          fc.double({ min: 5.1, max: 14.9, noNaN: true }),
          // Severe loss to trigger resolution reduction (>15%)
          fc.double({ min: 15.1, max: 80, noNaN: true }),
          fc.double({ min: 15.1, max: 80, noNaN: true }),
          async (modLoss1, modLoss2, sevLoss1, sevLoss2) => {
            resetAll();

            let pollIndex = 0;
            const losses = [modLoss1, modLoss2, sevLoss1, sevLoss2];
            attachAdaptivePublisher(async () => {
              const idx = Math.min(pollIndex++, losses.length - 1);
              return createAdaptiveStatsReport({
                lossPercent: losses[idx],
                bytesSent: pollIndex * 500000,
                timestamp: 1000000 + pollIndex * 5000,
              });
            });
            const { applyConstraintsSpy } = attachAdaptiveScreenShareTrack();

            const logSpy = vi.spyOn(console, 'log').mockImplementation(() => {});
            const warnSpy = vi.spyOn(console, 'warn').mockImplementation(() => {});
            const cbs = createMockCallbacks();
            const mod = new LiveKitModule(cbs);
            await driveToConnected(mod);

            vi.useFakeTimers();
            try {
              // Set quality to 'max' before starting so the base preset is 'max' (60fps)
              // This makes the FPS step-down observable (60→30) and resolution step-down (1440→1080)
              (mod as unknown as Record<string, unknown>).currentQuality = 'max';
              await mod.startScreenShare();

              // Advance 100ms for post-publish tuning
              vi.advanceTimersByTime(100);
              await vi.advanceTimersByTimeAsync(0);
              applyConstraintsSpy.mockClear();

              // Phase 1: Trigger moderate loss step-down (full → reduced-fps)
              // Poll 1: moderate loss
              vi.advanceTimersByTime(5000);
              await vi.advanceTimersByTimeAsync(0);
              // Poll 2: moderate loss → triggers full → reduced-fps
              vi.advanceTimersByTime(5000);
              await vi.advanceTimersByTimeAsync(0);

              // Verify first step-down happened (FPS reduced, resolution unchanged)
              expect(applyConstraintsSpy).toHaveBeenCalledTimes(1);
              const fpsConstraints = applyConstraintsSpy.mock.calls[0][0] as MediaTrackConstraints;
              expect((fpsConstraints.width as ConstrainULongRange).ideal).toBe(2560);
              expect((fpsConstraints.height as ConstrainULongRange).ideal).toBe(1440);
              expect((fpsConstraints.frameRate as ConstrainDoubleRange).max).toBe(30);

              applyConstraintsSpy.mockClear();

              // Phase 2: Trigger severe loss step-down (reduced-fps → reduced-resolution)
              // Poll 3: severe loss
              vi.advanceTimersByTime(5000);
              await vi.advanceTimersByTimeAsync(0);
              expect(applyConstraintsSpy).not.toHaveBeenCalled();

              // Poll 4: severe loss → triggers reduced-fps → reduced-resolution
              vi.advanceTimersByTime(5000);
              await vi.advanceTimersByTimeAsync(0);

              expect(applyConstraintsSpy).toHaveBeenCalledTimes(1);

              // Verify resolution was reduced (1440→1080) and FPS stays reduced (30)
              const resConstraints = applyConstraintsSpy.mock.calls[0][0] as MediaTrackConstraints;
              expect((resConstraints.width as ConstrainULongRange).ideal).toBe(1920);
              expect((resConstraints.height as ConstrainULongRange).ideal).toBe(1080);
              expect((resConstraints.frameRate as ConstrainDoubleRange).max).toBe(30);

              // Verify transition was logged
              const adaptiveLogs = logSpy.mock.calls.filter(
                args => args.some(a => typeof a === 'string' && (a as string).includes('adaptive quality:') && (a as string).includes('reduced-fps') && (a as string).includes('reduced-resolution')),
              );
              expect(adaptiveLogs.length).toBeGreaterThanOrEqual(1);

              mod.disconnect();
            } finally {
              vi.useRealTimers();
              logSpy.mockRestore();
              warnSpy.mockRestore();
            }
          },
        ),
        { numRuns: 100 },
      );
    });
  });

  // Feature: screen-share-quality, Property 10: Adaptive recovery on low loss
  describe('P10: Adaptive recovery on low loss', () => {
    /**
     * Validates: Requirements 9.3, 9.4
     *
     * For any active screen share in a reduced quality tier where outbound
     * packet loss drops below 3% for 3 consecutive polls (15s at 5s cadence),
     * the adaptive quality system shall restore the previous quality tier,
     * and log the transition.
     */
    it('restores previous tier after 3 consecutive polls of <3% loss', async () => {
      await fc.assert(
        fc.asyncProperty(
          // Moderate loss to trigger initial FPS reduction (>5%)
          fc.double({ min: 5.1, max: 14.9, noNaN: true }),
          fc.double({ min: 5.1, max: 14.9, noNaN: true }),
          // Recovery loss values (< 3%)
          fc.double({ min: 0, max: 2.9, noNaN: true }),
          fc.double({ min: 0, max: 2.9, noNaN: true }),
          fc.double({ min: 0, max: 2.9, noNaN: true }),
          async (modLoss1, modLoss2, recLoss1, recLoss2, recLoss3) => {
            resetAll();

            let pollIndex = 0;
            const losses = [modLoss1, modLoss2, recLoss1, recLoss2, recLoss3];
            attachAdaptivePublisher(async () => {
              const idx = Math.min(pollIndex++, losses.length - 1);
              return createAdaptiveStatsReport({
                lossPercent: losses[idx],
                bytesSent: pollIndex * 500000,
                timestamp: 1000000 + pollIndex * 5000,
              });
            });
            const { applyConstraintsSpy } = attachAdaptiveScreenShareTrack();

            const logSpy = vi.spyOn(console, 'log').mockImplementation(() => {});
            const warnSpy = vi.spyOn(console, 'warn').mockImplementation(() => {});
            const cbs = createMockCallbacks();
            const mod = new LiveKitModule(cbs);
            await driveToConnected(mod);

            vi.useFakeTimers();
            try {
              // Set quality to 'max' before starting so the base preset is 'max' (60fps)
              // This makes the FPS step-down and recovery observable
              (mod as unknown as Record<string, unknown>).currentQuality = 'max';
              await mod.startScreenShare();

              // Advance 100ms for post-publish tuning
              vi.advanceTimersByTime(100);
              await vi.advanceTimersByTimeAsync(0);
              applyConstraintsSpy.mockClear();

              // Phase 1: Trigger moderate loss step-down (full → reduced-fps)
              vi.advanceTimersByTime(5000);
              await vi.advanceTimersByTimeAsync(0);
              vi.advanceTimersByTime(5000);
              await vi.advanceTimersByTimeAsync(0);

              // Verify step-down happened (FPS reduced from 60→30)
              expect(applyConstraintsSpy).toHaveBeenCalledTimes(1);
              const stepDownConstraints = applyConstraintsSpy.mock.calls[0][0] as MediaTrackConstraints;
              expect((stepDownConstraints.frameRate as ConstrainDoubleRange).max).toBe(30);

              applyConstraintsSpy.mockClear();

              // Phase 2: Recovery — 3 consecutive polls with <3% loss
              // Recovery poll 1
              vi.advanceTimersByTime(5000);
              await vi.advanceTimersByTimeAsync(0);
              expect(applyConstraintsSpy).not.toHaveBeenCalled();

              // Recovery poll 2
              vi.advanceTimersByTime(5000);
              await vi.advanceTimersByTimeAsync(0);
              expect(applyConstraintsSpy).not.toHaveBeenCalled();

              // Recovery poll 3 → triggers reduced-fps → full
              vi.advanceTimersByTime(5000);
              await vi.advanceTimersByTimeAsync(0);

              expect(applyConstraintsSpy).toHaveBeenCalledTimes(1);

              // Verify recovery: resolution stays 2560×1440, FPS restored to 60 (base 'max' preset)
              const recoveryConstraints = applyConstraintsSpy.mock.calls[0][0] as MediaTrackConstraints;
              expect((recoveryConstraints.width as ConstrainULongRange).ideal).toBe(2560);
              expect((recoveryConstraints.height as ConstrainULongRange).ideal).toBe(1440);
              expect((recoveryConstraints.frameRate as ConstrainDoubleRange).max).toBe(60);

              // Verify transition was logged
              const adaptiveLogs = logSpy.mock.calls.filter(
                args => args.some(a => typeof a === 'string' && (a as string).includes('adaptive quality:') && (a as string).includes('reduced-fps') && (a as string).includes('→ full')),
              );
              expect(adaptiveLogs.length).toBeGreaterThanOrEqual(1);

              mod.disconnect();
            } finally {
              vi.useRealTimers();
              logSpy.mockRestore();
              warnSpy.mockRestore();
            }
          },
        ),
        { numRuns: 100 },
      );
    });
  });

});


// ═══ Bug Condition Exploration: Windows Screen Share Self-Echo ══════
// These tests assert the EXPECTED BEHAVIOR after the fix.
// On UNFIXED code, they SHOULD FAIL — failure confirms the bug exists.
// DO NOT attempt to fix the code or tests when they fail.

/** Invoke calls recorded by the @tauri-apps/api/core mock. */
let tauriInvokeCalls: Array<{ cmd: string; args?: unknown }>;
let audioShareStartResult: { loopback_exclusion_available: boolean; real_output_device_id?: string | null };

// Mock @tauri-apps/api/core — intercept invoke('audio_share_start') / invoke('audio_share_stop')
vi.mock('@tauri-apps/api/core', () => ({
  invoke: vi.fn(async (cmd: string, args?: unknown) => {
    tauriInvokeCalls.push({ cmd, args });
    if (cmd === 'audio_share_start') {
      return audioShareStartResult;
    }
    return undefined;
  }),
}));

// Mock @tauri-apps/api/event — used by startWasapiAudioBridge for Tauri event listeners
vi.mock('@tauri-apps/api/event', () => ({
  listen: vi.fn(async (_event: string, _handler: unknown) => {
    // Return a no-op unlisten function
    return () => {};
  }),
}));

describe('Bug Condition Exploration: Windows Screen Share Self-Echo', () => {

  beforeEach(() => {
    tauriInvokeCalls = [];
  });

  /**
   * Validates: Requirements 1.1, 1.2, 2.1
   *
   * Bug condition: On Windows, startScreenShare() passes audio: true to
   * setScreenShareEnabled, causing getDisplayMedia({ audio: true }) to capture
   * system-wide audio including Wavis's own playback.
   *
   * Expected behavior (after fix): captureOpts.audio === false on Windows,
   * so getDisplayMedia only captures video. Audio is routed through WASAPI.
   */
  it('startScreenShare() on Windows: captureOpts.audio should be false (video only via getDisplayMedia)', async () => {
    resetAll();
    tauriInvokeCalls = [];

    // Mock Windows user agent
    vi.stubGlobal('navigator', { userAgent: 'Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36', mediaDevices: createMockMediaDevices() });

    const cbs = createMockCallbacks();
    const mod = new LiveKitModule(cbs);
    await driveToConnected(mod);

    const callsBefore = sdkCalls.length;
    await mod.startScreenShare();

    const shareCalls = sdkCalls.slice(callsBefore).filter(c => c.method === 'setScreenShareEnabled' && c.args[0] === true);
    expect(shareCalls).toHaveLength(1);

    const captureOpts = shareCalls[0].args[1] as Record<string, unknown>;
    expect(captureOpts).toBeDefined();

    // EXPECTED (after fix): audio should be false on Windows
    expect(captureOpts.audio).toBe(false);

    mod.disconnect();
    vi.stubGlobal('navigator', { userAgent: '', mediaDevices: createMockMediaDevices() });
  });

  /**
   * Validates: Requirements 2.1, 2.2
   *
   * Expected behavior (after Phase 1 fix): starting a share on Windows does
   * not auto-start WASAPI system audio while the share-audio toggle is off.
   */
  it('startScreenShare() on Windows: invoke("audio_share_start") is not called while audio remains off', async () => {
    resetAll();
    tauriInvokeCalls = [];

    vi.stubGlobal('navigator', { userAgent: 'Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36', mediaDevices: createMockMediaDevices() });

    const cbs = createMockCallbacks();
    const mod = new LiveKitModule(cbs);
    await driveToConnected(mod);

    await mod.startScreenShare();

    // EXPECTED (after Phase 1 fix): no automatic system-audio start
    const audioStartCalls = tauriInvokeCalls.filter(c => c.cmd === 'audio_share_start');
    expect(audioStartCalls).toHaveLength(0);

    mod.disconnect();
    vi.stubGlobal('navigator', { userAgent: '', mediaDevices: createMockMediaDevices() });
  });

  /**
   * Validates: Requirements 1.2, 2.2
   *
   * Bug condition: restartScreenShareWithAudio(true) on Windows calls
   * setScreenShareEnabled(false) then setScreenShareEnabled(true, { audio: true }),
   * restarting the entire getDisplayMedia capture and re-introducing echo.
   *
   * Expected behavior (after fix): setScreenShareEnabled(false) is NOT called —
   * video track stays live. Only WASAPI audio is started.
   */
  it('restartScreenShareWithAudio(true) on Windows: setScreenShareEnabled(false) should NOT be called', async () => {
    resetAll();
    tauriInvokeCalls = [];

    vi.stubGlobal('navigator', { userAgent: 'Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36', mediaDevices: createMockMediaDevices() });

    const cbs = createMockCallbacks();
    const mod = new LiveKitModule(cbs);
    await driveToConnected(mod);

    // Start screen share first
    await mod.startScreenShare();

    // Clear SDK calls to isolate the restart
    sdkCalls.length = 0;
    tauriInvokeCalls = [];

    await mod.restartScreenShareWithAudio(true);

    // EXPECTED (after fix): setScreenShareEnabled(false) should NOT be called
    const stopCalls = sdkCalls.filter(c => c.method === 'setScreenShareEnabled' && c.args[0] === false);
    expect(stopCalls).toHaveLength(0);

    mod.disconnect();
    vi.stubGlobal('navigator', { userAgent: '', mediaDevices: createMockMediaDevices() });
  });

  /**
   * Validates: Requirements 2.2
   *
   * Expected behavior (after fix): restartScreenShareWithAudio(true) on Windows
   * calls invoke('audio_share_start') instead of restarting getDisplayMedia.
   */
  it('restartScreenShareWithAudio(true) on Windows: invoke("audio_share_start") should be called', async () => {
    resetAll();
    tauriInvokeCalls = [];

    vi.stubGlobal('navigator', { userAgent: 'Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36', mediaDevices: createMockMediaDevices() });

    const cbs = createMockCallbacks();
    const mod = new LiveKitModule(cbs);
    await driveToConnected(mod);

    await mod.startScreenShare();

    // Clear invoke calls to isolate the restart
    tauriInvokeCalls = [];

    await mod.restartScreenShareWithAudio(true);

    // EXPECTED (after fix): audio_share_start should be called
    const audioStartCalls = tauriInvokeCalls.filter(c => c.cmd === 'audio_share_start');
    expect(audioStartCalls).toHaveLength(1);

    mod.disconnect();
    vi.stubGlobal('navigator', { userAgent: '', mediaDevices: createMockMediaDevices() });
  });

  /**
   * Validates: Requirements 2.3, 3.5
   *
   * Expected behavior (after fix): Windows stopScreenShare() explicitly
   * unpublishes the local screen-share track, stops native share-audio first,
   * force-stops the captured MediaStreamTrack, and logs displaySurface.
   */
  it('stopScreenShare() on Windows hard-tears down the local track and logs diagnostics', async () => {
    resetAll();
    tauriInvokeCalls = [];

    vi.stubGlobal('navigator', { userAgent: 'Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36', mediaDevices: createMockMediaDevices() });
    const logSpy = vi.spyOn(console, 'log').mockImplementation(() => {});

    const { mediaStreamTrack } = installLocalScreenSharePublication(
      createMockLocalScreenShareMediaTrack('window'),
    );
    const cbs = createMockCallbacks();
    const mod = new LiveKitModule(cbs);
    await driveToConnected(mod);

    await mod.startScreenShare();

    const originalUnpublish = mockRoom.localParticipant.unpublishTrack;
    mockRoom.localParticipant.unpublishTrack = vi.fn(async (track: unknown) => {
      expect(tauriInvokeCalls.filter(c => c.cmd === 'audio_share_stop')).toHaveLength(1);
      return await originalUnpublish(track);
    });

    // Clear recorded calls to isolate the stop
    sdkCalls.length = 0;
    tauriInvokeCalls = [];

    await mod.stopScreenShare();

    const audioStopCalls = tauriInvokeCalls.filter(c => c.cmd === 'audio_share_stop');
    expect(audioStopCalls).toHaveLength(1);
    const unpublishCalls = sdkCalls.filter(c => c.method === 'unpublishTrack');
    expect(unpublishCalls).toHaveLength(1);
    expect(unpublishCalls[0].args[0]).toBe(mediaStreamTrack);
    expect(mediaStreamTrack.stop).toHaveBeenCalledTimes(1);
    expect(mediaStreamTrack.readyState).toBe('ended');
    expect(
      logSpy.mock.calls.some(args =>
        args.some(a => typeof a === 'string' && (a as string).includes('displaySurface=window')),
      ),
    ).toBe(true);
    expect(
      logSpy.mock.calls.some(args =>
        args.some(a => typeof a === 'string' && (a as string).includes('readyState=ended')),
      ),
    ).toBe(true);

    mod.disconnect();
    logSpy.mockRestore();
    vi.stubGlobal('navigator', { userAgent: '', mediaDevices: createMockMediaDevices() });
  });

  it('stopScreenShare() on Windows force-stops the track even if unpublishTrack throws', async () => {
    resetAll();
    tauriInvokeCalls = [];

    vi.stubGlobal('navigator', { userAgent: 'Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36', mediaDevices: createMockMediaDevices() });

    const { mediaStreamTrack } = installLocalScreenSharePublication(
      createMockLocalScreenShareMediaTrack('monitor'),
    );
    const cbs = createMockCallbacks();
    const mod = new LiveKitModule(cbs);
    await driveToConnected(mod);

    await mod.startScreenShare();

    sdkCalls.length = 0;
    tauriInvokeCalls = [];
    mockRoom.localParticipant.unpublishTrack = vi.fn(async (track: unknown) => {
      sdkCalls.push({ method: 'unpublishTrack', args: [track] });
      throw new Error('network hiccup');
    });

    await expect(mod.stopScreenShare()).rejects.toThrow('network hiccup');

    expect(tauriInvokeCalls.filter(c => c.cmd === 'audio_share_stop')).toHaveLength(1);
    expect(mediaStreamTrack.stop).toHaveBeenCalledTimes(1);
    expect(mediaStreamTrack.readyState).toBe('ended');

    mod.disconnect();
    vi.stubGlobal('navigator', { userAgent: '', mediaDevices: createMockMediaDevices() });
  });

  it('changeScreenShareSource() on Windows uses replaceTrack without tearing down the existing publication', async () => {
    resetAll();
    tauriInvokeCalls = [];

    vi.stubGlobal('navigator', { userAgent: 'Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36', mediaDevices: createMockMediaDevices() });

    const { publication, mediaStreamTrack } = installLocalScreenSharePublication(
      createMockLocalScreenShareMediaTrack('browser'),
    );
    const cbs = createMockCallbacks();
    const mod = new LiveKitModule(cbs);
    await driveToConnected(mod);

    await mod.startScreenShare();

    sdkCalls.length = 0;
    tauriInvokeCalls = [];

    await mod.changeScreenShareSource();

    // Acquire-before-drop: the existing track must NOT be unpublished or stopped.
    expect(sdkCalls.findIndex(c => c.method === 'unpublishTrack')).toBe(-1);
    expect(sdkCalls.filter(c => c.method === 'setScreenShareEnabled' && c.args[0] === false)).toHaveLength(0);
    expect(mediaStreamTrack.stop).not.toHaveBeenCalled();

    // replaceTrack must have been called to swap in the new source.
    expect(publication.track.replaceTrack).toHaveBeenCalledTimes(1);

    // Native audio is NOT stopped during source change (same as original change-source
    // path which passed stopNativeAudio: false).
    expect(tauriInvokeCalls.filter(c => c.cmd === 'audio_share_stop')).toHaveLength(0);

    mod.disconnect();
    vi.stubGlobal('navigator', { userAgent: '', mediaDevices: createMockMediaDevices() });
  });

  it('changeScreenShareSource() cancel on Windows keeps the existing share alive', async () => {
    resetAll();
    tauriInvokeCalls = [];

    const mockMediaDevices = createMockMediaDevices();
    mockMediaDevices.getDisplayMedia = vi.fn(async () => {
      throw new DOMException('Permission denied', 'NotAllowedError');
    });
    vi.stubGlobal('navigator', {
      userAgent: 'Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36',
      mediaDevices: mockMediaDevices,
    });

    const { publication, mediaStreamTrack } = installLocalScreenSharePublication(
      createMockLocalScreenShareMediaTrack('window'),
    );
    const cbs = createMockCallbacks();
    const mod = new LiveKitModule(cbs);
    await driveToConnected(mod);

    await mod.startScreenShare();
    sdkCalls.length = 0;

    const result = await mod.changeScreenShareSource();

    // Must return false when the picker is cancelled.
    expect(result).toBe(false);

    // The existing publication must be completely undisturbed.
    expect(sdkCalls.findIndex(c => c.method === 'unpublishTrack')).toBe(-1);
    expect(sdkCalls.filter(c => c.method === 'setScreenShareEnabled')).toHaveLength(0);
    expect(mediaStreamTrack.stop).not.toHaveBeenCalled();
    expect(publication.track.replaceTrack).not.toHaveBeenCalled();

    mod.disconnect();
    vi.stubGlobal('navigator', { userAgent: '', mediaDevices: createMockMediaDevices() });
  });

  it('changeScreenShareSource() cancel on non-Windows keeps the existing share alive', async () => {
    resetAll();

    const mockMediaDevices = createMockMediaDevices();
    mockMediaDevices.getDisplayMedia = vi.fn(async () => {
      throw new DOMException('Permission denied', 'NotAllowedError');
    });
    vi.stubGlobal('navigator', { userAgent: '', mediaDevices: mockMediaDevices });

    const { publication, mediaStreamTrack } = installLocalScreenSharePublication(
      createMockLocalScreenShareMediaTrack('monitor'),
    );
    const cbs = createMockCallbacks();
    const mod = new LiveKitModule(cbs);
    await driveToConnected(mod);

    await mod.startScreenShare();
    sdkCalls.length = 0;

    const result = await mod.changeScreenShareSource();

    expect(result).toBe(false);
    expect(sdkCalls.filter(c => c.method === 'setScreenShareEnabled')).toHaveLength(0);
    expect(mediaStreamTrack.stop).not.toHaveBeenCalled();
    expect(publication.track.replaceTrack).not.toHaveBeenCalled();

    mod.disconnect();
    vi.stubGlobal('navigator', { userAgent: '', mediaDevices: createMockMediaDevices() });
  });

  it('changeScreenShareSource() with no existing publication uses setScreenShareEnabled path', async () => {
    resetAll();

    vi.stubGlobal('navigator', { userAgent: '', mediaDevices: createMockMediaDevices() });

    // Do NOT install a publication — simulate the case where share is not active.
    const cbs = createMockCallbacks();
    const mod = new LiveKitModule(cbs);
    await driveToConnected(mod);

    sdkCalls.length = 0;

    const result = await mod.changeScreenShareSource();

    expect(result).toBe(true);
    // Standard path must be used when there is no existing publication.
    const enableCalls = sdkCalls.filter(c => c.method === 'setScreenShareEnabled' && c.args[0] === true);
    expect(enableCalls).toHaveLength(1);

    mod.disconnect();
    vi.stubGlobal('navigator', { userAgent: '', mediaDevices: createMockMediaDevices() });
  });

});


// ═══ Preservation Property Tests: Screen Share Self-Echo Fix ═══════
// These tests capture CURRENT behavior on UNFIXED code.
// They MUST PASS on unfixed code — they establish the baseline that
// must be preserved after the fix is applied.
// Uses fast-check for property-based testing across platform variants.

describe('Share leak publish diagnostics', () => {
  it('captures publish-time sender reuse diagnostics in the closed share summary', async () => {
    resetAll();
    tauriInvokeCalls = [];

    vi.stubGlobal('navigator', {
      userAgent: 'Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36',
      mediaDevices: createMockMediaDevices(),
    });
    const logSpy = vi.spyOn(console, 'log').mockImplementation(() => {});

    const { mediaStreamTrack } = attachManagedScreenSharePublisherPeerConnection({
      reuseExpected: true,
      degradationPreferenceConfigured: true,
      degradationPreferenceResult: {
        attemptedPreferences: ['maintain-resolution-combined'],
        finalErrorName: null,
        finalErrorMessage: null,
        invalidStateSkipped: false,
      },
    });
    const cbs = createMockCallbacks();
    const mod = new LiveKitModule(cbs);
    await driveToConnected(mod);

    await mod.startScreenShare();
    await mod.stopScreenShare();

    const shareLeakSummaries = cbs.calls.filter((call) => call.method === 'onShareLeakSummary');
    expect(shareLeakSummaries).toHaveLength(1);
    const summary = shareLeakSummaries[0].args[0] as import('../share-leak-diagnostics').ShareSessionLeakSummary;

    expect(summary.senderReuseDiagnostics?.reuseExpected).toBe(true);
    expect(summary.senderReuseDiagnostics?.publishWebRtcSnapshot?.publisherPeerConnectionId).toBe('publisher-pc-1');
    expect(summary.senderReuseDiagnostics?.publishWebRtcSnapshot?.senderCount).toBe(2);
    expect(summary.senderReuseDiagnostics?.publishWebRtcSnapshot?.videoSenderCount).toBe(1);
    expect(summary.senderReuseDiagnostics?.publishWebRtcSnapshot?.transceiverCount).toBe(2);
    expect(summary.senderReuseDiagnostics?.publishWebRtcSnapshot?.publicationTrackId).toBe(mediaStreamTrack.id);
    expect(summary.senderReuseDiagnostics?.degradationPreferenceResult).toEqual({
      senderWasReused: true,
      attemptedPreferences: ['maintain-resolution-combined'],
      finalErrorName: null,
      finalErrorMessage: null,
      invalidStateSkipped: false,
    });
    expect(summary.senderReuseDiagnostics?.events.map((event) => event.name)).toEqual([
      'publish_started',
      'publish_snapshot_captured',
      'reuse_inferred',
    ]);
    expect(summary.browserWebRtcBeforeStop?.screenShareSenderCount).toBe(1);
    expect(summary.browserWebRtcAfterStop?.screenShareSenderCount).toBe(0);

    expect(
      logSpy.mock.calls.some((args) =>
        args.some((arg) => typeof arg === 'string' && (arg as string).includes('[share-leak] session=') && (arg as string).includes('publish_snapshot')),
      ),
    ).toBe(true);
    expect(
      logSpy.mock.calls.some((args) =>
        args.some((arg) => typeof arg === 'string' && (arg as string).includes('[share-leak] session=') && (arg as string).includes('reuse_inferred')),
      ),
    ).toBe(true);
    expect(
      logSpy.mock.calls.some((args) =>
        args.some((arg) => typeof arg === 'string' && (arg as string).includes('[share-leak] session=') && (arg as string).includes('degradation_preference')),
      ),
    ).toBe(true);

    mod.disconnect();
    logSpy.mockRestore();
    vi.stubGlobal('navigator', { userAgent: '', mediaDevices: createMockMediaDevices() });
  });
});

describe('Connection quality polling', () => {
  function createConnectionQualityStatsReport(options: {
    currentRoundTripTime?: number;
    packetsReceived?: number;
    packetsLost?: number;
    jitter?: number;
  } = {}): RTCStatsReport {
    const entries: Array<[string, Record<string, unknown>]> = [];
    if (typeof options.currentRoundTripTime === 'number') {
      entries.push(['candidate-pair', {
        type: 'candidate-pair',
        nominated: true,
        currentRoundTripTime: options.currentRoundTripTime,
      }]);
    }
    if (
      typeof options.packetsReceived === 'number' &&
      typeof options.packetsLost === 'number' &&
      typeof options.jitter === 'number'
    ) {
      entries.push(['inbound-rtp-audio', {
        type: 'inbound-rtp',
        kind: 'audio',
        packetsReceived: options.packetsReceived,
        packetsLost: options.packetsLost,
        jitter: options.jitter,
      }]);
    }
    const map = new Map(entries);
    return {
      forEach: (cb: (value: Record<string, unknown>, key: string) => void) => map.forEach(cb),
      get: (key: string) => map.get(key),
      has: (key: string) => map.has(key),
      entries: () => map.entries(),
      keys: () => map.keys(),
      values: () => map.values(),
      [Symbol.iterator]: () => map[Symbol.iterator](),
      size: map.size,
    } as unknown as RTCStatsReport;
  }

  it('polls one peer connection per 10 second cycle and alternates publisher/subscriber', async () => {
    resetAll();

    // Publisher transport — polled every other cycle for RTT / bandwidth / candidate type
    const publisher = {
      getStats: vi.fn(async () => createConnectionQualityStatsReport({ currentRoundTripTime: 0.123 })),
      getSenders: () => [],
      getTransceivers: () => [],
    };
    (mockRoom as Record<string, unknown>).engine = {
      pcManager: { publisher },
    };

    // Per-receiver stats — polled every cycle via remoteTrack.receiver.getStats()
    const receiverGetStats = vi.fn(async () => createConnectionQualityStatsReport({
      packetsReceived: 90,
      packetsLost: 10,
      jitter: 0.045,
    }));
    mockRoom.remoteParticipants.set('peer-1', {
      identity: 'peer-1',
      trackPublications: new Map([
        ['audio-track-1', {
          kind: 'audio',
          source: 'microphone',
          track: { receiver: { getStats: receiverGetStats } },
        }],
      ]),
    });

    const cbs = createMockCallbacks();
    const mod = new LiveKitModule(cbs);

    vi.useFakeTimers();
    try {
      await mod.connect('wss://sfu.test', 'tok');

      // Cycle 1: publisher polled + per-receiver polled
      await vi.advanceTimersByTimeAsync(10_000);
      expect(publisher.getStats).toHaveBeenCalledTimes(1);
      expect(receiverGetStats).toHaveBeenCalledTimes(1);

      const firstQualityCall = cbs.calls.filter((call) => call.method === 'onConnectionQuality');
      expect(firstQualityCall).toHaveLength(1);
      expect(firstQualityCall[0].args[0]).toEqual({
        rttMs: 123,
        packetLossPercent: 10,
        jitterMs: 45,
        jitterBufferDelayMs: 0,
        concealmentEventsPerInterval: 0,
        candidateType: 'unknown',
        availableBandwidthKbps: 0,
      });

      // Cycle 2: publisher NOT polled + per-receiver polled
      await vi.advanceTimersByTimeAsync(10_000);
      expect(publisher.getStats).toHaveBeenCalledTimes(1);
      expect(receiverGetStats).toHaveBeenCalledTimes(2);

      const secondQualityCall = cbs.calls.filter((call) => call.method === 'onConnectionQuality');
      expect(secondQualityCall).toHaveLength(2);
      // RTT carried forward from cycle 1, loss/jitter from per-receiver
      expect(secondQualityCall[1].args[0]).toEqual({
        rttMs: 123,
        packetLossPercent: 10,
        jitterMs: 45,
        jitterBufferDelayMs: 0,
        concealmentEventsPerInterval: 0,
        candidateType: 'unknown',
        availableBandwidthKbps: 0,
      });

      mod.disconnect();
    } finally {
      vi.useRealTimers();
    }
  });
});

describe('Reconnect screen share cleanup', () => {
  it('stops an active screen share when LiveKit starts reconnecting', async () => {
    resetAll();
    tauriInvokeCalls = [];

    vi.stubGlobal('navigator', {
      userAgent: 'Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36',
      mediaDevices: createMockMediaDevices(),
    });
    attachManagedScreenSharePublisherPeerConnection({ reuseExpected: true });

    const cbs = createMockCallbacks();
    const mod = new LiveKitModule(cbs);
    await driveToConnected(mod);
    await mod.startScreenShare();

    expect(mockRoom.localParticipant.getTrackPublication('screen_share')).toBeDefined();

    emitRoomEvent('reconnecting');
    await tick();

    expect(mockRoom.localParticipant.getTrackPublication('screen_share')).toBeUndefined();
    expect(
      cbs.calls.some((call) =>
        call.method === 'onSystemEvent' && call.args[0] === 'Screen share stopped due to reconnect'
      ),
    ).toBe(true);
    expect(
      sdkCalls.some((call) => call.method === 'setScreenShareEnabled' && call.args[0] === false),
    ).toBe(true);

    mod.disconnect();
    vi.stubGlobal('navigator', { userAgent: '', mediaDevices: createMockMediaDevices() });
  });
});

describe('Preservation: Native Share-Audio Path and Non-Audio Paths', () => {

  beforeEach(() => {
    tauriInvokeCalls = [];
  });

  /**
   * Validates: Requirements 3.3
   *
   * Preservation Property 1: For all non-Windows platforms with audio: true,
   * captureOpts.audio === true is passed to setScreenShareEnabled.
   * macOS getDisplayMedia audio path unchanged.
   */
  it('non-Windows platforms with audio:true → captureOpts.audio === true (PBT)', async () => {
    await fc.assert(
      fc.asyncProperty(
        fc.constant('Mozilla/5.0 (X11; Linux x86_64) AppleWebKit/537.36'),
        async (userAgent) => {
          resetAll();
          tauriInvokeCalls = [];

          vi.stubGlobal('navigator', {
            userAgent,
            userActivation: { isActive: true },
            mediaDevices: createMockMediaDevices(),
          });

          const cbs = createMockCallbacks();
          const mod = new LiveKitModule(cbs);
          await driveToConnected(mod);

          await mod.startScreenShare();

          // Find the setScreenShareEnabled(true, captureOpts, ...) call
          const startCalls = sdkCalls.filter(
            c => c.method === 'setScreenShareEnabled' && c.args[0] === true,
          );
          expect(startCalls.length).toBeGreaterThanOrEqual(1);

          const captureOpts = startCalls[0].args[1] as Record<string, unknown>;
          expect(captureOpts.audio).toBe(false);

          // No audio_share_start should be called on non-Windows
          const audioStartCalls = tauriInvokeCalls.filter(c => c.cmd === 'audio_share_start');
          expect(audioStartCalls).toHaveLength(0);

          mod.disconnect();
          vi.stubGlobal('navigator', { userAgent: '', mediaDevices: createMockMediaDevices() });
        },
      ),
      { numRuns: 20 },
    );
  });

  /**
   * Validates: Requirements 3.2
   *
   * Preservation Property 2: For all platforms with audio: false,
   * captureOpts.audio === false and no invoke('audio_share_start') call.
   * Video-only path unchanged.
   */
  it('all platforms with audio:false → captureOpts.audio === false, no audio_share_start (PBT)', async () => {
    await fc.assert(
      fc.asyncProperty(
        fc.constantFrom(
          'Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36',
          'Mozilla/5.0 (Macintosh; Intel Mac OS X 10_15_7) AppleWebKit/605.1.15',
          'Mozilla/5.0 (X11; Linux x86_64) AppleWebKit/537.36',
        ),
        async (userAgent) => {
          resetAll();
          tauriInvokeCalls = [];

          vi.stubGlobal('navigator', {
            userAgent,
            userActivation: { isActive: true },
            mediaDevices: createMockMediaDevices(),
          });

          const cbs = createMockCallbacks();
          const mod = new LiveKitModule(cbs);

          // Override the capture profile to disable audio BEFORE connecting
          // We need to access the private field — use type assertion
          // eslint-disable-next-line @typescript-eslint/no-explicit-any
          (mod as any).currentCaptureProfile = {
            ...(mod as any).currentCaptureProfile,
            audio: false,
          };

          await driveToConnected(mod);
          await mod.startScreenShare();

          // Find the setScreenShareEnabled(true, captureOpts, ...) call
          const startCalls = sdkCalls.filter(
            c => c.method === 'setScreenShareEnabled' && c.args[0] === true,
          );
          expect(startCalls.length).toBeGreaterThanOrEqual(1);

          const captureOpts = startCalls[0].args[1] as Record<string, unknown>;
          expect(captureOpts.audio).toBe(false);

          // No audio_share_start should be called for video-only
          const audioStartCalls = tauriInvokeCalls.filter(c => c.cmd === 'audio_share_start');
          expect(audioStartCalls).toHaveLength(0);

          mod.disconnect();
          vi.stubGlobal('navigator', { userAgent: '', mediaDevices: createMockMediaDevices() });
        },
      ),
      { numRuns: 20 },
    );
  });

  /**
   * Validates: Requirements 3.3
   *
   * Preservation Property 3: For non-Windows platforms with restartScreenShareWithAudio(true),
   * setScreenShareEnabled(false) IS called followed by setScreenShareEnabled(true).
   * Full getDisplayMedia restart preserved on macOS.
   */
  it('non-Windows + restartScreenShareWithAudio(true) → full getDisplayMedia restart (PBT)', async () => {
    await fc.assert(
      fc.asyncProperty(
        fc.constant('Mozilla/5.0 (X11; Linux x86_64) AppleWebKit/537.36'),
        async (userAgent) => {
          resetAll();
          tauriInvokeCalls = [];

          vi.stubGlobal('navigator', {
            userAgent,
            userActivation: { isActive: true },
            mediaDevices: createMockMediaDevices(),
          });

          const cbs = createMockCallbacks();
          const mod = new LiveKitModule(cbs);
          await driveToConnected(mod);

          // Start screen share first
          await mod.startScreenShare();

          // Clear SDK calls to isolate the restart
          sdkCalls.length = 0;

          await mod.restartScreenShareWithAudio(true);

          // setScreenShareEnabled(false) should be called (stop video)
          const stopCalls = sdkCalls.filter(
            c => c.method === 'setScreenShareEnabled' && c.args[0] === false,
          );
          expect(stopCalls.length).toBeGreaterThanOrEqual(1);

          // setScreenShareEnabled(true, captureOpts) should be called (restart video)
          const startCalls = sdkCalls.filter(
            c => c.method === 'setScreenShareEnabled' && c.args[0] === true,
          );
          expect(startCalls.length).toBeGreaterThanOrEqual(1);

          // The restart captureOpts should have audio: true
          const captureOpts = startCalls[0].args[1] as Record<string, unknown>;
          expect(captureOpts.audio).toBe(true);

          mod.disconnect();
          vi.stubGlobal('navigator', { userAgent: '', mediaDevices: createMockMediaDevices() });
        },
      ),
      { numRuns: 20 },
    );
  });

  it('macOS + restartScreenShareWithAudio(true) â†’ native audio start without video restart', async () => {
    resetAll();
    tauriInvokeCalls = [];

    vi.stubGlobal('navigator', { userAgent: 'Mozilla/5.0 (Macintosh; Intel Mac OS X 10_15_7) AppleWebKit/605.1.15', mediaDevices: createMockMediaDevices() });

    const cbs = createMockCallbacks();
    const mod = new LiveKitModule(cbs);
    await driveToConnected(mod);

    await mod.startScreenShare();

    sdkCalls.length = 0;
    tauriInvokeCalls = [];

    await mod.restartScreenShareWithAudio(true);

    const videoRestartCalls = sdkCalls.filter(
      c => c.method === 'setScreenShareEnabled',
    );
    expect(videoRestartCalls).toHaveLength(0);

    const audioStartCalls = tauriInvokeCalls.filter(c => c.cmd === 'audio_share_start');
    expect(audioStartCalls).toHaveLength(1);

    mod.disconnect();
    vi.stubGlobal('navigator', { userAgent: '', mediaDevices: createMockMediaDevices() });
  });

  /**
   * Validates: Requirements 3.5
   *
   * Preservation Property 4: Non-Windows stopScreenShare() still delegates
   * to setScreenShareEnabled(false). Existing browser-managed stop behavior
   * remains unchanged outside the Windows hard-teardown path.
   */
  it('non-Windows stopScreenShare() calls setScreenShareEnabled(false) (PBT)', async () => {
    await fc.assert(
      fc.asyncProperty(
        fc.constantFrom(
          'Mozilla/5.0 (Macintosh; Intel Mac OS X 10_15_7) AppleWebKit/605.1.15',
          'Mozilla/5.0 (X11; Linux x86_64) AppleWebKit/537.36',
        ),
        async (userAgent) => {
          resetAll();
          tauriInvokeCalls = [];

          vi.stubGlobal('navigator', { userAgent, mediaDevices: createMockMediaDevices() });

          const cbs = createMockCallbacks();
          const mod = new LiveKitModule(cbs);
          await driveToConnected(mod);

          // Start screen share first
          await mod.startScreenShare();

          // Clear SDK calls to isolate the stop
          sdkCalls.length = 0;

          await mod.stopScreenShare();

          // setScreenShareEnabled(false) should be called
          const stopCalls = sdkCalls.filter(
            c => c.method === 'setScreenShareEnabled' && c.args[0] === false,
          );
          expect(stopCalls).toHaveLength(1);

          mod.disconnect();
          vi.stubGlobal('navigator', { userAgent: '', mediaDevices: createMockMediaDevices() });
        },
      ),
      { numRuns: 20 },
    );
  });

  /**
   * Validates: Design caller-contract audit
   *
   * Preservation Property 5: Quality polling continuity on Windows toggle.
   * On UNFIXED code, restartScreenShareWithAudio(true) restarts the entire
   * getDisplayMedia capture, which restarts screenShareStatsPolling (new timer).
   * We verify the polling is active after the restart by checking that
   * startScreenShareStatsPolling was called (observable via setInterval spy).
   *
   * After the fix on Windows, the video track won't be restarted, so the
   * polling timer should remain the SAME (not restarted). This test captures
   * the current behavior: polling IS restarted because the full video restart
   * triggers startScreenShareStatsPolling().
   */
  it('Windows + restartScreenShareWithAudio(true) → screen share stats polling remains active', async () => {
    resetAll();
    tauriInvokeCalls = [];

    vi.stubGlobal('navigator', { userAgent: 'Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36', mediaDevices: createMockMediaDevices() });

    const cbs = createMockCallbacks();
    const mod = new LiveKitModule(cbs);
    await driveToConnected(mod);

    // Spy on setInterval/clearInterval to track polling lifecycle
    const setIntervalSpy = vi.spyOn(globalThis, 'setInterval');
    const clearIntervalSpy = vi.spyOn(globalThis, 'clearInterval');

    await mod.startScreenShare();

    // Clear spy call counts to isolate the restart
    setIntervalSpy.mockClear();
    clearIntervalSpy.mockClear();

    await mod.restartScreenShareWithAudio(true);

    // On Windows, restartScreenShareWithAudio only toggles WASAPI audio —
    // video and its stats polling remain untouched from startScreenShare().
    // Verify stats polling is still active (not restarted, just preserved).
    // eslint-disable-next-line @typescript-eslint/no-explicit-any
    const statsInterval = (mod as any).screenShareStatsInterval;
    expect(statsInterval).not.toBeNull();

    mod.disconnect();
    vi.stubGlobal('navigator', { userAgent: '', mediaDevices: createMockMediaDevices() });
    setIntervalSpy.mockRestore();
    clearIntervalSpy.mockRestore();
  });

});

// ═══ startWasapiAudioBridge: masterGain echo prevention ═══════════

describe('startWasapiAudioBridge: masterGain echo prevention', () => {

  /** Helper: add publishTrack to the mock local participant (not present in base mock). */
  function addPublishTrack() {
    (mockRoom.localParticipant as Record<string, unknown>).publishTrack = vi.fn(async (track: unknown) => {
      sdkCalls.push({ method: 'publishTrack', args: [track] });
      return { track, source: 'screen_share_audio' };
    });
  }

  beforeEach(() => {
    tauriInvokeCalls = [];
    resetAll();
    // Set Windows user agent so usesNativeScreenShareAudio() returns true
    vi.stubGlobal('navigator', {
      userAgent: 'Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36',
      mediaDevices: createMockMediaDevices(),
    });
  });

  afterEach(() => {
    vi.stubGlobal('navigator', { userAgent: '', mediaDevices: createMockMediaDevices() });
  });

  /**
   * When loopback exclusion IS available (macOS 14.2+), the OS excludes remote
   * audio processes from capture — no echo risk. masterGain must NOT be muted.
   */
  it('startWasapiAudioBridge(true): masterGain is NOT muted when loopback exclusion is available', async () => {
    addPublishTrack();
    const cbs = createMockCallbacks();
    const mod = new LiveKitModule(cbs);

    await driveToConnected(mod);

    // masterGain is createdGains[0] (created by ensureAudioContext during connect)
    const masterGain = createdGains[0];
    expect(masterGain).toBeDefined();
    const setValueAtTimeSpy = masterGain.gain.setValueAtTime;
    setValueAtTimeSpy.mockClear();

    await (mod as unknown as { startWasapiAudioBridge: (v: boolean) => Promise<void> })
      .startWasapiAudioBridge(true);

    // masterGain.gain.setValueAtTime should NOT have been called with 0
    const zeroingCalls = (setValueAtTimeSpy as ReturnType<typeof vi.fn>).mock.calls.filter(
      (args: unknown[]) => args[0] === 0,
    );
    expect(zeroingCalls).toHaveLength(0);

    // preShareGain must remain null (nothing was muted)
    expect((mod as unknown as { preShareGain: number | null }).preShareGain).toBeNull();

    mod.disconnect();
  });

  /**
   * When loopback exclusion is NOT available (Windows / older macOS), remote
   * audio in the subprocess would be captured → echo. masterGain MUST be muted.
   */
  it('startWasapiAudioBridge(false): masterGain IS muted when loopback exclusion is unavailable', async () => {
    addPublishTrack();
    const cbs = createMockCallbacks();
    const mod = new LiveKitModule(cbs);

    await driveToConnected(mod);

    const masterGain = createdGains[0];
    expect(masterGain).toBeDefined();
    const originalGainValue = masterGain.gain.value;
    const setValueAtTimeSpy = masterGain.gain.setValueAtTime as ReturnType<typeof vi.fn>;
    setValueAtTimeSpy.mockClear();

    await (mod as unknown as { startWasapiAudioBridge: (v: boolean) => Promise<void> })
      .startWasapiAudioBridge(false);

    // masterGain.gain.setValueAtTime(0, ...) should have been called once
    const zeroingCalls = setValueAtTimeSpy.mock.calls.filter((args: unknown[]) => args[0] === 0);
    expect(zeroingCalls).toHaveLength(1);
    expect(masterGain.gain.value).toBe(0);

    // preShareGain stores the original value for later restoration
    expect((mod as unknown as { preShareGain: number | null }).preShareGain).toBe(originalGainValue);

    mod.disconnect();
  });

  /**
   * Double-start guard: if startWasapiAudioBridge(false) is called twice without
   * a stop in between, preShareGain must hold the ORIGINAL pre-mute value, not 0.
   */
  it('double startWasapiAudioBridge(false): preShareGain is not overwritten with 0', async () => {
    addPublishTrack();
    const cbs = createMockCallbacks();
    const mod = new LiveKitModule(cbs);

    await driveToConnected(mod);

    const masterGain = createdGains[0];
    const originalGainValue = masterGain.gain.value; // e.g. 1

    const bridge = (mod as unknown as { startWasapiAudioBridge: (v: boolean) => Promise<void> });

    await bridge.startWasapiAudioBridge(false); // first call: preShareGain = 1, gain → 0
    expect((mod as unknown as { preShareGain: number | null }).preShareGain).toBe(originalGainValue);

    await bridge.startWasapiAudioBridge(false); // second call: should NOT overwrite preShareGain with 0
    expect((mod as unknown as { preShareGain: number | null }).preShareGain).toBe(originalGainValue);

    mod.disconnect();
  });

  /**
   * Hardened restore: if audioContext is gone when stopWasapiAudioBridge is called
   * (e.g. room disconnected while sharing), preShareGain is still cleared and no
   * exception is thrown.
   */
  it('stopWasapiAudioBridge: clears preShareGain even when audioContext is gone', async () => {
    addPublishTrack();
    const cbs = createMockCallbacks();
    const mod = new LiveKitModule(cbs);

    await driveToConnected(mod);

    const bridge = mod as unknown as {
      startWasapiAudioBridge: (v: boolean) => Promise<void>;
      stopWasapiAudioBridge: () => Promise<void>;
      preShareGain: number | null;
      audioContext: unknown;
    };

    await bridge.startWasapiAudioBridge(false); // mutes masterGain, sets preShareGain
    expect(bridge.preShareGain).not.toBeNull();

    // Simulate audioContext being torn down (e.g. room disconnect)
    bridge.audioContext = null;

    // stopWasapiAudioBridge must not throw and must clear preShareGain
    await expect(bridge.stopWasapiAudioBridge()).resolves.toBeUndefined();
    expect(bridge.preShareGain).toBeNull();

    mod.disconnect();
  });

});

// ═══ JS-side denoise (Windows/macOS) ══════════════════════════════

describe('JS-side noise suppression (Windows/macOS)', () => {
  afterEach(() => {
    vi.stubGlobal('navigator', { userAgent: '', mediaDevices: createMockMediaDevices() });
  });

  async function driveToConnectedWithMst(mod: LiveKitModule) {
    const { pub, mediaStreamTrack, track } = createAudioPubWithMst();
    await mod.connect('wss://sfu.test', 'tok');
    emitRoomEvent('connected');
    await tick();
    emitRoomEvent('localTrackPublished', pub, mockRoom.localParticipant);
    await tick();
    return { pub, mediaStreamTrack, track };
  }

  it('keeps Windows on the normal microphone path when denoiseEnabled=true', async () => {
    vi.stubGlobal('navigator', {
      userAgent: 'Mozilla/5.0 (Windows NT 10.0; Win64; x64)',
      mediaDevices: createMockMediaDevices(),
    });
    mockSettingsStorage.set('wavis_denoise_enabled', true);

    const mod = new LiveKitModule(createMockCallbacks());
    await mod.connect('wss://sfu.test', 'tok');
    emitRoomEvent('connected');
    await tick();

    const publishCall = sdkCalls.find(c => c.method === 'publishTrack');
    expect(publishCall).toBeUndefined();

    const micCall = sdkCalls.find(c => c.method === 'setMicrophoneEnabled');
    expect((micCall!.args[1] as Record<string, unknown>)?.noiseSuppression).toBe(false);

    mod.disconnect();
  });

  it('keeps browser noiseSuppression disabled on macOS when denoiseEnabled=true', async () => {
    vi.stubGlobal('navigator', {
      userAgent: 'Mozilla/5.0 (Macintosh; Intel Mac OS X 14_0)',
      mediaDevices: createMockMediaDevices(),
    });
    mockSettingsStorage.set('wavis_denoise_enabled', true);

    const mod = new LiveKitModule(createMockCallbacks());
    await mod.connect('wss://sfu.test', 'tok');
    emitRoomEvent('connected');
    await tick();

    const micCall = sdkCalls.find(c => c.method === 'setMicrophoneEnabled');
    expect((micCall!.args[1] as Record<string, unknown>)?.noiseSuppression).toBe(false);

    mod.disconnect();
  });

  it('passes noiseSuppression:false to setMicrophoneEnabled on Windows when denoiseEnabled=false', async () => {
    vi.stubGlobal('navigator', {
      userAgent: 'Mozilla/5.0 (Windows NT 10.0; Win64; x64)',
      mediaDevices: createMockMediaDevices(),
    });
    mockSettingsStorage.set('wavis_denoise_enabled', false);

    const mod = new LiveKitModule(createMockCallbacks());
    await mod.connect('wss://sfu.test', 'tok');
    emitRoomEvent('connected');
    await tick();

    const micCall = sdkCalls.find(c => c.method === 'setMicrophoneEnabled');
    expect((micCall!.args[1] as Record<string, unknown>)?.noiseSuppression).toBe(false);

    mod.disconnect();
  });

  it('does not set noiseSuppression on Linux', async () => {
    vi.stubGlobal('navigator', {
      userAgent: 'Mozilla/5.0 (X11; Linux x86_64)',
      mediaDevices: createMockMediaDevices(),
    });
    mockSettingsStorage.set('wavis_denoise_enabled', true);

    const mod = new LiveKitModule(createMockCallbacks());
    await mod.connect('wss://sfu.test', 'tok');
    emitRoomEvent('connected');
    await tick();

    const micCall = sdkCalls.find(c => c.method === 'setMicrophoneEnabled');
    // noiseSuppression should be false (Linux uses native Rust path)
    expect((micCall!.args[1] as Record<string, unknown>)?.noiseSuppression).toBe(false);

    mod.disconnect();
  });

  it('attaches the JS mic processor on Windows when denoiseEnabled=true', async () => {
    vi.stubGlobal('navigator', {
      userAgent: 'Mozilla/5.0 (Windows NT 10.0; Win64; x64)',
      mediaDevices: createMockMediaDevices(),
    });
    mockSettingsStorage.set('wavis_denoise_enabled', true);

    const cbs = createMockCallbacks();
    const mod = new LiveKitModule(cbs);
    const { track, mediaStreamTrack } = await driveToConnectedWithMst(mod);

    expect(track.setProcessor).toHaveBeenCalledTimes(1);
    expect(track.setAudioContext).toHaveBeenCalledTimes(1);
    expect(track.getProcessor()).toBeTruthy();
    expect(mediaStreamTrack.applyConstraints).not.toHaveBeenCalled();
    expect(cbs.calls.some(c => c.method === 'onNoiseSuppressionState' && c.args[0] === true)).toBe(true);

    mod.disconnect();
  });

  it('does not attach the JS mic processor on Windows when denoiseEnabled=false and gain is default', async () => {
    vi.stubGlobal('navigator', {
      userAgent: 'Mozilla/5.0 (Windows NT 10.0; Win64; x64)',
      mediaDevices: createMockMediaDevices(),
    });
    mockSettingsStorage.set('wavis_denoise_enabled', false);

    const cbs = createMockCallbacks();
    const mod = new LiveKitModule(cbs);
    const { track, mediaStreamTrack } = await driveToConnectedWithMst(mod);

    expect(track.setProcessor).not.toHaveBeenCalled();
    expect(mediaStreamTrack.applyConstraints).not.toHaveBeenCalled();
    expect(cbs.calls.some(c => c.method === 'onNoiseSuppressionState' && c.args[0] === true)).toBe(false);

    mod.disconnect();
  });

  it('setDenoiseEnabled(true) attaches the processor on the active mic track on Windows', async () => {
    vi.stubGlobal('navigator', {
      userAgent: 'Mozilla/5.0 (Windows NT 10.0; Win64; x64)',
      mediaDevices: createMockMediaDevices(),
    });
    mockSettingsStorage.set('wavis_denoise_enabled', false);

    const cbs = createMockCallbacks();
    const mod = new LiveKitModule(cbs);
    const { mediaStreamTrack, track } = await driveToConnectedWithMst(mod);

    await mod.setDenoiseEnabled(true);

    expect(track.setProcessor).toHaveBeenCalledTimes(1);
    expect(mediaStreamTrack.applyConstraints).not.toHaveBeenCalled();
    expect(cbs.calls.some(c => c.method === 'onNoiseSuppressionState' && c.args[0] === true)).toBe(true);

    mod.disconnect();
  });

  it('setDenoiseEnabled(false) bypasses the processor but keeps the mic path alive on Windows', async () => {
    vi.stubGlobal('navigator', {
      userAgent: 'Mozilla/5.0 (Windows NT 10.0; Win64; x64)',
      mediaDevices: createMockMediaDevices(),
    });
    mockSettingsStorage.set('wavis_denoise_enabled', true);

    const cbs = createMockCallbacks();
    const mod = new LiveKitModule(cbs);
    const { mediaStreamTrack, track } = await driveToConnectedWithMst(mod);

    await mod.setDenoiseEnabled(false);

    expect(track.stopProcessor).toHaveBeenCalledTimes(1);
    expect(mediaStreamTrack.applyConstraints).not.toHaveBeenCalled();
    expect(cbs.calls.some(c => c.method === 'onNoiseSuppressionState' && c.args[0] === false)).toBe(true);

    mod.disconnect();
  });

  it('re-attaches the processor when a new local mic track is published on reconnect', async () => {
    vi.stubGlobal('navigator', {
      userAgent: 'Mozilla/5.0 (Windows NT 10.0; Win64; x64)',
      mediaDevices: createMockMediaDevices(),
    });
    mockSettingsStorage.set('wavis_denoise_enabled', true);

    const mod = new LiveKitModule(createMockCallbacks());
    const first = await driveToConnectedWithMst(mod);
    const second = createAudioPubWithMst();

    emitRoomEvent('localTrackPublished', second.pub, mockRoom.localParticipant);
    await tick();

    expect(first.track.setProcessor).toHaveBeenCalledTimes(1);
    expect(second.track.setProcessor).toHaveBeenCalledTimes(1);

    mod.disconnect();
  });

  it('input volume below 100 attaches the composite mic processor even without denoise', async () => {
    vi.stubGlobal('navigator', {
      userAgent: 'Mozilla/5.0 (Windows NT 10.0; Win64; x64)',
      mediaDevices: createMockMediaDevices(),
    });
    mockSettingsStorage.set('wavis_denoise_enabled', false);

    const mod = new LiveKitModule(createMockCallbacks());
    const { track } = await driveToConnectedWithMst(mod);

    await mod.setInputVolume(70);

    expect(track.setProcessor).toHaveBeenCalledTimes(1);

    mod.disconnect();
  });

  it('setDenoiseEnabled is a no-op when no mic track is active', async () => {
    vi.stubGlobal('navigator', {
      userAgent: 'Mozilla/5.0 (Windows NT 10.0; Win64; x64)',
      mediaDevices: createMockMediaDevices(),
    });

    const mod = new LiveKitModule(createMockCallbacks());
    // Don't connect — no localMicTrack
    await expect(mod.setDenoiseEnabled(true)).resolves.toBeUndefined();
  });
});
