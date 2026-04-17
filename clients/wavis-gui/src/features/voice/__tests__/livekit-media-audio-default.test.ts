import { beforeEach, describe, expect, it, vi } from 'vitest';

let sdkCalls: Array<{ method: string; args: unknown[] }> = [];
let roomEventHandlers = new Map<string, Array<(...args: unknown[]) => void>>();
let invokeCalls: Array<{ cmd: string; args?: unknown }> = [];
let mockRoom: ReturnType<typeof createMockRoom>;

function createMockLocalParticipant() {
  return {
    setScreenShareEnabled: vi.fn(async (enabled: boolean, captureOpts?: unknown, publishOpts?: unknown) => {
      sdkCalls.push({ method: 'setScreenShareEnabled', args: [enabled, captureOpts, publishOpts] });
      return enabled;
    }),
    setMicrophoneEnabled: vi.fn(async () => {}),
    getTrackPublication: vi.fn(() => undefined),
    trackPublications: new Map(),
    connectionQuality: 'excellent' as unknown,
    identity: 'self',
  };
}

function createMockRoom() {
  const localParticipant = createMockLocalParticipant();
  return {
    connect: vi.fn(async () => {}),
    disconnect: vi.fn(() => {}),
    on: vi.fn((event: string, handler: (...args: unknown[]) => void) => {
      if (!roomEventHandlers.has(event)) roomEventHandlers.set(event, []);
      roomEventHandlers.get(event)!.push(handler);
    }),
    off: vi.fn((event: string, handler: (...args: unknown[]) => void) => {
      const handlers = roomEventHandlers.get(event) ?? [];
      roomEventHandlers.set(event, handlers.filter((candidate) => candidate !== handler));
    }),
    localParticipant,
    switchActiveDevice: vi.fn(async () => {}),
  };
}

function emitRoomEvent(event: string, ...args: unknown[]) {
  const handlers = roomEventHandlers.get(event) ?? [];
  for (const handler of handlers) handler(...args);
}

async function driveToConnected(mod: LiveKitModule) {
  await mod.connect('wss://sfu.test', 'tok');
  emitRoomEvent('connected');
  await new Promise<void>((resolve) => setTimeout(resolve, 0));
  emitRoomEvent('localTrackPublished', { track: { kind: 'audio' }, source: 'microphone' }, mockRoom.localParticipant);
}

vi.mock('livekit-client', () => ({
  Room: vi.fn(() => mockRoom),
  VideoPreset: vi.fn(function (opts: { width: number; height: number; maxBitrate: number }) {
    return { width: opts.width, height: opts.height, maxBitrate: opts.maxBitrate };
  }),
  RoomEvent: {
    Connected: 'connected',
    Disconnected: 'disconnected',
    Reconnecting: 'reconnecting',
    Reconnected: 'reconnected',
    TrackSubscribed: 'trackSubscribed',
    TrackUnsubscribed: 'trackUnsubscribed',
    ActiveSpeakersChanged: 'activeSpeakersChanged',
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
}));

vi.mock('@tauri-apps/api/core', () => ({
  invoke: vi.fn(async (cmd: string, args?: unknown) => {
    invokeCalls.push({ cmd, args });
    return { loopback_exclusion_available: true };
  }),
}));

vi.mock('@tauri-apps/api/event', () => ({
  listen: vi.fn(async () => () => {}),
}));

vi.mock('@features/settings/settings-store', () => ({
  getAudioOutputDevice: vi.fn(async () => null),
  setStoreValue: vi.fn(async () => {}),
  STORE_KEYS: {},
}));

vi.stubGlobal('AudioContext', function AudioContextMock(this: Record<string, unknown>) {
  this.state = 'running';
  this.currentTime = 0;
  this.destination = {};
  this.createGain = vi.fn(() => {
    const node = {
      gain: {
        value: 1,
        setValueAtTime: vi.fn((value: number) => {
          node.gain.value = value;
        }),
      },
      connect: vi.fn(),
      disconnect: vi.fn(),
    };
    return node;
  });
  this.createAnalyser = vi.fn(() => ({
    fftSize: 2048,
    connect: vi.fn(),
    disconnect: vi.fn(),
    getFloatTimeDomainData: vi.fn(),
  }));
  this.createMediaStreamSource = vi.fn(() => ({ connect: vi.fn(), disconnect: vi.fn() }));
  this.close = vi.fn(async () => {});
  this.resume = vi.fn(async () => {});
  return this;
});

vi.stubGlobal('document', {
  createElement: vi.fn(() => ({ pause: vi.fn(), remove: vi.fn(), srcObject: null, muted: false, autoplay: false })),
  body: { appendChild: vi.fn((node: unknown) => node) },
  addEventListener: vi.fn(),
  removeEventListener: vi.fn(),
});

vi.stubGlobal('navigator', {
  userAgent: '',
  mediaDevices: {
    addEventListener: vi.fn(),
    removeEventListener: vi.fn(),
    enumerateDevices: vi.fn(async () => []),
  },
});

vi.stubGlobal('MediaStream', function MediaStreamMock(this: Record<string, unknown>) {
  this.getTracks = () => [];
  return this;
});

import { DEFAULT_CAPTURE_PROFILE, LiveKitModule, type MediaCallbacks } from '../livekit-media';

function createMockCallbacks(): MediaCallbacks {
  return {
    onMediaConnected: vi.fn(),
    onMediaFailed: vi.fn(),
    onMediaDisconnected: vi.fn(),
    onAudioLevels: vi.fn(),
    onLocalAudioLevel: vi.fn(),
    onActiveSpeakers: vi.fn(),
    onConnectionQuality: vi.fn(),
    onScreenShareSubscribed: vi.fn(),
    onScreenShareUnsubscribed: vi.fn(),
    onLocalScreenShareEnded: vi.fn(),
    onParticipantMuteChanged: vi.fn(),
    onSystemEvent: vi.fn(),
  };
}

beforeEach(() => {
  sdkCalls = [];
  roomEventHandlers = new Map();
  invokeCalls = [];
  mockRoom = createMockRoom();
  vi.stubGlobal('navigator', {
    userAgent: '',
    mediaDevices: {
      addEventListener: vi.fn(),
      removeEventListener: vi.fn(),
      enumerateDevices: vi.fn(async () => []),
    },
  });
});

describe('screen share audio defaults', () => {
  it('DEFAULT_CAPTURE_PROFILE starts with audio disabled', () => {
    expect(DEFAULT_CAPTURE_PROFILE.audio).toBe(false);
  });

  it('startScreenShare does not auto-start WASAPI when audio is disabled by default', async () => {
    vi.stubGlobal('navigator', {
      userAgent: 'Mozilla/5.0 (Windows NT 10.0; Win64; x64)',
      mediaDevices: {
        addEventListener: vi.fn(),
        removeEventListener: vi.fn(),
        enumerateDevices: vi.fn(async () => []),
      },
    });

    const mod = new LiveKitModule(createMockCallbacks());
    await driveToConnected(mod);

    await mod.startScreenShare();

    const audioStartCalls = invokeCalls.filter((call) => call.cmd === 'audio_share_start');
    expect(audioStartCalls).toHaveLength(0);
  });
});
