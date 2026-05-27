import { vi } from 'vitest';

export type MockCameraOperation =
  | { type: 'publish'; tier: string; deviceId: string | null }
  | { type: 'setQuality'; tier: string }
  | { type: 'replaceDevice'; deviceId: string | null }
  | { type: 'unpublish' };

export type MockVoiceRoomCameraLiveKitModule = {
  callbacks: Record<string, (...args: unknown[]) => void>;
  connectCalls: Array<{ sfuUrl: string; token: string }>;
  disconnectCalls: number;
  publishCameraCalls: Array<{ deviceId: string | null; quality: { tier: string } }>;
  unpublishCameraCalls: number;
  setCameraQualityCalls: Array<{ tier: string }>;
  replaceCameraDeviceCalls: Array<string | null>;
  cameraOperationLog: MockCameraOperation[];
  setPublishingLayersCalls: number;
  localCameraTrack: MediaStreamTrack | null;
  connect: (sfuUrl: string, token: string) => Promise<void>;
  disconnect: () => void;
  setMicEnabled: (enabled: boolean) => Promise<void>;
  publishCamera: (opts: {
    deviceId: string | null;
    quality: { tier: string };
  }) => Promise<{ trackId: string }>;
  unpublishCamera: () => Promise<void>;
  setCameraQuality: (quality: { tier: string }) => Promise<void>;
  replaceCameraDevice: (deviceId: string | null) => Promise<{ trackId: string }>;
  getLocalCameraTrack: () => MediaStreamTrack | null;
  applyRemoteCameraVisibility: (visibleParticipantIds: ReadonlySet<string>) => void;
  setParticipantVolume: (id: string, vol: number) => void;
  setMasterVolume: (vol: number) => void;
  setScreenShareAudioVolume: (id: string, vol: number) => void;
  attachScreenShareAudio: (id: string) => void;
  detachScreenShareAudio: (id: string) => void;
  startWasapiAudioBridge: (loopbackExclusionAvailable: boolean) => Promise<void>;
  stopWasapiAudioBridge: () => Promise<void>;
  startScreenShare: () => Promise<boolean>;
  stopScreenShare: () => Promise<void>;
  getActiveScreenShares: () => Array<{ identity: string; stream: MediaStream; startedAtMs: number }>;
};

type HarnessState = {
  lastLiveKitModule: MockVoiceRoomCameraLiveKitModule | null;
  messageHandler: ((message: unknown) => void) | null;
  statusHandler: ((status: string) => void) | null;
  sentMessages: Array<Record<string, unknown>>;
  videoInputDevice: string | null;
  enumeratedVideoDevices: string[];
  publishCameraImpl:
    | ((module: MockVoiceRoomCameraLiveKitModule, opts: {
      deviceId: string | null;
      quality: { tier: string };
    }) => Promise<{ trackId: string }>)
    | null;
  setCameraQualityImpl:
    | ((module: MockVoiceRoomCameraLiveKitModule, quality: {
      tier: string;
    }) => Promise<void>)
    | null;
  replaceCameraDeviceImpl:
    | ((module: MockVoiceRoomCameraLiveKitModule, deviceId: string | null) => Promise<{ trackId: string }>)
    | null;
};

export type VoiceRoomCameraHarness = {
  state: HarnessState;
  voiceRoom: typeof import('../voice-room');
  driveToActive: () => Promise<void>;
  connectMedia: () => Promise<void>;
  emitMessage: (message: unknown) => void;
  cleanup: () => void;
  tick: () => Promise<void>;
};

