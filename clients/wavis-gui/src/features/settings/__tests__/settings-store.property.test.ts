import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';
import * as fc from 'fast-check';

const mockStorage = new Map<string, unknown>();

let lastLiveKitModule: MockLiveKitModule | null = null;
let messageHandler: ((message: unknown) => void) | null = null;
let replaceCameraDeviceImpl: ((deviceId: string | null) => Promise<{ trackId: string }>) | null =
  null;

interface MockLiveKitModule {
  callbacks: Record<string, (...args: unknown[]) => void>;
  publishCameraCalls: Array<{ deviceId: string | null; quality: { tier: string } }>;
  replaceCameraDeviceCalls: Array<string | null>;
  unpublishCameraCalls: number;
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
  setParticipantVolume: (id: string, vol: number) => void;
  setMasterVolume: (vol: number) => void;
  setParticipantPassthrough: (id: string, enabled: boolean) => void;
  setPassthroughFilterSettings: (settings: { enabled: boolean; strength: number }) => void;
  setScreenShareAudioVolume: (id: string, vol: number) => void;
  attachScreenShareAudio: (id: string) => void;
  detachScreenShareAudio: (id: string) => void;
  startWasapiAudioBridge: (loopbackExclusionAvailable: boolean) => Promise<void>;
  stopWasapiAudioBridge: () => Promise<void>;
  startScreenShare: () => Promise<boolean>;
  stopScreenShare: () => Promise<void>;
  getActiveScreenShares: () => Array<{
    identity: string;
    stream: MediaStream;
    startedAtMs: number;
  }>;
  applyRemoteCameraVisibility: (visibleParticipantIds: ReadonlySet<string>) => void;
}

function createMockLiveKitModule(
  callbacks: Record<string, (...args: unknown[]) => void>,
): MockLiveKitModule {
  const module: MockLiveKitModule = {
    callbacks,
    publishCameraCalls: [],
    replaceCameraDeviceCalls: [],
    unpublishCameraCalls: 0,
    localCameraTrack: null,
    connect: vi.fn(async () => {}),
    disconnect: vi.fn(() => {}),
    setMicEnabled: vi.fn(async () => {}),
    publishCamera: vi.fn(async (opts) => {
      module.publishCameraCalls.push(opts);
      const trackId = `camera-${opts.deviceId ?? 'default'}`;
      module.localCameraTrack = {
        id: trackId,
        kind: 'video',
      } as MediaStreamTrack;
      return { trackId };
    }),
    unpublishCamera: vi.fn(async () => {
      module.unpublishCameraCalls += 1;
      module.localCameraTrack = null;
    }),
    setCameraQuality: vi.fn(async () => {}),
    replaceCameraDevice: vi.fn(async (deviceId: string | null) => {
      module.replaceCameraDeviceCalls.push(deviceId);
      if (replaceCameraDeviceImpl) {
        return replaceCameraDeviceImpl(deviceId);
      }
      const trackId = `camera-${deviceId ?? 'default'}`;
      module.localCameraTrack = {
        id: trackId,
        kind: 'video',
      } as MediaStreamTrack;
      return { trackId };
    }),
    getLocalCameraTrack: vi.fn(() => module.localCameraTrack),
    setParticipantVolume: vi.fn(() => {}),
    setMasterVolume: vi.fn(() => {}),
    setParticipantPassthrough: vi.fn(() => {}),
    setPassthroughFilterSettings: vi.fn(() => {}),
    setScreenShareAudioVolume: vi.fn(() => {}),
    attachScreenShareAudio: vi.fn(() => {}),
    detachScreenShareAudio: vi.fn(() => {}),
    startWasapiAudioBridge: vi.fn(async () => {}),
    stopWasapiAudioBridge: vi.fn(async () => {}),
    startScreenShare: vi.fn(async () => true),
    stopScreenShare: vi.fn(async () => {}),
    getActiveScreenShares: vi.fn(() => []),
    applyRemoteCameraVisibility: vi.fn(() => {}),
  };

  return module;
}

vi.mock('@tauri-apps/plugin-store', () => ({
  load: vi.fn().mockResolvedValue({
    get: vi.fn(async <T>(key: string): Promise<T | undefined> => {
      return mockStorage.get(key) as T | undefined;
    }),
    set: vi.fn(async (key: string, value: unknown): Promise<void> => {
      mockStorage.set(key, value);
    }),
    save: vi.fn(async (): Promise<void> => {}),
  }),
}));

