import { beforeEach, describe, expect, it, vi } from 'vitest';

type MockAudioContext = {
  state: string;
  currentTime: number;
  sampleRate: number;
  destination: object;
  audioWorklet: { addModule: ReturnType<typeof vi.fn> };
  createGain: ReturnType<typeof vi.fn>;
  createMediaStreamDestination: ReturnType<typeof vi.fn>;
  close: ReturnType<typeof vi.fn>;
  resume: ReturnType<typeof vi.fn>;
};

const audioContexts: MockAudioContext[] = [];
const workletContexts: MockAudioContext[] = [];
const tauriListeners: Array<{ event: string; unlisten: ReturnType<typeof vi.fn> }> = [];

let mockRoom: {
  localParticipant: {
    publishTrack: ReturnType<typeof vi.fn>;
    unpublishTrack: ReturnType<typeof vi.fn>;
    trackPublications: Map<string, { track: MediaStreamTrack; source: string; stream?: string }>;
  };
};

vi.mock('livekit-client', () => ({
  Room: vi.fn(),
  RoomEvent: {},
  Track: {
    Source: {
      ScreenShare: 'screen_share',
      ScreenShareAudio: 'screen_share_audio',
    },
  },
  VideoPreset: vi.fn((opts: unknown) => opts),
}));

vi.mock('@tauri-apps/api/core', () => ({
  invoke: vi.fn(async () => undefined),
}));

vi.mock('@tauri-apps/api/event', () => ({
  listen: vi.fn(async (event: string) => {
    const unlisten = vi.fn();
    tauriListeners.push({ event, unlisten });
    return unlisten;
  }),
}));

vi.mock('@features/settings/settings-store', () => ({
  getAudioOutputDevice: vi.fn(async () => null),
  setStoreValue: vi.fn(async () => undefined),
  STORE_KEYS: { AUDIO_OUTPUT_DEVICE_ID: 'audio-output-device-id' },
}));

function createMockAudioContext(options?: { sampleRate?: number }): MockAudioContext {
  const ctx: MockAudioContext = {
    state: 'running',
    currentTime: 0,
    sampleRate: options?.sampleRate ?? 44_100,
    destination: {},
    audioWorklet: {
      addModule: vi.fn(async () => undefined),
    },
    createGain: vi.fn(() => {
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
    }),
    createMediaStreamDestination: vi.fn(() => ({
      stream: {
        getAudioTracks: () => [{ kind: 'audio', id: 'wasapi-track' }],
      },
      disconnect: vi.fn(),
    })),
    close: vi.fn(async () => {
      ctx.state = 'closed';
    }),
    resume: vi.fn(async () => undefined),
  };
  audioContexts.push(ctx);
  return ctx;
}

function createCallbacks() {
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
  vi.resetModules();
  vi.clearAllMocks();
  audioContexts.length = 0;
  workletContexts.length = 0;
  tauriListeners.length = 0;

  class MediaDevicesMock {
    addEventListener = vi.fn();
    removeEventListener = vi.fn();
    async enumerateDevices() {
      return [];
    }
  }

  vi.stubGlobal('MediaDevices', MediaDevicesMock);
  vi.stubGlobal('navigator', {
    userAgent: 'Mozilla/5.0 (Windows NT 10.0; Win64; x64)',
    mediaDevices: new MediaDevicesMock(),
  });
  vi.stubGlobal('document', {
    addEventListener: vi.fn(),
    removeEventListener: vi.fn(),
    createElement: vi.fn((tag: string) => ({ tagName: tag })),
    body: { appendChild: vi.fn() },
  });
  vi.stubGlobal('MediaStream', function MediaStreamMock(this: { tracks?: MediaStreamTrack[] }, tracks?: MediaStreamTrack[]) {
    this.tracks = tracks ?? [];
    return this;
  });
  function AudioContextMock(this: MockAudioContext, options?: { sampleRate?: number }) {
    return createMockAudioContext(options);
  }

  function AudioWorkletNodeMock(this: object, ctx: MockAudioContext) {
    workletContexts.push(ctx);
    return {
      port: { postMessage: vi.fn() },
      connect: vi.fn(),
      disconnect: vi.fn(),
    };
  }

  vi.stubGlobal('AudioContext', vi.fn(AudioContextMock));
  vi.stubGlobal('AudioWorkletNode', vi.fn(AudioWorkletNodeMock));

  const trackPublications = new Map<string, { track: MediaStreamTrack; source: string; stream?: string }>();
  mockRoom = {
    localParticipant: {
      publishTrack: vi.fn(async (track: MediaStreamTrack, opts: { source: string; stream?: string }) => {
        const publication = { track, source: opts.source, stream: opts.stream };
        trackPublications.set('screen-share-audio', publication);
        return publication;
      }),
      unpublishTrack: vi.fn(async (track: MediaStreamTrack) => {
        for (const [key, publication] of trackPublications.entries()) {
          if (publication.track === track) trackPublications.delete(key);
        }
      }),
      trackPublications,
    },
  };
});