function createMockLiveKitModule(
  state: HarnessState,
  callbacks: Record<string, (...args: unknown[]) => void>,
): MockVoiceRoomCameraLiveKitModule {
  const module: MockVoiceRoomCameraLiveKitModule = {
    callbacks,
    connectCalls: [],
    disconnectCalls: 0,
    publishCameraCalls: [],
    unpublishCameraCalls: 0,
    setCameraQualityCalls: [],
    replaceCameraDeviceCalls: [],
    cameraOperationLog: [],
    setPublishingLayersCalls: 0,
    localCameraTrack: null,
    connect: vi.fn(async (sfuUrl: string, token: string) => {
      module.connectCalls.push({ sfuUrl, token });
    }),
    disconnect: vi.fn(() => {
      module.disconnectCalls += 1;
    }),
    setMicEnabled: vi.fn(async () => {}),
    publishCamera: vi.fn(async (opts) => {
      module.publishCameraCalls.push(opts);
      module.cameraOperationLog.push({
        type: 'publish',
        tier: opts.quality.tier,
        deviceId: opts.deviceId,
      });
      if (state.publishCameraImpl) {
        return state.publishCameraImpl(module, opts);
      }

      module.localCameraTrack = {
        id: `camera-${opts.deviceId ?? 'default'}`,
        kind: 'video',
        stop: vi.fn(),
      } as unknown as MediaStreamTrack;
      return { trackId: module.localCameraTrack.id };
    }),
    unpublishCamera: vi.fn(async () => {
      module.unpublishCameraCalls += 1;
      module.cameraOperationLog.push({ type: 'unpublish' });
      module.localCameraTrack?.stop();
      module.localCameraTrack = null;
    }),
    setCameraQuality: vi.fn(async (quality: { tier: string }) => {
      module.setCameraQualityCalls.push(quality);
      module.cameraOperationLog.push({ type: 'setQuality', tier: quality.tier });
      if (state.setCameraQualityImpl) {
        await state.setCameraQualityImpl(module, quality);
      }
    }),
    replaceCameraDevice: vi.fn(async (deviceId: string | null) => {
      module.replaceCameraDeviceCalls.push(deviceId);
      module.cameraOperationLog.push({ type: 'replaceDevice', deviceId });
      if (state.replaceCameraDeviceImpl) {
        return state.replaceCameraDeviceImpl(module, deviceId);
      }

      module.localCameraTrack = {
        id: `camera-${deviceId ?? 'default'}`,
        kind: 'video',
        stop: vi.fn(),
      } as unknown as MediaStreamTrack;
      return { trackId: module.localCameraTrack.id };
    }),
    getLocalCameraTrack: vi.fn(() => module.localCameraTrack),
    applyRemoteCameraVisibility: vi.fn(() => {}),
    setParticipantVolume: vi.fn(() => {}),
    setMasterVolume: vi.fn(() => {}),
    setScreenShareAudioVolume: vi.fn(() => {}),
    attachScreenShareAudio: vi.fn(() => {}),
    detachScreenShareAudio: vi.fn(() => {}),
    startWasapiAudioBridge: vi.fn(async () => {}),
    stopWasapiAudioBridge: vi.fn(async () => {}),
    startScreenShare: vi.fn(async () => true),
    stopScreenShare: vi.fn(async () => {}),
    getActiveScreenShares: vi.fn(() => []),
  };

  return module;
}