vi.mock('../../voice/livekit-media', () => ({
  LiveKitModule: vi.fn(function (
    this: Record<string, unknown>,
    callbacks: Record<string, (...args: unknown[]) => void>,
  ) {
    const module = createMockLiveKitModule(callbacks);
    lastLiveKitModule = module;
    Object.assign(this as Record<string, unknown>, module);
    return this;
  }),
}));

vi.mock('../../voice/native-media', () => ({
  NativeMediaModule: vi.fn(function (
    this: Record<string, unknown>,
    callbacks: Record<string, (...args: unknown[]) => void>,
  ) {
    const module = createMockLiveKitModule(callbacks);
    lastLiveKitModule = module;
    Object.assign(this as Record<string, unknown>, module);
    return this;
  }),
}));

vi.mock('@shared/websocket', () => ({
  SignalingClient: vi.fn(function (this: Record<string, unknown>) {
    this.status = 'disconnected';
    this.send = vi.fn();
    this.onMessage = vi.fn((handler: (message: unknown) => void) => {
      messageHandler = handler;
      return () => {
        messageHandler = null;
      };
    });
    this.onStatusChange = vi.fn(() => () => {});
    this.connectWithAuth = vi.fn(async () => {
      this.status = 'connected';
    });
    this.disconnect = vi.fn(() => {
      this.status = 'disconnected';
    });
    return this;
  }),
}));

vi.mock('@features/auth/auth', () => ({
  getServerUrl: vi.fn(async () => 'https://test.wavis.dev'),
  getDisplayName: vi.fn(async () => 'TestUser'),
  getUsername: vi.fn(async () => 'TestUser'),
  updateUsername: vi.fn(async () => undefined),
  getAccessToken: vi.fn(async () => 'mock-token'),
  isTokenExpired: vi.fn(async () => false),
  refreshTokens: vi.fn(async () => true),
  onTokensRefreshed: vi.fn(() => () => {}),
}));

vi.mock('@shared/helpers', () => ({
  toWsUrl: vi.fn((url: string) => url.replace('https://', 'wss://') + '/ws'),
}));

vi.mock('../../voice/audio-devices', () => ({
  setActiveLiveKitModule: vi.fn(),
}));

vi.mock('../../voice/notification-sounds', () => ({
  playNotificationSound: vi.fn(async () => {}),
  updateCachedNotificationVolume: vi.fn(),
  updateCachedSoundVolumes: vi.fn(),
  prewarmAudioContext: vi.fn(),
}));

vi.mock('@shared/hotkey-bridge', () => ({
  registerMuteHotkey: vi.fn(async () => {}),
  unregisterMuteHotkey: vi.fn(async () => {}),
}));

vi.mock('@tauri-apps/api/core', () => ({
  invoke: vi.fn(async () => ({})),
}));

vi.mock('@tauri-apps/api/event', () => ({
  listen: vi.fn(async () => () => {}),
  emit: vi.fn(async () => {}),
}));

vi.mock('@tauri-apps/api/webviewWindow', () => ({
  WebviewWindow: Object.assign(vi.fn(), {
    getByLabel: vi.fn(async () => null),
  }),
}));

vi.mock('@features/settings/settings-store', async () => {
  return vi.importActual('../settings-store');
});

import { getVideoInputDevice, setVideoInputDevice } from '../settings-store';
import {
  changeSelectedCamera,
  getState,
  initSession,
  leaveRoom,
  toggleCameraIntent,
} from '../../voice/voice-room';

const tick = () => new Promise<void>((resolve) => setTimeout(resolve, 0));

function resetHarness(): void {
  mockStorage.clear();
  lastLiveKitModule = null;
  messageHandler = null;
  replaceCameraDeviceImpl = null;
}

async function resetSession(): Promise<void> {
  leaveRoom();
  await tick();
  resetHarness();
}

function setEnumeratedVideoDevices(deviceIds: string[]): void {
  vi.stubGlobal('window', {});
  vi.stubGlobal('navigator', {
    userAgent: 'Mozilla/5.0 (Windows NT 10.0; Win64; x64)',
    mediaDevices: {
      enumerateDevices: vi.fn(async () =>
        deviceIds.map((deviceId, index) => ({
          kind: 'videoinput',
          deviceId,
          label: `Camera ${index + 1}`,
        })),
      ),
    },
  });
}