describe('LiveKitModule WASAPI audio isolation', () => {
  it('uses a dedicated AudioContext for the WASAPI bridge', async () => {
    const { LiveKitModule } = await import('../livekit-media');

    const mod = new LiveKitModule(createCallbacks());
    const mainAudioContext = (mod as unknown as { ensureAudioContext: () => MockAudioContext }).ensureAudioContext();
    (mod as unknown as { room: typeof mockRoom }).room = mockRoom;

    await mod.startWasapiAudioBridge();

    expect(audioContexts).toHaveLength(2);
    expect(workletContexts).toHaveLength(1);
    expect(workletContexts[0]).not.toBe(mainAudioContext);
    expect((mod as unknown as { wasapiAudioCtx: MockAudioContext | null }).wasapiAudioCtx).toBe(workletContexts[0]);
  });

  it('publishes synthetic screen share audio with a stable logical stream name', async () => {
    const { LiveKitModule } = await import('../livekit-media');

    const mod = new LiveKitModule(createCallbacks());
    (mod as unknown as { room: typeof mockRoom }).room = mockRoom;

    await mod.startWasapiAudioBridge();

    expect(mockRoom.localParticipant.publishTrack).toHaveBeenCalledTimes(1);
    expect(mockRoom.localParticipant.publishTrack).toHaveBeenCalledWith(
      expect.anything(),
      expect.objectContaining({
        source: 'screen_share_audio',
        stream: 'screen_share',
      }),
    );
  });

  it('closes the dedicated AudioContext when the WASAPI bridge stops', async () => {
    const { LiveKitModule } = await import('../livekit-media');

    const mod = new LiveKitModule(createCallbacks());
    const mainAudioContext = (mod as unknown as { ensureAudioContext: () => MockAudioContext }).ensureAudioContext();
    (mod as unknown as { room: typeof mockRoom }).room = mockRoom;

    await mod.startWasapiAudioBridge();

    const wasapiAudioContext = (mod as unknown as { wasapiAudioCtx: MockAudioContext | null }).wasapiAudioCtx;
    await mod.stopWasapiAudioBridge();

    expect(wasapiAudioContext).not.toBeNull();
    expect(wasapiAudioContext?.close).toHaveBeenCalledTimes(1);
    expect(mainAudioContext.close).not.toHaveBeenCalled();
    expect((mod as unknown as { wasapiAudioCtx: MockAudioContext | null }).wasapiAudioCtx).toBeNull();
  });

  it('closes the dedicated AudioContext during disconnect()', async () => {
    const { LiveKitModule } = await import('../livekit-media');

    const mod = new LiveKitModule(createCallbacks());
    const mainAudioContext = (mod as unknown as { ensureAudioContext: () => MockAudioContext }).ensureAudioContext();
    (mod as unknown as { room: typeof mockRoom; disposed: boolean }).room = {
      ...mockRoom,
      disconnect: vi.fn(),
      off: vi.fn(),
    } as typeof mockRoom & { disconnect: ReturnType<typeof vi.fn>; off: ReturnType<typeof vi.fn> };

    await mod.startWasapiAudioBridge();

    const wasapiAudioContext = (mod as unknown as { wasapiAudioCtx: MockAudioContext | null }).wasapiAudioCtx;
    mod.disconnect();

    expect(wasapiAudioContext).not.toBeNull();
    expect(wasapiAudioContext?.close).toHaveBeenCalledTimes(1);
    expect(mainAudioContext.close).toHaveBeenCalledTimes(1);
    expect((mod as unknown as { wasapiAudioCtx: MockAudioContext | null }).wasapiAudioCtx).toBeNull();
  });
});