export async function setupVoiceRoomCameraHarness(): Promise<VoiceRoomCameraHarness> {
  const state: HarnessState = {
    lastLiveKitModule: null,
    messageHandler: null,
    statusHandler: null,
    sentMessages: [],
    videoInputDevice: 'camera-1',
    enumeratedVideoDevices: ['camera-1'],
    publishCameraImpl: null,
    setCameraQualityImpl: null,
    replaceCameraDeviceImpl: null,
  };

  vi.resetModules();

  vi.doMock('../livekit-media', () => ({
    LiveKitModule: vi.fn(function (
      this: Record<string, unknown>,
      callbacks: Record<string, (...args: unknown[]) => void>,
    ) {
      const module = createMockLiveKitModule(state, callbacks);
      state.lastLiveKitModule = module;
      Object.assign(this as Record<string, unknown>, module);
      return this;
    }),
  }));

  vi.doMock('../native-media', () => ({
    NativeMediaModule: vi.fn(function (
      this: Record<string, unknown>,
      callbacks: Record<string, (...args: unknown[]) => void>,
    ) {
      const module = createMockLiveKitModule(state, callbacks);
      state.lastLiveKitModule = module;
      Object.assign(this as Record<string, unknown>, module);
      return this;
    }),
  }));

  vi.doMock('@shared/websocket', () => ({
    SignalingClient: vi.fn(function (this: Record<string, unknown>) {
      this.status = 'disconnected';
      this.send = vi.fn((message: Record<string, unknown>) => {
        state.sentMessages.push(message);
      });
      this.onMessage = vi.fn((handler: (message: unknown) => void) => {
        state.messageHandler = handler;
        return () => {
          state.messageHandler = null;
        };
      });
      this.onStatusChange = vi.fn((handler: (status: string) => void) => {
        state.statusHandler = handler;
        return () => {
          state.statusHandler = null;
        };
      });
      this.connectWithAuth = vi.fn(async () => {
        this.status = 'connected';
      });
      this.disconnect = vi.fn(() => {
        this.status = 'disconnected';
      });
      return this;
    }),
  }));

  vi.doMock('@features/auth/auth', () => ({
    getServerUrl: vi.fn(async () => 'https://test.wavis.dev'),
    getDisplayName: vi.fn(async () => 'TestUser'),
    getAccessToken: vi.fn(async () => 'mock-token'),
    isTokenExpired: vi.fn(async () => false),
    refreshTokens: vi.fn(async () => true),
    onTokensRefreshed: vi.fn(() => () => {}),
  }));

  vi.doMock('@shared/helpers', () => ({
    toWsUrl: vi.fn((url: string) => url.replace('https://', 'wss://') + '/ws'),
  }));

  vi.doMock('../audio-devices', () => ({
    setActiveLiveKitModule: vi.fn(),
  }));

  vi.doMock('@features/settings/settings-store', () => ({
    DEFAULT_PASSTHROUGH_VOLUME: 20,
    getDefaultVolume: vi.fn(async () => 70),
    getReconnectConfig: vi.fn(async () => ({
      strategy: 'exponential' as const,
      baseDelayMs: 1_000,
      maxDelayMs: 30_000,
      maxRetries: 10,
    })),
    getMuteHotkey: vi.fn(async () => 'Ctrl+Shift+M'),
    getProfileColor: vi.fn(async () => '#E06C75'),
    getChannelVolumes: vi.fn(async () => null),
    getWindowsSharePath: vi.fn(async () => 'browser'),
    getVideoInputDevice: vi.fn(async () => state.videoInputDevice),
    setVideoInputDevice: vi.fn(async (deviceId: string | null) => {
      state.videoInputDevice = deviceId;
    }),
    getNotificationVolume: vi.fn(async () => 100),
    getSoundVolumes: vi.fn(async () => ({})),
  }));

  vi.doMock('@shared/hotkey-bridge', () => ({
    registerMuteHotkey: vi.fn(async () => {}),
    unregisterMuteHotkey: vi.fn(async () => {}),
  }));

  vi.doMock('../notification-sounds', () => ({
    playNotificationSound: vi.fn(async () => {}),
  }));

  vi.doMock('@tauri-apps/api/core', () => ({
    invoke: vi.fn(async () => ({})),
  }));

  vi.doMock('@tauri-apps/api/event', () => ({
    listen: vi.fn(async () => () => {}),
    emit: vi.fn(async () => {}),
  }));

  vi.doMock('@tauri-apps/api/webviewWindow', () => ({
    WebviewWindow: Object.assign(vi.fn(), {
      getByLabel: vi.fn(async () => null),
    }),
  }));

  vi.stubGlobal('window', {});
  vi.stubGlobal('navigator', {
    userAgent: 'Mozilla/5.0 (Windows NT 10.0; Win64; x64)',
    mediaDevices: {
      enumerateDevices: vi.fn(async () =>
        state.enumeratedVideoDevices.map((deviceId, index) => ({
          kind: 'videoinput',
          deviceId,
          label: `Camera ${index + 1}`,
        })),
      ),
    },
  });

  const voiceRoom = await import('../voice-room');

  const tick = async (): Promise<void> => {
    await Promise.resolve();
    await Promise.resolve();
  };

  const emitMessage = (message: unknown): void => {
    state.messageHandler?.(message);
  };

  const connectMedia = async (): Promise<void> => {
    emitMessage({ type: 'media_token', sfuUrl: 'wss://sfu', token: 'token-1' });
    await tick();
    state.lastLiveKitModule?.callbacks.onMediaConnected?.();
    await tick();
  };

  const driveToActive = async (): Promise<void> => {
    voiceRoom.initSession('channel-1', 'Test Room', 'owner', () => {});
    await tick();
    emitMessage({ type: 'auth_success' });
    emitMessage({
      type: 'joined',
      peerId: 'self-peer',
      roomId: 'room-1',
      participants: [
        { participantId: 'self-peer', displayName: 'TestUser', userId: 'user-1' },
        { participantId: 'peer-2', displayName: 'Alice', userId: 'user-2' },
      ],
    });
    emitMessage({
      type: 'sub_room_state',
      rooms: [
        {
          subRoomId: 'room-1',
          roomNumber: 1,
          isDefault: true,
          participantIds: ['self-peer', 'peer-2'],
        },
      ],
    });
    await tick();
  };

  const cleanup = (): void => {
    try {
      voiceRoom.leaveRoom();
    } catch {
      // No-op.
    }
    vi.unstubAllGlobals();
  };

  return {
    state,
    voiceRoom,
    driveToActive,
    connectMedia,
    emitMessage,
    cleanup,
    tick,
  };
}