async function driveToConnectedRoom(): Promise<void> {
  initSession('channel-1', 'Test Room', 'owner', () => {});
  await tick();

  messageHandler?.({ type: 'auth_success' });
  messageHandler?.({
    type: 'joined',
    peerId: 'self-peer',
    roomId: 'room-1',
    participants: [
      { participantId: 'self-peer', displayName: 'TestUser', userId: 'user-1' },
      { participantId: 'peer-2', displayName: 'Alice', userId: 'user-2' },
    ],
  });
  messageHandler?.({
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

  messageHandler?.({ type: 'media_token', sfuUrl: 'wss://sfu', token: 'token-1' });
  await tick();
  lastLiveKitModule?.callbacks.onMediaConnected?.();
  await tick();
}

async function publishLocalCamera(deviceId: string): Promise<string> {
  await setVideoInputDevice(deviceId);
  await driveToConnectedRoom();
  await toggleCameraIntent();
  const trackId = getState().videoTilesById['self-peer']?.track?.id;
  expect(trackId).toBe(`camera-${deviceId}`);
  return trackId ?? '';
}

beforeEach(async () => {
  await resetSession();
});

afterEach(async () => {
  vi.unstubAllGlobals();
  vi.restoreAllMocks();
  leaveRoom();
  await tick();
});

describe('Feature: video-feed, Property 9: Settings persistence and fallback', () => {
  // Feature: video-feed, Property 9: Settings persistence and fallback
  it('round-trips any persisted camera device id or null', async () => {
    await fc.assert(
      fc.asyncProperty(fc.option(fc.string(), { nil: null }), async (deviceId) => {
        mockStorage.clear();
        await setVideoInputDevice(deviceId);
        await expect(getVideoInputDevice()).resolves.toBe(deviceId);
      }),
      { numRuns: 100 },
    );
  });

  it('falls back to the browser default when the persisted camera is absent, without changing the stored value', async () => {
    const missingAndPresentDeviceArb = fc
      .tuple(fc.string({ minLength: 1, maxLength: 24 }), fc.string({ minLength: 1, maxLength: 24 }))
      .filter(([missingDeviceId, presentDeviceId]) => missingDeviceId !== presentDeviceId);

    await fc.assert(
      fc.asyncProperty(missingAndPresentDeviceArb, async ([missingDeviceId, presentDeviceId]) => {
        await resetSession();
        setEnumeratedVideoDevices([presentDeviceId]);
        await setVideoInputDevice(missingDeviceId);

        await driveToConnectedRoom();
        await toggleCameraIntent();

        expect(lastLiveKitModule).not.toBeNull();
        expect(lastLiveKitModule!.publishCameraCalls).toHaveLength(1);
        expect(lastLiveKitModule!.publishCameraCalls[0]).toMatchObject({
          deviceId: null,
        });
        await expect(getVideoInputDevice()).resolves.toBe(missingDeviceId);
        expect(
          getState().events.some(
            (event) =>
              event.message.includes('device_not_found') && event.message.includes(missingDeviceId),
          ),
        ).toBe(true);

        leaveRoom();
        await tick();
      }),
      { numRuns: 100 },
    );
  }, 30_000);

  it('keeps the stored selection and active local track unchanged on non-absence camera-change failures', async () => {
    const cameraFailureArb = fc
      .record({
        initialDeviceId: fc.string({ minLength: 1, maxLength: 24 }),
        nextDeviceId: fc.string({ minLength: 1, maxLength: 24 }),
        warningKind: fc.constantFrom<'permission_denied' | 'device_in_use' | 'hardware_error'>(
          'permission_denied',
          'device_in_use',
          'hardware_error',
        ),
      })
      .filter(({ initialDeviceId, nextDeviceId }) => initialDeviceId !== nextDeviceId);

    await fc.assert(
      fc.asyncProperty(cameraFailureArb, async ({ initialDeviceId, nextDeviceId, warningKind }) => {
        await resetSession();
        setEnumeratedVideoDevices([initialDeviceId, nextDeviceId]);
        const initialTrackId = await publishLocalCamera(initialDeviceId);

        replaceCameraDeviceImpl =
          warningKind === 'hardware_error'
            ? async () => {
                throw new Error('device swap failed');
              }
            : async () => {
                throw { kind: warningKind };
              };

        await changeSelectedCamera(nextDeviceId);

        expect(lastLiveKitModule).not.toBeNull();
        expect(lastLiveKitModule!.replaceCameraDeviceCalls).toEqual([nextDeviceId]);
        await expect(getVideoInputDevice()).resolves.toBe(initialDeviceId);
        expect(getState().cameraSelectedDeviceId).toBe(initialDeviceId);
        expect(getState().videoTilesById['self-peer']?.track?.id).toBe(initialTrackId);
        expect(getState().events.some((event) => event.message.includes(`(${warningKind})`))).toBe(
          true,
        );

        leaveRoom();
        await tick();
      }),
      { numRuns: 100 },
    );
  }, 30_000);
});
